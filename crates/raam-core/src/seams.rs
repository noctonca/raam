//! The small host-implemented seams (docs/ARCHITECTURE.md "The seams").
//! `Clock` lives in clock.rs, `TileSource` in source.rs and the video seam
//! in video.rs; here are the ones that are a single trait each.

use raam_model::ClipInfo;

/// Wakes the host's event loop from another thread. On Android it wraps
/// `AndroidAppWaker`; a test wakes a condvar.
pub trait Waker: Send + Sync {
    fn wake(&self);
}

/// Clip metadata without decoding: the Android host's `AMediaExtractor`
/// probe; a stub elsewhere. `unplayable` is the host's judgment of its own
/// decoder (device capability data lives with the host, never in the model
/// or a shared table).
pub trait MediaProbe: Send + Sync {
    fn probe(&self, path: &str) -> Result<ClipInfo, String>;
    /// Why this device can't play the clip, if it can't.
    fn unplayable(&self, info: &ClipInfo) -> Option<String>;
}

/// Test-only debug switches (`debug.video.*`): Android system properties,
/// desktop env vars, web URL query. An unset switch reads as "".
pub trait DebugSwitches: Send + Sync {
    fn get(&self, name: &str) -> String;
}
