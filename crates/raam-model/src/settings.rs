//! The one `Settings` type and its value enums. The UI edits it, the
//! pipeline reads it, the engine persists it (string-keyed JSON rows,
//! mapped by hand in raam-engine's db module).

use crate::limits;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScaleMode {
    Fill,
    Fit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FitBackground {
    Blurred,
    Black,
}

/// How long a clip stays, with Frameo's three choices (its default is
/// "Play once"; ours is `Continue`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoPlayback {
    /// "Play once and continue": the slideshow moves on when the clip ends.
    Continue,
    /// "Loop videos": loops until the interval is up, then (like Frameo,
    /// which turns looping off when its timer fires) finishes that pass.
    Loop,
    /// "Play once": plays once, then holds the last frame for an interval
    /// (Frameo restarts its timer at the end) or until Next.
    Wait,
}

impl VideoPlayback {
    pub const ALL: [VideoPlayback; 3] = [
        VideoPlayback::Continue,
        VideoPlayback::Loop,
        VideoPlayback::Wait,
    ];

    pub fn label(self) -> &'static str {
        match self {
            VideoPlayback::Continue => "Play once and continue",
            VideoPlayback::Loop => "Loop for the interval",
            VideoPlayback::Wait => "Play once, then wait",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VideoPlayback::Continue => "continue",
            VideoPlayback::Loop => "loop",
            VideoPlayback::Wait => "wait",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "loop" => VideoPlayback::Loop,
            "wait" => VideoPlayback::Wait,
            _ => VideoPlayback::Continue,
        }
    }
}

/// The colour of the separators between collage tiles. White is what
/// Frameo ships; black matches this project's other backgrounds and is
/// the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GapColour {
    Black,
    White,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClockStyle {
    Off,
    TopRight,
    BottomLeft,
}

impl ClockStyle {
    pub fn label(self) -> &'static str {
        match self {
            ClockStyle::Off => "Off",
            ClockStyle::TopRight => "Top right",
            ClockStyle::BottomLeft => "Bottom left",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionChoice {
    Rotate,
    Fade,
    DirectionalWipe,
    Cube,
    Crosswarp,
    Swap,
}

impl TransitionChoice {
    pub const ALL: [TransitionChoice; 6] = [
        TransitionChoice::Rotate,
        TransitionChoice::Fade,
        TransitionChoice::DirectionalWipe,
        TransitionChoice::Cube,
        TransitionChoice::Crosswarp,
        TransitionChoice::Swap,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            TransitionChoice::Rotate => "Rotate through all",
            TransitionChoice::Fade => "Fade",
            TransitionChoice::DirectionalWipe => "Directional wipe",
            TransitionChoice::Cube => "Cube",
            TransitionChoice::Crosswarp => "Crosswarp",
            TransitionChoice::Swap => "Swap",
        }
    }

    /// The matching gl-transitions shader name, `None` for rotate.
    pub fn shader_name(&self) -> Option<&'static str> {
        match self {
            TransitionChoice::Rotate => None,
            TransitionChoice::Fade => Some("fade"),
            TransitionChoice::DirectionalWipe => Some("directionalwipe"),
            TransitionChoice::Cube => Some("cube"),
            TransitionChoice::Crosswarp => Some("crosswarp"),
            TransitionChoice::Swap => Some("swap"),
        }
    }
}

pub struct Settings {
    pub server_url: String,
    pub api_key: String,
    pub interval_secs: f32,
    pub transition: TransitionChoice,
    pub ken_burns_enabled: bool,
    pub fill_by_default: bool,
    pub fit_background: FitBackground,
    /// Frameo's "Slideshow collage max"; 1 turns collages off (Frameo has a
    /// separate on/off switch, off by default).
    pub collage_max: usize,
    /// The screen-derived default (Frameo's, from the screen diagonal),
    /// shown beside the setting.
    pub screen_default_max: usize,
    pub gap_colour: GapColour,
    pub clock_style: ClockStyle,
    pub clock_24h: bool,
    pub sleep_enabled: bool,
    /// Minutes after local midnight.
    pub sleep_min: u32,
    pub wake_min: u32,
    /// The two photo sources; at least one stays on.
    pub immich_enabled: bool,
    pub local_enabled: bool,
    pub cache_cap_mb: u32,
    /// How long a clip stays, sound (off by default) and its volume.
    pub video_playback: VideoPlayback,
    pub video_sound: bool,
    pub video_volume: f32,
    /// Lip-sync calibration (ms) on top of Android's reported output
    /// latency; +180 measured on the SNUG frame with a filmed flash/beep.
    pub audio_delay_ms: i32,
    /// The menus' and settings' theme (the slideshow is photos either way).
    pub dark_theme: bool,
}

impl Settings {
    /// First-run defaults; the engine's saved rows land over these.
    pub fn defaults(server_url: &str, api_key: &str) -> Self {
        Self {
            server_url: server_url.to_string(),
            api_key: api_key.to_string(),
            interval_secs: 10.0,
            transition: TransitionChoice::Rotate,
            ken_burns_enabled: true,
            fill_by_default: true,
            fit_background: FitBackground::Blurred,
            collage_max: 3,
            screen_default_max: 3,
            gap_colour: GapColour::Black,
            clock_style: ClockStyle::TopRight,
            clock_24h: true,
            sleep_enabled: true,
            sleep_min: limits::DEFAULT_SLEEP_MIN,
            wake_min: limits::DEFAULT_WAKE_MIN,
            immich_enabled: true,
            local_enabled: true,
            cache_cap_mb: limits::DEFAULT_CAP_MB,
            video_playback: VideoPlayback::Continue,
            video_sound: false,
            video_volume: 0.5,
            audio_delay_ms: limits::AUDIO_DELAY_DEFAULT_MS,
            dark_theme: true,
        }
    }
}
