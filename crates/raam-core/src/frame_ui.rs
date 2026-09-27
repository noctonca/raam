//! The product's chrome, built on the kit. The controller (app.rs) hands
//! it the `AppState` and applies the `Actions` it records; it never
//! touches GL or a host.
//!
//! - The menu is an M3 floating toolbar over the slideshow, bottom-centred,
//!   with a one-line status in the person's words.
//! - Settings are opaque and full screen, in the gallery's list/detail
//!   shell: six sections (Photos, Slideshow, Videos, Display, Sleep,
//!   Server), two sub-pages (albums, hidden photos) and the dialogs behind
//!   Value rows. The host skips the slideshow under them
//!   (`AppState::opaque`).
//!
//! `presets` and the `Stats` fixtures are the QA surface: every screen
//! reachable by name for the desktop host's `--page` and the ux-qa pass.
use crate::icons;
use crate::kit::{self, DialogResult, ListItem, Tone, ToolItem, Trailing};
use crate::theme::{self, Type, scheme, size, space};
use egui::{Align, CornerRadius, Ui, UiBuilder};
use raam_model::limits::{AUDIO_DELAY_RANGE, CAP_CHOICES_MB, DEFAULT_CAP_MB, LARGEST_LAYOUT};
use raam_model::{
    AlbumRow, ClockStyle, Corner, FitBackground, GapColour, ScaleMode, Settings, Stats,
    TransitionChoice, VideoPlayback,
};
use std::collections::HashMap;
use std::time::Duration;

/// Sleep and wake times move in steps of this many minutes.
const TIME_STEP: u32 = 15;

/// The server URL as it will be used (Postel's Law): trimmed, with
/// `http://` added when no scheme was typed. `None` when it can't be a
/// web address at all. The controller applies this form when the menu
/// closes.
pub fn normalise_url(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() || v.contains(char::is_whitespace) {
        return None;
    }
    if v.starts_with("http://") || v.starts_with("https://") {
        Some(v.trim_end_matches('/').to_owned())
    } else {
        Some(format!("http://{}", v.trim_end_matches('/')))
    }
}

#[derive(Default, Debug)]
pub struct Actions {
    pub next: bool,
    pub prev: bool,
    pub close: bool,
    pub toggle_scale: bool,
    pub clear_cache: bool,
    pub rescan: bool,
    pub sync_now: bool,
    pub hide: bool,
    pub undo_hide: bool,
    pub unhide: Option<String>,
    pub export: bool,
    /// (Immich album id, picked).
    pub select_album: Vec<(String, bool)>,
}

impl Actions {
    pub fn any(&self) -> bool {
        self.next
            || self.prev
            || self.close
            || self.toggle_scale
            || self.clear_cache
            || self.rescan
            || self.sync_now
            || self.hide
            || self.undo_hide
            || self.unhide.is_some()
            || self.export
            || !self.select_album.is_empty()
    }
}

// ---------------------------------------------------------------------------
// App state.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Menu,
    Settings,
}

/// The settings' sections, in the nav pane's order: most used first, the
/// server last (UX.md, Serial Position).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Photos,
    Slideshow,
    Videos,
    Display,
    Sleep,
    Server,
}

impl Section {
    pub const ALL: [Section; 6] = [
        Section::Photos,
        Section::Slideshow,
        Section::Videos,
        Section::Display,
        Section::Sleep,
        Section::Server,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Photos => "Photos",
            Section::Slideshow => "Slideshow",
            Section::Videos => "Videos",
            Section::Display => "Display",
            Section::Sleep => "Sleep",
            Section::Server => "Server",
        }
    }

    fn icon(self) -> char {
        match self {
            Section::Photos => icons::PHOTO_LIBRARY,
            Section::Slideshow => icons::SLIDESHOW,
            Section::Videos => icons::MOVIE,
            Section::Display => icons::DISPLAY_SETTINGS,
            Section::Sleep => icons::BEDTIME,
            Section::Server => icons::DNS,
        }
    }
}

/// A page under a section, reached by a chevron row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sub {
    None,
    Albums,
    Hidden,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialog {
    None,
    Interval,
    Transition,
    Playback,
    AudioDelay,
    SleepAt,
    WakeAt,
    CacheSize,
    ClearCache,
}

/// What the controller shows in the menu's status line and the UI can't
/// derive itself.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The shown layout; `None` before the first collage, which disables
    /// the menu's playback items.
    pub layout: Option<String>,
    /// The weather worker's line ("Lisbon 18°C"); empty while waiting.
    pub weather: String,
    pub online: bool,
}

pub struct AppState {
    pub screen: Screen,
    pub section: Section,
    pub sub: Sub,
    pub dialog: Dialog,
    pub settings: Settings,
    pub paused: bool,
    pub keyboard: egui_keyboard::Keyboard,
    pub actions: Actions,
    /// The shown photo's scaling, set by the controller each frame; labels
    /// the Fill/Fit item with the one it switches to.
    pub shown_scale: Option<ScaleMode>,
    /// The tapped tile is a clip (Hide, but no Fill/Fit).
    pub shown_video: bool,
    /// The library's counts, copied in by the controller before each run.
    pub library: Stats,
    /// Seconds left to undo the last Hide, set by the controller.
    pub undo_secs: Option<u64>,
    /// Picks not yet seen back in `library.albums`, so a checkbox doesn't
    /// flick back while the writer thread commits.
    pub pending_albums: HashMap<String, bool>,
    pub status: Status,
    /// The host runs a weather worker (the web demo doesn't), set by the
    /// controller; without one the Weather switch is greyed out.
    pub has_weather: bool,
    /// The time an open Sleep at / Wake at dialog is editing.
    time_draft: u32,
    /// The interval an open Photo interval dialog is editing.
    interval_draft: f32,
    /// The delay an open Audio delay dialog is editing.
    delay_draft: f32,
    /// Focus the server URL field on the next pass (a preset, for QA).
    focus_url: bool,
}

fn keyboard() -> egui_keyboard::Keyboard {
    // Roboto has no arrows: shift and backspace get Material Symbols, and
    // Done (which puts the keyboard away) the check.
    egui_keyboard::Keyboard::default().key_labels(
        icons::SHIFT.to_string(),
        icons::BACKSPACE.to_string(),
        icons::CHECK.to_string(),
    )
}

impl AppState {
    pub fn new(server_url: &str, api_key: &str) -> Self {
        Self {
            screen: Screen::Menu,
            section: Section::Photos,
            sub: Sub::None,
            dialog: Dialog::None,
            settings: Settings::defaults(server_url, api_key),
            paused: false,
            keyboard: keyboard(),
            actions: Actions::default(),
            shown_scale: None,
            shown_video: false,
            library: Stats::default(),
            undo_secs: None,
            pending_albums: HashMap::new(),
            status: Status::default(),
            has_weather: true,
            time_draft: 0,
            interval_draft: 10.0,
            delay_draft: 0.0,
            focus_url: false,
        }
    }

    /// Back to a closed overlay's resting state. The keyboard is replaced
    /// rather than kept: its 20-frame focus hysteresis would otherwise flash
    /// it up for a few frames the next time the overlay opens.
    pub fn reset_on_close(&mut self) {
        self.screen = Screen::Menu;
        self.sub = Sub::None;
        self.dialog = Dialog::None;
        self.keyboard = keyboard();
    }

    /// Whether the full-screen settings are up: the host then skips drawing
    /// the slideshow under them.
    pub fn opaque(&self) -> bool {
        self.screen == Screen::Settings
    }
}

