//! The provider model. A source of media is a `Provider`: it lists
//! `MediaRef`s and fetches a preview (or, for a clip, the video) for one.
//! A `MediaRef` carries only what the renderer needs - a stable id, the
//! size after rotation, the capture time (UTC ms, normalised here at the
//! boundary), the kind and an optional focus - so collage planning,
//! Fill/Fit and Ken Burns never know where a photo came from.
//!
//! Identity: every item also carries the SHA-1 of its original file.
//! Curation is keyed by that hash, so a local file renamed or moved keeps
//! its curation, and a photo in both sources shares one curation and plays
//! once. Immich's `checksum` is that SHA-1 already; the local folder
//! computes it (memoised by path, size and mtime).
//!
//! Two providers: `ImmichProvider` (the albums picked in settings; its
//! preview cache lives in library.rs, on top of it) and `LocalFolder`.
use crate::immich;
use raam_core::clock;
use raam_core::seams::MediaProbe;
use raam_model::limits;
use raam_model::{
    AlbumId, Focus, MediaKind, MediaRef, ProviderError, RemoteId, SourceKind, UserId,
};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

pub trait Provider {
    fn kind(&self) -> SourceKind;
    /// Everything this source currently offers.
    fn list(&mut self) -> Result<Vec<MediaRef>, ProviderError>;
    /// A JPEG of the item with its short side about `short_side` px (Immich
    /// serves its own fixed-size preview), oriented upright.
    fn fetch_preview(
        &mut self,
        media: &MediaRef,
        short_side: u32,
    ) -> Result<Vec<u8>, ProviderError>;
    /// The item's focus, if the source knows one.
    fn fetch_focus(&mut self, _media: &MediaRef) -> Result<Option<Focus>, ProviderError> {
        Ok(None)
    }
    /// The clip to play, written to `dest`; the bytes written. It stops
    /// after `max_bytes + 1`, so a count over `max_bytes` is a clip too big
    /// to keep, cut short.
    fn fetch_video(
        &mut self,
        _media: &MediaRef,
        _dest: &Path,
        _max_bytes: u64,
    ) -> Result<u64, ProviderError> {
        Err(ProviderError::Failed(
            "this source has no clips to fetch".into(),
        ))
    }
}

// ---- Immich --------------------------------------------------------------

pub struct ImmichProvider {
    client: Option<immich::Client>,
    /// The picked albums that are on the server, set by the library thread
    /// before each `list()`.
    albums: Vec<AlbumId>,
}

impl Default for ImmichProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ImmichProvider {
    pub fn new() -> Self {
        Self {
            client: None,
            albums: Vec::new(),
        }
    }

    pub fn set_albums(&mut self, albums: Vec<AlbumId>) {
        self.albums = albums;
    }

    pub fn albums(&self) -> Result<Vec<immich::RemoteAlbum>, ProviderError> {
        self.connected()?.albums()
    }

    pub fn user_id(&self) -> Result<UserId, ProviderError> {
        self.connected()?.user_id()
    }

    /// The client, once `with_config` has made one.
    fn connected(&self) -> Result<&immich::Client, ProviderError> {
        self.client
            .as_ref()
            .ok_or_else(|| ProviderError::Failed("no Immich client".into()))
    }

    /// Follows a server or key change from settings.
    fn client(&mut self, config: immich::Config) -> Result<&immich::Client, ProviderError> {
        let client = match self.client.take() {
            Some(c) if c.config == config => c,
            _ => immich::Client::new(config)?,
        };
        Ok(self.client.insert(client))
    }

    pub fn with_config(&mut self, config: immich::Config) -> Result<&mut Self, ProviderError> {
        self.client(config)?;
        Ok(self)
    }
}

impl Provider for ImmichProvider {
    fn kind(&self) -> SourceKind {
        SourceKind::Immich
    }

