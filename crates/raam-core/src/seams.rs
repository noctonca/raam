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
    /// Why no clip plays here at all, on a host with no player (the
    /// desktop's `NoVideo`). The engine then marks every clip unplayable
    /// with this reason as it lists it, so none is fetched just to be
    /// probed, and none is planned. `None` on a host with decoders.
    fn no_player(&self) -> Option<String> {
        None
    }
}

/// Test-only debug switches (`debug.video.*`): Android system properties,
/// desktop env vars, web URL query. An unset switch reads as "".
pub trait DebugSwitches: Send + Sync {
    fn get(&self, name: &str) -> String;
}

/// The screen and the wake alarm, for the controller's sleep/wake state
/// machine. The Android host implements it over JNI and root (power.rs);
/// a host without the concept (desktop, web) passes no power at all and
/// the schedule stays off.
pub trait Power {
    /// Arms the RTC wake alarm for `epoch_ms`. An error keeps the app
    /// awake (the controller logs and retries next pass).
    fn set_wake_alarm(&mut self, epoch_ms: i64) -> Result<(), String>;
    /// Turns the screen off now; called only after the alarm is armed.
    fn sleep_screen(&mut self);
    /// Lights the screen. The host may make it flags-only per its wake
    /// mechanism; failures are the controller's to log.
    fn wake_screen(&mut self) -> Result<(), String>;
    /// Whether the screen is interactive, for the wake logs.
    fn is_interactive(&self) -> Result<bool, String>;
}
