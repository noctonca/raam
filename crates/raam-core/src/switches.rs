//! Test-only fault injection (`debug.video.fail`), held as a flag the host
//! snapshots once per loop pass — the start of Raam's `DebugSwitches` seam.
//! The injection sites (gl.rs, player.rs) read the flag and never the
//! Android property, so they stay portable.

use std::sync::atomic::{AtomicU8, Ordering};

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
        x if x == Fail::Panic as u8 => Fail::Panic,
        _ => Fail::None,
    }
}
