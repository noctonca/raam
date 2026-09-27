//! 006/011's thin RAII wrapper around the raw `AMediaExtractor` NDK API (the
//! `ndk` crate has no binding for it), plus 027's `probe`: what the slideshow
//! needs to know about a clip before planning or playing it, and whether
//! this frame's decoder can play it at all.
use ndk::media::media_format::MediaFormat;
use raam_model::ClipInfo;
use std::fmt;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::ptr::NonNull;

/// Why `Extractor::open` failed.
pub enum OpenError {
    /// The file itself couldn't be opened (gone from the cache, say).
    File(String),
    /// It opened, but not as media the extractor reads.
    Media(String),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::File(why) | OpenError::Media(why) => f.write_str(why),
        }
    }
}

impl From<OpenError> for String {
    fn from(e: OpenError) -> String {
        e.to_string()
    }
}

pub struct Extractor {
    ptr: NonNull<ndk_sys::AMediaExtractor>,
    // Kept alive for the extractor's lifetime, even though
    // AMediaExtractor_setDataSourceFd is documented to dup() the fd.
    _file: File,
}

// Only ever touched from the one thread that owns it.
unsafe impl Send for Extractor {}

impl Extractor {
    /// Opens `path` with `AMediaExtractor_setDataSourceFd`, never the
    /// path-based call: on this device's libmediandk.so that one must run on
    /// a Java thread and segfaults on a plain Rust thread (006).
    pub fn open(path: &str) -> Result<Self, OpenError> {
        let file = File::open(path).map_err(|e| OpenError::File(format!("open {path}: {e}")))?;
        let len = file
            .metadata()
            .map_err(|e| OpenError::File(format!("stat {path}: {e}")))?
            .len();
        let ptr = unsafe { ndk_sys::AMediaExtractor_new() };
        let ptr = NonNull::new(ptr)
            .ok_or_else(|| OpenError::Media("AMediaExtractor_new returned null".into()))?;
        let status = unsafe {
            ndk_sys::AMediaExtractor_setDataSourceFd(
                ptr.as_ptr(),
                file.as_raw_fd(),
                0,
                len as ndk_sys::off64_t,
            )
        };
        if status != ndk_sys::media_status_t::AMEDIA_OK {
            unsafe { ndk_sys::AMediaExtractor_delete(ptr.as_ptr()) };
            return Err(OpenError::Media(format!(
                "AMediaExtractor_setDataSourceFd({path}): {status:?}"
            )));
        }
        Ok(Self { ptr, _file: file })
    }

    pub fn track_count(&self) -> usize {
        unsafe { ndk_sys::AMediaExtractor_getTrackCount(self.ptr.as_ptr()) }
    }

    pub fn track_format(&self, idx: usize) -> Option<MediaFormat> {
        let fmt_ptr = unsafe { ndk_sys::AMediaExtractor_getTrackFormat(self.ptr.as_ptr(), idx) };
        NonNull::new(fmt_ptr).map(|p| unsafe { MediaFormat::from_ptr(p) })
    }

    /// The first track whose MIME type starts with `prefix` ("video/" or
    /// "audio/"), with its format.
    pub fn find_track(&self, prefix: &str) -> Option<(usize, MediaFormat)> {
        (0..self.track_count()).find_map(|idx| {
            let mut format = self.track_format(idx)?;
            format
                .str("mime")
                .is_some_and(|m| m.starts_with(prefix))
                .then_some((idx, format))
        })
    }

    pub fn select_track(&self, idx: usize) -> Result<(), String> {
        let status = unsafe { ndk_sys::AMediaExtractor_selectTrack(self.ptr.as_ptr(), idx) };
        (status == ndk_sys::media_status_t::AMEDIA_OK)
            .then_some(())
            .ok_or(format!("selectTrack({idx}): {status:?}"))
    }

    /// Reads the current sample into `buf`: its size, or negative at the end.
    pub fn read_sample_data(&self, buf: &mut [u8]) -> isize {
        unsafe {
            ndk_sys::AMediaExtractor_readSampleData(self.ptr.as_ptr(), buf.as_mut_ptr(), buf.len())
        }
    }

    pub fn sample_time_us(&self) -> i64 {
        unsafe { ndk_sys::AMediaExtractor_getSampleTime(self.ptr.as_ptr()) }
    }

    /// Advances to the next sample; false once exhausted.
    pub fn advance(&self) -> bool {
        unsafe { ndk_sys::AMediaExtractor_advance(self.ptr.as_ptr()) }
    }

    /// To the sync sample at or before `us` (0 = the start, for a loop).
    pub fn seek_to(&self, us: i64) -> Result<(), String> {
        let status = unsafe {
            ndk_sys::AMediaExtractor_seekTo(
                self.ptr.as_ptr(),
                us,
                ndk_sys::SeekMode::AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC,
            )
        };
        (status == ndk_sys::media_status_t::AMEDIA_OK)
            .then_some(())
            .ok_or(format!("seekTo({us}): {status:?}"))
    }
}

impl Drop for Extractor {
    fn drop(&mut self) {
        unsafe { ndk_sys::AMediaExtractor_delete(self.ptr.as_ptr()) };
    }
}

/// The largest picture the frame's decoder declares (`OMX.rk.video_decoder.avc`
/// in /system/etc/media_codecs.xml: 1920x1088), and the only codec it has
/// in hardware: H.264. There is no HEVC decoder at all.
pub const MAX_LONG: u32 = 1920;
pub const MAX_SHORT: u32 = 1088;

/// Why this frame can't play a probed clip, if it can't. Device
/// capability data lives with the host (docs/ARCHITECTURE.md), so this is
/// the Android host's judgment, reached through the `MediaProbe` seam.
pub fn unplayable(info: &ClipInfo) -> Option<String> {
    if info.mime != "video/avc" {
        return Some(format!("{} (only H.264 has a decoder here)", info.mime));
    }
    let (long, short) = (
        info.coded_w.max(info.coded_h),
        info.coded_w.min(info.coded_h),
    );
    if long > MAX_LONG || short > MAX_SHORT {
        return Some(format!(
            "{}x{} is over the decoder's {MAX_LONG}x{MAX_SHORT}",
            info.coded_w, info.coded_h
        ));
    }
    None
}

/// Reads a clip's tracks without decoding anything. Works on any thread.
pub fn probe(path: &str) -> Result<ClipInfo, String> {
    let ex = Extractor::open(path)?;
    let (_, mut video) = ex.find_track("video/").ok_or("no video track")?;
    let mime = video.str("mime").unwrap_or("?").to_string();
    let audio = ex
        .find_track("audio/")
        .and_then(|(_, mut f)| f.str("mime").map(str::to_string));
    Ok(ClipInfo {
        mime,
        coded_w: video.i32("width").unwrap_or(0).max(0) as u32,
        coded_h: video.i32("height").unwrap_or(0).max(0) as u32,
        rotation: video.i32("rotation-degrees").unwrap_or(0).rem_euclid(360),
        duration_us: video.i64("durationUs").unwrap_or(0),
        has_audio: audio.is_some(),
        audio,
    })
}
