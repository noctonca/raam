//! Weather thread: finds the frame's location once from its public IP
//! (ipwho.is, falling back to ipapi.co; both keyless HTTPS), then polls
//! Open-Meteo's current conditions every 15 minutes. Never touches the
//! render thread: results land in a mutex, a version counter bumps, and the
//! host's `Waker` wakes the loop so the overlay rebuilds its text.
//!
//! It calls out only while enabled (`set_enabled`, from the controller's
//! `SetWeather` effect: the setting is on, the clock shows, the app is in
//! front). It starts disabled and waits on a condvar, so with the setting
//! off there is no location lookup and no weather call at all. Turning it
//! off and on again within the 15 minutes makes no extra call.
//!
//! Privacy: the IP lookup necessarily sends the frame's public IP to the geo
//! service; only the city is logged, never the IP or the coordinates.
use raam_core::seams::Waker;
use raam_core::{clock, weather_icons};
use raam_model::limits::{
    WEATHER_HTTP_TIMEOUT, WEATHER_REFRESH, WEATHER_RETRY_MAX, WEATHER_RETRY_MIN,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Location {
    pub city: String,
    lat: f64,
    lon: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Current {
    pub temp_c: f64,
    pub code: i64,
    pub is_day: bool,
}

#[derive(Default)]
struct State {
    enabled: bool,
    location: Option<Location>,
    current: Option<Current>,
}

pub struct WeatherShared {
    state: Mutex<State>,
    /// Signalled when `enabled` changes, so a waiting worker re-checks.
    toggled: Condvar,
    version: AtomicU64,
}

impl WeatherShared {
    /// Lets the worker call out, or stops it; it notices at once, even
    /// mid-wait. The snapshot is kept either way.
    pub fn set_enabled(&self, on: bool) {
        let mut s = self.state.lock().unwrap();
        if s.enabled != on {
            s.enabled = on;
            log::info!("weather calls {}", if on { "on" } else { "off" });
            self.toggled.notify_all();
        }
    }

    /// Blocks until the worker is enabled and `due` (a `clock::now()`
    /// reading) has come.
    fn wait_turn(&self, due: Duration) {
        let mut s = self.state.lock().unwrap();
        loop {
            if !s.enabled {
                s = self.toggled.wait(s).unwrap();
                continue;
            }
            let left = due.saturating_sub(clock::now());
            if left.is_zero() {
                return;
            }
            s = self.toggled.wait_timeout(s, left).unwrap().0;
        }
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> (Option<String>, Option<Current>) {
        let s = self.state.lock().unwrap();
        (s.location.as_ref().map(|l| l.city.clone()), s.current)
    }

    fn publish(&self, waker: &dyn Waker, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
        self.version.fetch_add(1, Ordering::AcqRel);
        waker.wake();
    }
}

/// Why a location lookup or a weather call failed. The worker retries
/// every one with backoff; the variant says what went wrong, for the log.
#[derive(Debug)]
enum WeatherError {
    /// The request didn't complete (DNS, connect, TLS, timeout).
    Transport { host: String, source: ureq::Error },
    /// The service answered with an error status.
    Status {
        host: String,
        status: ureq::http::StatusCode,
    },
    /// The answer's body didn't read as JSON.
    Body { host: String, source: ureq::Error },
    /// ipwho.is answered but couldn't place the IP, in its words.
    NotPlaced(String),
    /// The JSON has no such field, or not of the expected type.
    Missing(&'static str),
}

impl std::fmt::Display for WeatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeatherError::Transport { host, source } => write!(f, "GET {host}: {source}"),
            WeatherError::Status { host, status } => write!(f, "{host} returned {status}"),
            WeatherError::Body { host, source } => write!(f, "{host} body: {source}"),
            WeatherError::NotPlaced(why) => write!(f, "ipwho.is: {why}"),
            WeatherError::Missing(field) => write!(f, "no {field}"),
        }
    }
}

impl std::error::Error for WeatherError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WeatherError::Transport { source, .. } | WeatherError::Body { source, .. } => {
                Some(source)
            }
            WeatherError::Status { .. } | WeatherError::NotPlaced(_) | WeatherError::Missing(_) => {
                None
            }
        }
    }
}

/// The worker's two calls; a test counts them through a fake.
trait Api: Send + 'static {
    fn locate(&self) -> Result<Location, WeatherError>;
    fn current(&self, loc: &Location) -> Result<Current, WeatherError>;
}

struct Http(ureq::Agent);

