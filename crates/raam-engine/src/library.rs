//! Keeping the DB in step with the providers (provider.rs), on two
//! threads.
//!
//! The library thread does the slow work, provider-agnostically:
//! - it syncs each enabled provider's `list()` into `asset` rows (upserted
//!   by the provider's id; items that are gone go, with their files): the
//!   local folder at start, every 5 minutes and on "Rescan", the Immich
//!   album list and the picked albums every 30 minutes (every minute while
//!   offline, and 2 s after the pick changes);
//! - it materialises previews one at a time: every local photo gets an
//!   upright preview (uncapped; it is derived from a file on the frame), and
//!   every Immich photo is prefetched, with its faces, into the Immich cache
//!   until the size cap. Playback adds on-demand fetches that evict the least
//!   recently shown (`store_immich_preview`).
//!
//! The writer thread takes the render thread's writes (settings, curation,
//! source toggles, the server) so they land at once even while the library
//! thread is mid-download: curation must survive the frame losing power a
//! second later. After each curation change it rewrites the JSON export.
//!
//! The fetch thread reads the DB directly; `generation` tells it when the
//! set of showable photos changed.
//!
//! The Immich provider plays the union of the albums picked in settings
//! (`collection`). Picking an album syncs it 2 s later (so a run of taps is
//! one sync); un-picking one drops its assets at once on the writer thread,
//! offline too, unless another picked album holds them. Every sync first
//! checks the key still reads the same Immich user: another server or user
//! is another library, which starts over from the default album. Cached
//! files are removed only after the DB lock is let go.
use crate::db::{self, Db};
use crate::immich;
use crate::provider::{ImmichProvider, LocalFolder, Provider};
use crate::stall::{self, Site, Writer};
use crate::{Host, Paths};
use raam_core::{clock, schedule};
use raam_model::limits::{
    ALBUM_PICK_DEBOUNCE, CAP_CHOICES_MB, DEFAULT_CAP_MB, LIBRARY_IDLE_WAIT, LRU_BATCH_ENFORCE,
    LRU_BATCH_STORE, PREVIEW_SHORT_SIDE, SCAN_EVERY, SYNC_EVERY, SYNC_RETRY,
};
use raam_model::{
    AlbumId, AssetId, CurationKey, MediaRef, Prefetch, ProviderError, RemoteId, ScaleMode,
    SourceKind, Stats,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum Cmd {
    // To the writer thread.
    SaveSettings {
        rows: Vec<(&'static str, serde_json::Value)>,
        sleep: schedule::Schedule,
    },
    SetScale(CurationKey, Option<ScaleMode>),
    SetHidden(CurationKey, bool),
    SetSourceEnabled(SourceKind, bool),
    SetServer(immich::Config),
    ExportCuration,
    /// Pick or un-pick an album (Immich album id).
    SelectAlbum(AlbumId, bool),
    /// The frame couldn't decode this clip (asset id, why).
    SetUnplayable(AssetId, String),
    // To the library thread.
    SetCap(u32),
    ClearCache,
    Rescan,
    SyncNow,
    ServerChanged,
    Refresh,
    /// The pick changed; sync shortly.
    AlbumsChanged,
}

impl Cmd {
    /// Which thread takes it. Exhaustive, so a new command has to be given
    /// a thread, and each thread's handler crashes on the other's.
    fn is_for_writer(&self) -> bool {
        match self {
            Cmd::SaveSettings { .. }
            | Cmd::SetScale(..)
            | Cmd::SetHidden(..)
            | Cmd::SetSourceEnabled(..)
            | Cmd::SetServer(_)
            | Cmd::ExportCuration
            | Cmd::SelectAlbum(..)
            | Cmd::SetUnplayable(..) => true,
            Cmd::SetCap(_)
            | Cmd::ClearCache
            | Cmd::Rescan
            | Cmd::SyncNow
            | Cmd::ServerChanged
            | Cmd::Refresh
            | Cmd::AlbumsChanged => false,
        }
    }
}

pub struct Library {
    pub db: Db,
    writer_tx: Sender<Cmd>,
    library_tx: Sender<Cmd>,
    generation: AtomicU64,
    online: AtomicBool,
    immich_on: AtomicBool,
    local_on: AtomicBool,
    cap_bytes: AtomicU64,
    config: Mutex<immich::Config>,
    stats: Mutex<(u64, Stats)>,
    export_note: Mutex<String>,
    pub cache_dir: PathBuf,
    pub local_preview_dir: PathBuf,
    pub local_dir: String,
    pub host: Host,
    pub paths: Paths,
}

impl Library {
    /// Which photos may be shown changed (sync, scan, toggle, hide, online).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn bump(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.host.waker.wake();
    }

    pub fn online(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    /// Reachability as last seen by a sync or an on-demand fetch. Offline,
    /// the queue only holds Immich photos that are in the cache.
    pub fn set_online(&self, online: bool) {
        if self.online.swap(online, Ordering::AcqRel) != online {
            log::info!(
                "library: Immich now {}",
                if online {
                    "online"
                } else {
                    "OFFLINE, playing from the cache"
                }
            );
            self.bump();
        }
    }

    pub fn enabled(&self, kind: SourceKind) -> bool {
        match kind {
            SourceKind::Immich => self.immich_on.load(Ordering::Relaxed),
            SourceKind::Local => self.local_on.load(Ordering::Relaxed),
        }
    }

    /// Persisted by the writer thread, which then bumps the generation.
    pub fn set_enabled(&self, kind: SourceKind, on: bool) {
        let flag = match kind {
            SourceKind::Immich => &self.immich_on,
            SourceKind::Local => &self.local_on,
        };
        if flag.swap(on, Ordering::AcqRel) != on {
            self.send(Cmd::SetSourceEnabled(kind, on));
        }
    }

    pub fn cap_bytes(&self) -> u64 {
        self.cap_bytes.load(Ordering::Relaxed)
    }

    pub fn config(&self) -> immich::Config {
        self.config.lock().unwrap().clone()
    }

    /// A server is set: none on a fresh install, or once it's removed.
    pub fn has_server(&self) -> bool {
        !self.config.lock().unwrap().url.trim().is_empty()
    }

    /// To whichever thread takes `cmd`. A thread that has stopped has
    /// panicked, which already ends the app, so a failed send is moot.
    pub fn send(&self, cmd: Cmd) {
        let tx = if cmd.is_for_writer() {
            &self.writer_tx
        } else {
            &self.library_tx
        };
        let _ = tx.send(cmd);
    }

    /// Changes whenever the stats do.
    pub fn stats_version(&self) -> u64 {
        self.stats.lock().unwrap().0
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().1.clone()
    }

    fn publish(&self, s: Stats) {
        let mut cur = self.stats.lock().unwrap();
        if cur.1 != s {
            cur.0 += 1;
            cur.1 = s;
            drop(cur);
            self.host.waker.wake();
        }
    }
}

const MIB: u64 = 1024 * 1024;
/// The free space the panel shows moves in these steps, so it doesn't
/// redraw for every block written.
const FREE_SPACE_STEP: u64 = 10 * MIB;

pub fn spawn(db: Db, paths: Paths, cap_mb: u32, host: Host) -> Arc<Library> {
    let (writer_tx, writer_rx) = std::sync::mpsc::channel();
    let (library_tx, library_rx) = std::sync::mpsc::channel();
    let cache_dir = paths.files_dir.join("immich-cache");
    let local_preview_dir = paths.files_dir.join("local-previews");
    for d in [&cache_dir, &local_preview_dir] {
        if let Err(e) = std::fs::create_dir_all(d) {
            log::error!("library: create {}: {e}", d.display());
        }
    }
    let (immich_row, local_row, key) = {
        let conn = db.lock().unwrap();
        (
            db::source(&conn, SourceKind::Immich),
            db::source(&conn, SourceKind::Local),
            db::immich_key(&conn),
        )
    };
    let lib = Arc::new(Library {
        db,
        writer_tx,
        library_tx,
        generation: AtomicU64::new(1),
        online: AtomicBool::new(false),
        immich_on: AtomicBool::new(immich_row.enabled),
        local_on: AtomicBool::new(local_row.enabled),
        cap_bytes: AtomicU64::new(u64::from(cap_mb) * MIB),
        config: Mutex::new(immich::Config {
            url: immich_row.base_url,
            key,
        }),
        stats: Mutex::new((0, Stats::default())),
        export_note: Mutex::new(String::new()),
        cache_dir,
        local_preview_dir,
        local_dir: if local_row.base_url.is_empty() {
            paths.local_dir_default.clone()
        } else {
            local_row.base_url
        },
        host,
        paths,
    });
    let l = lib.clone();
    std::thread::spawn(move || writer_loop(l, writer_rx));
    let l = lib.clone();
    std::thread::spawn(move || library_loop(l, library_rx));
    lib
}

/// The first-run cap: 1 GB, or a quarter of the free space if that is less.
pub fn default_cap_mb(files_dir: &Path) -> u32 {
    let free_mb = free_bytes(files_dir) / MIB;
    if free_mb == 0 {
        DEFAULT_CAP_MB
    } else {
        // Never under the smallest cap the picker offers.
        let quarter = u32::try_from(free_mb / 4).unwrap_or(u32::MAX);
        DEFAULT_CAP_MB.min(quarter).max(CAP_CHOICES_MB[0])
    }
}

// `statvfs`'s field widths vary by platform (u32 or u64 on macOS, Linux
// and 32-bit Android), so `u64::from` would be a useless conversion on
// some and `as` is the one spelling that is lossless and clean on all.
#[allow(
    clippy::cast_lossless,
    reason = "statvfs field widths vary by platform"
)]
fn free_bytes(path: &Path) -> u64 {
    let Ok(c) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
        return 0;
    };
    // SAFETY: `statvfs` is plain integers; all-zero is a valid value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated string that outlives the call, and
    // `st` is a valid, exclusively borrowed `statvfs` for it to fill.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return 0;
    }
    st.f_bavail as u64 * st.f_frsize as u64
}