// ---------------------------------------------------------------------------
// Fixtures and presets: named screens for the desktop host's `--page` and
// the ux-qa pass.
// ---------------------------------------------------------------------------

const MB: i64 = 1_048_576;

/// Every state the settings show: albums picked, missing ("Iceland"),
/// bigger than the cache ("Recents"), partly synced ("Lisbon"), names
/// with accents; hidden photos; clips with one unplayable; all the notes.
pub fn sample_stats() -> Stats {
    let album =
        |id: &str, name: &str, n: i64, selected: bool, missing: bool, synced: i64| AlbumRow {
            remote_id: id.into(),
            name: name.into(),
            asset_count: n,
            selected,
            missing,
            synced,
        };
    // The counts agree everywhere they show: the picked albums (Digital
    // Frame 227 + Lisbon 186; Iceland is gone) hold 413, of which 347
    // are synced (227 + 120) and 280 saved on the frame, in 84 MB: 0.3 MB
    // each, so a 1 GB cache holds about 3,413. The folder's 6 include 2
    // also in Immich, so the menu says 417.
    Stats {
        immich_assets: 347,
        immich_cached: 280,
        cache_bytes: 84 * MB,
        cap_bytes: DEFAULT_CAP_MB as i64 * MB,
        local_assets: 6,
        local_ready: 6,
        shared: 2,
        local_dir: "/sdcard/Pictures/Frame".into(),
        local_note: "scanned 17:34".into(),
        immich_note: "synced 17:34".into(),
        prefetch_note: "running".into(),
        free_bytes: 12_400_000_000,
        hidden: vec![
            raam_model::HiddenItem {
                key: "local:IMG_2041".into(),
                label: "Folder: IMG_2041.jpg".into(),
            },
            raam_model::HiddenItem {
                key: "immich:a1b2c3d4".into(),
                label: "Immich, taken 12 Mar 2023 (a1b2c3d4)".into(),
            },
            raam_model::HiddenItem {
                key: "gone:9f8e7d6c".into(),
                label: "not in any source now (9f8e7d6c)".into(),
            },
        ],
        export_note: "exported to /sdcard/Pictures/frame-curation.json".into(),
        albums: vec![
            album("a1", "Digital Frame", 227, true, false, 227),
            album("a2", "Lisbon", 186, true, false, 120),
            album("a3", "Iceland", 0, true, true, 0),
            album("a4", "Recents", 10_966, false, false, 0),
            album("a5", "Favorites", 412, false, false, 0),
            album("a6", "Zürich 2019", 58, false, false, 0),
            album("a7", "Fête de la Musique", 31, false, false, 0),
            album("a8", "São Paulo", 97, false, false, 0),
        ],
        albums_note: "updated 17:34".into(),
        videos: 6,
        videos_ready: 5,
        videos_unplayable: 1,
        unplayable_reasons: vec!["1440x1920 is above this frame's 1080p decoder".into()],
    }
}

/// A first run: nothing synced, scanned or hidden, and no server yet.
pub fn first_run_stats() -> Stats {
    Stats {
        cap_bytes: DEFAULT_CAP_MB as i64 * MB,
        local_dir: "/sdcard/Pictures/Frame".into(),
        immich_note: "not synced yet".into(),
        albums_note: "saved list, not refreshed yet".into(),
        ..Default::default()
    }
}

/// The sample with no album picked.
fn none_picked_stats() -> Stats {
    let mut s = sample_stats();
    for a in &mut s.albums {
        a.selected = false;
    }
    s.albums.retain(|a| !a.missing);
    s.immich_assets = 0;
    s.immich_note = "no album picked".into();
    s
}

/// The sample with Recents picked too: more than the cache holds.
fn too_big_stats() -> Stats {
    let mut s = sample_stats();
    for a in &mut s.albums {
        if a.name == "Recents" {
            a.selected = true;
            a.synced = 10_966;
        }
    }
    s.immich_assets = 347 + 10_966;
    s
}

/// Every preset name. Any of them takes a fixture suffix: `-empty` (a first
/// run), `-nopick` (albums listed, none picked) or `-full` (the picked
/// albums hold more than the cache).
pub const PAGES: &[&str] = &[
    "menu",
    "menu-undo",
    "menu-paused",
    "set-photos",
    "set-albums",
    "set-hidden",
    "set-slideshow",
    "set-slideshow-interval",
    "set-slideshow-transition",
    "set-videos",
    "set-videos-playback",
    "set-videos-delay",
    "set-display",
    "set-sleep",
    "set-sleep-at",
    "set-sleep-wake",
    "set-server",
    "set-server-keyboard",
    "set-server-cache",
    "set-server-clear",
];

/// A screenshot run (the desktop's `--screenshot`, the web's `shot=1`)
/// steps a virtual clock this far a pass, as a 60 Hz display would, so a
/// shot can't depend on the machine's load or the display's refresh rate.
pub const SHOT_PASS: Duration = Duration::from_micros(16_667);

/// A screenshot is taken once egui asks for no pass sooner than this. Not
/// "never": a focused field's cursor blinks, asking every 500 ms forever.
pub const SHOT_SETTLED: Duration = Duration::from_millis(200);

/// The state a preset name shows, or `None` if there's no such preset.
pub fn preset(name: &str) -> Option<AppState> {
    let (base, lib) = if let Some(b) = name.strip_suffix("-empty") {
        (b, first_run_stats())
    } else if let Some(b) = name.strip_suffix("-nopick") {
        (b, none_picked_stats())
    } else if let Some(b) = name.strip_suffix("-full") {
        (b, too_big_stats())
    } else {
        (name, sample_stats())
    };
    if !PAGES.contains(&base) {
        return None;
    }
    let first_run = lib.albums.is_empty();
    let mut st = if first_run {
        AppState::new("", "")
    } else {
        AppState::new("immich.local:2283", "sGx1kQ7w9uYvB3")
    };
    st.status = if first_run {
        Status {
            layout: None,
            weather: String::new(),
            online: false,
        }
    } else {
        Status {
            layout: Some("single photo".into()),
            weather: "Lisbon 18°C".into(),
            online: true,
        }
    };
    st.shown_scale = (!first_run).then_some(ScaleMode::Fill);
    st.library = lib;
    let settings = |st: &mut AppState, section, sub, dialog| {
        st.screen = Screen::Settings;
        st.section = section;
        st.sub = sub;
        st.dialog = dialog;
    };
    match base {
        "menu" => {}
        "menu-undo" => st.undo_secs = Some(5),
        "menu-paused" => st.paused = true,
        "set-photos" => settings(&mut st, Section::Photos, Sub::None, Dialog::None),
        "set-albums" => settings(&mut st, Section::Photos, Sub::Albums, Dialog::None),
        "set-hidden" => settings(&mut st, Section::Photos, Sub::Hidden, Dialog::None),
        "set-slideshow" => settings(&mut st, Section::Slideshow, Sub::None, Dialog::None),
        "set-slideshow-interval" => {
            settings(&mut st, Section::Slideshow, Sub::None, Dialog::Interval);
            st.interval_draft = st.settings.interval_secs;
        }
        "set-slideshow-transition" => {
            settings(&mut st, Section::Slideshow, Sub::None, Dialog::Transition)
        }
        "set-videos" => settings(&mut st, Section::Videos, Sub::None, Dialog::None),
        "set-videos-playback" => settings(&mut st, Section::Videos, Sub::None, Dialog::Playback),
        "set-videos-delay" => {
            settings(&mut st, Section::Videos, Sub::None, Dialog::AudioDelay);
            st.delay_draft = st.settings.audio_delay_ms as f32;
        }
        "set-display" => settings(&mut st, Section::Display, Sub::None, Dialog::None),
        "set-sleep" => settings(&mut st, Section::Sleep, Sub::None, Dialog::None),
        "set-sleep-at" => {
            settings(&mut st, Section::Sleep, Sub::None, Dialog::SleepAt);
            st.time_draft = st.settings.sleep_min;
        }
        "set-sleep-wake" => {
            settings(&mut st, Section::Sleep, Sub::None, Dialog::WakeAt);
            st.time_draft = st.settings.wake_min;
        }
        "set-server" => settings(&mut st, Section::Server, Sub::None, Dialog::None),
        "set-server-keyboard" => {
            settings(&mut st, Section::Server, Sub::None, Dialog::None);
            st.focus_url = true;
        }
        "set-server-cache" => settings(&mut st, Section::Server, Sub::None, Dialog::CacheSize),
        "set-server-clear" => settings(&mut st, Section::Server, Sub::None, Dialog::ClearCache),
        _ => return None,
    }
    Some(st)
}

