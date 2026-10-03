//! The data engine: fetch/library/writer/weather threads, SQLite, HTTP,
//! the providers (Immich, local folder) and the cache. Native only —
//! never compiled for wasm. Implements the core's `TileSource`, wakes
//! the host through the core's `Waker` seam, and runs the App
//! controller's effects (`run_effects`).

// The engine's panics are `lock().unwrap()`, TigerStyle's policy (a
// poisoned lock is a crashed invariant), and expects that can't fire: a
// `# Panics` section on every function that takes a lock says nothing.
#![allow(clippy::missing_panics_doc)]

pub mod db;
pub mod fetch;
pub mod immich;
pub mod library;
pub mod provider;
pub mod stall;
pub mod weather;

use raam_core::app::Effect;
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

/// Runs the App controller's effects on the engine, in order: a native
/// host's step after each `frame` pass. The fetch and weather workers
/// start with the first window, so before it a host has neither to give
/// (and the controller sends nothing for them).
pub fn run_effects(
    effects: Vec<Effect>,
    lib: &library::Library,
    fetch: Option<&fetch::FetchShared>,
    weather: Option<&weather::WeatherShared>,
) {
    for effect in effects {
        match effect {
            Effect::SaveSettings { rows, sleep } => {
                lib.send(library::Cmd::SaveSettings { rows, sleep })
            }
            Effect::SetScale(key, mode) => lib.send(library::Cmd::SetScale(key, mode)),
            Effect::SetHidden(key, hidden) => lib.send(library::Cmd::SetHidden(key, hidden)),
            Effect::SetSourceEnabled(kind, on) => lib.set_enabled(kind, on),
            Effect::SetServer { url, key } => {
                lib.send(library::Cmd::SetServer(immich::Config { url, key }))
            }
            Effect::ExportCuration => lib.send(library::Cmd::ExportCuration),
            Effect::SelectAlbum(album, on) => lib.send(library::Cmd::SelectAlbum(album, on)),
            Effect::SetCap(cap) => lib.send(library::Cmd::SetCap(cap)),
            Effect::ClearCache => lib.send(library::Cmd::ClearCache),
            Effect::Rescan => lib.send(library::Cmd::Rescan),
            Effect::SyncNow => lib.send(library::Cmd::SyncNow),
            Effect::SetMaxGroup(max) => {
                if let Some(f) = fetch {
                    f.set_max_group(max);
                }
            }
            Effect::SetWeather(on) => {
                if let Some(w) = weather {
                    w.set_enabled(on);
                }
            }
        }
    }
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