// ---- the writer thread -----------------------------------------------------

fn writer_loop(lib: Arc<Library>, rx: Receiver<Cmd>) {
    for cmd in rx {
        let t = clock::now();
        let conn = lib.db.lock().unwrap();
        let mut curation_changed = false;
        // Removed after the lock is let go.
        let mut stale_files = Vec::new();
        match cmd {
            Cmd::SaveSettings { rows, sleep } => match db::save_settings(&conn, &rows, sleep) {
                Ok(()) => log::info!(
                    "db: settings saved ({} keys + sleep schedule) in {:?}",
                    rows.len(),
                    clock::elapsed(t)
                ),
                Err(e) => log::error!("db: saving settings failed: {e}"),
            },
            Cmd::SetScale(key, mode) => match db::set_scale(&conn, &key, mode) {
                Ok(_) => {
                    log::info!(
                        "db: {key} Fill/Fit override -> {mode:?} in {:?}",
                        clock::elapsed(t)
                    );
                    curation_changed = true;
                }
                Err(e) => log::error!("db: Fill/Fit override for {key} failed: {e}"),
            },
            Cmd::SetHidden(key, hidden) => match db::set_hidden(&conn, &key, hidden) {
                Ok(_) => {
                    log::info!(
                        "db: {key} {} in {:?}",
                        if hidden { "hidden" } else { "unhidden" },
                        clock::elapsed(t)
                    );
                    curation_changed = true;
                    lib.bump();
                }
                Err(e) => log::error!("db: hiding {key} failed: {e}"),
            },
            Cmd::SetSourceEnabled(kind, on) => {
                if let Err(e) = db::set_source_enabled(&conn, kind, on) {
                    log::error!("db: source toggle failed: {e}");
                }
                log::info!(
                    "library: source {} {}",
                    kind.as_str(),
                    if on { "enabled" } else { "disabled" }
                );
                lib.bump();
                // Catch up on what changed while it was off.
                lib.send(if on && kind == SourceKind::Local {
                    Cmd::Rescan
                } else if on {
                    Cmd::SyncNow
                } else {
                    Cmd::Refresh
                });
            }
            Cmd::SetServer(config) => {
                if let Err(e) = db::set_immich_server(&conn, &config.url, &config.key) {
                    log::error!("db: saving the server failed: {e}");
                }
                // Never log the key.
                if config.url.is_empty() {
                    log::info!("library: Immich server removed, its photos wait for one");
                } else {
                    log::info!(
                        "library: Immich server changed to {}, resyncing",
                        config.url
                    );
                }
                *lib.config.lock().unwrap() = config;
                // With a server gone or back, other photos may play.
                lib.bump();
                lib.send(Cmd::ServerChanged);
            }
            Cmd::ExportCuration => curation_changed = true,
            Cmd::SetUnplayable(asset, reason) => {
                log::warn!("library: asset {asset} can't be played here ({reason}), left out");
                stale_files = db::mark_unplayable(&conn, asset, &reason);
                lib.bump();
                lib.send(Cmd::Refresh);
            }
            Cmd::SelectAlbum(album, on) => match db::select_album(&conn, &album, on) {
                Ok((dropped, files)) => {
                    log::info!(
                        "library: album {album} {}, {dropped} assets dropped in {:?}",
                        if on { "picked" } else { "un-picked" },
                        clock::elapsed(t)
                    );
                    stale_files = files;
                    if dropped > 0 {
                        lib.bump();
                    }
                    // A sync either way: a pick needs its assets, and an
                    // un-pick refreshes the album list and the status.
                    lib.send(Cmd::AlbumsChanged);
                }
                Err(e) => log::error!("db: picking album {album} failed: {e}"),
            },
            Cmd::SetCap(_)
            | Cmd::ClearCache
            | Cmd::Rescan
            | Cmd::SyncNow
            | Cmd::ServerChanged
            | Cmd::Refresh
            | Cmd::AlbumsChanged => unreachable!("a library thread command reached the writer"),
        }
        // Small, and rewritten whole after each change: the frame has no
        // other backup of it.
        let json = curation_changed.then(|| db::curation_json(&conn));
        drop(conn);
        if !stale_files.is_empty() {
            let t = clock::now();
            db::remove_files(&stale_files);
            log::info!(
                "library: {} cached files removed in {:?}, outside the DB lock",
                stale_files.len(),
                clock::elapsed(t)
            );
        }
        let json = json.transpose().unwrap_or_else(|e| {
            // The export on disk stays as it was: older, but whole.
            log::error!("curation export: listing the curation failed: {e}");
            *lib.export_note.lock().unwrap() = format!("export failed: {e}");
            None
        });
        if let Some(json) = json {
            let export = &lib.paths.curation_export;
            stall::at(
                &lib.host,
                Site::Export,
                Writer::Curation,
                "the curation export",
            );
            let note = match write_atomic(export, json.as_bytes(), || {
                stall::at(
                    &lib.host,
                    Site::Rename,
                    Writer::Curation,
                    "the curation export",
                )
            }) {
                Ok(()) => format!("exported to {}", export.display()),
                Err(e) => {
                    log::error!("curation export: {e}");
                    format!("export failed: {e}")
                }
            };
            *lib.export_note.lock().unwrap() = note;
            lib.send(Cmd::Refresh);
        }
    }
}