/// A desktop stand-in for the controller: logs the actions the UI recorded
/// and fakes their effect on the fixture, so a tap on the desktop shows
/// what it would do.
pub fn stand_in(st: &mut AppState) {
    if !st.actions.any() {
        return;
    }
    let a = std::mem::take(&mut st.actions);
    log::info!("frame_ui actions {a:?}");
    let lib = &mut st.library;
    if a.toggle_scale {
        st.shown_scale = st.shown_scale.map(|m| {
            if m == ScaleMode::Fill {
                ScaleMode::Fit
            } else {
                ScaleMode::Fill
            }
        });
    }
    if a.hide {
        st.undo_secs = Some(5);
    }
    if a.undo_hide {
        st.undo_secs = None;
    }
    if let Some(key) = a.unhide {
        lib.hidden.retain(|h| h.key != key);
    }
    if a.clear_cache {
        lib.cache_bytes = 0;
        lib.immich_cached = 0;
    }
    for (id, on) in a.select_album {
        if let Some(al) = lib.albums.iter_mut().find(|al| al.remote_id == id) {
            al.selected = on;
        }
    }
    if a.close {
        st.reset_on_close();
    }
}

// ---------------------------------------------------------------------------
// Formatting.
// ---------------------------------------------------------------------------

/// An empty or unknown value (UX.md: never a blank or a fake zero).
const NONE: &str = "—";