    /// The union of the picked albums, one item per asset however many of
    /// them it is in (each album is its own call; see `album_assets`).
    fn list(&mut self) -> Result<Vec<MediaRef>, ProviderError> {
        let client = self.connected()?;
        let mut out: Vec<MediaRef> = Vec::new();
        let mut index: HashMap<RemoteId, usize> = HashMap::new();
        for album in &self.albums {
            for a in client.album_assets(album)? {
                if let Some(&i) = index.get(&a.id) {
                    out[i].collections.push(album.clone());
                    continue;
                }
                index.insert(a.id.clone(), out.len());
                out.push(MediaRef {
                    id: a.id,
                    sha1: a.sha1_hex,
                    location: None,
                    width: a.width,
                    height: a.height,
                    taken_at_ms: a.taken_at_ms,
                    kind: if a.is_video {
                        MediaKind::Video
                    } else {
                        MediaKind::Photo
                    },
                    focus: None,
                    stamp: None,
                    collections: vec![album.clone()],
                });
            }
        }
        Ok(out)
    }

    fn fetch_preview(
        &mut self,
        media: &MediaRef,
        _short_side: u32,
    ) -> Result<Vec<u8>, ProviderError> {
        self.connected()?.preview(&media.id)
    }

    fn fetch_focus(&mut self, media: &MediaRef) -> Result<Option<Focus>, ProviderError> {
        Ok(Some(self.connected()?.faces(&media.id)?))
    }

    fn fetch_video(
        &mut self,
        media: &MediaRef,
        dest: &Path,
        max_bytes: u64,
    ) -> Result<u64, ProviderError> {
        self.connected()?.download_video(&media.id, dest, max_bytes)
    }
}

// ---- the local folder ------------------------------------------------------

pub struct LocalFolder {
    pub dir: String,
    /// Last scan's items by path, so unchanged files aren't rehashed.
    memo: HashMap<String, MediaRef>,
    /// Clips this frame can't play, by path and stamp, so they are
    /// logged once rather than at every scan.
    skipped: HashMap<String, (i64, i64)>,
    granted: bool,
    probe: Arc<dyn MediaProbe>,
    /// The host's one-time storage grant (docs/ARCHITECTURE.md, the root
    /// map), called once when the folder can't be read.
    grant: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl LocalFolder {
    /// `known`: the items already in the DB, as a head start for the memo.
    pub fn new(
        dir: String,
        known: Vec<MediaRef>,
        probe: Arc<dyn MediaProbe>,
        grant: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        let memo = known
            .into_iter()
            .filter_map(|m| Some((m.location.clone()?, m)))
            .collect();
        Self {
            dir,
            memo,
            skipped: HashMap::new(),
            granted: false,
            probe,
            grant,
        }
    }
}

impl LocalFolder {
    /// Creates the folder on first use. One that photos were found in is
    /// never made again, empty: gone, it's unmounted or moved, and an
    /// empty listing would make the sync drop every photo and preview.
    /// Missing, it's an error that drops nothing.
    fn create_if_new(&self, dir: &Path) -> std::io::Result<()> {
        if self.memo.is_empty() {
            std::fs::create_dir_all(dir)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "gone, with {} photos known there: not mounted?",
                    self.memo.len()
                ),
            ))
        }
    }
}

impl Provider for LocalFolder {
    fn kind(&self) -> SourceKind {
        SourceKind::Local
    }