impl Api for Http {
    fn locate(&self) -> Result<Location, WeatherError> {
        locate(&self.0)
    }
    fn current(&self, loc: &Location) -> Result<Current, WeatherError> {
        current_weather(&self.0, loc)
    }
}

/// Starts the worker disabled; the host enables it on `SetWeather`.
pub fn spawn(waker: Arc<dyn Waker>) -> Arc<WeatherShared> {
    let client = crate::immich::agent(
        WEATHER_HTTP_TIMEOUT,
        concat!(
            "raam/",
            env!("CARGO_PKG_VERSION"),
            " (github.com/noctonca/raam)"
        ),
    );
    spawn_with(waker, Http(client))
}

fn spawn_with(waker: Arc<dyn Waker>, api: impl Api) -> Arc<WeatherShared> {
    let shared = Arc::new(WeatherShared {
        state: Mutex::new(State::default()),
        toggled: Condvar::new(),
        version: AtomicU64::new(0),
    });
    let s = shared.clone();
    std::thread::spawn(move || weather_loop(s, waker, api));
    shared
}

fn weather_loop(shared: Arc<WeatherShared>, waker: Arc<dyn Waker>, api: impl Api) {
    let mut due = clock::now();
    let mut backoff = WEATHER_RETRY_MIN;
    loop {
        shared.wait_turn(due);
        let location = shared.state.lock().unwrap().location.clone();
        let t = clock::now();
        // The wait before the next call on success; `None` retries.
        let next = match location {
            None => match api.locate() {
                Ok(loc) => {
                    log::info!(
                        "location resolved by IP: {} in {:?}",
                        loc.city,
                        clock::elapsed(t)
                    );
                    shared.publish(waker.as_ref(), |s| s.location = Some(loc));
                    Some(Duration::ZERO)
                }
                Err(e) => {
                    log::warn!("IP location failed ({e}), retrying in {backoff:?}");
                    None
                }
            },
            Some(loc) => match api.current(&loc) {
                Ok(c) => {
                    log::info!(
                        "Open-Meteo current: {:.1}C code={} is_day={} ({}) in {:?}",
                        c.temp_c,
                        c.code,
                        c.is_day,
                        weather_icons::describe(c.code, c.is_day).0,
                        clock::elapsed(t)
                    );
                    shared.publish(waker.as_ref(), |s| s.current = Some(c));
                    Some(WEATHER_REFRESH)
                }
                Err(e) => {
                    log::warn!("Open-Meteo failed ({e}), retrying in {backoff:?}");
                    None
                }
            },
        };
        let wait = match next {
            Some(wait) => {
                backoff = WEATHER_RETRY_MIN;
                wait
            }
            None => {
                let wait = backoff;
                backoff = (backoff * 2).min(WEATHER_RETRY_MAX);
                wait
            }
        };
        due = clock::now() + wait;
    }
}

fn get_json(client: &ureq::Agent, url: &str) -> Result<serde_json::Value, WeatherError> {
    let host = || host_of(url).to_string();
    let mut resp = client
        .get(url)
        .call()
        .map_err(|source| WeatherError::Transport {
            host: host(),
            source,
        })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(WeatherError::Status {
            host: host(),
            status,
        });
    }
    resp.body_mut()
        .read_json()
        .map_err(|source| WeatherError::Body {
            host: host(),
            source,
        })
}

fn host_of(url: &str) -> &str {
    url.trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or(url)
}

fn locate(client: &ureq::Agent) -> Result<Location, WeatherError> {
    let primary = get_json(client, "https://ipwho.is/").and_then(|v| {
        if v["success"].as_bool() != Some(true) {
            return Err(WeatherError::NotPlaced(
                v["message"].as_str().unwrap_or("success=false").to_string(),
            ));
        }
        parse_location(&v, "city", "latitude", "longitude")
    });
    match primary {
        Ok(l) => Ok(l),
        Err(e) => {
            log::warn!("ipwho.is failed ({e}), trying ipapi.co");
            let v = get_json(client, "https://ipapi.co/json/")?;
            parse_location(&v, "city", "latitude", "longitude")
        }
    }
}

fn parse_location(
    v: &serde_json::Value,
    city: &str,
    lat: &str,
    lon: &str,
) -> Result<Location, WeatherError> {
    Ok(Location {
        city: v[city]
            .as_str()
            .ok_or(WeatherError::Missing("city"))?
            .to_string(),
        lat: v[lat].as_f64().ok_or(WeatherError::Missing("latitude"))?,
        lon: v[lon].as_f64().ok_or(WeatherError::Missing("longitude"))?,
    })
}