// ---- the library thread ----------------------------------------------------

struct Loop {
    immich: ImmichProvider,
    local: LocalFolder,
    next_scan: Duration,
    next_sync: Duration,
    /// Nothing left to materialise (or the cache is at its cap).
    idle: bool,
    /// Assets whose preview failed this run, not retried until the next sync.
    failed: std::collections::HashSet<AssetId>,
    local_note: String,
    immich_note: String,
    prefetch: Prefetch,
    albums_note: String,
    test_cap: Option<u32>,
    /// The cap from settings, put back when the debug override is cleared.
    setting_cap: u64,
}

fn library_loop(lib: Arc<Library>, rx: Receiver<Cmd>) {
    // An empty curation table is seeded once from a curation export
    // (Raam's own, or the prototype's), if one is there.
    match db::import_curation(&lib.db.lock().unwrap(), &lib.paths.curation_export) {
        Ok(0) => {}
        Ok(n) => {
            log::info!(
                "library: curation imported: {n} items from {}",
                lib.paths.curation_export.display()
            );
            lib.bump();
        }
        Err(e) => log::warn!("library: curation import: {e}"),
    }
    let known_local = {
        let conn = lib.db.lock().unwrap();
        let t = clock::now();
        let (rows, files) = db::sweep(&conn, &[&lib.cache_dir, &lib.local_preview_dir]);
        log::info!(
            "library: startup sweep dropped {rows} rows with no file and {files} files with no row in {:?}",
            clock::elapsed(t)
        );
        db::local_known(&conn)
    };
    let mut st = Loop {
        immich: ImmichProvider::new(),
        local: LocalFolder::new(
            lib.local_dir.clone(),
            known_local,
            lib.host.probe.clone(),
            lib.host.grant_storage.clone(),
        ),
        next_scan: clock::now(),
        next_sync: clock::now(),
        idle: false,
        failed: Default::default(),
        local_note: String::new(),
        immich_note: "not synced yet".into(),
        prefetch: Prefetch::Idle,
        albums_note: "saved list, not refreshed yet".into(),
        test_cap: None,
        setting_cap: lib.cap_bytes(),
    };
    loop {
        // Test-only: `debug.video.cap_mb` overrides the cap setting.
        let test_cap = lib
            .host
            .switches
            .get("debug.video.cap_mb")
            .trim()
            .parse::<u32>()
            .ok();
        if test_cap != st.test_cap {
            log::info!("library: debug cap override {test_cap:?} MB");
            st.test_cap = test_cap;
            lib.cap_bytes.store(
                test_cap.map_or(st.setting_cap, |mb| u64::from(mb) * MIB),
                Ordering::Relaxed,
            );
            enforce_cap(&lib);
            st.idle = false;
        }

        if clock::now() >= st.next_scan {
            if lib.enabled(SourceKind::Local) {
                let t = clock::now();
                match sync_provider(&lib, &mut st.local) {
                    Ok(n) => {
                        st.local_note = format!("scanned {}", hm_now());
                        log::info!(
                            "library: scanned {}: {n} in {:?}",
                            lib.local_dir,
                            clock::elapsed(t)
                        );
                    }
                    Err(e) => {
                        log::error!("library: scan failed: {e}");
                        st.local_note = e;
                    }
                }
                st.idle = false;
            }
            st.next_scan = clock::now() + SCAN_EVERY;
        }
        // Offline (a failed fetch counts too): retry the sync every minute,
        // which is also how the queue comes back online.
        if !lib.online() && st.next_sync > clock::now() + SYNC_RETRY {
            st.next_sync = clock::now() + SYNC_RETRY;
        }
        if clock::now() >= st.next_sync {
            // A fresh install has no server yet: nothing to sync (or spam
            // the log with) until settings provide one.
            if lib.enabled(SourceKind::Immich) && !lib.has_server() {
                st.immich_note = "no server configured".into();
                st.albums_note = "no server configured".into();
            } else if lib.enabled(SourceKind::Immich) {
                let t = clock::now();
                // The album list first (the picker's, and which picked
                // albums still exist), then the union of the picked ones.
                let result = st
                    .immich
                    .with_config(lib.config())
                    .map_err(|e| e.to_string())
                    .and_then(|p| {
                        // Which library this is (another server or user starts
                        // over). A key that may not read the user is left
                        // unchecked rather than failing the sync.
                        match p.user_id() {
                            Ok(user) => {
                                let stale = db::check_library(&lib.db.lock().unwrap(), &user)
                                    .map_err(|e| e.to_string())?;
                                if let Some(files) = stale {
                                    db::remove_files(&files);
                                    lib.bump();
                                }
                            }
                            Err(e) => log::warn!(
                                "library: can't tell which library the key reads ({e}), not checked"
                            ),
                        }
                        let albums = p.albums().map_err(|e| e.to_string())?;
                        let picked = db::update_albums(&lib.db.lock().unwrap(), &albums)
                            .map_err(|e| e.to_string())?;
                        log::info!(
                            "library: {} albums on the server, {} picked, listed in {:?}",
                            albums.len(),
                            picked.len(),
                            clock::elapsed(t)
                        );
                        let none = picked.is_empty();
                        p.set_albums(picked);
                        sync_provider(&lib, p).map(|n| (n, none))
                    });
                match result {
                    Ok((n, none)) => {
                        log::info!("library: album sync: {n} in {:?}", clock::elapsed(t));
                        st.albums_note = format!("updated {}", hm_now());
                        st.immich_note = if none {
                            "no album picked".to_string()
                        } else {
                            format!("synced {}", hm_now())
                        };
                        st.failed.clear();
                        lib.set_online(true);
                    }
                    Err(e) => {
                        log::error!("library: album sync failed: {e}");
                        if lib.online() || !st.immich_note.starts_with("offline") {
                            st.immich_note = format!("offline since {}", hm_now());
                            st.albums_note = format!(
                                "can't reach the server ({}), showing the saved list",
                                hm_now()
                            );
                        }
                        lib.set_online(false);
                    }
                }
                st.idle = false;
            }
            st.next_sync = clock::now() + if lib.online() { SYNC_EVERY } else { SYNC_RETRY };
        }
        let worked = !st.idle && materialise_one(&lib, &mut st);
        if !worked {
            st.idle = true;
        }
        publish_stats(&lib, &st);

        let wait = if worked {
            Duration::ZERO
        } else {
            st.next_scan
                .min(st.next_sync)
                .saturating_sub(clock::now())
                .min(LIBRARY_IDLE_WAIT)
        };
        let first = match rx.recv_timeout(wait) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        for cmd in first.into_iter().chain(rx.try_iter()) {
            handle(&lib, &mut st, cmd);
        }
    }
}