    fn list(&mut self) -> Result<Vec<MediaRef>, ProviderError> {
        let dir = Path::new(&self.dir);
        if !dir.is_dir() && self.create_if_new(dir).is_ok() {
            log::info!("local: created {}", dir.display());
        }
        let mut files = Vec::new();
        if let Err(e) = list_media(dir, &mut files) {
            // Unreadable, or missing because it couldn't be created: both
            // are what no storage permission looks like. Grant once.
            if self.granted {
                return Err(ProviderError::Failed(format!(
                    "cannot read {}: {e}",
                    dir.display()
                )));
            }
            self.granted = true;
            log::warn!(
                "local: {} not usable ({e}), granting storage",
                dir.display()
            );
            let granted = (self.grant)();
            log::info!(
                "local: storage grant {}",
                if granted { "given" } else { "refused" }
            );
            // The remount after a grant lands asynchronously.
            std::thread::sleep(limits::STORAGE_GRANT_SETTLE);
            if !dir.is_dir() {
                self.create_if_new(dir)
                    .map_err(|e| ProviderError::Failed(format!("{}: {e}", dir.display())))?;
                log::info!("local: created {}", dir.display());
            }
            files.clear();
            list_media(dir, &mut files).map_err(|e| {
                ProviderError::Failed(format!("cannot read {}: {e}", dir.display()))
            })?;
        }
        let mut out = Vec::with_capacity(files.len());
        let mut memo = HashMap::with_capacity(files.len());
        let mut seen = std::collections::HashSet::new();
        let mut hashed = 0;
        let mut skipped = HashMap::new();
        for (path, stamp) in files {
            if self.skipped.get(&path) == Some(&stamp) {
                skipped.insert(path, stamp);
                continue;
            }
            let item = match self.memo.remove(&path) {
                Some(m) if m.stamp == Some(stamp) => m,
                _ if is_video_name(&path) => {
                    match describe_video(self.probe.as_ref(), &path, stamp) {
                        Ok(m) => {
                            hashed += 1;
                            m
                        }
                        Err(e) => {
                            log::warn!("local: skipping {path}: {e}");
                            skipped.insert(path, stamp);
                            continue;
                        }
                    }
                }
                _ => match describe(&path, stamp) {
                    Ok(m) => {
                        hashed += 1;
                        m
                    }
                    Err(e) => {
                        log::warn!("local: skipping {path}: {e}");
                        continue;
                    }
                },
            };
            // Two copies of one file in the folder are one item.
            if seen.insert(item.id.clone()) {
                out.push(item.clone());
            }
            memo.insert(path, item);
        }
        self.memo = memo;
        self.skipped = skipped;
        if hashed > 0 {
            log::info!("local: hashed {hashed} new or changed files");
        }
        Ok(out)
    }

    fn fetch_preview(
        &mut self,
        media: &MediaRef,
        short_side: u32,
    ) -> Result<Vec<u8>, ProviderError> {
        let path = media
            .location
            .as_deref()
            .ok_or(ProviderError::Failed("no location".into()))?;
        let t = clock::now();
        let p = make_preview(Path::new(path), short_side)
            .map_err(|e| ProviderError::Failed(format!("{path}: {e}")))?;
        log::info!(
            "local: preview of {path}: source {}x{} {}, preview {}x{} {} KB in {:?}",
            p.src.0,
            p.src.1,
            p.note,
            p.size.0,
            p.size.1,
            p.jpeg.len() / 1024,
            clock::elapsed(t)
        );
        Ok(p.jpeg)
    }
}

/// Why a file in the local folder couldn't be described or previewed.
#[derive(Debug)]
enum LocalError {
    /// Opening or reading the file.
    Io(std::io::Error),
    /// The JPEG's header or data doesn't decode, or won't scale.
    Decode(jpeg_decoder::Error),
    /// A JPEG, but neither RGB nor grayscale.
    PixelFormat(jpeg_decoder::PixelFormat),
    /// The decoder returned fewer or more pixels than the size it gave.
    SizeMismatch,
    /// The upright preview wouldn't encode.
    Encode(jpeg_encoder::EncodingError),
    /// The host's probe couldn't read the clip, in its own words (the
    /// probe seam's errors are still messages, raam#94).
    Probe(String),
    /// The frame can't decode this clip, and why.
    Unplayable(String),
}

impl From<std::io::Error> for LocalError {
    fn from(e: std::io::Error) -> Self {
        LocalError::Io(e)
    }
}

