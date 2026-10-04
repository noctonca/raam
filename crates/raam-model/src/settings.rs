//! The one `Settings` type and its value enums. The UI edits it, the
//! pipeline reads it, the engine persists it (string-keyed JSON rows,
//! mapped by hand in raam-engine's db module). Each value enum's stored
//! name is its `as_str`, read back by `parse`, so raam-core's rows and
//! the engine's load share one spelling.

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

impl FitBackground {
    pub const ALL: [FitBackground; 2] = [FitBackground::Blurred, FitBackground::Black];

    pub fn as_str(self) -> &'static str {
        match self {
            FitBackground::Blurred => "blurred",
            FitBackground::Black => "black",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
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

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
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

impl GapColour {
    pub const ALL: [GapColour; 2] = [GapColour::Black, GapColour::White];

    pub fn as_str(self) -> &'static str {
        match self {
            GapColour::Black => "black",
            GapColour::White => "white",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// The clock overlay's look. Style and position are separate choices
/// (`clock_corner`): both looks can sit in any corner.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClockStyle {
    Off,
    /// The big bold white clock with a date/temperature row.
    Simple,
    /// The cream stack: date, time, weather and its description.
    Detailed,
}

impl ClockStyle {
    pub const ALL: [ClockStyle; 3] = [ClockStyle::Off, ClockStyle::Simple, ClockStyle::Detailed];

    pub fn label(self) -> &'static str {
        match self {
            ClockStyle::Off => "Off",
            ClockStyle::Simple => "Simple",
            ClockStyle::Detailed => "Detailed",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ClockStyle::Off => "off",
            ClockStyle::Simple => "simple",
            ClockStyle::Detailed => "detailed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// A screen corner, for anchoring the clock overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Corner::TopLeft => "Top left",
            Corner::TopRight => "Top right",
            Corner::BottomLeft => "Bottom left",
            Corner::BottomRight => "Bottom right",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Corner::TopLeft => "topleft",
            Corner::TopRight => "topright",
            Corner::BottomLeft => "bottomleft",
            Corner::BottomRight => "bottomright",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn is_top(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::TopRight)
    }

    pub fn is_left(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::BottomLeft)
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
    pub clock_corner: Corner,
    pub clock_24h: bool,
    /// The local weather on the clock. Off by default: it sends the
    /// frame's public IP to a geolocation service and asks Open-Meteo
    /// every 15 minutes while the clock shows.
    pub weather_enabled: bool,
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
            clock_style: ClockStyle::Simple,
            clock_corner: Corner::TopRight,
            clock_24h: true,
            weather_enabled: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Each `ALL` must list every variant once: the settings pages pick
    /// by `ALL[i]` and find the current choice by its position there. A
    /// new variant breaks a match below, which names where it goes.
    #[test]
    fn every_all_lists_every_variant() {
        fn check<T: Copy + PartialEq + std::fmt::Debug>(all: &[T], slot: fn(T) -> usize) {
            for (i, v) in all.iter().enumerate() {
                assert_eq!(slot(*v), i, "{v:?} is out of place in ALL");
            }
        }
        check(&VideoPlayback::ALL, |v| match v {
            VideoPlayback::Continue => 0,
            VideoPlayback::Loop => 1,
            VideoPlayback::Wait => 2,
        });
        assert_eq!(VideoPlayback::ALL.len(), 3);
        check(&FitBackground::ALL, |v| match v {
            FitBackground::Blurred => 0,
            FitBackground::Black => 1,
        });
        assert_eq!(FitBackground::ALL.len(), 2);
        check(&GapColour::ALL, |v| match v {
            GapColour::Black => 0,
            GapColour::White => 1,
        });
        assert_eq!(GapColour::ALL.len(), 2);
        check(&ClockStyle::ALL, |v| match v {
            ClockStyle::Off => 0,
            ClockStyle::Simple => 1,
            ClockStyle::Detailed => 2,
        });
        assert_eq!(ClockStyle::ALL.len(), 3);
        check(&Corner::ALL, |v| match v {
            Corner::TopLeft => 0,
            Corner::TopRight => 1,
            Corner::BottomLeft => 2,
            Corner::BottomRight => 3,
        });
        assert_eq!(Corner::ALL.len(), 4);
        check(&TransitionChoice::ALL, |v| match v {
            TransitionChoice::Rotate => 0,
            TransitionChoice::Fade => 1,
            TransitionChoice::DirectionalWipe => 2,
            TransitionChoice::Cube => 3,
            TransitionChoice::Crosswarp => 4,
            TransitionChoice::Swap => 5,
        });
        assert_eq!(TransitionChoice::ALL.len(), 6);
    }

    /// `parse` finds a value by its `as_str` in `ALL`, so two variants
    /// sharing a stored name would load as the first of them.
    #[test]
    fn every_stored_name_is_distinct_and_parses_back() {
        fn check<T: Copy + PartialEq + std::fmt::Debug>(
            all: &[T],
            as_str: fn(T) -> &'static str,
            parse: fn(&str) -> Option<T>,
        ) {
            for (i, v) in all.iter().enumerate() {
                assert_eq!(parse(as_str(*v)), Some(*v), "{v:?}");
                for w in &all[i + 1..] {
                    assert_ne!(as_str(*v), as_str(*w), "{v:?} and {w:?}");
                }
            }
            assert_eq!(parse("no such name"), None);
        }
        check(
            &FitBackground::ALL,
            FitBackground::as_str,
            FitBackground::parse,
        );
        check(
            &VideoPlayback::ALL,
            VideoPlayback::as_str,
            VideoPlayback::parse,
        );
        check(&GapColour::ALL, GapColour::as_str, GapColour::parse);
        check(&ClockStyle::ALL, ClockStyle::as_str, ClockStyle::parse);
        check(&Corner::ALL, Corner::as_str, Corner::parse);
    }

    // Not under miri: proptest reads the working directory (its
    // regressions file) and the OS's randomness, which miri's isolation
    // refuses, and miri is here for unsafe code, which these don't touch.
    #[cfg(not(miri))]
    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// Every stored name, so the text below lands on and near them.
        fn names() -> Vec<&'static str> {
            let mut all = Vec::new();
            all.extend(FitBackground::ALL.map(FitBackground::as_str));
            all.extend(VideoPlayback::ALL.map(VideoPlayback::as_str));
            all.extend(GapColour::ALL.map(GapColour::as_str));
            all.extend(ClockStyle::ALL.map(ClockStyle::as_str));
            all.extend(Corner::ALL.map(Corner::as_str));
            all
        }

        /// A stored name as a row might hold it: exact, cased or padded
        /// differently, cut short, or any text at all.
        fn row_text() -> impl Strategy<Value = String> {
            let near = (
                prop::sample::select(names()),
                0..5u8,
                any::<prop::sample::Index>(),
            )
                .prop_map(|(name, edit, at)| match edit {
                    0 => name.to_string(),
                    1 => name.to_uppercase(),
                    2 => format!(" {name}"),
                    3 => format!("{name}\0"),
                    _ => name[..at.index(name.len())].to_string(),
                });
            prop_oneof![near, any::<String>()]
        }

        /// `parse` takes a row back only to the value that writes exactly
        /// that text, and `as_str` of what it found is the row: a name
        /// near another's never loads as a different setting.
        fn parses_only_its_own_name<T: Copy + std::fmt::Debug>(
            text: &str,
            as_str: fn(T) -> &'static str,
            parse: fn(&str) -> Option<T>,
            all: &[T],
        ) -> Result<(), TestCaseError> {
            match parse(text) {
                Some(v) => prop_assert_eq!(as_str(v), text),
                None => prop_assert!(all.iter().all(|v| as_str(*v) != text), "{text:?}"),
            }
            Ok(())
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn any_row_text_parses_only_to_its_own_value(text in row_text()) {
                parses_only_its_own_name(
                    &text, FitBackground::as_str, FitBackground::parse, &FitBackground::ALL)?;
                parses_only_its_own_name(
                    &text, VideoPlayback::as_str, VideoPlayback::parse, &VideoPlayback::ALL)?;
                parses_only_its_own_name(&text, GapColour::as_str, GapColour::parse, &GapColour::ALL)?;
                parses_only_its_own_name(
                    &text, ClockStyle::as_str, ClockStyle::parse, &ClockStyle::ALL)?;
                parses_only_its_own_name(&text, Corner::as_str, Corner::parse, &Corner::ALL)?;
            }
        }
    }
}