fn hm_now() -> String {
    schedule::fmt_hm(schedule::local_now().0 / 60)
}

fn handle(lib: &Library, st: &mut Loop, cmd: Cmd) {
    match cmd {
        Cmd::SetCap(mb) => {
            st.setting_cap = u64::from(mb) * MIB;
            if st.test_cap.is_none() {
                lib.cap_bytes.store(st.setting_cap, Ordering::Relaxed);
            }
            log::info!("library: cache cap {mb} MB");
            enforce_cap(lib);
            st.idle = false;
        }
        Cmd::ClearCache => {
            let t = clock::now();
            let conn = lib.db.lock().unwrap();
            let (cleared, files) = match db::clear_immich_cache(&conn) {
                Ok(cleared) => cleared,
                Err(e) => {
                    log::error!("library: clearing the Immich cache failed: {e}");
                    (0, Vec::new())
                }
            };
            // Not on a host with no player: nothing has changed for it.
            let stale = match lib.host.probe.no_player() {
                Some(why) => db::mark_clips_unplayable(&conn, &why).1,
                None => Vec::new(),
            };
            drop(conn);
            db::remove_files(&files);
            db::remove_files(&stale);
            log::info!(
                "library: Immich cache cleared, {cleared} previews in {:?}",
                clock::elapsed(t)
            );
            st.idle = false;
            st.failed.clear();
            lib.bump();
        }
        Cmd::Rescan => st.next_scan = clock::now(),
        Cmd::SyncNow | Cmd::ServerChanged => {
            st.next_sync = clock::now();
            st.idle = false;
            st.failed.clear();
        }
        Cmd::Refresh => st.idle = false,
        Cmd::AlbumsChanged => {
            // Another tap meanwhile pushes it back.
            st.next_sync = clock::now() + ALBUM_PICK_DEBOUNCE;
            st.idle = false;
            st.failed.clear();
        }
        Cmd::SaveSettings { .. }
        | Cmd::SetScale(..)
        | Cmd::SetHidden(..)
        | Cmd::SetSourceEnabled(..)
        | Cmd::SetServer(_)
        | Cmd::ExportCuration
        | Cmd::SelectAlbum(..)
        | Cmd::SetUnplayable(..) => unreachable!("a writer command reached the library thread"),
    }
}