impl std::fmt::Display for LocalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalError::Io(e) => write!(f, "{e}"),
            LocalError::Decode(e) => write!(f, "jpeg: {e}"),
            LocalError::PixelFormat(p) => write!(f, "unsupported pixel format {p:?}"),
            LocalError::SizeMismatch => f.write_str("decoded size mismatch"),
            LocalError::Encode(e) => write!(f, "jpeg encode: {e}"),
            LocalError::Probe(why) => write!(f, "unreadable clip: {why}"),
            LocalError::Unplayable(why) => write!(f, "can't play here: {why}"),
        }
    }
}

impl std::error::Error for LocalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LocalError::Io(e) => Some(e),
            LocalError::Decode(e) => Some(e),
            LocalError::Encode(e) => Some(e),
            LocalError::PixelFormat(_)
            | LocalError::SizeMismatch
            | LocalError::Probe(_)
            | LocalError::Unplayable(_) => None,
        }
    }
}

/// Streaming SHA-1 of a file, lowercase hex: the content identity that
/// matches Immich's `checksum`. ring's legacy-use digest — SHA-1 is an
/// identity key here, not a security boundary.
fn sha1_of_file(path: &str) -> Result<String, LocalError> {
    let mut file = std::fs::File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    let mut buf = vec![0u8; limits::HASH_BUFFER_BYTES];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(immich::to_hex(ctx.finish().as_ref()))
}

/// A new or changed file's `MediaRef`: its SHA-1, its size after EXIF
/// rotation (from the JPEG header, no decode) and its capture time.
fn describe(path: &str, stamp: (i64, i64)) -> Result<MediaRef, LocalError> {
    let sha1 = sha1_of_file(path)?;
    let file = std::fs::File::open(path)?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::BufReader::new(file));
    dec.read_info().map_err(LocalError::Decode)?;
    let info = dec.info().expect(INFO_AFTER_OK);
    let (orientation, taken) = read_exif(Path::new(path));
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    let (width, height) = if (5..=8).contains(&orientation) {
        (h, w)
    } else {
        (w, h)
    };
    Ok(MediaRef {
        id: RemoteId::new(sha1.clone()),
        sha1: Some(sha1),
        location: Some(path.to_string()),
        width,
        height,
        // A photo with no EXIF date counts as taken when it was last written.
        taken_at_ms: taken.or(Some(stamp.1)),
        kind: MediaKind::Photo,
        focus: None,
        stamp: Some(stamp),
        collections: Vec::new(),
    })
}

fn is_video_name(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    VIDEO_EXTENSIONS.iter().any(|e| p.ends_with(e))
}

/// A clip in the folder: its SHA-1 (curation key, and the match with an
/// Immich original), its size as shown (after the container's rotation)
/// and whether this frame can decode it at all. Undecodable clips are
/// skipped here, so they never reach the queue.
fn describe_video(
    probe: &dyn MediaProbe,
    path: &str,
    stamp: (i64, i64),
) -> Result<MediaRef, LocalError> {
    let info = probe.probe(path).map_err(LocalError::Probe)?;
    if let Some(why) = probe.unplayable(&info) {
        return Err(LocalError::Unplayable(why));
    }
    let sha1 = sha1_of_file(path)?;
    let (width, height) = info.display();
    log::info!(
        "local: {path}: {} {}x{} rotation {} ({width}x{height} shown), {:.1}s, audio {:?}",
        info.mime,
        info.coded_w,
        info.coded_h,
        info.rotation,
        info.duration_us as f64 / 1e6,
        info.audio
    );
    Ok(MediaRef {
        id: RemoteId::new(sha1.clone()),
        sha1: Some(sha1),
        location: Some(path.to_string()),
        width,
        height,
        taken_at_ms: Some(stamp.1),
        kind: MediaKind::Video,
        focus: None,
        stamp: Some(stamp),
        collections: Vec::new(),
    })
}

/// Clips the folder also takes (only H.264 plays; see `describe_video`).
const VIDEO_EXTENSIONS: [&str; 4] = [".mp4", ".m4v", ".mov", ".3gp"];

