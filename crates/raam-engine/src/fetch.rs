//! The fetch thread: the collage planner, fed from the DB. Every photo from
//! the enabled sources (the Immich albums picked in settings and the local
//! folder, library.rs) goes into one merged queue, so each source shows in
//! proportion to its size. Pixels come from the local preview, the Immich
//! cache, or (online, uncached) the server, which also caches them;
//! offline, only cached Immich photos are queued. The shuffle is seeded and
//! the seed plus the shown photo are saved in `playback`, so a relaunch
//! (the wake alarm) resumes where it was.
//!
//! The queue is a shuffled list that is walked like Frameo walks its
//! ordered media list: the next group is planned from the assets' oriented
//! `width`/`height` (Immich already swaps them for EXIF rotation), using
//! `collage::pick`, before any pixels are fetched. The plan is parked for
//! the render thread, then its previews are fetched and handed over one
//! tile at a time, so at most one decoded preview is in memory at once.
//! Prev asks for an earlier plan by its asset ids through a request slot;
//! the host's `Waker` wakes the render loop whenever something lands.
//!
//! The render thread reaches all this only through the core's `TileSource`
//! seam (raam-core source.rs), which `FetchShared` implements; the seam's
//! types (`Plan`, `MediaItem`, `Photo`, `TilePhoto`) live in raam-model.
use crate::Host;
use crate::db;
use crate::library::{self, Library};
use crate::provider::{ImmichProvider, Provider};
use raam_core::clock;
use raam_core::collage::{self, Orientation};
use raam_core::source::{TileSource, shrink_to_cover};
use raam_model::limits::{
    FETCH_BACKOFF_WAIT, FETCH_EMPTY_WAIT, FETCH_FAILED_WAIT, FETCH_SKIP_WAIT, FETCH_SLOT_POLL,
    FETCH_TILE_POLL,
};
use raam_model::{MediaItem, MediaRef, Photo, Plan, SourceKind, TilePhoto, VideoClip};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Slot {
    plan: Option<Plan>,
    tile: Option<TilePhoto>,
    request: Option<Plan>,
    failed: Option<u64>,
    /// True from parking a plan until the render thread shows that collage
    /// (or the plan fails): the fetch thread plans nothing new meanwhile.
    pending: bool,
}

/// What the fetch thread needs to size tiles: the screen and the
/// separator half-margin, in px.
#[derive(Clone, Copy)]
pub struct Screen {
    pub width: i32,
    pub height: i32,
    pub margin: i32,
}

pub struct FetchShared {
    slot: Mutex<Slot>,
    /// The collage max setting (1 = off), read at each planning step.
    max_group: AtomicUsize,
    /// Clips are passed over while the render thread backs off after
    /// a decoder failure.
    skip_videos: AtomicBool,
}

impl TileSource for FetchShared {
    fn take_plan(&self) -> Option<Plan> {
        self.slot.lock().unwrap().plan.take()
    }

    fn take_tile(&self, seq: u64) -> Option<TilePhoto> {
        let mut slot = self.slot.lock().unwrap();
        match &slot.tile {
            Some(t) if t.seq == seq => slot.tile.take(),
            Some(_) => {
                slot.tile = None;
                None
            }
            None => None,
        }
    }

    fn take_failed(&self) -> Option<u64> {
        self.slot.lock().unwrap().failed.take()
    }

    fn consumed(&self) {
        self.slot.lock().unwrap().pending = false;
    }

    fn request(&self, plan: Plan) {
        let mut slot = self.slot.lock().unwrap();
        slot.request = Some(plan);
        slot.failed = None;
    }

    fn tile_is_clip(&self, seq: u64) -> bool {
        self.slot
            .lock()
            .unwrap()
            .tile
            .as_ref()
            .is_some_and(|t| t.seq == seq && t.photo.video.is_some())
    }

    fn set_skip_videos(&self, skip: bool) {
        if self.skip_videos.swap(skip, Ordering::Relaxed) != skip {
            log::info!(
                "clips {} while planning",
                if skip { "passed over" } else { "planned again" }
            );
        }
    }
}

impl FetchShared {
    pub fn set_max_group(&self, max: usize) {
        self.max_group.store(max, Ordering::Relaxed);
    }
}

pub fn spawn(host: Host, max_group: usize, screen: Screen, lib: Arc<Library>) -> Arc<FetchShared> {
    let shared = Arc::new(FetchShared {
        slot: Mutex::new(Slot::default()),
        max_group: AtomicUsize::new(max_group),
        skip_videos: AtomicBool::new(false),
    });
    let s = shared.clone();
    std::thread::spawn(move || fetch_loop(s, host, screen, lib));
    shared
}