fn publish_stats(lib: &Library, st: &Loop) {
    let conn = lib.db.lock().unwrap();
    let (immich_assets, immich_cached) = db::counts(&conn, SourceKind::Immich);
    let (local_assets, local_ready) = db::counts(&conn, SourceKind::Local);
    let cache_bytes = db::cached_bytes(&conn, SourceKind::Immich);
    let shared = db::shared_count(&conn);
    let hidden = db::hidden_list(&conn);
    let albums = db::albums(&conn);
    let (videos, videos_ready, videos_unplayable, unplayable_reasons) = db::video_counts(&conn);
    drop(conn);
    lib.publish(Stats {
        immich_assets,
        immich_cached,
        cache_bytes,
        cap_bytes: db::sql_int(lib.cap_bytes()),
        local_assets,
        local_ready,
        shared,
        local_dir: lib.local_dir.clone(),
        local_note: st.local_note.clone(),
        immich_note: st.immich_note.clone(),
        prefetch: st.prefetch,
        free_bytes: free_bytes(&lib.cache_dir) / FREE_SPACE_STEP * FREE_SPACE_STEP,
        hidden,
        export_note: lib.export_note.lock().unwrap().clone(),
        albums,
        albums_note: st.albums_note.clone(),
        videos,
        videos_ready,
        videos_unplayable,
        unplayable_reasons,
    });
}

/// Brings a provider's `asset` rows in line with its `list()`. Items are
/// upserted by the provider's id; for the local folder that is the content
/// hash, so a renamed or moved file only updates `location`. Whatever the
/// provider no longer lists goes, with its files. Curation is untouched.
/// One row that won't write fails the whole sync, rolled back: a commit
/// with it missing would count it as gone.
fn sync_provider(lib: &Library, provider: &mut dyn Provider) -> Result<String, String> {
    let kind = provider.kind();
    let mut items = provider.list().map_err(|e| e.to_string())?;
    let conn = lib.db.lock().unwrap();
    let source = db::source_id(&conn, kind).ok_or("no source row")?;
    if kind == SourceKind::Immich {
        // An album un-picked while the list was downloading must not come
        // back: keep only what the albums picked now hold.
        let picked: std::collections::HashSet<AlbumId> =
            db::selected_albums(&conn).into_iter().collect();
        let listed = items.len();
        for m in &mut items {
            m.collections.retain(|c| picked.contains(c));
        }
        items.retain(|m| !m.collections.is_empty());
        if items.len() != listed {
            log::info!(
                "library: {} listed assets left out, their album was un-picked mid-sync",
                listed - items.len()
            );
        }
    }
    let mut existing: std::collections::HashMap<RemoteId, (AssetId, u32, u32, Option<String>)> =
        conn.prepare(
            "SELECT remote_id, id, width, height, location FROM asset WHERE source_id = ?1",
        )
        .and_then(|mut s| {
            s.query_map([source], |r| {
                Ok((
                    RemoteId::new(r.get::<_, String>(0)?),
                    (AssetId::new(r.get(1)?), r.get(2)?, r.get(3)?, r.get(4)?),
                ))
            })?
            .collect()
        })
        .map_err(|e| e.to_string())?;
    let (mut added, mut changed, mut moved) = (0, 0, 0);
    let now = db::now_ms();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for m in &items {
        let (bytes, mtime) = m.stamp.map_or((None, None), |(b, t)| (Some(b), Some(t)));
        match existing.remove(&m.id) {
            Some((_, w, h, loc)) if (w, h) == (m.width, m.height) && loc == m.location => {}
            Some((id, w, h, loc)) => {
                if (w, h) == (m.width, m.height) {
                    log::info!("library: asset {id} moved {loc:?} -> {:?}", m.location);
                    moved += 1;
                } else {
                    changed += 1;
                }
                tx.execute(
                    "UPDATE asset SET width = ?2, height = ?3, taken_at_ms = ?4, location = ?5, file_bytes = ?6,
                       file_mtime_ms = ?7, hash = ?8 WHERE id = ?1",
                    rusqlite::params![id.get(), m.width, m.height, m.taken_at_ms, m.location, bytes, mtime, m.sha1],
                )
                .map_err(|e| format!("updating asset {id}: {e}"))?;
            }
            None => {
                tx.execute(
                    "INSERT INTO asset (source_id, remote_id, hash, location, kind, width, height, taken_at_ms,
                       added_at_ms, file_bytes, file_mtime_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    rusqlite::params![
                        source,
                        m.id.as_str(),
                        m.sha1,
                        m.location,
                        m.kind.as_str(),
                        m.width,
                        m.height,
                        m.taken_at_ms,
                        now,
                        bytes,
                        mtime
                    ],
                )
                .map_err(|e| format!("adding {}: {e}", m.id))?;
                added += 1;
            }
        }
    }
    // A focus the provider already knows (none of today's two do in
    // `list`: Immich's faces are fetched lazily, the folder has none).
    for (m, focus) in items.iter().filter_map(|m| Some((m, m.focus?))) {
        tx.execute(
            "UPDATE asset SET focus_x = ?3, focus_y = ?4, faces_checked = 1 WHERE source_id = ?1 AND remote_id = ?2",
            rusqlite::params![source, m.id.as_str(), focus.centre.0, focus.centre.1],
        )
        .map_err(|e| format!("focus of {}: {e}", m.id))?;
    }
    let removed = existing.len();
    for (id, _, _, loc) in existing.values() {
        if let Some(loc) = loc {
            log::info!("library: {loc} is gone, dropping asset {id}");
        }
    }
    let gone: Vec<AssetId> = existing.values().map(|v| v.0).collect();
    let files = db::delete_assets(&tx, &gone).map_err(|e| format!("dropping assets: {e}"))?;
    if kind == SourceKind::Immich {
        db::set_memberships(&tx, source, &items).map_err(|e| format!("memberships: {e}"))?;
    }
    // A host with no player: the clips just listed are out before anything
    // can plan them or fetch them to probe.
    let (left_out, stale) = match lib.host.probe.no_player() {
        Some(why) => db::mark_clips_unplayable(&tx, &why),
        None => (0, Vec::new()),
    };
    tx.commit().map_err(|e| e.to_string())?;
    drop(conn);
    db::remove_files(&files);
    db::remove_files(&stale);
    if left_out > 0 {
        log::info!("library: {left_out} clips left out, this host plays none");
    }
    if added + changed + moved + removed + left_out > 0 {
        lib.bump();
    }
    Ok(format!(
        "{} items ({added} new, {changed} changed, {moved} moved, {removed} removed)",
        items.len()
    ))
}