/// 10966 as "10,966".
fn count(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// A photo interval: "10 s".
fn fmt_secs(v: f32) -> String {
    format!("{v:.0} s")
}

/// The Photo interval picker's - and +: 1 s at a time up to 30 s, where
/// every second is felt, then in 5 s steps to 120 s (landing on multiples
/// of 5), so no end of the range is more than 23 taps away.
fn interval_step(v: f32, dir: i32) -> f32 {
    let next = if dir > 0 {
        if v < 30.0 {
            v + 1.0
        } else {
            ((v / 5.0).floor() + 1.0) * 5.0
        }
    } else if v <= 30.0 {
        v - 1.0
    } else {
        ((v / 5.0).ceil() - 1.0) * 5.0
    };
    next.clamp(5.0, 120.0)
}

/// An audio delay: "+180 ms".
fn fmt_delay(v: f32) -> String {
    format!("{:+.0} ms", v)
}

fn photos(n: i64) -> String {
    format!("{} photo{}", count(n), if n == 1 { "" } else { "s" })
}

fn cap_label(mb: u32) -> String {
    if mb >= 1024 {
        format!("{} GB", mb / 1024)
    } else {
        format!("{mb} MB")
    }
}

fn size(bytes: i64) -> String {
    let mb = bytes as f64 / MB as f64;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// A library note as the start of a sentence.
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// About how many previews the cache holds, from the average cached one
/// (about 300 KB), as the library works it out.
fn cache_fits(lib: &Stats) -> i64 {
    let avg = if lib.immich_cached > 0 {
        lib.cache_bytes / lib.immich_cached
    } else {
        300 * 1024
    };
    lib.cap_bytes / avg.max(1)
}

// ---------------------------------------------------------------------------
// Drawing.
// ---------------------------------------------------------------------------

pub fn draw(ui: &mut Ui, st: &mut AppState) {
    let ctx = ui.ctx().clone();
    st.keyboard.pump_events(&ctx);
    // Keycaps in the theme being drawn. Boundary contrast (keycap against
    // keyboard): light 1.29 letters, 1.31 function keys; dark 1.57 and 2.06.
    // In light, secondary-container is the keyboard's own tone (1.0:1), so
    // the function keys take the fixed secondary-dim instead.
    let s = scheme(ui);
    st.keyboard.set_colours(if s.dark {
        egui_keyboard::KeyColours {
            surface: s.surface_container_lowest,
            key: s.surface_container_highest,
            on_key: s.on_surface,
            function: s.secondary_container,
            on_function: s.on_secondary_container,
            radius: theme::shape::S,
        }
    } else {
        egui_keyboard::KeyColours {
            surface: s.surface_container_highest,
            key: s.surface_container_lowest,
            on_key: s.on_surface,
            function: s.secondary_fixed_dim,
            on_function: s.on_secondary_fixed,
            radius: theme::shape::S,
        }
    });
    match st.screen {
        Screen::Menu => draw_menu(&ctx, st),
        Screen::Settings => draw_settings(ui, st),
    }
    st.keyboard.show(&ctx);
}

/// Whether an album is picked, counting a pick not yet seen back from the
/// writer thread.
fn is_picked(st: &AppState, a: &AlbumRow) -> bool {
    st.pending_albums
        .get(&a.remote_id)
        .copied()
        .unwrap_or(a.selected)
}

/// The picked albums that still exist, and the photos they hold (the
/// server's counts, so they agree with the album list's rows).
fn picked_albums(st: &AppState) -> (Vec<&AlbumRow>, i64) {
    let picked: Vec<&AlbumRow> = st
        .library
        .albums
        .iter()
        .filter(|a| is_picked(st, a) && !a.missing)
        .collect();
    let n = picked.iter().map(|a| a.asset_count).sum();
    (picked, n)
}

fn no_server(s: &Settings) -> bool {
    s.server_url.trim().is_empty()
}

/// The menu's status: one line of what someone at the frame cares about,
/// in their words. What's showing (the first album, and how many more
/// sources), how many photos, the weather if known, and when it sleeps.
/// With nothing to show, what's missing and where to fix it.
fn status_line(st: &AppState) -> String {
    let s = &st.settings;
    let lib = &st.library;
    let (albums, album_photos) = picked_albums(st);
    let mut names: Vec<&str> = Vec::new();
    let mut total = 0;
    if s.immich_enabled && !no_server(s) {
        names.extend(albums.iter().map(|a| a.name.as_str()));
        total += album_photos;
    }
    if s.local_enabled && lib.local_ready > 0 {
        names.push("On-device folder");
        // Photos in both sources play once.
        total += lib.local_ready - if total > 0 { lib.shared } else { 0 };
    }
    let mut parts = Vec::new();
    if total == 0 {
        parts.push("No photos yet".to_string());
        if s.immich_enabled && no_server(s) {
            parts.push("add a server in Settings".into());
        } else if s.immich_enabled && albums.is_empty() {
            parts.push("pick albums in Settings".into());
        }
    } else {
        parts.push(match names.as_slice() {
            [one] => one.to_string(),
            [first, rest @ ..] => format!("{first} + {} more", rest.len()),
            [] => String::new(),
        });
        parts.push(photos(total));
        if s.immich_enabled && !no_server(s) && !st.status.online {
            parts.push("offline: saved photos only".into());
        }
    }
    if !st.status.weather.is_empty() {
        parts.push(st.status.weather.clone());
    }
    if s.sleep_enabled {
        parts.push(format!("sleeps at {}", kit::fmt_hm(s.sleep_min)));
    }
    parts.join("  ·  ")
}

#[derive(Clone, Copy)]
enum MenuAct {
    Pause,
    Prev,
    Next,
    Scale,
    Hide,
    Undo,
    Settings,
    Close,
}

fn draw_menu(ctx: &egui::Context, st: &mut AppState) {
    let status = status_line(st);
    let undo = st.undo_secs.map(|n| format!("Undo hide ({n} s)"));
    // Nothing to play (a first run): playback items are disabled.
    let playing = st.status.layout.is_some();
    let mut items: Vec<(ToolItem, MenuAct)> = Vec::new();
    let item = |icon, label| ToolItem {
        icon,
        label,
        enabled: true,
    };
    let playback = |icon, label| ToolItem {
        icon,
        label,
        enabled: playing,
    };
    items.push(if st.paused {
        (playback(icons::PLAY_ARROW, "Resume"), MenuAct::Pause)
    } else {
        (playback(icons::PAUSE, "Pause"), MenuAct::Pause)
    });
    items.push((playback(icons::SKIP_PREVIOUS, "Previous"), MenuAct::Prev));
    items.push((playback(icons::SKIP_NEXT, "Next"), MenuAct::Next));
    if let Some(mode) = st.shown_scale {
        // Labelled with what it switches to.
        items.push(match mode {
            ScaleMode::Fill => (item(icons::FIT_SCREEN, "Fit to frame"), MenuAct::Scale),
            ScaleMode::Fit => (item(icons::CROP, "Fill frame"), MenuAct::Scale),
        });
    }
    // A clip can be hidden but not re-scaled.
    if st.shown_scale.is_some() || st.shown_video {
        items.push(match &undo {
            Some(label) => (item(icons::UNDO, label), MenuAct::Undo),
            None => (item(icons::VISIBILITY_OFF, "Hide"), MenuAct::Hide),
        });
    }
    items.push((item(icons::SETTINGS, "Settings"), MenuAct::Settings));
    items.push((item(icons::CLOSE, "Close"), MenuAct::Close));
    let tools: Vec<ToolItem> = items
        .iter()
        .map(|(t, _)| ToolItem {
            icon: t.icon,
            label: t.label,
            enabled: t.enabled,
        })
        .collect();
    let hit = kit::floating_toolbar(ctx, "frame.menu", &[&status], &tools, "Undo hide (5 s)");
    if let Some(i) = hit {
        match items[i].1 {
            MenuAct::Pause => st.paused = !st.paused,
            MenuAct::Prev => st.actions.prev = true,
            MenuAct::Next => st.actions.next = true,
            MenuAct::Scale => st.actions.toggle_scale = true,
            MenuAct::Hide => st.actions.hide = true,
            MenuAct::Undo => st.actions.undo_hide = true,
            MenuAct::Settings => {
                st.screen = Screen::Settings;
                st.sub = Sub::None;
            }
            MenuAct::Close => st.actions.close = true,
        }
    }
}

fn draw_settings(ui: &mut Ui, st: &mut AppState) {
    let s = scheme(ui);
    // Behind the panels, and what the detail pane's rounded corner shows.
    ui.painter().rect_filled(ui.max_rect(), 0.0, s.surface);
    // The on-screen keyboard covers the bottom: lay out above it.
    let safe = st.keyboard.safe_rect(ui.ctx()).intersect(ui.max_rect());
    let mut back = false;
    ui.scope_builder(UiBuilder::new().max_rect(safe), |ui| {
        egui::Panel::top("frame.top")
            .exact_size(size::TOP_BAR)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(s.surface))
            .show(ui, |ui| {
                back = kit::top_bar_nav(ui, icons::ARROW_BACK, "Settings", |_| {}).clicked();
            });
        egui::Panel::left("frame.nav")
            .exact_size(size::LIST_PANE)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(s.surface)
                    .inner_margin(egui::Margin::symmetric(space::M as i8, 0)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for sec in Section::ALL {
                    if kit::nav_item(ui, sec.icon(), sec.label(), st.section == sec).clicked() {
                        st.section = sec;
                        st.sub = Sub::None;
                    }
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(s.surface_container_low)
                    .corner_radius(CornerRadius {
                        nw: theme::shape::L,
                        ..Default::default()
                    })
                    .inner_margin(egui::Margin::same(space::XL as i8)),
            )
            .show(ui, |ui| {
                let id = format!("frame.{:?}.{:?}", st.section, st.sub);
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        match (st.section, st.sub) {
                            (Section::Photos, Sub::Albums) => albums_page(ui, st),
                            (Section::Photos, Sub::Hidden) => hidden_page(ui, st),
                            (Section::Photos, _) => photos_page(ui, st),
                            (Section::Slideshow, _) => slideshow_page(ui, st),
                            (Section::Videos, _) => videos_page(ui, st),
                            (Section::Display, _) => display_page(ui, st),
                            (Section::Sleep, _) => sleep_page(ui, st),
                            (Section::Server, _) => server_page(ui, st),
                        }
                        ui.add_space(space::XXL);
                    });
            });
    });
    if back {
        if st.sub != Sub::None {
            st.sub = Sub::None;
        } else {
            st.screen = Screen::Menu;
            // The keyboard's hysteresis would flash it over the menu.
            st.keyboard = keyboard();
        }
    }
    dialogs(ui.ctx(), st);
}

/// A group of components on the content edge (400), as the gallery's
/// component page places them.
fn on_content_edge<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: space::L as i8,
            right: space::L as i8,
            ..Default::default()
        })
        .show(ui, add)
        .inner
}

/// A note directly under the row it's about, 8 either side (a group's gap).
fn note_under(ui: &mut Ui, tone: Tone, icon: char, text: &str) {
    ui.add_space(space::S);
    kit::note(ui, tone, icon, text);
    ui.add_space(space::S);
}

/// The one thing wrong with the picked albums worth a note, if any: a
/// missing album (unless `missing` is false: the album picker says that on
/// the album's own row), none picked, or more than the cache holds.
fn albums_problem(st: &AppState, missing: bool) -> Option<String> {
    let lib = &st.library;
    if lib.albums.is_empty() {
        return None;
    }
    let picked = |a: &AlbumRow| is_picked(st, a);
    let gone: Vec<&str> = lib
        .albums
        .iter()
        .filter(|a| missing && a.missing && picked(a))
        .map(|a| a.name.as_str())
        .collect();
    match gone.as_slice() {
        [one] => {
            return Some(format!(
                "“{one}” is not on the server any more, so it plays nothing."
            ));
        }
        [first, ..] => {
            return Some(format!(
                "“{first}” and {} more are not on the server any more, so they play nothing.",
                gone.len() - 1
            ));
        }
        [] => {}
    }
    if !lib.albums.iter().any(picked) {
        return Some("No album picked, so Immich adds no photos.".into());
    }
    // The albums' own counts, as their rows show them.
    let fits = cache_fits(lib);
    let (_, total) = picked_albums(st);
    if total > fits {
        return Some(format!(
            "The picked albums hold {} and about {} fit in the cache: the rest play only while the server can be reached.",
            photos(total),
            count(fits)
        ));
    }
    None
}

