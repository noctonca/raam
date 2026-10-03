//! The Store seam's currency: settings as rows of `(key, JSON value)`.
//! The controller compares rows to decide when settings are dirty and
//! hands the same rows out in its save effect; raam-engine's writer puts
//! them in the `setting` table, and other hosts keep them wherever they
//! can (localStorage on the web). The rows are built here, not in the
//! engine, so the dirty check needs no engine.

use raam_model::{Settings, TransitionChoice};

/// The `setting` rows for everything in `Settings` except the server and
/// key (on `source`/`credential`) and the sleep schedule (`schedule`).
pub fn settings_rows(s: &Settings) -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        ("slideshow.interval_s", json!(s.interval_secs)),
        ("slideshow.transition", json!(transition_str(s.transition))),
        ("slideshow.ken_burns", json!(s.ken_burns_enabled)),
        ("display.fill_by_default", json!(s.fill_by_default)),
        ("display.fit_background", json!(s.fit_background.as_str())),
        ("collage.max", json!(s.collage_max)),
        ("collage.gap_colour", json!(s.gap_colour.as_str())),
        ("overlay.clock_style", json!(s.clock_style.as_str())),
        ("overlay.clock_corner", json!(s.clock_corner.as_str())),
        ("overlay.weather", json!(s.weather_enabled)),
        ("locale.clock_24h", json!(s.clock_24h)),
        (
            "ui.theme",
            json!(if s.dark_theme { "dark" } else { "light" }),
        ),
        ("cache.cap_mb", json!(s.cache_cap_mb)),
        ("video.playback", json!(s.video_playback.as_str())),
        ("video.sound", json!(s.video_sound)),
        ("video.volume", json!(s.video_volume)),
        ("video.audio_delay_ms", json!(s.audio_delay_ms)),
    ]
}

/// The stored name of a transition choice ("rotate" for the rotation).
pub fn transition_str(t: TransitionChoice) -> &'static str {
    t.shader_name().unwrap_or("rotate")
}
