//! Every named limit in the product, in one file, each with where its
//! value comes from (measured, chosen, or hardware). Style and geometry
//! tuning (Ken Burns zoom, margins, colours) stays with its module — those
//! are aesthetics, not limits.
//!
//! Device capability data (the RK decoder's 1920x1088 ceiling) is NOT here:
//! the host provides it (docs/ARCHITECTURE.md, "The seams").

use core::time::Duration;

// ---- memory budget (docs/ARCHITECTURE.md "Memory budget") ---------------
// The design target is the envelope measured on the frame over a few
// minutes of slideshow (collages and transitions, menu closed), counting
// PSS + Mali + the app's ion buffers, since PSS alone misses GPU and ion
// memory.

/// App total (PSS + GPU + window buffers), steady state. Measured ~67 MB.
pub const APP_TOTAL_STEADY_MB: u32 = 80;
/// Mali peak. Measured 63.5 MB peak.
pub const MALI_PEAK_MB: u32 = 64;
/// Tile textures per plan. Measured max 12.2 MB.
pub const TILE_TEXTURES_PER_PLAN_MB: u32 = 12;
/// Transition scratch: screen-sized render targets, ~4 MB each at 1280x800.
pub const TRANSITION_SCRATCH_TARGETS: u32 = 2;

// ---- sleep schedule ------------------------------------------------------

/// Default sleep window, chosen: 23:00-05:00 local.
pub const DEFAULT_SLEEP_MIN: u32 = 23 * 60;
pub const DEFAULT_WAKE_MIN: u32 = 5 * 60;
/// A schedule time is minutes after local midnight, below this.
pub const MINUTES_PER_DAY: u32 = 24 * 60;
/// Awake by hand during sleep hours: back to sleep after this long
/// untouched. Chosen.
pub const DEFAULT_MANUAL_IDLE: Duration = Duration::from_secs(10 * 60);

// ---- the app controller (input, overlay, saving) --------------------------

/// A press that moves less than this is a tap, not a swipe. Chosen for
/// fingers on the 10.1" panel.
pub const TAP_SLOP_PX: f32 = 40.0;
/// The menu closes on its own this long after the last input. Chosen to
/// match the vendor app's feel.
pub const AUTO_DISMISS: Duration = Duration::from_secs(15);
/// A screen opened by key hands its first control the focus within this
/// many passes, or not at all: egui draws a menu or dialog unseen on its
/// first pass to size it, and the control takes the focus on the next.
/// Keys wait meanwhile, so it also bounds how long they can wait.
pub const FOCUS_CLAIM_PASSES: u32 = 2;
/// With the menu open, never block in the host loop longer than this, so
/// egui's cursor and repaint stay live. Chosen.
pub const MAX_EGUI_WAIT: Duration = Duration::from_secs(1);
/// How long "Undo hide" stays available. Chosen.
pub const UNDO_HIDE: Duration = Duration::from_secs(8);
/// Settings writes are debounced this long after the last change. Chosen.
pub const SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
/// The screen still on this long after the schedule put it to sleep means
/// the device ignored the sleep (or someone woke it by hand): the
/// controller treats it as a manual wake. Chosen: far longer than a
/// screen-off takes to land.
pub const SLEEP_CONFIRM: Duration = Duration::from_secs(30);

// ---- slideshow pipeline ----------------------------------------------------

/// Transition length. Chosen on the frame; the measured 1.33-1.51 s there
/// includes compose overhead.
pub const TRANSITION_DURATION: Duration = Duration::from_millis(1300);
/// The blur working buffer's width, px. Chosen on the frame: the
/// smallest that still looks like Frameo's background blur.
pub const BLUR_WIDTH_PX: i32 = 128;
/// How many shown collages Prev can walk back through. Chosen.
pub const HISTORY_LEN: usize = 10;
/// After a GPU allocation failed (Mali out of memory), the next plan is
/// asked for this much later. Chosen: long enough for pressure to pass.
pub const GPU_RETRY: Duration = Duration::from_secs(5);

// ---- video (probe, live player, backoff) -----------------------------------