fn photos_page(ui: &mut Ui, st: &mut AppState) {
    kit::page_title(ui, "Photos", "Where the slideshow's photos come from.");
    ui.spacing_mut().item_spacing.y = 0.0;
    let lib = st.library.clone();
    let problem = albums_problem(st, true);
    let (picked, picked_photos) = picked_albums(st);
    let picked = picked.len();
    let s = &mut st.settings;
    let server = !no_server(s);

    // One section for both sources: the album note then sits between two
    // rows of its group, not at its end, where the next header read closer
    // to it than the rows to each other (qa.py sections: PROXIMITY).
    kit::section_header(ui, "Sources");
    // Neither switch can turn off the only source that is on.
    let immich_sup = if !s.local_enabled {
        "The only source that's on: turn the folder on first".to_string()
    } else if !s.immich_enabled {
        "Off: no photos from the server".to_string()
    } else if !server {
        // An unfinished setup shows as unfinished (Zeigarnik).
        "No server yet".to_string()
    } else if lib.albums.is_empty() {
        // Nothing synced yet: no counts to show, not a fake zero.
        sentence(&lib.immich_note)
    } else {
        // The picked albums' counts, as the Albums row and the list show
        // them.
        format!(
            "{} · {} on the frame · {}",
            photos(picked_photos),
            count(lib.immich_cached),
            lib.immich_note
        )
    };
    ui.add_enabled_ui(s.local_enabled, |ui| {
        kit::list_item(
            ui,
            ListItem::new("Immich")
                .icon(icons::CLOUD)
                .supporting(&immich_sup)
                .trailing(Trailing::Switch(&mut s.immich_enabled)),
        );
    });
    if s.immich_enabled {
        let sup = if !server {
            "Add the server first".to_string()
        } else if lib.albums.is_empty() {
            "No albums yet: the list loads from the server".to_string()
        } else {
            format!(
                "{picked} album{} · {}",
                if picked == 1 { "" } else { "s" },
                photos(picked_photos)
            )
        };
        if kit::list_item(
            ui,
            ListItem::new("Albums")
                .icon(icons::PHOTO_ALBUM)
                .supporting(&sup)
                .trailing(Trailing::Chevron),
        )
        .clicked()
        {
            // With no server there's no list to pick from: go and add one.
            if server {
                st.sub = Sub::Albums;
            } else {
                st.section = Section::Server;
            }
        }
        if let Some(p) = &problem {
            note_under(ui, Tone::Warning, icons::WARNING, p);
        }
    }

    let s = &mut st.settings;
    let local_sup = if !s.immich_enabled {
        "The only source that's on: turn Immich on first".to_string()
    } else if lib.local_note.is_empty() {
        // Not scanned yet (the Rescan row says so): no counts to show.
        lib.local_dir.clone()
    } else {
        let mut v = format!(
            "{} · {} · {} ready",
            lib.local_dir,
            photos(lib.local_assets),
            count(lib.local_ready)
        );
        if lib.shared > 0 {
            v.push_str(&format!(" · {} also in Immich", count(lib.shared)));
        }
        v
    };
    ui.add_enabled_ui(s.immich_enabled, |ui| {
        kit::list_item(
            ui,
            ListItem::new("On-device folder")
                .icon(icons::FOLDER)
                .supporting(&local_sup)
                .trailing(Trailing::Switch(&mut s.local_enabled)),
        );
    });
    let scanned = if lib.local_note.is_empty() {
        "Not scanned yet".to_string()
    } else {
        sentence(&lib.local_note)
    };
    let local_on = s.local_enabled;
    ui.add_enabled_ui(local_on, |ui| {
        if kit::list_item(
            ui,
            ListItem::new("Rescan folder")
                .icon(icons::REFRESH)
                .supporting(&scanned),
        )
        .clicked()
        {
            st.actions.rescan = true;
        }
    });

    kit::section_header(ui, "Curation");
    let hidden_title = format!("Hidden photos ({})", lib.hidden.len());
    // With none hidden, no chevron to an empty page: the row is disabled
    // and says so.
    let any_hidden = !lib.hidden.is_empty();
    ui.add_enabled_ui(any_hidden, |ui| {
        let row = if any_hidden {
            ListItem::new(&hidden_title)
                .supporting("Kept out of the slideshow; unhide them here")
                .trailing(Trailing::Chevron)
        } else {
            ListItem::new(&hidden_title)
                .supporting("None: hide a photo from the menu while it shows")
        };
        if kit::list_item(ui, row.icon(icons::VISIBILITY_OFF)).clicked() {
            st.sub = Sub::Hidden;
        }
    });
    let export = if lib.export_note.is_empty() {
        "Saved after every change".to_string()
    } else {
        sentence(&lib.export_note)
    };
    if kit::list_item(
        ui,
        ListItem::new("Export curation")
            .icon(icons::FILE_EXPORT)
            .supporting(&export),
    )
    .clicked()
    {
        st.actions.export = true;
    }
}

fn albums_page(ui: &mut Ui, st: &mut AppState) {
    kit::page_title(
        ui,
        "Immich albums",
        "Photos from every album you pick play as one shuffle.",
    );
    ui.spacing_mut().item_spacing.y = 0.0;
    let lib = st.library.clone();
    // Picks the list now agrees with are no longer pending.
    st.pending_albums.retain(|id, on| {
        lib.albums
            .iter()
            .any(|a| &a.remote_id == id && a.selected != *on)
    });

    kit::section_header(ui, "Album list");
    let note = sentence(&lib.albums_note);
    let note = if note.is_empty() {
        NONE.to_string()
    } else {
        note
    };
    if kit::list_item(
        ui,
        ListItem::new("Refresh list")
            .icon(icons::SYNC)
            .supporting(&note),
    )
    .clicked()
    {
        st.actions.sync_now = true;
    }
    if lib.albums.is_empty() {
        note_under(
            ui,
            Tone::Info,
            icons::INFO,
            "No albums yet: the list loads from the server.",
        );
        return;
    }

    kit::section_header(ui, "Albums to show");
    let fits = cache_fits(&lib);
    for album in &lib.albums {
        let mut on = st
            .pending_albums
            .get(&album.remote_id)
            .copied()
            .unwrap_or(album.selected);
        let (sup, warn) = if album.missing {
            (
                "Not on the server any more, so it plays nothing".to_string(),
                true,
            )
        } else if album.asset_count > fits {
            (
                format!(
                    "{}: more than the cache holds (about {})",
                    photos(album.asset_count),
                    count(fits)
                ),
                true,
            )
        } else if on && album.selected && album.synced < album.asset_count {
            (
                format!(
                    "{} of {} synced",
                    count(album.synced),
                    count(album.asset_count)
                ),
                false,
            )
        } else {
            (photos(album.asset_count), false)
        };
        let item = ListItem::new(&album.name)
            .blank_icon()
            .supporting(&sup)
            .warning(warn)
            .trailing(Trailing::Checkbox(&mut on));
        if kit::list_item(ui, item).changed() {
            st.pending_albums.insert(album.remote_id.clone(), on);
            st.actions.select_album.push((album.remote_id.clone(), on));
        }
    }
    // Under the list it's about.
    if let Some(p) = albums_problem(st, false) {
        note_under(ui, Tone::Warning, icons::WARNING, &p);
    }
}

