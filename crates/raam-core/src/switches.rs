//! Test-only switches (the `DebugSwitches` seam, seams.rs) the host
//! snapshots once per loop pass: fault injection (`debug.video.fail`) and
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
    /// `panic`: panic right after startup (the Android host's lib.rs).
    Panic,
}

/// Each injectable fault and its `debug.video.fail` value: the one table
/// both the property's decode and the atomic's read go through.
const FAILS: [(Fail, &str); 6] = [
    (Fail::Rt, "rt"),
    (Fail::Probe, "probe"),
    (Fail::Live, "live"),
    (Fail::Hang, "hang"),
    (Fail::Open, "open"),
    (Fail::Panic, "panic"),
];

static FAIL: AtomicU8 = AtomicU8::new(Fail::None as u8);

/// Host only, at startup and then once per loop pass.
pub fn set_fail(prop: &str) {
    let prop = prop.trim();
    let value = FAILS
        .iter()
        .find(|(_, name)| *name == prop)
        .map_or(Fail::None, |(fail, _)| *fail);
    FAIL.store(value as u8, Ordering::Relaxed);
}

pub fn fail() -> Fail {
    let stored = FAIL.load(Ordering::Relaxed);
    FAILS
        .iter()
        .find(|(fail, _)| *fail as u8 == stored)
        .map_or(Fail::None, |(fail, _)| *fail)
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