/// A clip's first frame: how long a probe or the live player may take.
/// Chosen; the frame's decoder usually delivers in ~0.4 s.
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the live clip waits for the previous decoder's release
/// (one decoder at a time on this class of VPU). Chosen.
pub const LIVE_DECODER_WAIT: Duration = Duration::from_secs(5);
/// A decoder not released this long after its player stopped: the VPU is
/// wedged (a stop that never returns). Chosen.
pub const DECODER_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);
/// Decoder-failure backoff: base doubling to the cap (30 s, 1, 2, 4, 8,
/// then 10 min). Chosen against the RK VPU running out of ion memory.
pub const DECODER_BACKOFF_BASE_SECS: u64 = 30;
pub const DECODER_BACKOFF_CAP_SECS: u64 = 600;
/// A live clip's frame 0 waits this long for its sound to pre-roll, then
/// plays without it. Chosen on the frame.
pub const AUDIO_PREROLL_WAIT: Duration = Duration::from_millis(1500);
/// How often the loop looks again while a decoder's release is awaited:
/// the host's reaper thread doesn't wake it. Chosen on the frame.
pub const DECODER_RELEASE_POLL: Duration = Duration::from_millis(100);

// ---- the Android player's A/V clock ----------------------------------------

/// A video frame this late against the media clock is dropped, not shown.
/// Chosen on the frame.
pub const DROP_LATE_US: i64 = 60_000;
/// PCM handed to the audio output per batch: 0.1 s of 44.1 kHz stereo
/// s16 (hardware-derived).
pub const PCM_BATCH_BYTES: usize = 35_280;
/// How long the video thread waits for audio's first batch before starting
/// without it. Chosen.
pub const AUDIO_ALIGN_TIMEOUT: Duration = Duration::from_millis(1000);
/// OpenSL buffer-queue depth. Chosen on the frame.
pub const AUDIO_OUT_BUFFERS: u32 = 4;

// ---- library (sync, cache) ---------------------------------------------------

/// The cache cap's ceiling default: 1 GB, or a quarter of free space if
/// that is less (`default_cap_mb`). Chosen.
pub const DEFAULT_CAP_MB: u32 = 1024;
pub const CAP_CHOICES_MB: [u32; 6] = [50, 250, 500, 1024, 2048, 4096];
/// Local folder rescan cadence. Chosen.
pub const SCAN_EVERY: Duration = Duration::from_secs(300);
/// Immich sync cadence while online. Chosen.
pub const SYNC_EVERY: Duration = Duration::from_secs(1800);
/// Sync retry cadence while offline — also how the queue comes back
/// online. Chosen.
pub const SYNC_RETRY: Duration = Duration::from_secs(60);
/// Picking an album syncs this long after the last pick, so a run of taps
/// is one sync. Chosen.
pub const ALBUM_PICK_DEBOUNCE: Duration = Duration::from_secs(2);
/// The library thread's longest sleep with no work and no command: how
/// soon it sees a `debug.video.cap_mb` change or the offline retry come
/// due. Chosen.
pub const LIBRARY_IDLE_WAIT: Duration = Duration::from_secs(5);
/// After the one-time storage grant, the wait before the photos folder is
/// read again: the remount the grant causes lands asynchronously. Chosen.
pub const STORAGE_GRANT_SETTLE: Duration = Duration::from_millis(500);
/// The read buffer for hashing a local file. Chosen: big enough that the
/// SHA-1 of a 28 MB clip is a few hundred reads, small next to the heap.
pub const HASH_BUFFER_BYTES: usize = 64 * 1024;
/// Wi-Fi networks are scanned for again this often while their list is
/// open. A scan takes about 11 s on a USB adapter (both bands). Chosen.
pub const WIFI_RESCAN: Duration = Duration::from_secs(30);
/// Local previews: the short side at least this, like Immich's 1440
/// preview.
pub const PREVIEW_SHORT_SIDE: u32 = 1440;
/// LRU eviction batch sizes: making room for one incoming file, and
/// enforcing a lowered cap. Chosen.
pub const LRU_BATCH_STORE: usize = 16;
pub const LRU_BATCH_ENFORCE: usize = 32;
/// Test-only: the longest a `debug.video.stall` may hold a write. Long
/// enough for a person to pull the plug on a cue; short enough that a
/// forgotten prop costs a minute per write, not a hung library. Chosen.
pub const DEBUG_STALL_MAX: Duration = Duration::from_secs(60);