/// The merged queue. Its order is the entries sorted by a hash of (seed,
/// asset id), a keyed shuffle: photos added or removed mid-pass (a sync, a
/// scan, a source toggled, going offline) leave everyone else's order
/// alone, and the position is just the rank of the last photo planned, so
/// resuming needs only the seed and that photo.
struct Queue {
    entries: Vec<MediaItem>,
    seed: u64,
    /// Rank of the last planned photo; `None` = the start of a pass.
    cursor: Option<u64>,
    generation: u64,
    counter: u64,
    only_videos: bool,
}

impl Queue {
    fn rank(&self, asset: i64) -> u64 {
        rank(self.seed, asset)
    }

    fn sort(&mut self) {
        let seed = self.seed;
        self.entries.sort_by_key(|e| rank(seed, e.asset));
    }

    fn start(&self) -> usize {
        match self.cursor {
            Some(c) => self.entries.partition_point(|e| self.rank(e.asset) <= c),
            None => 0,
        }
    }

    /// Up to the next `n` entries. Like Frameo's partition of its list, the
    /// group at the end of a pass can be short; the next pass reshuffles.
    fn peek(&mut self, n: usize) -> Vec<MediaItem> {
        if self.start() >= self.entries.len() {
            self.reshuffle();
        }
        let start = self.start();
        let end = (start + n).min(self.entries.len());
        self.entries[start..end].to_vec()
    }

    fn advance(&mut self, window: &[MediaItem], n: usize) {
        if let Some(last) = window[..n.min(window.len())].last() {
            self.cursor = Some(self.rank(last.asset));
        }
    }

    fn reshuffle(&mut self) {
        self.counter += 1;
        self.seed = new_seed(self.counter);
        self.cursor = None;
        self.sort();
        log::info!(
            "queue reshuffled ({} photos, seed {:016x})",
            self.entries.len(),
            self.seed
        );
    }
}

