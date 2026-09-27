//! Test-only switches the host snapshots once per loop pass — the start of
//! Raam's `DebugSwitches` seam: fault injection (`debug.video.fail`) and
//! the two video holds. The sites that act on them (gl.rs, video.rs, the
//! Android player) read the flags and never the Android property, so they
//! stay portable.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// `debug.video.fail` values. An unknown value reads as `None`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fail {
    None,
    /// `rt`: every render target made after startup fails (gl.rs).
    Rt,
    /// `probe`: the probe decoder fails at once (player.rs).
    Probe,
    /// `live`: the live decoder fails at once (player.rs).
    Live,
    /// `hang`: the live decoder fails and holds its release 15 s, as a
    /// wedged stop does (player.rs).
    Hang,
    /// `open`: opening a player fails at once, as a failed SurfaceTexture
    /// setup does (the Android host's video.rs).
    Open,
    /// `panic`: panic right after startup (lib.rs).
    Panic,
}

static FAIL: AtomicU8 = AtomicU8::new(Fail::None as u8);

/// Host only, at startup and then once per loop pass.
pub fn set_fail(prop: &str) {
    let value = match prop.trim() {
        "rt" => Fail::Rt,
        "probe" => Fail::Probe,
        "live" => Fail::Live,
        "hang" => Fail::Hang,
        "open" => Fail::Open,
        "panic" => Fail::Panic,
        _ => Fail::None,
    };
    FAIL.store(value as u8, Ordering::Relaxed);
}

pub fn fail() -> Fail {
    match FAIL.load(Ordering::Relaxed) {
        x if x == Fail::Rt as u8 => Fail::Rt,
        x if x == Fail::Probe as u8 => Fail::Probe,
        x if x == Fail::Live as u8 => Fail::Live,
        x if x == Fail::Hang as u8 => Fail::Hang,
        x if x == Fail::Open as u8 => Fail::Open,
        x if x == Fail::Panic as u8 => Fail::Panic,
        _ => Fail::None,
    }
}

static HOLD_FIRST: AtomicBool = AtomicBool::new(false);
static SHOW_STILL: AtomicBool = AtomicBool::new(false);

/// Host only, at startup and then once per loop pass.
/// `debug.video.hold_first=1` keeps a live clip on frame 0 (to screenshot
/// the slide it landed on); `debug.video.show_still=1` draws the composed
/// still instead of the live picture.
pub fn set_video_holds(hold_first: bool, show_still: bool) {
    HOLD_FIRST.store(hold_first, Ordering::Relaxed);
    SHOW_STILL.store(show_still, Ordering::Relaxed);
}

pub fn hold_first() -> bool {
    HOLD_FIRST.load(Ordering::Relaxed)
}

pub fn show_still() -> bool {
    SHOW_STILL.load(Ordering::Relaxed)
}
