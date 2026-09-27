//! The browser's side of the small seams: the clock, the log, the panic
//! report, and the URL-query debug switches.

use raam_core::clock;
use raam_core::seams::DebugSwitches;
use raam_model::LocalTime;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;
use wasm_bindgen::JsValue;

/// `performance.now()` for monotonic time, `Date.now()` for the wall, and
/// the browser's own timezone for local time.
pub fn clock_source() -> clock::Source {
    clock::Source {
        monotonic: || {
            let ms = web_sys::window()
                .and_then(|w| w.performance())
                .map_or(0.0, |p| p.now());
            Duration::from_secs_f64(ms.max(0.0) / 1000.0)
        },
        wall: || Duration::from_secs_f64(js_sys::Date::now().max(0.0) / 1000.0),
        local: |epoch| {
            let d = js_sys::Date::new(&JsValue::from_f64(epoch as f64 * 1000.0));
            LocalTime {
                hour: d.get_hours() as i32,
                min: d.get_minutes() as i32,
                sec: d.get_seconds() as i32,
                mday: d.get_date() as i32,
                mon: d.get_month() as i32,
                wday: d.get_day() as i32,
            }
        },
    }
}

/// The log facade onto the browser console (what android_logger and the
/// desktop's stderr logger are on their hosts).
struct Console;

impl log::Log for Console {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = JsValue::from_str(&format!("[{}] {}", r.target(), r.args()));
        match r.level() {
            log::Level::Error => web_sys::console::error_1(&line),
            log::Level::Warn => web_sys::console::warn_1(&line),
            _ => web_sys::console::log_1(&line),
        }
    }

    fn flush(&self) {}
}

/// The console logger, and a panic hook that reports the panic there:
/// wasm's default hook prints nothing useful. A panic still ends the demo
/// (crash, don't limp); the page shows it as a failed start.
pub fn init_log() {
    static CONSOLE: Console = Console;
    let _ = log::set_logger(&CONSOLE);
    log::set_max_level(log::LevelFilter::Info);
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&JsValue::from_str(&format!("raam panicked: {info}")));
    }));
}

thread_local! {
    static SWITCHES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Seeds the debug switches from the page's query string: any key that
/// starts `debug.` (e.g. `?debug.video.fail=rt`). `set_switch`, exported
/// to the page, changes one live.
pub fn load_switches(query: &web_sys::UrlSearchParams) {
    SWITCHES.with(|s| {
        let mut s = s.borrow_mut();
        for entry in query.entries() {
            let Ok(pair) = entry else { continue };
            let pair = js_sys::Array::from(&pair);
            let (k, v) = (pair.get(0).as_string(), pair.get(1).as_string());
            if let (Some(k), Some(v)) = (k, v)
                && k.starts_with("debug.")
            {
                log::warn!("debug switch {k}={v} (from the URL)");
                s.insert(k, v);
            }
        }
    });
}

pub fn set_switch(name: &str, value: &str) {
    log::warn!("debug switch {name}={value}");
    SWITCHES.with(|s| s.borrow_mut().insert(name.to_string(), value.to_string()));
}

/// The web's `DebugSwitches`: the query string and `set_switch`.
pub struct QuerySwitches;

impl DebugSwitches for QuerySwitches {
    fn get(&self, name: &str) -> String {
        SWITCHES.with(|s| s.borrow().get(name).cloned().unwrap_or_default())
    }
}