/// splitmix64 of the seed and the asset id.
fn rank(seed: u64, asset: i64) -> u64 {
    let mut z = seed ^ (asset as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn new_seed(counter: u64) -> u64 {
    // Wall time, through the Clock seam, so each boot shuffles differently;
    // monotonic time reads much the same at every boot.
    let nanos = clock::wall().as_nanos() as u64;
    rank(nanos, counter as i64)
}

fn fetch_loop(shared: Arc<FetchShared>, host: Host, screen: Screen, lib: Arc<Library>) {
    let mut immich = ImmichProvider::new();
    let mut queue = Queue {
        entries: Vec::new(),
        seed: new_seed(0),
        cursor: None,
        generation: 0,
        counter: 0,
        only_videos: false,
    };
    if let Some((current, seed)) = db::load_playback(&lib.db.lock().unwrap()) {
        queue.seed = seed;
        // Just below the saved photo's rank, so it is the first one shown.
        queue.cursor = current.and_then(|c| rank(seed, c).checked_sub(1));
        log::info!("resuming the saved queue at asset {current:?} (seed {seed:016x})");
    }
    let mut seq: u64 = 0;
    // The last planned (not Prev-requested) collage's first photo: saved to
    // `playback` once the render thread has shown it.
    let mut to_save: Option<i64> = None;
    let mut empty_logged = false;

    loop {
        let requested = loop {
            {
                let mut slot = shared.slot.lock().unwrap();
                if let Some(p) = slot.request.take() {
                    break Some(p);
                }
                if !slot.pending {
                    break None;
                }
            }
            std::thread::sleep(FETCH_SLOT_POLL);
        };
        if let Some(asset) = to_save.take()
            && let Err(e) = db::save_playback(&lib.db.lock().unwrap(), asset, queue.seed)
        {
            log::error!("saving playback: {e}");
        }
        let is_request = requested.is_some();

        let mut plan = match requested {
            Some(p) => p,
            None => {
                let generation = lib.generation();
                // Test-only: `debug.video.only_videos=1` plays only the clips.
                let only_videos = host.switches.get("debug.video.only_videos").trim() == "1";
                if generation != queue.generation || only_videos != queue.only_videos {
                    queue.only_videos = only_videos;
                    let online = lib.online();
                    let t = clock::now();
                    queue.entries = db::eligible(&lib.db.lock().unwrap(), !online);
                    if only_videos {
                        queue.entries.retain(|e| e.video);
                        log::warn!("debug.video.only_videos is set: the queue holds only clips");
                    }
                    // Test-only: `debug.video.only_path=<text>` keeps only
                    // items whose file path contains it (the sync clip).
                    let only_path = host
                        .switches
                        .get("debug.video.only_path")
                        .trim()
                        .to_string();
                    if !only_path.is_empty() {
                        queue.entries.retain(|e| {
                            e.location
                                .as_deref()
                                .is_some_and(|l| l.contains(&only_path))
                        });
                        log::warn!(
                            "debug.video.only_path={only_path}: {} items left",
                            queue.entries.len()
                        );
                    }
                    queue.generation = generation;
                    queue.sort();
                    let local = queue
                        .entries
                        .iter()
                        .filter(|e| e.source == SourceKind::Local)
                        .count();
                    log::info!(
                        "queue reloaded: {} items ({} Immich{}, {local} local, {} clips) in {:?}, {} left this pass",
                        queue.entries.len(),
                        queue.entries.len() - local,
                        if online { "" } else { " cached, offline" },
                        queue.entries.iter().filter(|e| e.video).count(),
                        clock::elapsed(t),
                        queue.entries.len() - queue.start().min(queue.entries.len()),
                    );
                }
                if queue.entries.is_empty() {
                    if !empty_logged {
                        log::warn!("nothing to show yet (no enabled source has a photo ready)");
                        empty_logged = true;
                    }
                    std::thread::sleep(FETCH_EMPTY_WAIT);
                    continue;
                }
                empty_logged = false;
                let mut max = shared.max_group.load(Ordering::Relaxed).max(1);
                let mut window = queue.peek(max.min(collage::LARGEST_LAYOUT));
                // Backing off after a decoder failure: a clip at the front
                // is passed over (the queue moves past it).
                if shared.skip_videos.load(Ordering::Relaxed)
                    && queue.entries.iter().all(|e| e.video)
                {
                    // Nothing but clips (`only_videos`): wait the backoff out
                    // rather than spin through them.
                    std::thread::sleep(FETCH_BACKOFF_WAIT);
                    continue;
                }
                if shared.skip_videos.load(Ordering::Relaxed)
                    && window.first().is_some_and(|e| e.video)
                {
                    log::info!(
                        "clip {} passed over (backing off after a decoder failure)",
                        window[0].asset
                    );
                    queue.advance(&window, 1);
                    std::thread::sleep(FETCH_SKIP_WAIT);
                    continue;
                }
                // A clip is always shown alone, as Frameo shows it (its own
                // page). A window starting with one is that clip; one further
                // in ends the window before it, so it starts the next plan.
                match window.iter().position(|e| e.video) {
                    Some(0) => {
                        window.truncate(1);
                        max = 1;
                    }
                    Some(i) => window.truncate(i),
                    None => {}
                }
                let orientations: Vec<Orientation> = window
                    .iter()
                    .map(|e| Orientation::of(e.width, e.height))
                    .collect();
                let counter = &mut queue.counter;
                let choice = collage::pick(&orientations, max, |n| {
                    *counter += 1;
                    pseudo_random_below(n, *counter)
                });
                queue.advance(&window, choice.consumed());
                let assets: Vec<MediaItem> = choice
                    .slot_to_item
                    .iter()
                    .map(|&i| window[i].clone())
                    .collect();
                log::info!(
                    "planned {} from window {:?} (max {max}): slots {:?}",
                    choice
                        .layout
                        .map_or("1 (single)", |i| collage::LAYOUTS[i].name),
                    window
                        .iter()
                        .zip(&orientations)
                        .map(|(e, o)| match (e.video, *o == Orientation::Landscape) {
                            (true, _) => 'V',
                            (false, true) => 'L',
                            (false, false) => 'P',
                        })
                        .collect::<String>(),
                    assets
                        .iter()
                        .map(|a| format!("{}{}:{}x{}", label(a.source), a.asset, a.width, a.height))
                        .collect::<Vec<_>>(),
                );
                Plan {
                    seq: 0,
                    layout: choice.layout,
                    assets,
                }
            }
        };
        seq += 1;
        plan.seq = seq;
        {
            let mut slot = shared.slot.lock().unwrap();
            slot.plan = Some(plan.clone());
            slot.tile = None;
            slot.pending = true;
        }
        host.waker.wake();

        let plan_start = clock::now();
        let mut abandoned = false;
        let rects = match plan.layout {
            Some(i) => collage::LAYOUTS[i].rects(screen.width, screen.height, screen.margin),
            None => vec![collage::Rect {
                x: 0,
                y: 0,
                w: screen.width,
                h: screen.height,
            }],
        };
        for (slot_idx, entry) in plan.assets.iter().enumerate() {
            let fetch_start = clock::now();
            match load_photo(&lib, &mut immich, entry) {
                Ok((mut photo, from)) => {
                    let preview = (photo.width, photo.height);
                    let resize_time = if photo.video.is_some() {
                        Duration::ZERO
                    } else {
                        shrink_to_cover(&mut photo, rects[slot_idx])
                    };
                    let planned = Orientation::of(entry.width, entry.height);
                    let decoded = Orientation::of(photo.width, photo.height);
                    if planned != decoded {
                        log::warn!(
                            "asset {} planned as {planned:?} from metadata {}x{} but the preview is {}x{}",
                            entry.asset,
                            entry.width,
                            entry.height,
                            photo.width,
                            photo.height
                        );
                    }
                    log::info!(
                        "fetched tile {slot_idx}/{} {}x{} (preview {}x{}, shrunk in {resize_time:?}) {}{} from {from} (requested={is_request}) in {:?} - face {:?}, fill centre ({:.3},{:.3})",
                        plan.assets.len(),
                        photo.width,
                        photo.height,
                        preview.0,
                        preview.1,
                        label(entry.source),
                        entry.asset,
                        clock::elapsed(fetch_start),
                        photo.face_focal,
                        photo.fill_centre.0,
                        photo.fill_centre.1,
                    );
                    shared.slot.lock().unwrap().tile = Some(TilePhoto {
                        seq,
                        slot: slot_idx,
                        photo,
                    });
                    host.waker.wake();
                    // Hand over one decoded preview at a time.
                    loop {
                        {
                            let mut slot = shared.slot.lock().unwrap();
                            // The last tile taken: the plan is fetched, even
                            // if the render thread has already shown it and
                            // called `consumed` (the first collage, or one
                            // the slideshow was waiting on). Checked before
                            // the test below, which would read that as
                            // abandoned and skip saving the queue position.
                            if slot.tile.is_none() && slot_idx + 1 == plan.assets.len() {
                                break;
                            }
                            // A Prev request, or the render thread dropped
                            // this plan (`consumed` without showing it: a
                            // backoff, or no GPU memory for a tile).
                            if slot.request.is_some() || !slot.pending {
                                if !slot.pending {
                                    slot.tile = None;
                                }
                                abandoned = true;
                                break;
                            }
                            if slot.tile.is_none() {
                                break;
                            }
                        }
                        std::thread::sleep(FETCH_TILE_POLL);
                    }
                    if abandoned {
                        break;
                    }
                }
                Err(e) => {
                    log::error!("fetch of asset {} (plan {seq}) failed: {e}", entry.asset);
                    let mut slot = shared.slot.lock().unwrap();
                    slot.failed = Some(seq);
                    slot.pending = false;
                    drop(slot);
                    host.waker.wake();
                    abandoned = true;
                    if !is_request {
                        std::thread::sleep(FETCH_FAILED_WAIT);
                    }
                    break;
                }
            }
        }
        if abandoned {
            log::info!("plan {seq} abandoned");
        } else {
            log::info!(
                "plan {seq} ({} tiles) fetched in {:?}",
                plan.assets.len(),
                clock::elapsed(plan_start)
            );
            if !is_request {
                to_save = plan.assets.first().map(|a| a.asset);
            }
        }
    }
}

fn label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Immich => "immich:",
        SourceKind::Local => "local:",
    }
}

