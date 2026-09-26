//! Weather thread: finds the frame's location once from its public IP
//! (ipwho.is, falling back to ipapi.co; both keyless HTTPS), then polls
//! Open-Meteo's current conditions every 15 minutes. Never touches the
//! render thread: results land in a mutex, a version counter bumps, and the
//! `AndroidAppWaker` wakes the loop so the overlay rebuilds its text.
//!
//! Privacy: the IP lookup necessarily sends the frame's public IP to the geo
//! service; only the city is logged, never the IP or the coordinates.
use raam_core::seams::Waker;
use raam_core::{clock, weather_icons};
use raam_model::limits::{
    WEATHER_HTTP_TIMEOUT, WEATHER_REFRESH, WEATHER_RETRY_MAX, WEATHER_RETRY_MIN,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
    location: Option<Location>,
    current: Option<Current>,
}

pub struct WeatherShared {
    state: Mutex<State>,
    version: AtomicU64,
}

impl WeatherShared {
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

pub fn spawn(waker: Arc<dyn Waker>) -> Arc<WeatherShared> {
    let shared = Arc::new(WeatherShared {
        state: Mutex::new(State::default()),
        version: AtomicU64::new(0),
    });
    let s = shared.clone();
    std::thread::spawn(move || weather_loop(s, waker));
    shared
}

fn weather_loop(shared: Arc<WeatherShared>, waker: Arc<dyn Waker>) {
    let client = crate::immich::agent(
        WEATHER_HTTP_TIMEOUT,
        concat!(
            "raam/",
            env!("CARGO_PKG_VERSION"),
            " (github.com/noctonca/raam)"
        ),
    );

    let mut backoff = WEATHER_RETRY_MIN;
    let location = loop {
        let t = clock::now();
        match locate(&client) {
            Ok(loc) => {
                log::info!(
                    "location resolved by IP: {} in {:?}",
                    loc.city,
                    clock::elapsed(t)
                );
                break loc;
            }
            Err(e) => {
                log::warn!("IP location failed ({e}), retrying in {backoff:?}");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(WEATHER_RETRY_MAX);
            }
        }
    };
    shared.publish(waker.as_ref(), |s| s.location = Some(location.clone()));

    let mut backoff = WEATHER_RETRY_MIN;
    loop {
        let t = clock::now();
        match current_weather(&client, &location) {
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
                backoff = WEATHER_RETRY_MIN;
                std::thread::sleep(WEATHER_REFRESH);
            }
            Err(e) => {
                log::warn!("Open-Meteo failed ({e}), retrying in {backoff:?}");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(WEATHER_RETRY_MAX);
            }
        }
    }
}

fn get_json(client: &ureq::Agent, url: &str) -> Result<serde_json::Value, String> {
    let mut resp = client
        .get(url)
        .call()
        .map_err(|e| format!("GET {}: {e}", host_of(url)))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{} returned {status}", host_of(url)));
    }
    resp.body_mut()
        .read_json()
        .map_err(|e| format!("{} body: {e}", host_of(url)))
}

fn host_of(url: &str) -> &str {
    url.trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or(url)
}

fn locate(client: &ureq::Agent) -> Result<Location, String> {
    let primary = get_json(client, "https://ipwho.is/").and_then(|v| {
        if v["success"].as_bool() != Some(true) {
            return Err(format!(
                "ipwho.is: {}",
                v["message"].as_str().unwrap_or("success=false")
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
) -> Result<Location, String> {
    Ok(Location {
        city: v[city].as_str().ok_or("no city")?.to_string(),
        lat: v[lat].as_f64().ok_or("no latitude")?,
        lon: v[lon].as_f64().ok_or("no longitude")?,
    })
}

fn current_weather(client: &ureq::Agent, loc: &Location) -> Result<Current, String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.3}&longitude={:.3}&current=temperature_2m,weather_code,is_day",
        loc.lat, loc.lon
    );
    let v = get_json(client, &url)?;
    let c = &v["current"];
    Ok(Current {
        temp_c: c["temperature_2m"].as_f64().ok_or("no temperature_2m")?,
        code: c["weather_code"].as_i64().ok_or("no weather_code")?,
        is_day: c["is_day"].as_i64().unwrap_or(1) != 0,
    })
}