/// Makes one missing preview: local photos first (they are always kept),
/// then Immich prefetch, with faces, until the cap. Returns whether there
/// was work to do.
fn materialise_one(lib: &Library, st: &mut Loop) -> bool {
    let local_on = lib.enabled(SourceKind::Local);
    let immich_on = lib.enabled(SourceKind::Immich) && lib.has_server() && lib.online();
    let next = db::next_to_make(&lib.db.lock().unwrap(), local_on, immich_on, &st.failed);
    let next = match next {
        Ok(next) => next,
        Err(e) => {
            log::error!("library: finding the next preview to make: {e}");
            return false;
        }
    };
    let Some(db::ToMake {
        asset,
        kind,
        remote_id,
        location,
        faces_checked,
        is_video,
    }) = next
    else {
        if immich_on && st.prefetch != Prefetch::Complete {
            log::info!("library: every photo has its preview (Immich prefetch complete)");
            st.prefetch = Prefetch::Complete;
        }
        return false;
    };
    let media = MediaRef::stored(remote_id, location);
    let t = clock::now();
    if is_video {
        // Only Immich clips get here (a prefetch; nothing is evicted for it).
        let provider = match st.immich.with_config(lib.config()) {
            Ok(p) => p,
            Err(e) => {
                log::error!("library: {e}");
                return false;
            }
        };
        return match fetch_immich_video(lib, provider, asset, &media, false) {
            Ok(Some(_)) => {
                st.prefetch = Prefetch::Running;
                log::info!(
                    "library: prefetched clip {asset} in {:?}",
                    clock::elapsed(t)
                );
                true
            }
            Ok(None) => {
                if st.prefetch != Prefetch::Full {
                    log::info!(
                        "library: prefetch stopped at the {} MB cap",
                        lib.cap_bytes() / MIB
                    );
                }
                st.prefetch = Prefetch::Full;
                false
            }
            Err(e) => {
                log::warn!("library: clip {asset}: {e}");
                st.failed.insert(asset);
                if e.is_transport() {
                    lib.set_online(false);
                }
                true
            }
        };
    }
    let provider: &mut dyn Provider = match kind {
        SourceKind::Local => &mut st.local,
        SourceKind::Immich => match st.immich.with_config(lib.config()) {
            Ok(p) => p,
            Err(e) => {
                log::error!("library: {e}");
                return false;
            }
        },
    };
    let bytes = match provider.fetch_preview(&media, PREVIEW_SHORT_SIDE) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("library: preview of asset {asset} failed: {e}");
            st.failed.insert(asset);
            if kind == SourceKind::Immich && e.is_transport() {
                lib.set_online(false);
            }
            return true;
        }
    };
    if !faces_checked {
        match provider.fetch_focus(&media) {
            Ok(focus) => {
                let _ = db::set_focus(&lib.db.lock().unwrap(), asset, focus);
            }
            Err(e) => log::warn!("library: focus for asset {asset}: {e}"),
        }
    }
    match kind {
        SourceKind::Local => {
            let path = lib.local_preview_dir.join(format!("{asset}.jpg"));
            let what = format!("asset {asset}");
            if let Err(e) = write_atomic(&path, &bytes, || {
                stall::at(&lib.host, Site::Rename, Writer::Local, &what)
            }) {
                log::error!("library: {e}");
                st.failed.insert(asset);
                return true;
            }
            stall::at(&lib.host, Site::Row, Writer::Local, &what);
            let _ = db::insert_cached(
                &lib.db.lock().unwrap(),
                asset,
                &path,
                db::sql_int(bytes.len()),
                db::now_ms(),
            );
            // A new local photo just became showable.
            lib.bump();
            true
        }
        SourceKind::Immich => match store_immich_preview(lib, asset, &bytes, false) {
            Ok(true) => {
                st.prefetch = Prefetch::Running;
                log::info!(
                    "library: prefetched asset {asset} ({} KB) in {:?}",
                    bytes.len() / 1024,
                    clock::elapsed(t)
                );
                true
            }
            Ok(false) => {
                if st.prefetch != Prefetch::Full {
                    log::info!(
                        "library: prefetch stopped at the {} MB cap",
                        lib.cap_bytes() / MIB
                    );
                }
                st.prefetch = Prefetch::Full;
                false
            }
            Err(e) => {
                log::error!("library: storing asset {asset}: {e}");
                st.failed.insert(asset);
                true
            }
        },
    }
}