/// Every .jpg/.jpeg (and clip) under `dir` (recursively, skipping
/// dot-files) with its (bytes, mtime ms).
///
/// An unreadable subfolder is logged and passed over; only `dir` itself
/// failing is an error (it is how no storage permission looks).
fn list_media(dir: &Path, out: &mut Vec<(String, (i64, i64))>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
        if name.starts_with('.') {
            continue;
        }
        let meta = e.metadata()?;
        if meta.is_dir() {
            if let Err(err) = list_media(&e.path(), out) {
                log::warn!("local: skipping folder {}: {err}", e.path().display());
            }
        } else if name.ends_with(".jpg") || name.ends_with(".jpeg") || is_video_name(&name) {
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            out.push((
                e.path().to_string_lossy().into_owned(),
                (crate::db::sql_int(meta.len()), mtime_ms),
            ));
        }
    }
    Ok(())
}

/// jpeg-decoder has the info once `read_info` or `decode` returned Ok.
const INFO_AFTER_OK: &str = "jpeg-decoder's info after a successful read";

struct Preview {
    jpeg: Vec<u8>,
    size: (u32, u32),
    src: (u32, u32),
    note: String,
}

/// An upright preview with its short side at least `short_side` px (or the
/// original size): decoded at 1/2, 1/4 or 1/8 scale in the DCT where that
/// still covers it, so a 12 MP photo never needs ~48 MB of RGBA.
fn make_preview(path: &Path, short_side: u32) -> Result<Preview, LocalError> {
    let (orientation, _) = read_exif(path);
    let file = std::fs::File::open(path)?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::BufReader::new(file));
    dec.read_info().map_err(LocalError::Decode)?;
    let info = dec.info().expect(INFO_AFTER_OK);
    let (src_w, src_h) = (u32::from(info.width), u32::from(info.height));
    let short = src_w.min(src_h).max(1);
    let (req_w, req_h) = if short > short_side {
        (
            // Scaled down (short_side < short), so no larger than the
            // u16 side it came from.
            u16::try_from((src_w * short_side).div_ceil(short)).expect("a scaled-down u16 side"),
            u16::try_from((src_h * short_side).div_ceil(short)).expect("a scaled-down u16 side"),
        )
    } else {
        (info.width, info.height)
    };
    let (w, h) = dec.scale(req_w, req_h).map_err(LocalError::Decode)?;
    let pixels = dec.decode().map_err(LocalError::Decode)?;
    let info = dec.info().expect(INFO_AFTER_OK);
    let rgb = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&l| [l, l, l]).collect(),
        other => return Err(LocalError::PixelFormat(other)),
    };
    if rgb.len() != usize::from(w) * usize::from(h) * 3 {
        return Err(LocalError::SizeMismatch);
    }
    let (ow, oh, rgb) = apply_orientation(orientation, w, h, rgb);
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 88)
        .encode(&rgb, ow, oh, jpeg_encoder::ColorType::Rgb)
        .map_err(LocalError::Encode)?;
    Ok(Preview {
        jpeg,
        size: (u32::from(ow), u32::from(oh)),
        src: (src_w, src_h),
        note: format!("orientation {orientation}, decoded at {w}x{h}"),
    })
}

/// The eight EXIF orientations applied to raw RGB rows (forty lines beat
/// the `image` crate's whole dependency tree).
/// Sides are u16, as JPEG's are.
fn apply_orientation(orientation: u8, w: u16, h: u16, rgb: Vec<u8>) -> (u16, u16, Vec<u8>) {
    if orientation <= 1 || orientation > 8 {
        return (w, h, rgb);
    }
    let swap = matches!(orientation, 5..=8);
    let sides = if swap { (h, w) } else { (w, h) };
    let (w, h) = (usize::from(w), usize::from(h));
    let ow = usize::from(sides.0);
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match orientation {
                2 => (w - 1 - x, y),         // mirrored horizontally
                3 => (w - 1 - x, h - 1 - y), // rotated 180
                4 => (x, h - 1 - y),         // mirrored vertically
                5 => (y, x),                 // transposed
                6 => (h - 1 - y, x),         // rotated 90 CW
                7 => (h - 1 - y, w - 1 - x), // transverse
                8 => (y, w - 1 - x),         // rotated 270 CW
                _ => unreachable!(),
            };
            let src = (y * w + x) * 3;
            let dst = (dy * ow + dx) * 3;
            out[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
        }
    }
    (sides.0, sides.1, out)
}

