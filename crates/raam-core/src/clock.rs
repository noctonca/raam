//! The Clock seam: every read of the current time goes through here, never
//! through `std::time::Instant`/`SystemTime` directly (both panic on wasm,
//! and the simulation tests need a fake source). The host installs its
//! source once, first thing; reading before that is a bug and panics.
//!
//! Local time is the host's third hand on the clock: the timezone
//! conversion happens host-side (bionic's `localtime_r` applies
//! `persist.sys.timezone` itself on Android), and the core only ever sees
//! the plain `LocalTime`.
//!
//! File timestamps (the local folder's mtimes) are data, not clock reads,
//! and stay `SystemTime` in the engine.

use raam_model::LocalTime;
use std::sync::OnceLock;
use std::time::Duration;

pub struct Source {
    /// Monotonic time since an arbitrary epoch; never goes backwards.
    pub monotonic: fn() -> Duration,
    /// Wall-clock time since the unix epoch.
    pub wall: fn() -> Duration,
    /// The local time at a wall-clock reading (unix seconds), in the
    /// device's timezone.
    pub local: fn(epoch: i64) -> LocalTime,
}

static SOURCE: OnceLock<Source> = OnceLock::new();

/// Host only, exactly once, before anything reads the clock.
pub fn set_source(source: Source) {
    assert!(SOURCE.set(source).is_ok(), "clock source installed twice");
}

fn source() -> &'static Source {
    SOURCE
        .get()
        .expect("clock read before the host installed a source")
}

/// Monotonic now.
pub fn now() -> Duration {
    (source().monotonic)()
}

/// Time since `since`, an earlier `now()` reading. Saturates to zero, as
/// `Instant::elapsed` did.
pub fn elapsed(since: Duration) -> Duration {
    now().saturating_sub(since)
}

/// Wall-clock time since the unix epoch.
pub fn wall() -> Duration {
    (source().wall)()
}

/// The local time at `epoch` (unix seconds).
pub fn local(epoch: i64) -> LocalTime {
    (source().local)(epoch)
}