/// Writes an Immich preview into the cache and records it. With `evict`
/// (on-demand, for a photo about to be shown) the least recently shown
/// previews make room; without it (prefetch) nothing is written once the
/// cap would be passed, so prefetch never churns the cache. Ok(false) =
/// not stored.
pub fn store_immich_preview(
    lib: &Library,
    asset: AssetId,
    bytes: &[u8],
    evict: bool,
) -> Result<bool, String> {
    let cap = db::sql_int(lib.cap_bytes());
    let len = db::sql_int(bytes.len());
    if len > cap {
        return Ok(false);
    }
    if !evict && db::cached_bytes(&lib.db.lock().unwrap(), SourceKind::Immich) + len > cap {
        return Ok(false);
    }
    let path = lib.cache_dir.join(format!("{asset}.jpg"));
    // Only the on-demand fetch evicts.
    let writer = if evict {
        Writer::Fetch
    } else {
        Writer::Prefetch
    };
    let what = format!("asset {asset}");
    write_atomic(&path, bytes, || {
        stall::at(&lib.host, Site::Rename, writer, &what)
    })?;
    stall::at(&lib.host, Site::Row, writer, &what);
    let conn = lib.db.lock().unwrap();
    let evicted = if evict {
        make_room(&conn, Some(asset), len, cap, LRU_BATCH_STORE).0
    } else {
        Vec::new()
    };
    let stored = db::insert_cached(&conn, asset, &path, len, db::now_ms());
    drop(conn);
    db::remove_files(&evicted);
    if let Err(e) = stored {
        // Typically the asset went while its preview downloaded (its album
        // was un-picked): the file has no row to belong to.
        db::remove_files(&[path]);
        return Err(e.to_string());
    }
    Ok(true)
}