/// (EXIF orientation, DateTimeOriginal as UTC epoch ms). EXIF times are
/// naive local times, so bionic's mktime applies the frame's zone.
fn read_exif(path: &Path) -> (u8, Option<i64>) {
    let Ok(file) = std::fs::File::open(path) else {
        return (1, None);
    };
    let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file))
    else {
        return (1, None);
    };
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        // Out of range is as unknown as missing: upright.
        .and_then(|o| u8::try_from(o).ok())
        .unwrap_or(1);
    let taken = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            exif::Value::Ascii(v) => v.first().and_then(|b| exif::DateTime::from_ascii(b).ok()),
            _ => None,
        });
    let taken_ms = taken.and_then(|d| {
        // SAFETY: `tm` is plain integers plus, on some libcs, a `tm_zone`
        // pointer, for which null is valid; all-zero is a valid `tm`.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = i32::from(d.year) - 1900;
        tm.tm_mon = i32::from(d.month) - 1;
        tm.tm_mday = i32::from(d.day);
        tm.tm_hour = i32::from(d.hour);
        tm.tm_min = i32::from(d.minute);
        tm.tm_sec = i32::from(d.second);
        tm.tm_isdst = -1;
        // SAFETY: `tm` is a valid, exclusively borrowed `tm` for the call;
        // mktime only normalises it in place and reads the zone.
        let t = unsafe { libc::mktime(&mut tm) };
        // time_t is i32 on the frame's 32-bit ARM and i64 on 64-bit
        // hosts, where `i64::from` would be a useless conversion.
        #[allow(clippy::cast_lossless, reason = "time_t's width varies by target")]
        let ms = t as i64 * 1000;
        (t != -1).then_some(ms)
    });
    (orientation, taken_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use raam_model::ClipInfo;

    /// A probe that fails, or finds every clip unplayable.
    struct Probe(Result<(), &'static str>);

    impl MediaProbe for Probe {
        fn probe(&self, _path: &str) -> Result<ClipInfo, String> {
            self.0.map_err(String::from)?;
            Ok(ClipInfo {
                mime: "video/hevc".into(),
                coded_w: 1920,
                coded_h: 1080,
                rotation: 0,
                duration_us: 1_000_000,
                has_audio: false,
                audio: None,
            })
        }
        fn unplayable(&self, _info: &ClipInfo) -> Option<String> {
            Some("not H.264".into())
        }
    }

    fn jpeg(colour: jpeg_encoder::ColorType, pixel: &[u8]) -> Vec<u8> {
        let data = pixel.repeat(4 * 2);
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 100)
            .encode(&data, 4, 2, colour)
            .unwrap();
        out
    }

    /// A folder photos were found in that is gone (a drive not mounted
    /// yet) is an error, not an empty folder made in its place, which the
    /// sync would take as every photo deleted.
    #[test]
    fn a_folder_that_is_gone_is_an_error_not_made_again() {
        let dir = std::env::temp_dir().join(format!("raam-gone-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        let photo = dir.join("a.jpg").to_string_lossy().into_owned();
        let known = MediaRef {
            id: RemoteId::new("ab12".to_string()),
            sha1: Some("ab12".into()),
            location: Some(photo),
            width: 4,
            height: 2,
            taken_at_ms: None,
            kind: MediaKind::Photo,
            focus: None,
            stamp: Some((1, 2)),
            collections: Vec::new(),
        };
        let mut folder = LocalFolder::new(
            dir.to_string_lossy().into_owned(),
            vec![known],
            Arc::new(Probe(Ok(()))),
            Arc::new(|| false),
        );
        let e = folder.list().unwrap_err();
        assert!(e.to_string().contains("not mounted?"), "{e}");
        assert!(!dir.exists(), "made again, empty");

        // First use: nothing known there, so it's made.
        let mut fresh = LocalFolder::new(
            dir.to_string_lossy().into_owned(),
            Vec::new(),
            Arc::new(Probe(Ok(()))),
            Arc::new(|| false),
        );
        assert!(fresh.list().unwrap().is_empty());
        assert!(dir.is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file the folder can't use says why, as its own variant, and is
    /// skipped; a good one is described and previewed.
    #[test]
    fn a_local_file_is_described_or_says_why_not() {
        let dir = std::env::temp_dir().join(format!("raam-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p.to_string_lossy().into_owned()
        };

        let good = file(
            "good.jpg",
            &jpeg(jpeg_encoder::ColorType::Rgb, &[9, 99, 199]),
        );
        let m = describe(&good, (1, 2)).unwrap();
        assert_eq!((m.width, m.height, m.kind), (4, 2, MediaKind::Photo));
        assert_eq!(m.id.as_str().len(), 40, "a hex SHA-1");
        assert_eq!(make_preview(Path::new(&good), 1).unwrap().src, (4, 2));

        let junk = file("junk.jpg", b"not a jpeg");
        assert!(matches!(
            describe(&junk, (1, 2)),
            Err(LocalError::Decode(_))
        ));
        assert!(matches!(
            make_preview(Path::new(&junk), 1),
            Err(LocalError::Decode(_))
        ));
        let cmyk = file(
            "cmyk.jpg",
            &jpeg(jpeg_encoder::ColorType::Cmyk, &[0, 0, 0, 0]),
        );
        assert!(matches!(
            make_preview(Path::new(&cmyk), 1),
            Err(LocalError::PixelFormat(jpeg_decoder::PixelFormat::CMYK32))
        ));
        let gone = dir.join("gone.jpg").to_string_lossy().into_owned();
        assert!(matches!(describe(&gone, (1, 2)), Err(LocalError::Io(_))));

        let clip = file("clip.mp4", b"");
        assert!(matches!(
            describe_video(&Probe(Err("no moov")), &clip, (1, 2)),
            Err(LocalError::Probe(why)) if why == "no moov"
        ));
        assert!(matches!(
            describe_video(&Probe(Ok(())), &clip, (1, 2)),
            Err(LocalError::Unplayable(why)) if why == "not H.264"
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A 2x1 image: red then green.
    const RG: [u8; 6] = [255, 0, 0, 0, 255, 0];

    #[test]
    fn orientation_cases() {
        // 1: untouched.
        assert_eq!(apply_orientation(1, 2, 1, RG.to_vec()), (2, 1, RG.to_vec()));
        // 2: mirrored horizontally -> green, red.
        assert_eq!(
            apply_orientation(2, 2, 1, RG.to_vec()).2,
            vec![0, 255, 0, 255, 0, 0]
        );
        // 3: rotated 180 of a 2x1 = mirrored -> green, red.
        assert_eq!(
            apply_orientation(3, 2, 1, RG.to_vec()).2,
            vec![0, 255, 0, 255, 0, 0]
        );
        // 6: rotated 90 CW -> 1x2, red on top.
        let (w, h, px) = apply_orientation(6, 2, 1, RG.to_vec());
        assert_eq!((w, h), (1, 2));
        assert_eq!(px, RG.to_vec());
        // 8: rotated 270 CW -> 1x2, green on top.
        let (_, _, px) = apply_orientation(8, 2, 1, RG.to_vec());
        assert_eq!(px, vec![0, 255, 0, 255, 0, 0]);
    }
}