fn current_weather(client: &ureq::Agent, loc: &Location) -> Result<Current, WeatherError> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.3}&longitude={:.3}&current=temperature_2m,weather_code,is_day",
        loc.lat, loc.lon
    );
    parse_current(&get_json(client, &url)?)
}

/// Open-Meteo's `current` block; a missing `is_day` counts as day.
fn parse_current(v: &serde_json::Value) -> Result<Current, WeatherError> {
    let c = &v["current"];
    Ok(Current {
        temp_c: c["temperature_2m"]
            .as_f64()
            .ok_or(WeatherError::Missing("temperature_2m"))?,
        code: c["weather_code"]
            .as_i64()
            .ok_or(WeatherError::Missing("weather_code"))?,
        is_day: c["is_day"].as_i64().unwrap_or(1) != 0,
    })
}

/// The controller's read-only view (raam-core app.rs), in seam terms.
impl raam_core::app::WeatherInfo for WeatherShared {
    fn version(&self) -> u64 {
        WeatherShared::version(self)
    }
    fn snapshot(&self) -> (Option<String>, Option<raam_core::app::WeatherNow>) {
        let (city, current) = WeatherShared::snapshot(self);
        (
            city,
            current.map(|c| raam_core::app::WeatherNow {
                temp_c: c.temp_c,
                code: c.code,
                is_day: c.is_day,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct Calls {
        locate: AtomicUsize,
        current: AtomicUsize,
    }

    impl Calls {
        fn get(&self) -> (usize, usize) {
            (
                self.locate.load(Ordering::SeqCst),
                self.current.load(Ordering::SeqCst),
            )
        }
    }

    struct Fake(Arc<Calls>);

    impl Api for Fake {
        fn locate(&self) -> Result<Location, WeatherError> {
            self.0.locate.fetch_add(1, Ordering::SeqCst);
            Ok(Location {
                city: "Lisbon".into(),
                lat: 38.7,
                lon: -9.1,
            })
        }
        fn current(&self, _loc: &Location) -> Result<Current, WeatherError> {
            self.0.current.fetch_add(1, Ordering::SeqCst);
            Ok(Current {
                temp_c: 18.0,
                code: 0,
                is_day: true,
            })
        }
    }

    struct NoWake;

    impl Waker for NoWake {
        fn wake(&self) {}
    }

    /// Polls until `f` holds, for up to 5 s.
    fn eventually(f: impl Fn() -> bool) -> bool {
        (0..500).any(|_| {
            f() || {
                std::thread::sleep(Duration::from_millis(10));
                false
            }
        })
    }

    #[test]
    fn the_worker_calls_out_only_while_enabled() {
        // The test clock stands still, so the 15-minute refresh never
        // comes round: any call after the first pair is a toggle's.
        crate::install_test_clock();
        let calls = Arc::new(Calls::default());
        let w = spawn_with(Arc::new(NoWake), Fake(calls.clone()));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.get(), (0, 0), "a disabled worker called out");

        w.set_enabled(true);
        assert!(eventually(|| w.snapshot().1.is_some()));
        assert_eq!(calls.get(), (1, 1));
        assert_eq!(w.snapshot().0.as_deref(), Some("Lisbon"));

        // Off and on again within the refresh: no extra call, and the
        // location is never looked up twice.
        w.set_enabled(false);
        w.set_enabled(true);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.get(), (1, 1));
    }

    /// A payload short of a field names it; a whole one parses.
    #[test]
    fn a_payload_missing_a_field_says_which() {
        let v = serde_json::json!({"city": "Lisbon", "latitude": 38.7});
        assert!(matches!(
            parse_location(&v, "city", "latitude", "longitude"),
            Err(WeatherError::Missing("longitude"))
        ));
        let v = serde_json::json!({"city": "Lisbon", "latitude": 38.7, "longitude": -9.1});
        let loc = parse_location(&v, "city", "latitude", "longitude").unwrap();
        assert_eq!(
            (loc.city.as_str(), loc.lat, loc.lon),
            ("Lisbon", 38.7, -9.1)
        );

        let v = serde_json::json!({"current": {"temperature_2m": 18.5}});
        assert!(matches!(
            parse_current(&v),
            Err(WeatherError::Missing("weather_code"))
        ));
        let v = serde_json::json!({"current": {"temperature_2m": 18.5, "weather_code": 3}});
        let c = parse_current(&v).unwrap();
        assert_eq!((c.temp_c, c.code, c.is_day), (18.5, 3, true));
        let v =
            serde_json::json!({"current": {"temperature_2m": 1.0, "weather_code": 0, "is_day": 0}});
        assert!(!parse_current(&v).unwrap().is_day);
    }
}