fn hidden_page(ui: &mut Ui, st: &mut AppState) {
    kit::page_title(
        ui,
        "Hidden photos",
        "They never play. Unhide one to put it back in the slideshow.",
    );
    ui.spacing_mut().item_spacing.y = 0.0;
    let lib = st.library.clone();
    if lib.hidden.is_empty() {
        ui.add_space(space::L);
        kit::note(
            ui,
            Tone::Info,
            icons::INFO,
            "Nothing is hidden. To hide a photo, tap Hide in the menu while it shows.",
        );
        return;
    }
    kit::section_header(ui, "Most recently hidden first");
    for item in lib.hidden.iter().take(50) {
        let icon = if item.label.starts_with("Folder") {
            icons::FOLDER
        } else if item.label.starts_with("Immich") {
            icons::CLOUD
        } else {
            icons::IMAGE
        };
        let label = sentence(&item.label);
        if kit::list_item(
            ui,
            ListItem::new(&label)
                .icon(icon)
                .trailing(Trailing::Button("Unhide")),
        )
        .changed()
        {
            st.actions.unhide = Some(item.key.clone());
        }
    }
    if lib.hidden.len() > 50 {
        ui.add_space(space::S);
        on_content_edge(ui, |ui| {
            let s = scheme(ui);
            kit::paragraph(
                ui,
                &format!("And {} more.", lib.hidden.len() - 50),
                Type::BodyMedium,
                s.on_surface_variant,
            );
        });
    }
}

fn slideshow_page(ui: &mut Ui, st: &mut AppState) {
    kit::page_title(
        ui,
        "Slideshow",
        "How photos follow each other and fill the frame.",
    );
    ui.spacing_mut().item_spacing.y = 0.0;
    let s = &mut st.settings;

    kit::section_header(ui, "Pace");
    // A Value row like its neighbours (icon on 400, text on 440), opening
    // the number picker: a slider here put its title on 400.
    let interval = fmt_secs(s.interval_secs);
    if kit::list_item(
        ui,
        ListItem::new("Photo interval")
            .icon(icons::TIMER)
            .trailing(Trailing::Value(&interval)),
    )
    .clicked()
    {
        st.interval_draft = st.settings.interval_secs;
        st.dialog = Dialog::Interval;
    }
    let s = &mut st.settings;
    if kit::list_item(
        ui,
        ListItem::new("Transition")
            .icon(icons::ANIMATION)
            .trailing(Trailing::Value(s.transition.label())),
    )
    .clicked()
    {
        st.dialog = Dialog::Transition;
    }
    let s = &mut st.settings;
    kit::list_item(
        ui,
        ListItem::new("Ken Burns")
            .icon(icons::PAN_ZOOM)
            .supporting("Slow pan and zoom, towards faces when Immich knows them")
            .trailing(Trailing::Switch(&mut s.ken_burns_enabled)),
    );

    kit::section_header(ui, "Framing");
    let fill_sup = if s.fill_by_default {
        "Photos fill the frame, cropping their edges"
    } else {
        "Whole photos show, with a background around them"
    };
    kit::list_item(
        ui,
        ListItem::new("Fill frame by default")
            .icon(icons::CROP)
            .supporting(fill_sup)
            .trailing(Trailing::Switch(&mut s.fill_by_default)),
    );
    let mut bg = if s.fit_background == FitBackground::Blurred {
        0
    } else {
        1
    };
    let row = ListItem::new("Fit background")
        .icon(icons::WALLPAPER)
        .supporting("Around photos that don't fill the frame")
        .trailing(Trailing::Segmented {
            selected: &mut bg,
            options: &["Blurred", "Black"],
            seg_w: 120.0,
        });
    if kit::list_item(ui, row).changed() {
        s.fit_background = if bg == 0 {
            FitBackground::Blurred
        } else {
            FitBackground::Black
        };
    }
    let mut collage = s.collage_max.clamp(1, LARGEST_LAYOUT) - 1;
    let collage_sup = format!(
        "Photos side by side, at most this many · Default for this screen: {}",
        s.screen_default_max
    );
    let row = ListItem::new("Collage")
        .icon(icons::AUTO_AWESOME_MOSAIC)
        .supporting(&collage_sup)
        .trailing(Trailing::Segmented {
            selected: &mut collage,
            options: &["Off", "2", "3", "4"],
            seg_w: 72.0,
        });
    if kit::list_item(ui, row).changed() {
        s.collage_max = collage + 1;
    }
    let mut gap = if s.gap_colour == GapColour::Black {
        0
    } else {
        1
    };
    let collages = s.collage_max > 1;
    ui.add_enabled_ui(collages, |ui| {
        let sup = if collages {
            "Between the photos of a collage"
        } else {
            "Only used with collages on"
        };
        let row = ListItem::new("Gap colour")
            .icon(icons::FORMAT_COLOR_FILL)
            .supporting(sup)
            .trailing(Trailing::Segmented {
                selected: &mut gap,
                options: &["Black", "White"],
                seg_w: 120.0,
            });
        if kit::list_item(ui, row).changed() {
            s.gap_colour = if gap == 0 {
                GapColour::Black
            } else {
                GapColour::White
            };
        }
    });
}

fn videos_page(ui: &mut Ui, st: &mut AppState) {
    kit::page_title(ui, "Videos", "How video clips play in the slideshow.");
    ui.spacing_mut().item_spacing.y = 0.0;
    let lib = st.library.clone();
    let s = &mut st.settings;

    kit::section_header(ui, "Playback");
    if kit::list_item(
        ui,
        ListItem::new("When a clip plays")
            .icon(icons::MOVIE)
            .trailing(Trailing::Value(s.video_playback.label())),
    )
    .clicked()
    {
        st.dialog = Dialog::Playback;
    }

    let s = &mut st.settings;
    kit::section_header(ui, "Sound");
    let sound_sup = if s.video_sound {
        "Clips play with their sound"
    } else {
        "Off: clips play silently"
    };
    kit::list_item(
        ui,
        ListItem::new("Sound")
            .icon(icons::VOLUME_UP)
            .supporting(sound_sup)
            .trailing(Trailing::Switch(&mut s.video_sound)),
    );
    let sound_on = s.video_sound;
    let mut open_delay = false;
    ui.add_enabled_ui(sound_on, |ui| {
        ui.add_space(space::S);
        on_content_edge(ui, |ui| {
            ui.spacing_mut().slider_width = 400.0;
            let mut pct = (s.video_volume * 100.0).round();
            if kit::slider(ui, "Volume", &mut pct, 0.0..=100.0, Some(5.0), |v| {
                format!("{v:.0}%")
            })
            .changed()
            {
                s.video_volume = pct / 100.0;
            }
        });
        ui.add_space(space::S);
        let delay = fmt_delay(s.audio_delay_ms as f32);
        open_delay = kit::list_item(
            ui,
            ListItem::new("Audio delay")
                .icon(icons::AV_TIMER)
                .supporting("Lip-sync: positive holds the sound back")
                .trailing(Trailing::Value(&delay)),
        )
        .clicked();
    });
    if open_delay {
        st.delay_draft = st.settings.audio_delay_ms as f32;
        st.dialog = Dialog::AudioDelay;
    }

    kit::section_header(ui, "In the library");
    let clips = format!(
        "{} clip{} in the albums and folder · {} ready to play",
        count(lib.videos),
        if lib.videos == 1 { "" } else { "s" },
        count(lib.videos_ready)
    );
    kit::list_item(ui, ListItem::new("Clips").blank_icon().supporting(&clips));
    if lib.videos_unplayable > 0 {
        note_under(
            ui,
            Tone::Warning,
            icons::WARNING,
            &format!(
                "{} can't be played on this frame ({}). Clear Immich cache tries them again.",
                count(lib.videos_unplayable),
                lib.unplayable_reasons.join("; ")
            ),
        );
    }
}