/// Downloads an Immich clip (its playback transcode) into the cache, checks
/// the frame's decoder can take it, and records it. `evict`: make room by
/// evicting the least recently shown (on demand, for a clip about to play);
/// without it (prefetch) a clip that would pass the cap isn't kept.
/// Ok(None) = not kept (the cap). A clip that can't be played here is
/// marked so (out of the queue) and its file removed: an Err.
pub fn fetch_immich_video(
    lib: &Library,
    provider: &mut ImmichProvider,
    asset: AssetId,
    media: &MediaRef,
    evict: bool,
) -> Result<Option<(PathBuf, raam_model::ClipInfo)>, ProviderError> {
    // The fetch thread (on demand) and the library thread (prefetch) may
    // both be after the same clip; each writes its own part file.
    let tmp = lib.cache_dir.join(format!(
        "{asset}.mp4.{}.part",
        if evict { "fetch" } else { "prefetch" }
    ));
    let cap = db::sql_int(lib.cap_bytes());
    let room = if evict {
        cap
    } else {
        cap - db::cached_bytes(&lib.db.lock().unwrap(), SourceKind::Immich)
    };
    // Prefetch with the cache full: nothing to fetch it into.
    let Ok(max_bytes @ 1..) = u64::try_from(room) else {
        return Ok(None);
    };
    let t = clock::now();
    let len = match provider.fetch_video(media, &tmp, max_bytes) {
        Ok(n) => db::sql_int(n),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    // Before the probe: a download stopped at the cap is short, and a
    // probe of it would mark a good clip unplayable. Checked again against
    // the cache now, which may have grown meanwhile.
    if len > room
        || (!evict && db::cached_bytes(&lib.db.lock().unwrap(), SourceKind::Immich) + len > cap)
    {
        let _ = std::fs::remove_file(&tmp);
        return Ok(None);
    }
    let probed = lib.host.probe.probe(&tmp.to_string_lossy());
    log::info!(
        "library: clip {asset}: {} KB in {:?}: {}",
        len / 1024,
        clock::elapsed(t),
        match &probed {
            Ok(i) => format!(
                "{} {}x{} rotation {}, {:.1}s, audio {:?}",
                i.mime,
                i.coded_w,
                i.coded_h,
                i.rotation,
                i.duration_us as f64 / 1e6,
                i.audio
            ),
            Err(e) => e.clone(),
        }
    );
    let playable = probed
        .map_err(|e| format!("unreadable: {e}"))
        .and_then(|info| match lib.host.probe.unplayable(&info) {
            Some(why) => Err(why),
            None => Ok(info),
        });
    let info = match playable {
        Ok(info) => info,
        Err(why) => {
            let _ = std::fs::remove_file(&tmp);
            let files = db::mark_unplayable(&lib.db.lock().unwrap(), asset, &why);
            db::remove_files(&files);
            lib.bump();
            return Err(ProviderError::Failed(format!(
                "can't be played here: {why}"
            )));
        }
    };
    let path = lib.cache_dir.join(format!("{asset}.mp4"));
    // The data is synced already (fetch_video); the rename needs the
    // directory synced, before the row says the clip is here.
    std::fs::rename(&tmp, &path)
        .map_err(|e| ProviderError::Failed(format!("rename {}: {e}", path.display())))?;
    sync_parent(&path).map_err(ProviderError::Failed)?;
    let conn = lib.db.lock().unwrap();
    let evicted = if evict {
        make_room(&conn, Some(asset), len, cap, LRU_BATCH_STORE).0
    } else {
        Vec::new()
    };
    let stored = db::insert_cached_variant(&conn, asset, "video", &path, len, db::now_ms());
    let _ = db::set_playable(&conn, asset);
    drop(conn);
    db::remove_files(&evicted);
    if let Err(e) = stored {
        // The asset went while its clip downloaded (its album un-picked).
        db::remove_files(std::slice::from_ref(&path));
        return Err(ProviderError::Failed(e.to_string()));
    }
    lib.bump();
    Ok(Some((path, info)))
}

/// Evicts the least recently shown Immich files, never `keep` (the asset
/// being stored, if any), until `incoming` more bytes fit under `cap` (or
/// nothing is left to evict).
/// Returns the files to remove once the lock is let go, and how many went.
fn make_room(
    conn: &rusqlite::Connection,
    keep: Option<AssetId>,
    incoming: i64,
    cap: i64,
    batch: usize,
) -> (Vec<PathBuf>, usize) {
    let mut total = db::cached_bytes(conn, SourceKind::Immich);
    let mut files = Vec::new();
    let mut dropped = 0;
    while total + incoming > cap {
        let victims = db::lru_immich(conn, keep, batch);
        if victims.is_empty() {
            break;
        }
        for (id, bytes) in victims {
            if total + incoming <= cap {
                break;
            }
            files.extend(db::drop_cached(conn, id));
            total -= bytes;
            dropped += 1;
            log::info!(
                "library: evicted asset {id} ({} KB), least recently shown",
                bytes / 1024
            );
        }
    }
    (files, dropped)
}

/// Brings the cache under a lowered cap, least recently shown first.
fn enforce_cap(lib: &Library) {
    let conn = lib.db.lock().unwrap();
    let cap_bytes = lib.cap_bytes();
    let (files, dropped) = make_room(&conn, None, 0, db::sql_int(cap_bytes), LRU_BATCH_ENFORCE);
    drop(conn);
    db::remove_files(&files);
    if dropped > 0 {
        log::info!(
            "library: {dropped} previews evicted to fit the {} MB cap",
            cap_bytes / MIB
        );
        lib.bump();
    }
}

/// A power cut leaves either the old file or the whole new one, never an
/// empty or short one (raam#17). The frame mounts /data `noauto_da_alloc`,
/// so ext4 won't flush the data before the rename on its own: the temp
/// file is synced first, and the directory after, so the rename holds too.
/// Each call has its own temp file: the fetch and library threads can store
/// the same preview at once, and a shared one would be cut short under the
/// other's rename. The startup sweep deletes any a crash leaves.
/// `before_rename` runs with the temp file whole and synced: the
/// `debug.video.stall` hook (stall.rs).
fn write_atomic(path: &Path, bytes: &[u8], before_rename: impl FnOnce()) -> Result<(), String> {
    use std::io::Write;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", NEXT.fetch_add(1, Ordering::Relaxed)));
    let tmp = path.with_file_name(name);
    let mut file =
        std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    drop(file);
    before_rename();
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    sync_parent(path)
}

/// Syncs `path`'s directory, so a rename into it survives a power cut.
fn sync_parent(path: &Path) -> Result<(), String> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("sync {}: {e}", dir.display()))
}

/// The controller's read-only view (raam-core app.rs).
impl raam_core::app::LibraryInfo for Library {
    fn version(&self) -> u64 {
        Library::stats_version(self)
    }
    fn stats(&self) -> raam_model::Stats {
        Library::stats(self)
    }
    fn online(&self) -> bool {
        Library::online(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fetch and library threads can store the same preview at once
    /// (raam#17): each must leave a whole file, and neither may fail.
    #[test]
    fn two_writers_of_one_file_both_finish_whole() {
        let dir = std::env::temp_dir().join(format!("raam-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("7.jpg");
        let contents = [vec![0xaa; 64 * 1024], vec![0xbb; 96 * 1024]];
        std::thread::scope(|s| {
            for bytes in &contents {
                let path = &path;
                s.spawn(move || {
                    for _ in 0..200 {
                        write_atomic(path, bytes, || {}).unwrap();
                    }
                });
            }
        });
        let left = std::fs::read(&path).unwrap();
        assert!(contents.contains(&left), "{} bytes, a mix", left.len());
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["7.jpg"], "a temp file was left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The stall hook runs inside the window it is named for: the temp
    /// file whole on disk, the target not yet there.
    #[test]
    fn before_rename_runs_with_the_temp_file_whole() {
        let dir = std::env::temp_dir().join(format!("raam-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("8.jpg");
        let bytes = vec![0xcc; 32 * 1024];
        let mut ran = false;
        write_atomic(&path, &bytes, || {
            ran = true;
            assert!(!path.exists(), "renamed before the hook");
            let temps: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
            assert_eq!(temps.len(), 1);
            assert_eq!(std::fs::read(temps[0].path()).unwrap(), bytes);
        })
        .unwrap();
        assert!(ran);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