/// A photo's pixels and focus: the local preview or cached Immich preview
/// if there is one, otherwise (Immich, online) the provider's preview,
/// which is cached on the way (evicting the least recently shown) along
/// with its focus. Returns where it came from, for the log.
fn load_photo(
    lib: &Library,
    immich: &mut ImmichProvider,
    e: &MediaItem,
) -> Result<(Photo, &'static str), String> {
    if e.video {
        return load_video(lib, immich, e);
    }
    let cached = {
        let conn = lib.db.lock().unwrap();
        match db::cached_preview(&conn, e.asset) {
            Some(p) if p.is_file() => Some(p),
            Some(_) => {
                // Its file went missing: drop the row, fetch again below.
                db::drop_cached(&conn, e.asset);
                None
            }
            None => None,
        }
    };
    let (bytes, from) = match cached {
        Some(path) => {
            let bytes =
                std::fs::read(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
            let _ = db::touch_cached(&lib.db.lock().unwrap(), e.asset);
            (
                bytes,
                if e.source == SourceKind::Local {
                    "local preview"
                } else {
                    "cache"
                },
            )
        }
        None if e.source == SourceKind::Local => return Err("local preview not made yet".into()),
        None => {
            let provider = immich
                .with_config(lib.config())
                .map_err(|e| e.to_string())?;
            let media = MediaRef::stored(e.remote_id.clone(), None);
            let bytes = provider
                .fetch_preview(&media, 0)
                .inspect_err(|err| {
                    if err.is_transport() {
                        lib.set_online(false);
                    }
                })
                .map_err(|e| e.to_string())?;
            match provider.fetch_focus(&media) {
                Ok(focus) => {
                    let _ = db::set_focus(&lib.db.lock().unwrap(), e.asset, focus);
                }
                Err(err) => log::warn!("focus for asset {}: {err}", e.asset),
            }
            match library::store_immich_preview(lib, e.asset, &bytes, true) {
                Ok(true) => {}
                Ok(false) => log::warn!("asset {} not cached (bigger than the cap)", e.asset),
                Err(err) => log::error!("caching asset {}: {err}", e.asset),
            }
            (bytes, "server")
        }
    };
    let (width, height, rgba) = decode_jpeg_rgba(&bytes)?;
    let (fill_centre, face_focal, _) = db::focus(&lib.db.lock().unwrap(), e.asset);
    Ok((
        Photo {
            asset_id: e.asset,
            key: e.key.clone(),
            width,
            height,
            rgba,
            face_focal,
            fill_centre,
            video: None,
        },
        from,
    ))
}

/// Decodes a preview JPEG (RGB or grayscale) straight to the RGBA the GPU
/// upload wants.
fn decode_jpeg_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    let pixels = dec.decode().map_err(|err| format!("jpeg decode: {err}"))?;
    let info = dec.info().ok_or("no jpeg info")?;
    let (w, h) = (info.width as u32, info.height as u32);
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&l| [l, l, l, 255]).collect(),
        other => return Err(format!("unsupported pixel format {other:?}")),
    };
    Ok((w, h, rgba))
}