// ---- fetch thread (planning, tile handover) ---------------------------------

/// Polling cadences of the fetch loop's slot handshake. Chosen; the fetch
/// thread has no condvar to wait on, so it polls.
pub const FETCH_SLOT_POLL: Duration = Duration::from_millis(50);
pub const FETCH_TILE_POLL: Duration = Duration::from_millis(20);
pub const FETCH_EMPTY_WAIT: Duration = Duration::from_millis(500);
pub const FETCH_BACKOFF_WAIT: Duration = Duration::from_millis(500);
pub const FETCH_SKIP_WAIT: Duration = Duration::from_millis(100);
/// After a plan's fetch failed (not a Prev request): breathe before the
/// next attempt. Chosen.
pub const FETCH_FAILED_WAIT: Duration = Duration::from_secs(2);

// ---- HTTP ---------------------------------------------------------------------

/// Immich API calls. Chosen.
pub const IMMICH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// An Immich clip download (2-28 MB on the frame's line). Chosen.
pub const IMMICH_VIDEO_TIMEOUT: Duration = Duration::from_secs(300);
/// `POST /api/search/metadata` page size (the server's maximum).
pub const IMMICH_PAGE_SIZE: u32 = 1000;
/// Pages one album may take: 100k assets, far past any album a frame
/// shows, so only a server whose `nextPage` never ends reaches it. Chosen.
pub const IMMICH_MAX_PAGES: u32 = 100;
/// The largest response body read into memory (previews run ~0.2-2 MB;
/// clips stream to disk and never pass through here). Chosen.
pub const HTTP_BODY_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
/// Weather and geolocation calls. Chosen.
pub const WEATHER_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

// ---- weather cadence ----------------------------------------------------------

/// Open-Meteo poll cadence. Chosen: courteous to a free API.
pub const WEATHER_REFRESH: Duration = Duration::from_secs(15 * 60);
pub const WEATHER_RETRY_MIN: Duration = Duration::from_secs(5);
pub const WEATHER_RETRY_MAX: Duration = Duration::from_secs(5 * 60);

// ---- keys -----------------------------------------------------------------------

/// Keys and typed text waiting for the menu, which takes one press a pass.
/// Chosen: a held key's repeats wait one at a time, so only typing fills
/// it, and 64 is about two seconds of fast typing on the frame's slowest
/// passes; anything past that is a backlog nobody is watching.
pub const MAX_QUEUED_KEYS: usize = 64;

// ---- collage -------------------------------------------------------------------

/// The largest layout's slot count. A collage test asserts the layout
/// table agrees.
pub const LARGEST_LAYOUT: usize = 4;

// ---- overlay glyph atlas --------------------------------------------------------

/// The clock/weather glyph atlas texture width, px. Chosen: fits both
/// fonts' charsets at the largest size with room to spare.
pub const ATLAS_WIDTH: usize = 1024;

// ---- UI steppers ------------------------------------------------------------------

/// The lip-sync stepper's default and range (ms), like a TV's. +180 was
/// measured on the SNUG frame with a filmed flash/beep.
pub const AUDIO_DELAY_DEFAULT_MS: i32 = 180;
pub const AUDIO_DELAY_RANGE: (i32, i32) = (-100, 400);
/// The photo interval's range (s), chosen: below 5 s a photo can't be
/// taken in, above 2 min the frame looks stuck.
pub const INTERVAL_RANGE_SECS: (f32, f32) = (5.0, 120.0);

// ---- UI lists -------------------------------------------------------------------

/// Settings → Photos → Hidden lists this many, newest first, then "And N
/// more." Chosen: a page that stays quick to draw and scroll on the frame.
pub const HIDDEN_LIST_MAX: usize = 50;
