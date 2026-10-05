//! A thin RAII wrapper around the raw `AMediaExtractor` NDK API (the `ndk`
//! crate has no binding for it), plus `probe`: what the slideshow needs to
//! know about a clip before planning or playing it, and whether this
//! frame's decoder can play it at all.
use ndk::media::media_format::MediaFormat;
use raam_model::ClipInfo;
use std::fmt;
use std::fs::File;
use std::mem::MaybeUninit;
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

// SAFETY: an AMediaExtractor has no thread affinity, and moving the
// value moves sole ownership with it; it isn't Sync, so it is only ever
// touched from the one thread that owns it.
unsafe impl Send for Extractor {}

impl Extractor {
    /// Opens `path` with `AMediaExtractor_setDataSourceFd`, never the
    /// path-based call: on this device's libmediandk.so that one must run on
    /// a Java thread and segfaults on a plain Rust thread.
    pub fn open(path: &str) -> Result<Self, OpenError> {
        let file = File::open(path).map_err(|e| OpenError::File(format!("open {path}: {e}")))?;
        let len = file
            .metadata()
            .map_err(|e| OpenError::File(format!("stat {path}: {e}")))?
            .len();
        // SAFETY: no arguments; a null result is caught just below.
        let ptr = unsafe { ndk_sys::AMediaExtractor_new() };
        let ptr = NonNull::new(ptr)
            .ok_or_else(|| OpenError::Media("AMediaExtractor_new returned null".into()))?;
        // SAFETY: `ptr` is the live extractor just made, and the fd is
        // `file`'s, open for this call (and kept open in `_file` after it).
        let status = unsafe {
            ndk_sys::AMediaExtractor_setDataSourceFd(
                ptr.as_ptr(),
                file.as_raw_fd(),
                0,
                ndk_sys::off64_t::try_from(len).expect("a file's length fits off64_t"),
            )
        };
        if status != ndk_sys::media_status_t::AMEDIA_OK {
            // SAFETY: `ptr` is ours alone and no `Extractor` was built from
            // it, so no Drop deletes it a second time.
            unsafe { ndk_sys::AMediaExtractor_delete(ptr.as_ptr()) };
            return Err(OpenError::Media(format!(
                "AMediaExtractor_setDataSourceFd({path}): {status:?}"
            )));
        }
        Ok(Self { ptr, _file: file })
    }

    pub fn track_count(&self) -> usize {
        // SAFETY: `ptr` is non-null and live until Drop, which runs once.
        unsafe { ndk_sys::AMediaExtractor_getTrackCount(self.ptr.as_ptr()) }
    }

    pub fn track_format(&self, idx: usize) -> Option<MediaFormat> {
        // SAFETY: `ptr` is live until Drop; an out-of-range `idx` gives null.
        let fmt_ptr = unsafe { ndk_sys::AMediaExtractor_getTrackFormat(self.ptr.as_ptr(), idx) };
        // SAFETY: getTrackFormat hands back a new format the caller owns,
        // so `MediaFormat` may take it and delete it on drop.
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
        // SAFETY: `ptr` is live until Drop; a bad `idx` is an error status.
        let status = unsafe { ndk_sys::AMediaExtractor_selectTrack(self.ptr.as_ptr(), idx) };
        (status == ndk_sys::media_status_t::AMEDIA_OK)
            .then_some(())
            .ok_or(format!("selectTrack({idx}): {status:?}"))
    }

    /// Reads the current sample into `buf`: its size, or negative at the end.
    /// The codec's input buffer may be uninitialised, so it is taken as
    /// such and never seen as `&mut [u8]`.
    pub fn read_sample_data(&self, buf: &mut [MaybeUninit<u8>]) -> isize {
        // SAFETY: the extractor writes at most `buf.len()` bytes to
        // `buf`, which is valid for writes of that many; it never reads them.
        unsafe {
            ndk_sys::AMediaExtractor_readSampleData(
                self.ptr.as_ptr(),
                buf.as_mut_ptr().cast::<u8>(),
                buf.len(),
            )
        }
    }

    pub fn sample_time_us(&self) -> i64 {
        // SAFETY: `ptr` is non-null and live until Drop, which runs once.
        unsafe { ndk_sys::AMediaExtractor_getSampleTime(self.ptr.as_ptr()) }
    }

    /// Advances to the next sample; false once exhausted.
    pub fn advance(&self) -> bool {
        // SAFETY: `ptr` is non-null and live until Drop, which runs once.
        unsafe { ndk_sys::AMediaExtractor_advance(self.ptr.as_ptr()) }
    }

    /// To the sync sample at or before `us` (0 = the start, for a loop).
    pub fn seek_to(&self, us: i64) -> Result<(), String> {
        // SAFETY: `ptr` is non-null and live until Drop, which runs once.
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
        // SAFETY: `open` made `ptr` and only this Drop frees it, once;
        // nothing borrowed from it (track formats are copies) outlives `self`.
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
    // Missing, negative or 0 (a broken header) is refused, rather than a
    // 0x0 clip that passes `unplayable` and reaches planning.
    let side = |key: &str| {
        video
            .i32(key)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|&v| v > 0)
            .ok_or_else(|| format!("no usable video {key}"))
    };
    let coded_w = side("width")?;
    let coded_h = side("height")?;
    Ok(ClipInfo {
        mime,
        coded_w,
        coded_h,
        rotation: video.i32("rotation-degrees").unwrap_or(0).rem_euclid(360),
        // Missing: 0, a length not known; nothing plans by it.
        duration_us: video.i64("durationUs").unwrap_or(0),
        has_audio: audio.is_some(),
        audio,
    })
}