fn display_page(ui: &mut Ui, st: &mut AppState) {
    kit::page_title(
        ui,
        "Display",
        "How the frame looks, and what it shows over the photos.",
    );
    ui.spacing_mut().item_spacing.y = 0.0;
    let has_weather = st.has_weather;
    let s = &mut st.settings;

    kit::section_header(ui, "Appearance");
    let mut seg = if s.dark_theme { 1 } else { 0 };
    let row = ListItem::new("Theme")
        // Not dark_mode: its crescent reads as Sleep's bedtime moon.
        .icon(icons::CONTRAST)
        .supporting("For menus and settings")
        .trailing(Trailing::Segmented {
            selected: &mut seg,
            options: &["Light", "Dark"],
            seg_w: 120.0,
        });
    if kit::list_item(ui, row).changed() {
        s.dark_theme = seg == 1;
    }

    kit::section_header(ui, "Clock");
    let mut clock = ClockStyle::ALL
        .iter()
        .position(|c| *c == s.clock_style)
        .unwrap_or(0);
    let options: Vec<&str> = ClockStyle::ALL.iter().map(|c| c.label()).collect();
    let row = ListItem::new("Clock overlay")
        .icon(icons::SCHEDULE)
        .supporting("Time and weather over the photos")
        .trailing(Trailing::Segmented {
            selected: &mut clock,
            options: &options,
            seg_w: 120.0,
        });
    if kit::list_item(ui, row).changed() {
        s.clock_style = ClockStyle::ALL[clock];
    }
    // No icons below: a digital clock is illegible at 24 px, and the
    // overlay has the clock face. The empty slots keep the text on the
    // text edge.
    ui.add_enabled_ui(s.clock_style != ClockStyle::Off, |ui| {
        let mut corner = Corner::ALL
            .iter()
            .position(|c| *c == s.clock_corner)
            .unwrap_or(1);
        let options: Vec<&str> = Corner::ALL.iter().map(|c| c.label()).collect();
        let row = ListItem::new("Corner")
            .blank_icon()
            .trailing(Trailing::Segmented {
                selected: &mut corner,
                options: &options,
                seg_w: 140.0,
            });
        if kit::list_item(ui, row).changed() {
            s.clock_corner = Corner::ALL[corner];
        }
        kit::list_item(
            ui,
            ListItem::new("24-hour clock")
                .blank_icon()
                .supporting("17:34 rather than 5:34 PM")
                .trailing(Trailing::Switch(&mut s.clock_24h)),
        );
        // Open-Meteo's licence (CC BY 4.0) asks for credit; this line
        // gives it in the app, the README's credits in the repo.
        ui.add_enabled_ui(has_weather, |ui| {
            kit::list_item(
                ui,
                ListItem::new("Weather")
                    .blank_icon()
                    .supporting(if has_weather {
                        "Weather data by Open-Meteo.com"
                    } else {
                        "Not available here"
                    })
                    .trailing(Trailing::Switch(&mut s.weather_enabled)),
            );
        });
    });
}

fn sleep_page(ui: &mut Ui, st: &mut AppState) {
    kit::page_title(
        ui,
        "Sleep",
        "The screen turns off at night and on again in the morning.",
    );
    ui.spacing_mut().item_spacing.y = 0.0;
    let s = &mut st.settings;
    kit::section_header(ui, "Schedule");
    kit::list_item(
        ui,
        ListItem::new("Sleep schedule")
            .icon(icons::BEDTIME)
            .supporting("A tap on the screen wakes it for a while")
            .trailing(Trailing::Switch(&mut s.sleep_enabled)),
    );
    let (sleep, wake) = (kit::fmt_hm(s.sleep_min), kit::fmt_hm(s.wake_min));
    let mut open = Dialog::None;
    ui.add_enabled_ui(s.sleep_enabled, |ui| {
        if kit::list_item(
            ui,
            ListItem::new("Sleep at")
                .blank_icon()
                .trailing(Trailing::Value(&sleep)),
        )
        .clicked()
        {
            open = Dialog::SleepAt;
        }
        if kit::list_item(
            ui,
            ListItem::new("Wake at")
                .blank_icon()
                .trailing(Trailing::Value(&wake)),
        )
        .clicked()
        {
            open = Dialog::WakeAt;
        }
    });
    match open {
        Dialog::SleepAt => {
            st.time_draft = st.settings.sleep_min;
            st.dialog = open;
        }
        Dialog::WakeAt => {
            st.time_draft = st.settings.wake_min;
            st.dialog = open;
        }
        _ => {}
    }
}

fn server_page(ui: &mut Ui, st: &mut AppState) {
    // The fields share one width (UX.md, Similarity).
    const FIELD_W: f32 = 560.0;
    kit::page_title(ui, "Server", "The Immich server the albums come from.");
    ui.spacing_mut().item_spacing.y = 0.0;
    let lib = st.library.clone();

    kit::section_header(ui, "Immich server");
    let focus = std::mem::take(&mut st.focus_url);
    let s = &mut st.settings;
    // Postel's Law: a missing scheme is added, with the result shown. Only
    // what can't be a host name at all is an error.
    let raw = s.server_url.trim().to_owned();
    let url = normalise_url(&raw);
    let bad = !raw.is_empty() && url.is_none();
    let help = match &url {
        _ if raw.is_empty() => "Needed for Immich albums".to_owned(),
        None => "Not a web address".to_owned(),
        Some(u) if *u == raw => "The address of your Immich server".to_owned(),
        Some(u) => format!("Will use {u}"),
    };
    on_content_edge(ui, |ui| {
        let clip = ui.clip_rect();
        let r = kit::TextField::new(&mut s.server_url, "Server URL")
            .icon(icons::DNS)
            .hint("e.g. immich.local:2283")
            .error(bad)
            .supporting(&help)
            .width(FIELD_W)
            .show(ui);
        if focus {
            r.request_focus();
        }
        // Keep the focused field above the on-screen keyboard.
        if r.has_focus() && !clip.contains_rect(r.rect) {
            r.scroll_to_me(Some(Align::Center));
        }
        ui.add_space(space::S);
        let r = kit::TextField::new(&mut s.api_key, "API key")
            .icon(icons::KEY)
            .password(true)
            .supporting("Server and key apply when the menu closes")
            .width(FIELD_W)
            .show(ui);
        if r.has_focus() && !clip.contains_rect(r.rect) {
            r.scroll_to_me(Some(Align::Center));
        }
    });

    kit::section_header(ui, "Sync");
    let synced = if lib.immich_note.is_empty() {
        NONE.to_string()
    } else {
        sentence(&lib.immich_note)
    };
    let online = !raw.is_empty() && s.immich_enabled;
    ui.add_enabled_ui(online, |ui| {
        if kit::list_item(
            ui,
            ListItem::new("Sync Immich now")
                .icon(icons::SYNC)
                .supporting(&synced),
        )
        .clicked()
        {
            st.actions.sync_now = true;
        }
    });

    // Rarely used, and away from Sync: the destructive row is last.
    kit::section_header(ui, "Cache");
    // Chunks that aren't known are left out, not shown as "—" mid-line.
    let cap_mb = st.settings.cache_cap_mb;
    let mut usage = vec![if lib.cache_bytes > 0 {
        format!("{} of {}", size(lib.cache_bytes), cap_label(cap_mb))
    } else {
        format!("Nothing saved yet · up to {}", cap_label(cap_mb))
    }];
    if lib.free_bytes > 0 {
        usage.push(format!("{:.1} GB free", lib.free_bytes as f64 / 1e9));
    }
    // Downloading only means something once there's a server.
    if !no_server(&st.settings) {
        let prefetch = match lib.prefetch_note.as_str() {
            "" => None,
            "running" => Some("downloading"),
            "complete" => Some("all downloaded"),
            "stopped at the cap" => Some("full"),
            other => Some(other),
        };
        usage.extend(prefetch.map(str::to_owned));
    }
    let usage = usage.join(" · ");
    let cap = cap_label(st.settings.cache_cap_mb);
    if kit::list_item(
        ui,
        ListItem::new("Cache size")
            .icon(icons::SD_CARD)
            .supporting(&usage)
            .trailing(Trailing::Value(&cap)),
    )
    .clicked()
    {
        st.dialog = Dialog::CacheSize;
    }
    ui.add_enabled_ui(lib.cache_bytes > 0, |ui| {
        let sup = if lib.cache_bytes > 0 {
            "Photos download again as they play"
        } else {
            "Nothing saved yet"
        };
        if kit::list_item(
            ui,
            ListItem::new("Clear Immich cache")
                .icon(icons::DELETE)
                .supporting(sup),
        )
        .clicked()
        {
            st.dialog = Dialog::ClearCache;
        }
    });
}