/// A clip to play: the local file, the cached transcode, or (Immich,
/// online) the transcode downloaded now into the cache. Probed here, so the
/// render thread gets its size as shown and a clip this frame can't decode
/// never reaches it (it is marked, and left out of the queue from then on).
fn load_video(
    lib: &Library,
    immich: &mut ImmichProvider,
    e: &MediaItem,
) -> Result<(Photo, &'static str), String> {
    let (path, from) = match e.source {
        SourceKind::Local => (
            e.location.clone().ok_or("local clip has no location")?,
            "local file",
        ),
        SourceKind::Immich => {
            let cached = db::cached_video(&lib.db.lock().unwrap(), e.asset).filter(|p| p.is_file());
            match cached {
                Some(p) => {
                    let _ = db::touch_cached(&lib.db.lock().unwrap(), e.asset);
                    (p.to_string_lossy().into_owned(), "cache")
                }
                None => {
                    let provider = immich
                        .with_config(lib.config())
                        .map_err(|e| e.to_string())?;
                    let media = MediaRef::stored(e.remote_id.clone(), None);
                    match library::fetch_immich_video(lib, provider, e.asset, &media, true) {
                        Ok(Some((p, _))) => (p.to_string_lossy().into_owned(), "server"),
                        Ok(None) => return Err("clip bigger than the cache cap".into()),
                        Err(err) => {
                            if err.is_transport() {
                                lib.set_online(false);
                            }
                            return Err(err.to_string());
                        }
                    }
                }
            }
        }
    };
    let info = lib.host.probe.probe(&path)?;
    if let Some(why) = lib.host.probe.unplayable(&info) {
        let files = db::mark_unplayable(&lib.db.lock().unwrap(), e.asset, &why);
        db::remove_files(&files);
        lib.bump();
        return Err(format!("clip can't be played here: {why}"));
    }
    let (width, height) = info.display();
    Ok((
        Photo {
            asset_id: e.asset,
            key: e.key.clone(),
            width,
            height,
            rgba: Vec::new(),
            face_focal: None,
            fill_centre: (0.5, 0.5),
            video: Some(VideoClip { path, info }),
        },
        from,
    ))
}

/// A tiny xorshift64 draw seeded from the clock plus a caller-supplied
/// counter (so back-to-back calls at the same reading still differ), uniform
/// enough in `0..n` for a shuffle and Frameo's weighted layout pick without
/// pulling in a `rand` dependency.
fn pseudo_random_below(n: u32, counter: u64) -> u32 {
    let nanos = clock::now().as_nanos() as u64;
    let mut x = (nanos ^ counter.wrapping_mul(0x9E3779B97F4A7C15)) | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x % n.max(1) as u64) as u32
}
