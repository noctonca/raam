//! The desktop's hands on the platform: the Clock seam's source (libc's
//! `localtime_r` applies the system timezone, as bionic's does on the
//! frame), the debug switches from env vars, and where the engine keeps
//! its files.
use raam_core::clock;
use raam_core::seams::DebugSwitches;
use raam_model::LocalTime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// The system clocks as the core's Clock source. Installed once, first
/// thing in the live mode.
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

fn local_time(epoch: i64) -> LocalTime {
    let t = epoch as libc::time_t;
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    LocalTime {
        hour: tm.tm_hour,
        min: tm.tm_min,
        sec: tm.tm_sec,
        mday: tm.tm_mday,
        mon: tm.tm_mon,
        wday: tm.tm_wday,
    }
}

/// `debug.video.fail` reads the env var `RAAM_DEBUG_VIDEO_FAIL`, and so
/// on for every switch; a value set live (F5) wins over the env.
#[derive(Default)]
pub struct EnvSwitches {
    live: Mutex<HashMap<String, String>>,
}

impl EnvSwitches {
    pub fn env_name(name: &str) -> String {
        format!("RAAM_{}", name.replace('.', "_").to_uppercase())
    }

    /// Sets a switch for the rest of the run; "" clears it.
    pub fn set(&self, name: &str, value: &str) {
        self.live
            .lock()
            .unwrap()
            .insert(name.to_string(), value.to_string());
    }
}

impl DebugSwitches for EnvSwitches {
    fn get(&self, name: &str) -> String {
        if let Some(v) = self.live.lock().unwrap().get(name) {
            return v.clone();
        }
        std::env::var(Self::env_name(name)).unwrap_or_default()
    }
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set; pass --data and --photos".to_string())
}

/// Where the engine keeps the DB and its caches: the platform's app-data
/// dir.
pub fn data_dir() -> Result<PathBuf, String> {
    if cfg!(target_os = "macos") {
        return Ok(home()?.join("Library/Application Support/raam"));
    }
    match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        Some(d) => Ok(PathBuf::from(d).join("raam")),
        None => Ok(home()?.join(".local/share/raam")),
    }
}

/// The photos folder a fresh data dir watches, as /sdcard/Pictures/Frame
/// is on the frame. The engine creates it if it isn't there.
pub fn photos_dir() -> Result<PathBuf, String> {
    Ok(home()?.join("Pictures/Raam"))
}
