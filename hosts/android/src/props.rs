//! The host's hands on the platform clocks and the `debug.video.*` system
//! properties: the Clock seam's source (bionic's `localtime_r` applies
//! `persist.sys.timezone` itself, so no tz database in Rust), the
//! test-only schedule overrides, and the wake mechanism switch.
use raam_core::clock;
use raam_model::LocalTime;
use std::ffi::CString;
use std::time::Duration;

/// The system clocks as the core's Clock source. Installed once, first
/// thing in `android_main`.
pub fn clock_source() -> clock::Source {
    clock::Source {
        monotonic: system_monotonic,
        wall: system_wall,
        local: local_time,
    }
}

fn system_monotonic() -> Duration {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed()
}

fn system_wall() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// The plain local time at `epoch` (unix seconds).
pub fn local_time(epoch: i64) -> LocalTime {
    let tm = local_tm(epoch);
    LocalTime {
        hour: tm.tm_hour,
        min: tm.tm_min,
        sec: tm.tm_sec,
        mday: tm.tm_mday,
        mon: tm.tm_mon,
        wday: tm.tm_wday,
    }
}

/// Bionic's `localtime_r`, which applies `persist.sys.timezone` itself.
fn local_tm(t: i64) -> libc::tm {
    let t = t as libc::time_t; // i32 on armv7 (fine until 2038; the frame is 32-bit anyway)
    // SAFETY: an all-zero `tm` is a valid value (the zone pointer is just
    // null), and both pointers are to locals that outlive the call.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

pub fn prop(name: &str) -> String {
    let name = CString::new(name).unwrap();
    let mut buf = [0u8; 92]; // PROP_VALUE_MAX
    // SAFETY: `name` is a NUL-terminated CString alive for the call, and
    // `buf` is PROP_VALUE_MAX bytes, the most the call ever writes.
    let n = unsafe {
        libc::__system_property_get(name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_char)
    };
    String::from_utf8_lossy(&buf[..n.max(0) as usize]).into_owned()
}

/// TEST-ONLY overrides, set with `adb shell setprop` (they last until the
/// next reboot; `setprop <name> ""` clears one):
/// - `debug.video.sleep` / `debug.video.wake`: "HH:MM"
/// - `debug.video.idle`: manual-wake idle timeout in seconds
/// - `debug.video.mech`: how to turn the screen on: "flags", "wakelock"
///   or "both" (the default)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DebugProps {
    pub sleep: Option<u32>,
    pub wake: Option<u32>,
    pub idle: Option<Duration>,
    pub mech: String,
}

pub fn debug_props() -> DebugProps {
    DebugProps {
        sleep: raam_core::schedule::parse_hm(&prop("debug.video.sleep")),
        wake: raam_core::schedule::parse_hm(&prop("debug.video.wake")),
        idle: prop("debug.video.idle")
            .trim()
            .parse()
            .ok()
            .map(Duration::from_secs),
        mech: prop("debug.video.mech").trim().to_string(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WakeMech {
    Flags,
    WakeLock,
    Both,
}

impl WakeMech {
    pub fn from_prop(s: &str) -> Self {
        match s {
            "flags" => WakeMech::Flags,
            "wakelock" => WakeMech::WakeLock,
            _ => WakeMech::Both,
        }
    }
    pub fn flags(self) -> bool {
        self != WakeMech::WakeLock
    }
    pub fn wakelock(self) -> bool {
        self != WakeMech::Flags
    }
}
