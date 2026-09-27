//! The data engine: fetch/library/writer/weather threads, SQLite, HTTP,
//! the providers (Immich, local folder) and the cache. Native only —
//! never compiled for wasm. Implements the core's `TileSource` and wakes
//! the host through the core's `Waker` seam.

pub mod db;
pub mod fetch;
pub mod immich;
pub mod library;
pub mod provider;
pub mod weather;

use std::path::PathBuf;
use std::sync::Arc;

/// The host's side of the engine's seams, handed in at spawn time and
/// carried by the engine's threads.
#[derive(Clone)]
pub struct Host {
    pub waker: Arc<dyn raam_core::seams::Waker>,
    pub switches: Arc<dyn raam_core::seams::DebugSwitches>,
    pub probe: Arc<dyn raam_core::seams::MediaProbe>,
    /// The one-time storage grant for the photos folder (the root map in
    /// docs/ARCHITECTURE.md). Called once, lazily, when the folder can't
    /// be read; a host without the concept returns true.
    pub grant_storage: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// Where the engine keeps and finds its files; all host business.
#[derive(Clone)]
pub struct Paths {
    /// The app's private files directory (the DB and the caches).
    pub files_dir: PathBuf,
    /// The photos folder a fresh install watches, until settings say
    /// otherwise ("/sdcard/Pictures/Frame" on the frame).
    pub local_dir_default: String,
    /// The curation JSON export, next to the folder, not in it, so it is
    /// never scanned. Also where a fresh install imports curation from
    /// (`db::import_curation`).
    pub curation_export: PathBuf,
}

/// A fixed clock for the engine's tests, installed once per test process:
/// the DB only stamps rows with it, and a wait on it never runs out.
#[cfg(test)]
pub(crate) fn install_test_clock() {
    use raam_core::clock;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        clock::set_source(clock::Source {
            monotonic: || std::time::Duration::ZERO,
            wall: || std::time::Duration::from_secs(1_790_000_000),
            local: |_| raam_model::LocalTime {
                hour: 12,
                min: 0,
                sec: 0,
                mday: 1,
                mon: 0,
                wday: 0,
            },
        })
    });
}