fn dialogs(ctx: &egui::Context, st: &mut AppState) {
    match st.dialog {
        Dialog::None => {}
        Dialog::Interval => {
            let mut draft = st.interval_draft;
            let mut done = None;
            let open = kit::dialog(ctx, "frame.interval", "Photo interval", |ui| {
                let s = scheme(ui);
                ui.spacing_mut().item_spacing.y = 0.0;
                kit::paragraph(
                    ui,
                    "How long each photo stays before the next one.",
                    Type::BodyMedium,
                    s.on_surface_variant,
                );
                ui.add_space(space::L);
                kit::number_picker(ui, &mut draft, 5.0..=120.0, 1.0, interval_step, fmt_secs);
                ui.add_space(space::XL);
                done = kit::dialog_actions(ui, &["Cancel", "OK"]);
                done.is_none()
            });
            st.interval_draft = draft;
            if done == Some(1) {
                st.settings.interval_secs = draft;
            }
            if !open {
                st.dialog = Dialog::None;
            }
        }
        Dialog::Transition => {
            let labels: Vec<&str> = TransitionChoice::ALL.iter().map(|t| t.label()).collect();
            let sel = TransitionChoice::ALL
                .iter()
                .position(|t| *t == st.settings.transition)
                .unwrap_or(0);
            match kit::picker(ctx, "frame.transition", "Transition", &labels, sel) {
                DialogResult::Open => {}
                DialogResult::Dismissed => st.dialog = Dialog::None,
                DialogResult::Done(i) => {
                    st.settings.transition = TransitionChoice::ALL[i];
                    st.dialog = Dialog::None;
                }
            }
        }
        Dialog::Playback => {
            let labels: Vec<&str> = VideoPlayback::ALL.iter().map(|p| p.label()).collect();
            let sel = VideoPlayback::ALL
                .iter()
                .position(|p| *p == st.settings.video_playback)
                .unwrap_or(0);
            match kit::picker(ctx, "frame.playback", "When a clip plays", &labels, sel) {
                DialogResult::Open => {}
                DialogResult::Dismissed => st.dialog = Dialog::None,
                DialogResult::Done(i) => {
                    st.settings.video_playback = VideoPlayback::ALL[i];
                    st.dialog = Dialog::None;
                }
            }
        }
        Dialog::AudioDelay => {
            let mut draft = st.delay_draft;
            let mut done = None;
            let open = kit::dialog(ctx, "frame.delay", "Audio delay", |ui| {
                let s = scheme(ui);
                ui.spacing_mut().item_spacing.y = 0.0;
                kit::paragraph(
                    ui,
                    "If voices run ahead of lips, raise it; if they lag, lower it.",
                    Type::BodyMedium,
                    s.on_surface_variant,
                );
                ui.add_space(space::L);
                kit::number_picker(
                    ui,
                    &mut draft,
                    AUDIO_DELAY_RANGE.0 as f32..=AUDIO_DELAY_RANGE.1 as f32,
                    10.0,
                    |v, dir| {
                        (v + dir as f32 * 10.0)
                            .clamp(AUDIO_DELAY_RANGE.0 as f32, AUDIO_DELAY_RANGE.1 as f32)
                    },
                    fmt_delay,
                );
                ui.add_space(space::XL);
                done = kit::dialog_actions(ui, &["Cancel", "OK"]);
                done.is_none()
            });
            st.delay_draft = draft;
            if done == Some(1) {
                st.settings.audio_delay_ms = draft as i32;
            }
            if !open {
                st.dialog = Dialog::None;
            }
        }
        Dialog::CacheSize => {
            let labels: Vec<String> = CAP_CHOICES_MB.iter().map(|mb| cap_label(*mb)).collect();
            let labels: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            let sel = CAP_CHOICES_MB
                .iter()
                .position(|mb| *mb == st.settings.cache_cap_mb)
                .unwrap_or(3);
            match kit::picker(ctx, "frame.cache", "Cache size", &labels, sel) {
                DialogResult::Open => {}
                DialogResult::Dismissed => st.dialog = Dialog::None,
                DialogResult::Done(i) => {
                    st.settings.cache_cap_mb = CAP_CHOICES_MB[i];
                    st.library.cap_bytes = CAP_CHOICES_MB[i] as i64 * MB;
                    st.dialog = Dialog::None;
                }
            }
        }
        Dialog::SleepAt | Dialog::WakeAt => {
            let sleep = st.dialog == Dialog::SleepAt;
            let (title, body) = if sleep {
                ("Sleep at", "The screen turns off at this time.")
            } else {
                ("Wake at", "The screen turns on again at this time.")
            };
            let mut draft = st.time_draft;
            let mut done = None;
            let open = kit::dialog(ctx, "frame.time", title, |ui| {
                let s = scheme(ui);
                ui.spacing_mut().item_spacing.y = 0.0;
                kit::paragraph(ui, body, Type::BodyMedium, s.on_surface_variant);
                ui.add_space(space::L);
                kit::time_picker(ui, &mut draft, TIME_STEP);
                ui.add_space(space::XL);
                done = kit::dialog_actions(ui, &["Cancel", "OK"]);
                done.is_none()
            });
            st.time_draft = draft;
            if done == Some(1) {
                if sleep {
                    st.settings.sleep_min = draft;
                } else {
                    st.settings.wake_min = draft;
                }
            }
            if !open {
                st.dialog = Dialog::None;
            }
        }
        Dialog::ClearCache => {
            let body = format!(
                "Deletes the {} of Immich photos saved on the frame. They download again as they play.",
                size(st.library.cache_bytes)
            );
            let mut done = None;
            let open = kit::dialog(ctx, "frame.clear", "Clear the Immich cache?", |ui| {
                let s = scheme(ui);
                ui.spacing_mut().item_spacing.y = 0.0;
                kit::paragraph(ui, &body, Type::BodyMedium, s.on_surface_variant);
                ui.add_space(space::XL);
                done = kit::dialog_actions(ui, &["Cancel", "Clear"]);
                done.is_none()
            });
            if done == Some(1) {
                st.actions.clear_cache = true;
            }
            if !open {
                st.dialog = Dialog::None;
            }
        }
    }
}
