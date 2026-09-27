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

/// A fake clock the tests wind by hand. The source fns read thread-locals,
/// so parallel tests don't share time; the process-global source is
/// installed once, by whichever test gets there first, and every test
/// module shares it (a second `set_source` would panic).
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::Cell;
    use std::time::Duration;

    thread_local! {
        static NOW_MS: Cell<u64> = const { Cell::new(0) };
        static WALL_S: Cell<u64> = const { Cell::new(12 * 3600) };
    }

    pub fn install() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            super::set_source(super::Source {
                monotonic: || NOW_MS.with(|c| Duration::from_millis(c.get())),
                wall: || WALL_S.with(|c| Duration::from_secs(c.get())),
                local: |epoch| raam_model::LocalTime {
                    hour: ((epoch / 3600) % 24) as i32,
                    min: ((epoch / 60) % 60) as i32,
                    sec: (epoch % 60) as i32,
                    mday: 1,
                    mon: 0,
                    wday: 0,
                },
            });
        });
    }

    pub fn advance(d: Duration) {
        NOW_MS.with(|c| c.set(c.get() + d.as_millis() as u64));
        WALL_S.with(|c| c.set(c.get() + d.as_secs()));
    }

    pub fn set_wall_hm(h: u64, m: u64) {
        WALL_S.with(|c| c.set(h * 3600 + m * 60));
    }
}
