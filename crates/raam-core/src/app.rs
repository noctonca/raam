//! The App controller: the product's behaviour, shared by every host.
//! Events in, effects out: the host feeds lifecycle, touch and key events
//! plus a few per-pass inputs; the controller routes input (tap-slop, menu
//! open/close, auto-dismiss, undo-hide), runs the egui chrome, applies
//! settings to the slideshow, drives the sleep/wake state machine,
//! debounces settings saves, and computes the next wake deadline.
//!
//! It draws nothing. The host draws around `frame`: the slideshow, then
//! the clock overlay (rebuilt when `FrameOut::overlay` says so), then the
//! egui chrome (`FrameOut::egui` uploaded first when present), then the
//! swap. The outside world enters through the deps traits and leaves as
//! `Effect`s the host maps onto its engine and power plumbing.

use crate::frame_ui::{self, AppState, Section, Status, Sub};
use crate::network::{NetCommand, Network};
use crate::overlay;
use crate::schedule::{self, Schedule};
use crate::seams::Power;
use crate::slideshow::SlideshowSettings;
use crate::source::TileSource;
use crate::{clock, store, theme, weather_icons};
use raam_model::limits::{
    ALARM_RETRY, AUTO_DISMISS, DEFAULT_MANUAL_IDLE, MAX_EGUI_WAIT, MAX_QUEUED_KEYS, SAVE_DEBOUNCE,
    SLEEP_CONFIRM, TAP_SLOP_PX, UNDO_HIDE, WIFI_RESCAN,
};
use raam_model::{ClockStyle, Corner, ScaleMode, SourceKind, Stats};
use std::collections::VecDeque;
use std::time::Duration;

/// What the host's event loop feeds in each pass.
pub enum Event {
    /// The activity started (wake time may have arrived).
    Start,
    /// In front again.
    Resume,
    /// No longer in front.
    Pause,
    Touch(Touch),
    /// A physical keyboard's key went down or up (docs/UX.md, Keys).
    Key(KeyEvent),
    /// What a physical keyboard typed, for a text field: the character keys
    /// after the layout and shift, with no control characters.
    Text(String),
}

/// One physical key, in egui terms. The host leaves out its own keys (the
/// desktop's F5 and F12) and a key with no egui name (a lone Shift).
#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub key: egui::Key,
    pub pressed: bool,
    /// Held down and repeating.
    pub repeat: bool,
    pub modifiers: egui::Modifiers,
}

/// One pointer event, already in egui terms.
pub struct Touch {
    pub phase: egui::TouchPhase,
    pub pos: egui::Pos2,
    pub device_id: u64,
    pub touch_id: u64,
    pub force: f32,
}

/// A key or typed text waiting its turn (`App::keys`).
enum Keyed {
    Key(KeyEvent),
    Text(String),
}

/// TEST-ONLY schedule overrides (`debug.video.*` on Android), parsed by
/// the host. A cleared override puts the saved schedule back.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Overrides {
    pub sleep: Option<u32>,
    pub wake: Option<u32>,
    pub idle: Option<Duration>,
}

/// Per-pass facts only the host knows.
pub struct Inputs {
    /// A window surface exists to draw on.
    pub has_surface: bool,
    /// The screen in pixels, for egui's rect.
    pub screen: (i32, i32),
    /// The host's wake mechanism takes a wake lock (not flags-only).
    pub wakelock_allowed: bool,
    pub overrides: Overrides,
}

/// The controller's view of the slideshow pipeline. `Pipeline` implements
/// it in slideshow.rs; a test drives the controller with a fake.
pub trait Slideshow {
    fn settings_mut(&mut self) -> &mut SlideshowSettings;
    fn set_clock_paused(&mut self, paused: bool);
    /// The app went hidden: a playing clip (and its sound) stops with it.
    fn pause_video(&mut self);
    fn set_menu_open(&mut self, open: bool);
    fn clear_selection(&mut self);
    fn select_at(&mut self, x: f32, y: f32) -> Option<usize>;
    fn update(&mut self, source: &dyn TileSource);
    fn request_next(&mut self);
    fn request_prev(&mut self, source: &dyn TileSource);
    fn toggle_shown_scale(&mut self) -> Option<(String, Option<ScaleMode>)>;
    fn shown_photo(&self) -> Option<(String, i64)>;
    fn forget(&mut self, key: &str, source: &dyn TileSource);
    fn unforget(&mut self, key: &str);
    fn shown_scale_mode(&self) -> Option<ScaleMode>;
    fn shown_is_video(&self) -> bool;
    fn shown_layout(&self) -> Option<String>;
    fn is_animating(&self) -> bool;
    fn recompose_pending(&self) -> bool;
    fn next_deadline(&self) -> Option<Duration>;
}

/// The library's numbers for the status line and settings panel.
pub trait LibraryInfo {
    /// Bumped whenever the stats change.
    fn version(&self) -> u64;
    /// A copy of the stats, taken only when `version` moved.
    fn stats(&self) -> Stats;
    fn online(&self) -> bool;
}

/// The weather worker's snapshot for the clock overlay.
pub trait WeatherInfo {
    /// Bumped whenever the snapshot changes.
    fn version(&self) -> u64;
    fn snapshot(&self) -> (Option<String>, Option<WeatherNow>);
}

/// Current weather in seam terms (the engine's own type stays engine-side).
#[derive(Clone, Copy)]
pub struct WeatherNow {
    pub temp_c: f64,
    /// WMO weather code, as Open-Meteo reports it.
    pub code: i64,
    pub is_day: bool,
}

/// Everything the slideshow side needs; absent before the first window.
pub struct Stage<'a> {
    pub slideshow: &'a mut dyn Slideshow,
    pub source: &'a dyn TileSource,
    pub library: &'a dyn LibraryInfo,
    /// No weather worker (the web demo): the overlay shows the time and
    /// date, and the status line says nothing about weather.
    pub weather: Option<&'a dyn WeatherInfo>,
    /// No network worker (the web demo, Android for now): Settings →
    /// Connectivity says Wi-Fi can't be set up here.
    pub network: Option<&'a dyn Network>,
}

pub struct Deps<'a> {
    pub stage: Option<Stage<'a>>,
    /// No power means no sleep schedule (desktop, web, a failed JNI setup).
    pub power: Option<&'a mut dyn Power>,
}

/// The engine's commands from a `frame` pass, to run in order. The native
/// hosts hand them to `raam_engine::run_effects`; the web demo maps them
/// onto its stand-ins. What only a host can do is a `FrameOut` field.
pub enum Effect {
    SaveSettings {
        rows: Vec<(&'static str, serde_json::Value)>,
        sleep: Schedule,
    },
    SetScale(String, Option<ScaleMode>),
    SetHidden(String, bool),
    SetSourceEnabled(SourceKind, bool),
    /// The server and key apply when the menu closes, not per keystroke.
    /// An empty `url` removes the server.
    SetServer {
        url: String,
        key: String,
    },
    ExportCuration,
    SelectAlbum(String, bool),
    SetCap(u32),
    ClearCache,
    Rescan,
    SyncNow,
    /// The fetch side's collage group size.
    SetMaxGroup(usize),
    /// Whether the weather worker may call out: the setting is on, the
    /// clock shows, and the app is in front. Sent when that changes; the
    /// worker starts with it off.
    SetWeather(bool),
}

/// An egui pass ran: upload `textures_delta.set`, upload `primitives`,
/// then free `textures_delta.free` (in that order), then paint.
pub struct EguiOut {
    pub textures_delta: egui::TexturesDelta,
    pub primitives: Vec<egui::ClippedPrimitive>,
    /// How long `run_ui` and `tessellate` took, for the host's stats line.
    pub run: Duration,
    pub tess: Duration,
}

/// The clock overlay's text changed: rebuild it before drawing.
pub struct OverlayRebuild {
    pub style: ClockStyle,
    pub corner: Corner,
    pub content: overlay::Content,
}

pub struct FrameOut {
    /// How long the host may block in `poll_events`.
    pub wait: Option<Duration>,
    pub effects: Vec<Effect>,
    /// Hidden or not yet initialised: draw nothing, skip the swap.
    pub skip_draw: bool,
    /// The full-screen settings cover everything: the host skips the
    /// slideshow and clock overlay draws under them (the Mali-400 has no
    /// hidden-surface removal, so a covered transition still costs 23-58
    /// ms a frame). The overlay rebuild is still applied, so its text
    /// stays current.
    pub chrome_opaque: bool,
    /// Just came back from hidden: the host resets its frame stats.
    pub became_visible: bool,
    /// Set the music stream's volume. Sent while sound is on, whenever the
    /// volume differs from the last one sent (so at start, and on a
    /// change). The stream is the system's, so this is the host's to set,
    /// not the engine's.
    pub music_volume: Option<f32>,
    /// How long the slideshow's `update` took, for the host's stats line.
    pub advance: Duration,
    pub overlay: Option<OverlayRebuild>,
    pub egui: Option<EguiOut>,
    /// Paint the (possibly previously uploaded) chrome this pass.
    pub draw_egui: bool,
}

pub struct App {
    pub ctx: egui::Context,
    pub state: AppState,
    start: Duration,
    // Input and the overlay chrome.
    overlay_open: bool,
    closed_down: Option<egui::Pos2>,
    egui_down: bool,
    /// The modifiers egui was last told of.
    modifiers: egui::Modifiers,
    /// Keys and text in turn: egui moves the focus once a pass, and clicks
    /// before it moves, so each press gets a pass of its own, and none
    /// goes in while the focus is on its way to a screen opened by key.
    keys: VecDeque<Keyed>,
    last_input: Duration,
    last_touch: Duration,
    undo: Option<(String, Duration)>,
    stats_version: u64,
    net_version: u64,
    /// When the open network list last asked for a scan.
    scan_sent: Option<Duration>,
    // Frame-reuse state: egui only runs when it has input, asked for a
    // repaint that is now due, or the status line changed.
    egui_due: Duration,
    egui_uploaded: bool,
    last_status: String,
    // Re-derive the overlay's text only when one of these changes.
    clock_inputs: Option<(u64, u64, ClockStyle, Corner, bool, bool)>,
    weather_status: String,
    // What was last sent for saving, so only changes are written.
    saved_rows: Vec<(&'static str, serde_json::Value)>,
    saved_sleep: Schedule,
    saved_server: (String, String),
    saved_cap: u32,
    settings_dirty: Option<Duration>,
    // The schedule as the user set it, kept while a debug override is on.
    schedule_base: (u32, u32),
    // Change-driven effects.
    sent_max_group: usize,
    sent_enabled: (bool, bool),
    music_volume: Option<f32>,
    sent_weather: bool,
    // Lifecycle and the sleep state machine.
    resumed: bool,
    check_wake: bool,
    asleep_since: Option<Duration>,
    manual_wake: Option<Duration>,
    /// When setting the wake alarm last failed; the next try waits
    /// `ALARM_RETRY` after it.
    alarm_failed: Option<Duration>,
    prev_in_sleep: Option<bool>,
    hidden_since: Option<Duration>,
    hidden_wakes: u32,
    overrides: Overrides,
    manual_idle: Duration,
    /// For the visible-again log line; injected like the pipeline's.
    mem_free_kb: fn() -> Option<u64>,
}

impl App {
    /// `state` arrives with the saved settings already loaded, so the
    /// saved-state snapshots start in sync and nothing writes at boot.
    pub fn new(state: AppState, mem_free_kb: fn() -> Option<u64>) -> Self {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        // The design system: fonts (egui's defaults are off), both themes'
        // styles, and the saved theme choice.
        theme::install_fonts(&ctx);
        theme::install(&ctx, theme::Schemes::baked(), theme::Options::default());
        ctx.set_theme(if state.settings.dark_theme {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        let now = clock::now();
        Self {
            saved_rows: store::settings_rows(&state.settings),
            saved_sleep: Schedule {
                enabled: state.settings.sleep_enabled,
                sleep_min: state.settings.sleep_min,
                wake_min: state.settings.wake_min,
            },
            saved_server: (
                state.settings.server_url.clone(),
                state.settings.api_key.clone(),
            ),
            saved_cap: state.settings.cache_cap_mb,
            schedule_base: (state.settings.sleep_min, state.settings.wake_min),
            sent_max_group: state.settings.collage_max,
            sent_enabled: (state.settings.immich_enabled, state.settings.local_enabled),
            ctx,
            state,
            start: now,
            overlay_open: false,
            closed_down: None,
            egui_down: false,
            modifiers: egui::Modifiers::NONE,
            keys: VecDeque::new(),
            last_input: now,
            last_touch: now,
            undo: None,
            stats_version: 0,
            net_version: 0,
            scan_sent: None,
            egui_due: now,
            egui_uploaded: false,
            last_status: String::new(),
            clock_inputs: None,
            weather_status: String::from("weather: waiting"),
            settings_dirty: None,
            music_volume: None,
            sent_weather: false,
            resumed: false,
            check_wake: false,
            asleep_since: None,
            manual_wake: None,
            alarm_failed: None,
            prev_in_sleep: None,
            hidden_since: Some(now),
            hidden_wakes: 0,
            overrides: Overrides::default(),
            manual_idle: DEFAULT_MANUAL_IDLE,
            mem_free_kb,
        }
    }

    /// For the host's stats line.
    pub fn overlay_open(&self) -> bool {
        self.overlay_open
    }

    /// One pass of the product's behaviour. The host calls it every loop
    /// pass — before the first window too (`stage: None`), so wake-at-boot
    /// works — then draws unless `skip_draw` and executes the effects.
    pub fn frame(&mut self, events: &[Event], inputs: &Inputs, deps: &mut Deps) -> FrameOut {
        let mut out = FrameOut {
            wait: None,
            effects: Vec::new(),
            skip_draw: false,
            chrome_opaque: false,
            became_visible: false,
            music_volume: None,
            advance: Duration::ZERO,
            overlay: None,
            egui: None,
            draw_egui: false,
        };

        // Touches, keys and text, in the order they came.
        let mut input: Vec<&Event> = Vec::new();
        for e in events {
            match e {
                Event::Start => self.check_wake = true,
                Event::Resume => {
                    self.resumed = true;
                    self.check_wake = true;
                }
                Event::Pause => self.resumed = false,
                Event::Touch(_) | Event::Key(_) | Event::Text(_) => input.push(e),
            }
        }

        // A cleared override puts the saved schedule back, so a test
        // schedule can't outlive its test.
        let ov = inputs.overrides;
        if ov != self.overrides {
            if ov.sleep != self.overrides.sleep {
                if self.overrides.sleep.is_none() {
                    self.schedule_base.0 = self.state.settings.sleep_min;
                }
                self.state.settings.sleep_min = ov.sleep.unwrap_or(self.schedule_base.0);
            }
            if ov.wake != self.overrides.wake {
                if self.overrides.wake.is_none() {
                    self.schedule_base.1 = self.state.settings.wake_min;
                }
                self.state.settings.wake_min = ov.wake.unwrap_or(self.schedule_base.1);
            }
            self.manual_idle = ov.idle.unwrap_or(DEFAULT_MANUAL_IDLE);
            self.overrides = ov;
        }

        let sched = Schedule {
            enabled: self.state.settings.sleep_enabled && deps.power.is_some(),
            sleep_min: self.state.settings.sleep_min,
            wake_min: self.state.settings.wake_min,
        };
        let (now_sod, now_epoch) = schedule::local_now();
        let in_sleep = sched.asleep_at(now_sod / 60);
        // Time to the next sleep/wake boundary, the loop's longest wait.
        let boundary_wait = sched.enabled.then(|| {
            schedule::until(sched.sleep_min, now_sod).min(schedule::until(sched.wake_min, now_sod))
        });

        if std::mem::take(&mut self.check_wake) && !in_sleep {
            if let Some(p) = deps.power.as_deref_mut() {
                let before = p.is_interactive();
                if inputs.wakelock_allowed {
                    match p.wake_screen() {
                        Ok(()) => {
                            log::info!("wake: wake lock taken (interactive before: {before:?})")
                        }
                        Err(e) => log::error!("wake: wake lock failed: {e}"),
                    }
                } else {
                    log::info!("wake: flags only (interactive before: {before:?})");
                }
            }
            if self.asleep_since.take().is_some() {
                log::info!(
                    "schedule: woken at wake time ({})",
                    schedule::fmt_hm(now_sod / 60)
                );
            }
        }

        // Before the first window there is nothing to draw on or route to.
        let Some(stage) = deps.stage.as_mut() else {
            out.skip_draw = true;
            out.wait = boundary_wait;
            return out;
        };

        // Nothing to draw on: no window, or not resumed (screen off,
        // another activity in front). The slideshow's time stops and the
        // host blocks until the next event or schedule boundary.
        if !(inputs.has_surface && self.resumed) {
            if self.hidden_since.is_none() {
                log::info!(
                    "hidden (surface={} resumed={}), slideshow paused",
                    inputs.has_surface,
                    self.resumed
                );
                self.hidden_since = Some(clock::now());
                self.hidden_wakes = 0;
                self.prev_in_sleep = None;
                stage.slideshow.set_clock_paused(true);
                // A playing clip (and its sound) stops with it.
                stage.slideshow.pause_video();
                // So do the weather calls: nothing shows their answer.
                if std::mem::take(&mut self.sent_weather) {
                    out.effects.push(Effect::SetWeather(false));
                }
                if self.overlay_open {
                    self.overlay_open = false;
                    stage.slideshow.clear_selection();
                    stage.slideshow.set_menu_open(false);
                    self.egui_down = false;
                    self.egui_uploaded = false;
                    self.state.reset_on_close();
                    self.ctx.memory_mut(|m| m.stop_text_input());
                }
                self.closed_down = None;
                self.keys.clear();
            }
            self.hidden_wakes += 1;
            // Backstop for the alarm: if the process is alive and the CPU
            // awake at wake time, light the screen from here too (whatever
            // is in front then shows; the alarm brings us to the front).
            if self.asleep_since.is_some() && !in_sleep {
                log::info!("schedule: wake time reached in-process while hidden");
                self.asleep_since = None;
                if let Some(p) = deps.power.as_deref_mut()
                    && let Err(e) = p.wake_screen()
                {
                    log::error!("wake: wake lock failed: {e}");
                }
            }
            // Touches and keys were dropped, not routed, so they don't
            // replay on return.
            out.skip_draw = true;
            out.wait = boundary_wait.map(|w| w + PAST_BOUNDARY);
            return out;
        }

        if let Some(since) = self.hidden_since.take() {
            log::info!(
                "visible again after {:.1}s hidden ({} loop wakes while hidden), MemFree={:?}KB",
                clock::elapsed(since).as_secs_f64(),
                self.hidden_wakes,
                (self.mem_free_kb)()
            );
            self.egui_uploaded = false;
            out.became_visible = true;
            self.last_touch = clock::now();
            if self.asleep_since.take().is_some() && in_sleep {
                log::info!(
                    "schedule: woken by hand in sleep hours, back to sleep after {:?} untouched",
                    self.manual_idle
                );
                self.manual_wake = Some(clock::now());
            }
        }

        // The schedule, evaluated only while in front.
        if !in_sleep {
            self.manual_wake = None;
            self.alarm_failed = None;
        }
        if let Some(since) = self.asleep_since
            && clock::elapsed(since) >= SLEEP_CONFIRM
        {
            log::error!(
                "schedule: screen still on {}s after sleeping, treating it as a manual wake",
                SLEEP_CONFIRM.as_secs()
            );
            self.asleep_since = None;
            self.manual_wake = Some(clock::now());
        }
        if self.asleep_since.is_none() && in_sleep {
            let crossed = self.prev_in_sleep == Some(false);
            if !crossed && self.manual_wake.is_none() {
                log::info!(
                    "schedule: in front during sleep hours, sleeping after {:?} untouched",
                    self.manual_idle
                );
                self.manual_wake = Some(clock::now());
            }
            let idle_done = self.manual_wake.is_some_and(|m| {
                clock::elapsed(m).min(clock::elapsed(self.last_touch)) >= self.manual_idle
            });
            let retry_due = self
                .alarm_failed
                .is_none_or(|f| clock::elapsed(f) >= ALARM_RETRY);
            if (crossed || idle_done)
                && retry_due
                && !self.overlay_open
                && let Some(p) = deps.power.as_deref_mut()
            {
                let wake_in = schedule::until(sched.wake_min, now_sod);
                let wake_ms = (now_epoch + wake_in.as_secs() as i64) * 1000;
                match p.set_wake_alarm(wake_ms) {
                    Ok(()) => {
                        log::info!(
                            "schedule: sleeping at {} ({}), wake alarm set for {} (in {}s)",
                            schedule::fmt_hm(now_sod / 60),
                            if crossed {
                                "sleep time"
                            } else {
                                "idle in sleep hours"
                            },
                            schedule::fmt_hm(sched.wake_min),
                            wake_in.as_secs()
                        );
                        self.asleep_since = Some(clock::now());
                        self.manual_wake = None;
                        self.alarm_failed = None;
                        p.sleep_screen();
                    }
                    Err(e) => {
                        if self.alarm_failed.is_none() {
                            log::error!("schedule: wake alarm failed, staying awake: {e}");
                        } else {
                            log::warn!("schedule: wake alarm failed again, staying awake: {e}");
                        }
                        self.alarm_failed = Some(clock::now());
                    }
                }
            }
        }
        self.prev_in_sleep = Some(in_sleep);

        let mut egui_events = Vec::new();
        let mut presses = Vec::new();
        for e in input {
            // A key counts as a touch for the schedule's idle time.
            self.last_touch = clock::now();
            match e {
                Event::Touch(t) if !self.overlay_open => match t.phase {
                    egui::TouchPhase::Start => self.closed_down = Some(t.pos),
                    egui::TouchPhase::End => {
                        if let Some(d) = self.closed_down.take()
                            && d.distance(t.pos) <= TAP_SLOP_PX
                        {
                            self.overlay_open = true;
                            self.last_input = clock::now();
                            self.state.keys = false;
                            let tile = stage.slideshow.select_at(t.pos.x, t.pos.y);
                            log::info!("overlay opened by tap at {:?} on tile {tile:?}", t.pos);
                        }
                    }
                    egui::TouchPhase::Cancel => self.closed_down = None,
                    egui::TouchPhase::Move => {}
                },
                Event::Touch(t) => {
                    self.last_input = clock::now();
                    push_egui_touch(t, &mut self.egui_down, &mut egui_events, &mut presses);
                }
                Event::Key(k) => self.queue_key(Keyed::Key(*k)),
                Event::Text(t) => self.queue_key(Keyed::Text(t.clone())),
                Event::Start | Event::Resume | Event::Pause => {}
            }
        }
        let mut fed = false;
        while let Some(front) = self.keys.front() {
            if !self.overlay_open {
                // Nothing on the slideshow takes text. A key that opens
                // the menu holds the rest back for its focus.
                if let Some(Keyed::Key(k)) = self.keys.pop_front()
                    && self.slideshow_key(&k, stage.slideshow, stage.source)
                {
                    break;
                }
                continue;
            }
            let press = matches!(front, Keyed::Key(k) if k.pressed);
            if self.state.focus_pending() || (press && fed) {
                break;
            }
            fed |= press;
            self.last_input = clock::now();
            match self.keys.pop_front() {
                Some(Keyed::Key(k)) => {
                    // egui keeps the modifiers from these (Shift-Tab).
                    if k.modifiers != self.modifiers {
                        self.modifiers = k.modifiers;
                        egui_events.push(egui::Event::ModifiersChanged(k.modifiers));
                    }
                    egui_events.push(egui::Event::Key {
                        key: k.key,
                        physical_key: None,
                        pressed: k.pressed,
                        repeat: k.repeat,
                        modifiers: k.modifiers,
                    });
                }
                Some(Keyed::Text(t)) => egui_events.push(egui::Event::Text(t)),
                None => {}
            }
        }

        self.apply(stage.slideshow, &mut out.effects);
        // The volume is the music stream's, set when it changes while
        // sound is on (and once at start).
        let want_volume = self
            .state
            .settings
            .video_sound
            .then_some(self.state.settings.video_volume);
        if want_volume.is_some() && want_volume != self.music_volume {
            out.music_volume = want_volume;
            self.music_volume = want_volume;
        }
        // The weather worker calls out only while its answer can show.
        let shows_weather = self.state.settings.weather_enabled
            && self.state.settings.clock_style != ClockStyle::Off;
        if shows_weather != self.sent_weather {
            self.sent_weather = shows_weather;
            out.effects.push(Effect::SetWeather(shows_weather));
        }
        stage.slideshow.set_menu_open(self.overlay_open);
        let t = clock::now();
        stage.slideshow.update(stage.source);
        out.advance = clock::elapsed(t);

        // Clock overlay: over the slideshow (transitions included), under
        // the egui chrome. The host rebuilds and draws; the text and its
        // change detection live here.
        let minute = clock::wall().as_secs() / 60;
        let weather = stage.weather.filter(|_| shows_weather);
        let clock_inputs = (
            minute,
            weather.map_or(0, |w| w.version()),
            self.state.settings.clock_style,
            self.state.settings.clock_corner,
            self.state.settings.clock_24h,
            shows_weather,
        );
        if self.clock_inputs != Some(clock_inputs) {
            self.clock_inputs = Some(clock_inputs);
            let (city, current) = weather.map_or((None, None), |w| w.snapshot());
            // A reading with no number (NaN from a bad answer) counts as
            // none: "0°" or "NaN°C" would be a made-up temperature.
            let current = current.filter(|c| c.temp_c.is_finite());
            let w = current.map(|c| {
                let (description, icon) = weather_icons::describe(c.code, c.is_day);
                overlay::Weather {
                    icon: icon.ch(),
                    temp: format!("{}\u{b0}", c.temp_c.round() as i64),
                    description,
                    city: city.clone(),
                }
            });
            self.weather_status = match (&city, current) {
                (Some(city), Some(c)) => format!(
                    "{city} {:.1}\u{b0}C {}",
                    c.temp_c,
                    weather_icons::describe(c.code, c.is_day).0
                ),
                (Some(city), None) => format!("{city}, weather pending"),
                _ if weather.is_none() => String::new(),
                _ => "locating...".to_string(),
            };
            let now = clock::local((minute * 60) as i64);
            let content = overlay::content(
                self.state.settings.clock_style,
                self.state.settings.clock_24h,
                &now,
                w,
            );
            out.overlay = Some(OverlayRebuild {
                style: self.state.settings.clock_style,
                corner: self.state.settings.clock_corner,
                content,
            });
        }

        let mut force_redraw = false;
        let mut egui_delay = Duration::MAX;
        if self
            .undo
            .as_ref()
            .is_some_and(|u| clock::elapsed(u.1) >= UNDO_HIDE)
        {
            self.undo = None;
        }
        if self.overlay_open {
            out.draw_egui = true;
            self.state.shown_scale = stage.slideshow.shown_scale_mode();
            self.state.shown_video = stage.slideshow.shown_is_video();
            self.state.undo_secs = self
                .undo
                .as_ref()
                .map(|u| UNDO_HIDE.saturating_sub(clock::elapsed(u.1)).as_secs() + 1);
            let version = stage.library.version();
            let layout = stage.slideshow.shown_layout();
            self.state.has_weather = stage.weather.is_some();
            // A join under way, or the network list filling in, is being
            // watched: the menu stays up until the usual while after.
            let listing = self.state.screen == frame_ui::Screen::Settings
                && self.state.section == Section::Connectivity
                && matches!(self.state.sub, Sub::Networks | Sub::Join);
            if self
                .state
                .network
                .as_ref()
                .and_then(|n| n.wifi.as_ref().ok())
                .is_some_and(|w| w.joining() || (listing && w.scanning))
            {
                self.last_input = clock::now();
            }
            // The network snapshot, copied only when it moved.
            let mut net_moved = false;
            match stage.network {
                None => self.state.network = None,
                Some(n) => {
                    let v = n.version();
                    if v != self.net_version || self.state.network.is_none() {
                        self.net_version = v;
                        self.state.network = Some(n.snapshot());
                        net_moved = true;
                    }
                }
            }
            self.state.status = Status {
                layout,
                weather: self.weather_status.clone(),
                online: stage.library.online(),
            };
            // What the menu shows that egui can't see change by itself:
            // a run is due whenever any of it moved.
            let status = format!(
                "{:?}|{}|{}|{:?}|{:?}|{}|{}",
                self.state.status.layout,
                self.state.status.weather,
                self.state.status.online,
                self.state.shown_scale,
                self.state.undo_secs,
                self.state.shown_video,
                self.state.paused,
            );
            let raw_input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(inputs.screen.0 as f32, inputs.screen.1 as f32),
                )),
                time: Some(clock::elapsed(self.start).as_secs_f64()),
                predicted_dt: PREDICTED_DT,
                events: egui_events,
                ..Default::default()
            };
            // The screen these presses landed on (run_ui below may flip
            // it, e.g. Back: Settings -> Menu in the same pass).
            let menu_at_input = self.state.screen == frame_ui::Screen::Menu;
            let mut actions = frame_ui::Actions::default();
            // Keys waiting for the focus wait on egui's passes.
            let need_run = !self.egui_uploaded
                || !raw_input.events.is_empty()
                || self.state.focus_pending()
                || clock::now() >= self.egui_due
                || status != self.last_status
                || version != self.stats_version
                || net_moved;
            if need_run {
                // The copy follows the version read: a change between the
                // two lands here early and moves the version once more,
                // which costs one extra run, never a missed one.
                if version != self.stats_version {
                    self.stats_version = version;
                    self.state.library = stage.library.stats();
                }
                let t = clock::now();
                let state = &mut self.state;
                let mut full_output = self.ctx.run_ui(raw_input, |ui| frame_ui::draw(ui, state));
                let run = clock::elapsed(t);

                // The Display page's theme choice, applied and persisted
                // like any other setting. set_theme doesn't ask for a
                // repaint itself, so the next pass runs at once.
                let want = if self.state.settings.dark_theme {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                };
                if self.ctx.theme() != want {
                    self.ctx.set_theme(want);
                    self.egui_due = clock::now();
                }

                actions = std::mem::take(&mut self.state.actions);
                self.apply(stage.slideshow, &mut out.effects);
                if actions.next {
                    stage.slideshow.request_next();
                }
                if actions.prev {
                    stage.slideshow.request_prev(stage.source);
                }
                if actions.toggle_scale
                    && let Some((key, mode)) = stage.slideshow.toggle_shown_scale()
                {
                    out.effects.push(Effect::SetScale(key, mode));
                }
                if actions.hide
                    && let Some((key, asset)) = stage.slideshow.shown_photo()
                {
                    log::info!("hide asset {asset} ({key})");
                    out.effects.push(Effect::SetHidden(key.clone(), true));
                    stage.slideshow.forget(&key, stage.source);
                    self.undo = Some((key, clock::now()));
                }
                if actions.undo_hide
                    && let Some((key, _)) = self.undo.take()
                {
                    log::info!("undo hide of {key}");
                    out.effects.push(Effect::SetHidden(key.clone(), false));
                    stage.slideshow.unforget(&key);
                }
                if let Some(key) = actions.unhide.take() {
                    out.effects.push(Effect::SetHidden(key.clone(), false));
                    stage.slideshow.unforget(&key);
                }
                for (flag, effect) in [
                    (actions.clear_cache, Effect::ClearCache),
                    (actions.rescan, Effect::Rescan),
                    (actions.sync_now, Effect::SyncNow),
                    (actions.export, Effect::ExportCuration),
                ] {
                    if flag {
                        out.effects.push(effect);
                    }
                }
                for (album, on) in actions.select_album.drain(..) {
                    out.effects.push(Effect::SelectAlbum(album, on));
                }
                if let Some(n) = stage.network {
                    for cmd in actions.net.drain(..) {
                        log::info!("network: {cmd:?}");
                        n.send(cmd);
                    }
                }
                if self.state.settings.immich_enabled != self.sent_enabled.0 {
                    self.sent_enabled.0 = self.state.settings.immich_enabled;
                    out.effects.push(Effect::SetSourceEnabled(
                        SourceKind::Immich,
                        self.sent_enabled.0,
                    ));
                }
                if self.state.settings.local_enabled != self.sent_enabled.1 {
                    self.sent_enabled.1 = self.state.settings.local_enabled;
                    out.effects.push(Effect::SetSourceEnabled(
                        SourceKind::Local,
                        self.sent_enabled.1,
                    ));
                }
                if self.state.settings.cache_cap_mb != self.saved_cap {
                    self.saved_cap = self.state.settings.cache_cap_mb;
                    out.effects.push(Effect::SetCap(self.saved_cap));
                }
                // A time changed in Settings while a debug override holds it
                // is the user's: it becomes the one saved, and the one the
                // override's end puts back.
                if let Some(v) = self.overrides.sleep
                    && self.state.settings.sleep_min != v
                {
                    self.schedule_base.0 = self.state.settings.sleep_min;
                }
                if let Some(v) = self.overrides.wake
                    && self.state.settings.wake_min != v
                {
                    self.schedule_base.1 = self.state.settings.wake_min;
                }
                let rows = store::settings_rows(&self.state.settings);
                let sleep = Schedule {
                    enabled: self.state.settings.sleep_enabled,
                    sleep_min: self.state.settings.sleep_min,
                    wake_min: self.state.settings.wake_min,
                };
                if rows != self.saved_rows || sleep != self.saved_sleep {
                    self.settings_dirty.get_or_insert_with(clock::now);
                }
                if actions.next || actions.prev {
                    stage.slideshow.update(stage.source);
                }

                let t = clock::now();
                let primitives = self.ctx.tessellate(
                    std::mem::take(&mut full_output.shapes),
                    full_output.pixels_per_point,
                );
                let tess = clock::elapsed(t);
                self.egui_uploaded = true;
                self.last_status = status;

                let delay = full_output
                    .viewport_output
                    .get(&egui::ViewportId::ROOT)
                    .map_or(Duration::MAX, |v| v.repaint_delay);
                self.egui_due = clock::now()
                    .checked_add(delay)
                    .unwrap_or_else(|| clock::now() + EGUI_NEVER_DUE);
                out.egui = Some(EguiOut {
                    textures_delta: std::mem::take(&mut full_output.textures_delta),
                    primitives,
                    run,
                    tess,
                });
            }
            egui_delay = self.egui_due.saturating_sub(clock::now());

            // The network list, while it's open, keeps itself current.
            let wifi = self
                .state
                .network
                .as_ref()
                .and_then(|n| n.wifi.as_ref().ok());
            if listing
                && let Some(n) = stage.network
                && let Some(w) = wifi
                && w.enabled
            {
                let due = self
                    .scan_sent
                    .is_none_or(|t| clock::elapsed(t) >= WIFI_RESCAN);
                if due && !w.scanning && !w.joining() {
                    log::info!("network: {:?}", NetCommand::Scan);
                    n.send(NetCommand::Scan);
                    self.scan_sent = Some(clock::now());
                }
                if let Some(t) = self.scan_sent {
                    egui_delay = egui_delay.min(WIFI_RESCAN.saturating_sub(clock::elapsed(t)));
                }
            } else if !listing {
                // Opening the list again scans at once.
                self.scan_sent = None;
            }

            // A text field, not any focus: keys focus buttons too.
            let typing = self.ctx.text_edit_focused();
            // `run_ui`'s root Ui is itself a full-screen Background-order
            // layer, so on the menu screen "outside the chrome" means a hit
            // on nothing above it (the toolbar is an Area). The settings
            // draw as Panels IN the root layer, so there every tap would
            // read as outside: tap-to-dismiss is a menu-screen rule only
            // (found on the frame; settings leave via Back or the idle
            // timeout).
            let tapped_outside = menu_at_input
                && presses.iter().any(|p| {
                    self.ctx
                        .layer_id_at(*p)
                        .is_none_or(|l| l.order == egui::Order::Background)
                });
            let timed_out = !typing && clock::elapsed(self.last_input) >= AUTO_DISMISS;
            if actions.close || tapped_outside || timed_out {
                log::info!(
                    "overlay closed (button={} tap_outside={tapped_outside} timeout={timed_out})",
                    actions.close
                );
                self.overlay_open = false;
                stage.slideshow.clear_selection();
                stage.slideshow.set_menu_open(false);
                self.egui_down = false;
                self.egui_uploaded = false;
                self.state.reset_on_close();
                self.ctx.memory_mut(|m| m.stop_text_input());
                force_redraw = true;
                // The server and key apply (and save) when the menu
                // closes, not per keystroke. The URL goes out as the
                // settings page said it would ("Will use http://…"). An
                // emptied field removes the server; one that can't be a
                // web address keeps the old server until it's fixed.
                let raw = self.state.settings.server_url.trim();
                let url = if raw.is_empty() {
                    Some(String::new())
                } else {
                    frame_ui::normalise_url(raw)
                };
                if let Some(url) = url {
                    let server = (url, self.state.settings.api_key.trim().to_string());
                    if server != self.saved_server {
                        self.saved_server = server.clone();
                        out.effects.push(Effect::SetServer {
                            url: server.0,
                            key: server.1,
                        });
                    }
                }
            }
        }
        out.chrome_opaque = self.overlay_open && self.state.opaque();
        // Save changed settings once they settle, or at once when the menu
        // has closed. A debug schedule override is never saved.
        if let Some(since) = self.settings_dirty
            && (!self.overlay_open || clock::elapsed(since) >= SAVE_DEBOUNCE)
        {
            self.settings_dirty = None;
            let rows = store::settings_rows(&self.state.settings);
            let sleep = Schedule {
                enabled: self.state.settings.sleep_enabled,
                sleep_min: if self.overrides.sleep.is_some() {
                    self.schedule_base.0
                } else {
                    self.state.settings.sleep_min
                },
                wake_min: if self.overrides.wake.is_some() {
                    self.schedule_base.1
                } else {
                    self.state.settings.wake_min
                },
            };
            if rows != self.saved_rows || sleep != self.saved_sleep {
                self.saved_rows = rows.clone();
                self.saved_sleep = sleep;
                out.effects.push(Effect::SaveSettings { rows, sleep });
            }
        }

        // A scaling change lands in `update` next pass, so run one more.
        out.wait = if force_redraw
            || !self.keys.is_empty()
            || stage.slideshow.is_animating()
            || stage.slideshow.recompose_pending()
        {
            Some(Duration::ZERO)
        } else {
            let mut w = stage.slideshow.next_deadline();
            if self.state.settings.clock_style != ClockStyle::Off {
                w = min_wait(w, until_next_minute());
            }
            if let Some(b) = boundary_wait {
                w = min_wait(w, b + PAST_BOUNDARY);
            }
            if let Some(m) = self.manual_wake {
                let idle = clock::elapsed(m).min(clock::elapsed(self.last_touch));
                let mut due = self.manual_idle.saturating_sub(idle);
                if let Some(f) = self.alarm_failed {
                    due = due.max(ALARM_RETRY.saturating_sub(clock::elapsed(f)));
                }
                w = min_wait(w, due + PAST_BOUNDARY);
            }
            if let Some(since) = self.settings_dirty {
                w = min_wait(
                    w,
                    SAVE_DEBOUNCE.saturating_sub(clock::elapsed(since)) + PAST_DEBOUNCE,
                );
            }
            if self.overlay_open {
                if egui_delay < MAX_EGUI_WAIT {
                    w = min_wait(w, egui_delay);
                }
                if self.undo.is_some() {
                    w = min_wait(w, UNDO_TICK);
                }
                if !self.ctx.text_edit_focused() {
                    w = min_wait(
                        w,
                        AUTO_DISMISS.saturating_sub(clock::elapsed(self.last_input)),
                    );
                }
            }
            w
        };
        out
    }

    /// Queues a key or typed text for its turn. The menu takes one press a
    /// pass, so a held key's repeats (about 30 a second) would outrun a
    /// slow frame and keep the focus moving long after the key is let go:
    /// a repeat waits only when no press of its key does. Past
    /// `MAX_QUEUED_KEYS` the newest is dropped, so what was typed first
    /// still lands in order.
    fn queue_key(&mut self, item: Keyed) {
        if let Keyed::Key(k) = &item
            && k.repeat
            && self
                .keys
                .iter()
                .any(|q| matches!(q, Keyed::Key(w) if w.pressed && w.key == k.key))
        {
            return;
        }
        if self.keys.len() >= MAX_QUEUED_KEYS {
            log::warn!("key queue full: a key dropped");
            return;
        }
        self.keys.push_back(item);
    }

    /// A key while the menu is closed (docs/UX.md, Keys): the arrows go to
    /// the previous and next photo, Escape does nothing, and any other key
    /// opens the menu as a tap does. A repeat or a shortcut (Ctrl, Alt,
    /// Command held) is not a tap. True if it opened the menu.
    fn slideshow_key(
        &mut self,
        k: &KeyEvent,
        slideshow: &mut dyn Slideshow,
        source: &dyn TileSource,
    ) -> bool {
        let m = k.modifiers;
        if !k.pressed || k.repeat || m.ctrl || m.alt || m.command || m.mac_cmd {
            return false;
        }
        // Nothing shown yet: nothing to go back or on from (the menu
        // disables Previous and Next then too).
        let showing = slideshow.shown_layout().is_some();
        match k.key {
            egui::Key::ArrowRight if showing => slideshow.request_next(),
            egui::Key::ArrowLeft if showing => slideshow.request_prev(source),
            egui::Key::ArrowRight | egui::Key::ArrowLeft | egui::Key::Escape => {}
            key => {
                self.overlay_open = true;
                self.last_input = clock::now();
                self.state.keys = true;
                // A point on no tile picks the first, as Frameo's menu key
                // picks the first photo of the page.
                let tile = slideshow.select_at(-1.0, -1.0);
                log::info!("overlay opened by key {key:?} on tile {tile:?}");
                return true;
            }
        }
        false
    }

    /// The UI's settings, applied to the slideshow (and the fetch side's
    /// group size, as an effect, when it changed).
    fn apply(&mut self, slideshow: &mut dyn Slideshow, effects: &mut Vec<Effect>) {
        if self.state.settings.collage_max != self.sent_max_group {
            self.sent_max_group = self.state.settings.collage_max;
            effects.push(Effect::SetMaxGroup(self.sent_max_group));
        }
        let ss = &self.state.settings;
        let s = slideshow.settings_mut();
        s.gap_colour = ss.gap_colour;
        s.dwell = Duration::from_secs_f32(ss.interval_secs);
        s.transition = ss.transition.shader_name();
        s.ken_burns = ss.ken_burns_enabled;
        s.fill_by_default = ss.fill_by_default;
        s.fit_background = ss.fit_background;
        s.video_playback = ss.video_playback;
        s.video_sound = ss.video_sound;
        // Hidden time never counts: the controller pauses the clock while
        // there is nothing to draw on, and this only runs while drawing.
        slideshow.set_clock_paused(self.state.paused);
    }
}

/// Waits that end at a boundary (sleep, wake, manual idle) end this long
/// after it, so the pass that wakes sees the boundary crossed, not just
/// short of it.
const PAST_BOUNDARY: Duration = Duration::from_millis(50);
/// The same for the settings save, whose debounce check is `>=`.
const PAST_DEBOUNCE: Duration = Duration::from_millis(10);
/// The same for the clock's minute, so the overlay draws the new minute.
const PAST_MINUTE: Duration = Duration::from_millis(20);
/// "Undo hide" counts down on screen, so redraw this often while it shows.
const UNDO_TICK: Duration = Duration::from_millis(250);
/// egui asked for no repaint (`Duration::MAX` overflows the clock): due
/// this far ahead, which any input or deadline cuts short.
const EGUI_NEVER_DUE: Duration = Duration::from_secs(3600);
/// egui's frame-time hint (s), for its animations. Chosen.
const PREDICTED_DT: f32 = 1.0 / 30.0;

fn min_wait(a: Option<Duration>, b: Duration) -> Option<Duration> {
    Some(a.map_or(b, |a| a.min(b)))
}

/// Time to the next wall-clock minute, plus a few ms so the wake lands
/// after the boundary rather than just before it.
fn until_next_minute() -> Duration {
    let now = clock::wall();
    let into = Duration::from_millis((now.as_millis() % 60_000) as u64);
    Duration::from_secs(60) - into + PAST_MINUTE
}

/// A touch as egui events: mouse-style pointer events for
/// buttons/sliders/keyboard plus real `Event::Touch` for `ScrollArea`'s
/// touch-drag-to-scroll (egui pans a `ScrollArea` on a drag only once it
/// has seen a real touch).
fn push_egui_touch(
    t: &Touch,
    down: &mut bool,
    events: &mut Vec<egui::Event>,
    presses: &mut Vec<egui::Pos2>,
) {
    let touch = egui::Event::Touch {
        device_id: egui::TouchDeviceId(t.device_id),
        id: egui::TouchId(t.touch_id),
        phase: t.phase,
        pos: t.pos,
        force: Some(t.force),
    };
    let button = |pressed| egui::Event::PointerButton {
        pos: t.pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    match t.phase {
        egui::TouchPhase::Start => {
            *down = true;
            presses.push(t.pos);
            events.extend([egui::Event::PointerMoved(t.pos), button(true), touch]);
        }
        egui::TouchPhase::Move => {
            if *down {
                events.extend([egui::Event::PointerMoved(t.pos), touch]);
            }
        }
        egui::TouchPhase::End => {
            *down = false;
            events.extend([
                egui::Event::PointerMoved(t.pos),
                button(false),
                touch,
                egui::Event::PointerGone,
            ]);
        }
        egui::TouchPhase::Cancel => {
            *down = false;
            events.extend([touch, egui::Event::PointerGone]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::fake::{advance, install as install_clock, set_wall_hm};
    use raam_model::{Plan, TransitionChoice};
    use std::cell::Cell;

    #[derive(Default)]
    struct FakeShow {
        settings: Option<SlideshowSettings>,
        selected: Cell<u32>,
        nexts: u32,
        prevs: u32,
    }

    impl FakeShow {
        fn new() -> Self {
            FakeShow {
                settings: Some(SlideshowSettings {
                    dwell: Duration::from_secs(10),
                    transition: None,
                    ken_burns: true,
                    fill_by_default: true,
                    fit_background: raam_model::FitBackground::Blurred,
                    gap_colour: raam_model::GapColour::Black,
                    video_playback: raam_model::VideoPlayback::Continue,
                    video_sound: false,
                }),
                selected: Cell::new(0),
                nexts: 0,
                prevs: 0,
            }
        }
    }

    impl Slideshow for FakeShow {
        fn settings_mut(&mut self) -> &mut SlideshowSettings {
            self.settings.as_mut().unwrap()
        }
        fn set_clock_paused(&mut self, _paused: bool) {}
        fn pause_video(&mut self) {}
        fn set_menu_open(&mut self, _open: bool) {}
        fn clear_selection(&mut self) {}
        fn select_at(&mut self, _x: f32, _y: f32) -> Option<usize> {
            self.selected.set(self.selected.get() + 1);
            Some(0)
        }
        fn update(&mut self, _source: &dyn TileSource) {}
        fn request_next(&mut self) {
            self.nexts += 1;
        }
        fn request_prev(&mut self, _source: &dyn TileSource) {
            self.prevs += 1;
        }
        fn toggle_shown_scale(&mut self) -> Option<(String, Option<ScaleMode>)> {
            None
        }
        fn shown_photo(&self) -> Option<(String, i64)> {
            None
        }
        fn forget(&mut self, _key: &str, _source: &dyn TileSource) {}
        fn unforget(&mut self, _key: &str) {}
        fn shown_scale_mode(&self) -> Option<ScaleMode> {
            None
        }
        fn shown_is_video(&self) -> bool {
            false
        }
        fn shown_layout(&self) -> Option<String> {
            Some("1 (single)".into())
        }
        fn is_animating(&self) -> bool {
            false
        }
        fn recompose_pending(&self) -> bool {
            false
        }
        fn next_deadline(&self) -> Option<Duration> {
            None
        }
    }

    struct FakeSource;
    impl TileSource for FakeSource {
        fn take_plan(&self) -> Option<Plan> {
            None
        }
        fn take_tile(&self, _seq: u64) -> Option<raam_model::TilePhoto> {
            None
        }
        fn take_failed(&self) -> Option<u64> {
            None
        }
        fn consumed(&self) {}
        fn request(&self, _plan: Plan) {}
        fn tile_is_clip(&self, _seq: u64) -> bool {
            false
        }
        fn set_skip_videos(&self, _skip: bool) {}
    }

    struct FakeLib {
        version: Cell<u64>,
        /// How many times the stats were copied out.
        fetches: Cell<u32>,
    }
    impl LibraryInfo for FakeLib {
        fn version(&self) -> u64 {
            self.version.get()
        }
        fn stats(&self) -> Stats {
            self.fetches.set(self.fetches.get() + 1);
            Stats::default()
        }
        fn online(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct FakeWeather {
        city: Option<String>,
        now: Option<WeatherNow>,
    }
    impl WeatherInfo for FakeWeather {
        fn version(&self) -> u64 {
            1
        }
        fn snapshot(&self) -> (Option<String>, Option<WeatherNow>) {
            (self.city.clone(), self.now)
        }
    }

    /// A network worker that records what it's sent and serves a fixed
    /// snapshot.
    struct FakeNet {
        version: std::cell::Cell<u64>,
        snap: std::cell::RefCell<crate::network::NetSnapshot>,
        sent: std::cell::RefCell<Vec<String>>,
    }

    impl FakeNet {
        fn new() -> Self {
            FakeNet {
                version: std::cell::Cell::new(1),
                snap: std::cell::RefCell::new(crate::network::sample()),
                sent: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn update(&self, f: impl FnOnce(&mut crate::network::NetSnapshot)) {
            f(&mut self.snap.borrow_mut());
            self.version.set(self.version.get() + 1);
        }
    }

    impl Network for FakeNet {
        fn version(&self) -> u64 {
            self.version.get()
        }
        fn snapshot(&self) -> crate::network::NetSnapshot {
            self.snap.borrow().clone()
        }
        fn send(&self, cmd: NetCommand) {
            self.sent.borrow_mut().push(format!("{cmd:?}"));
        }
    }

    #[derive(Default)]
    struct FakePower {
        alarms: Vec<i64>,
        slept: u32,
        woken: u32,
        fail_alarm: bool,
        /// Every `set_wake_alarm` call, failed ones included.
        alarm_tries: u32,
    }

    impl Power for FakePower {
        fn set_wake_alarm(&mut self, epoch_ms: i64) -> Result<(), String> {
            self.alarm_tries += 1;
            if self.fail_alarm {
                return Err("no alarm".into());
            }
            self.alarms.push(epoch_ms);
            Ok(())
        }
        fn sleep_screen(&mut self) {
            self.slept += 1;
        }
        fn wake_screen(&mut self) -> Result<(), String> {
            self.woken += 1;
            Ok(())
        }
        fn is_interactive(&self) -> Result<bool, String> {
            Ok(true)
        }
    }

    struct Rig {
        app: App,
        show: FakeShow,
        source: FakeSource,
        lib: FakeLib,
        /// `None`: a host without a weather worker (the web demo).
        weather: Option<FakeWeather>,
        power: Option<FakePower>,
        /// `None`: a host without a network worker.
        net: Option<FakeNet>,
        ov: Overrides,
    }

    impl Rig {
        fn new(power: Option<FakePower>) -> Self {
            install_clock();
            advance(Duration::from_secs(1));
            let mut app = App::new(AppState::new("", ""), || None);
            // The rigs start in front with a surface up.
            let _ = app.frame(
                &[Event::Start, Event::Resume],
                &Inputs {
                    has_surface: true,
                    screen: (1280, 800),
                    wakelock_allowed: true,
                    overrides: Overrides::default(),
                },
                &mut Deps {
                    stage: None,
                    power: None,
                },
            );
            Rig {
                app,
                show: FakeShow::new(),
                source: FakeSource,
                lib: FakeLib {
                    version: Cell::new(1),
                    fetches: Cell::new(0),
                },
                weather: Some(FakeWeather::default()),
                power,
                net: None,
                ov: Overrides::default(),
            }
        }

        fn frame(&mut self, events: &[Event]) -> FrameOut {
            let mut out = self.app.frame(
                events,
                &Inputs {
                    has_surface: true,
                    screen: (1280, 800),
                    wakelock_allowed: true,
                    overrides: self.ov,
                },
                &mut Deps {
                    stage: Some(Stage {
                        slideshow: &mut self.show,
                        source: &self.source,
                        library: &self.lib,
                        weather: self.weather.as_ref().map(|w| w as &dyn WeatherInfo),
                        network: self.net.as_ref().map(|n| n as &dyn Network),
                    }),
                    power: self.power.as_mut().map(|p| p as &mut dyn Power),
                },
            );
            // The rig stands in for the host's painter: the texture deltas
            // count as applied (epaint asserts on dropping them unhandled).
            if let Some(e) = out.egui.as_mut() {
                e.textures_delta.clear();
            }
            out
        }

        fn tap(&mut self, x: f32, y: f32) -> FrameOut {
            let down = Event::Touch(Touch {
                phase: egui::TouchPhase::Start,
                pos: egui::pos2(x, y),
                device_id: 0,
                touch_id: 0,
                force: 1.0,
            });
            let up = Event::Touch(Touch {
                phase: egui::TouchPhase::End,
                pos: egui::pos2(x, y),
                device_id: 0,
                touch_id: 0,
                force: 0.0,
            });
            self.frame(&[down, up])
        }

        /// A key pressed and let go, with these modifiers.
        fn key_with(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> FrameOut {
            let k = |pressed| {
                Event::Key(KeyEvent {
                    key,
                    pressed,
                    repeat: false,
                    modifiers,
                })
            };
            self.frame(&[k(true), k(false)])
        }

        fn key(&mut self, key: egui::Key) -> FrameOut {
            self.key_with(key, egui::Modifiers::NONE)
        }

        fn focused(&self) -> Option<egui::Id> {
            self.app.ctx.memory(|m| m.focused())
        }

        /// A pass that runs egui, as the tap or key that changed the state
        /// behind a test's back would have.
        fn redraw(&mut self) -> FrameOut {
            self.app.egui_uploaded = false;
            self.frame(&[])
        }

        /// Opens the menu by key, then runs the pass after its unseen
        /// sizing pass, which a host runs at once (`wait` is zero).
        fn open_by_key(&mut self) {
            self.key(egui::Key::Enter);
            self.frame(&[]);
        }
    }

    #[test]
    fn the_library_stats_are_copied_only_when_they_moved() {
        let mut rig = Rig::new(None);
        rig.tap(640.0, 400.0);
        assert!(rig.app.overlay_open());
        for _ in 0..5 {
            rig.frame(&[]);
        }
        assert_eq!(rig.lib.fetches.get(), 1, "one copy while nothing moved");
        rig.lib.version.set(2);
        rig.frame(&[]);
        assert_eq!(rig.lib.fetches.get(), 2, "a new copy once it moved");
    }

    #[test]
    fn a_host_without_weather_leaves_it_out_of_the_status_line() {
        // The status line is published while the menu is up.
        let mut rig = Rig::new(None);
        rig.app.state.settings.weather_enabled = true;
        rig.tap(640.0, 400.0);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.app.state.status.weather, "locating...");
        assert!(rig.app.state.has_weather);
        let mut rig = Rig::new(None);
        rig.app.state.settings.weather_enabled = true;
        rig.weather = None;
        rig.tap(640.0, 400.0);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.app.state.status.weather, "");
        // The Weather switch greys out.
        assert!(!rig.app.state.has_weather);
    }

    #[test]
    fn a_temperature_that_isnt_a_number_shows_as_pending() {
        let mut rig = Rig::new(None);
        rig.app.state.settings.weather_enabled = true;
        rig.weather = Some(FakeWeather {
            city: Some("Paris".to_string()),
            now: Some(WeatherNow {
                temp_c: f64::NAN,
                code: 0,
                is_day: true,
            }),
        });
        rig.tap(640.0, 400.0);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.app.state.status.weather, "Paris, weather pending");
    }

    /// The `SetWeather` effects a pass sent.
    fn weather_sent(out: &FrameOut) -> Vec<bool> {
        out.effects
            .iter()
            .filter_map(|e| match e {
                Effect::SetWeather(on) => Some(*on),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_weather_calls_out_only_while_its_answer_can_show() {
        let mut rig = Rig::new(None);
        // Off by default: the worker is never told to start.
        assert_eq!(weather_sent(&rig.frame(&[])), [] as [bool; 0]);
        rig.tap(640.0, 400.0);
        assert_eq!(rig.app.state.status.weather, "");

        rig.app.state.settings.weather_enabled = true;
        assert_eq!(weather_sent(&rig.frame(&[])), [true]);
        assert_eq!(weather_sent(&rig.frame(&[])), [] as [bool; 0]);
        assert_eq!(rig.app.state.status.weather, "locating...");

        // The clock off hides the weather, so the calls stop.
        rig.app.state.settings.clock_style = ClockStyle::Off;
        assert_eq!(weather_sent(&rig.frame(&[])), [false]);
        rig.app.state.settings.clock_style = ClockStyle::Detailed;
        assert_eq!(weather_sent(&rig.frame(&[])), [true]);

        // Hidden (asleep, or another app in front): no calls either.
        assert_eq!(weather_sent(&rig.frame(&[Event::Pause])), [false]);
        assert_eq!(weather_sent(&rig.frame(&[])), [] as [bool; 0]);
        assert_eq!(weather_sent(&rig.frame(&[Event::Resume])), [true]);

        rig.app.state.settings.weather_enabled = false;
        assert_eq!(weather_sent(&rig.frame(&[])), [false]);
    }

    #[test]
    fn the_music_volume_is_sent_while_sound_is_on_and_only_on_a_change() {
        let mut rig = Rig::new(None);
        // Sound is off by default: the system's volume is left alone.
        assert_eq!(rig.frame(&[]).music_volume, None);

        rig.app.state.settings.video_sound = true;
        assert_eq!(rig.frame(&[]).music_volume, Some(0.5));
        assert_eq!(rig.frame(&[]).music_volume, None);
        rig.app.state.settings.video_volume = 0.8;
        assert_eq!(rig.frame(&[]).music_volume, Some(0.8));

        // Sound off leaves the stream where it was, and back on at the same
        // volume there is nothing to set; at another, there is.
        rig.app.state.settings.video_sound = false;
        assert_eq!(rig.frame(&[]).music_volume, None);
        rig.app.state.settings.video_sound = true;
        assert_eq!(rig.frame(&[]).music_volume, None);
        rig.app.state.settings.video_sound = false;
        rig.app.state.settings.video_volume = 0.3;
        assert_eq!(rig.frame(&[]).music_volume, None);
        rig.app.state.settings.video_sound = true;
        assert_eq!(rig.frame(&[]).music_volume, Some(0.3));
    }

    #[test]
    fn tap_within_slop_opens_the_menu_and_a_drag_does_not() {
        let mut rig = Rig::new(None);
        // A drag: down and up further apart than the slop.
        let down = Event::Touch(Touch {
            phase: egui::TouchPhase::Start,
            pos: egui::pos2(100.0, 100.0),
            device_id: 0,
            touch_id: 0,
            force: 1.0,
        });
        let up = Event::Touch(Touch {
            phase: egui::TouchPhase::End,
            pos: egui::pos2(100.0 + TAP_SLOP_PX + 1.0, 100.0),
            device_id: 0,
            touch_id: 0,
            force: 0.0,
        });
        rig.frame(&[down, up]);
        assert!(!rig.app.overlay_open());
        // A tap in place.
        rig.tap(100.0, 100.0);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.show.selected.get(), 1);
    }

    #[test]
    fn with_the_menu_closed_arrows_skip_and_other_keys_open_it() {
        let mut rig = Rig::new(None);
        rig.key(egui::Key::ArrowRight);
        rig.key(egui::Key::ArrowRight);
        rig.key(egui::Key::ArrowLeft);
        assert_eq!((rig.show.nexts, rig.show.prevs), (2, 1));
        assert!(!rig.app.overlay_open());
        // Escape, a shortcut and a repeat aren't taps.
        rig.key(egui::Key::Escape);
        rig.key_with(egui::Key::Q, egui::Modifiers::COMMAND);
        let held = Event::Key(KeyEvent {
            key: egui::Key::Space,
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::NONE,
        });
        rig.frame(&[held]);
        assert!(!rig.app.overlay_open());
        // Any other key opens the menu on the first photo, its first item
        // focused once the menu's unseen sizing pass is done: the next
        // pass is due at once.
        let out = rig.key(egui::Key::Space);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.show.selected.get(), 1);
        assert!(rig.app.state.keys);
        assert_eq!(out.wait, Some(Duration::ZERO));
        rig.frame(&[]);
        assert!(
            rig.focused().is_some(),
            "the menu's first item has the focus"
        );
    }

    #[test]
    fn keys_move_through_the_menu_act_and_escape_closes_it() {
        let mut rig = Rig::new(None);
        rig.open_by_key();
        let pause = rig.focused();
        assert!(pause.is_some());
        // Pause, Previous: Enter acts on the focused item, as a tap does.
        rig.key(egui::Key::ArrowRight);
        assert_ne!(rig.focused(), pause);
        rig.key(egui::Key::Enter);
        assert_eq!(rig.show.prevs, 1);
        assert!(rig.app.overlay_open());
        rig.key(egui::Key::Escape);
        assert!(!rig.app.overlay_open());
    }

    #[test]
    fn a_held_key_stops_moving_the_focus_once_let_go() {
        let mut rig = Rig::new(None);
        rig.open_by_key();
        let k = |pressed, repeat| {
            Event::Key(KeyEvent {
                key: egui::Key::ArrowRight,
                pressed,
                repeat,
                modifiers: egui::Modifiers::NONE,
            })
        };
        // A slow pass: the press and a hundred repeats, then the release.
        let mut held = vec![k(true, false)];
        held.extend((0..100).map(|_| k(true, true)));
        rig.frame(&held);
        assert!(rig.app.keys.len() <= MAX_QUEUED_KEYS);
        rig.frame(&[k(false, false)]);
        for _ in 0..3 {
            rig.frame(&[]);
        }
        let settled = rig.focused();
        for _ in 0..10 {
            rig.frame(&[]);
        }
        assert_eq!(rig.focused(), settled, "the focus still moves");
        assert!(rig.app.keys.is_empty());
    }

    #[test]
    fn typing_past_the_queue_keeps_what_came_first() {
        let mut rig = Rig::new(None);
        for i in 0..(MAX_QUEUED_KEYS + 10) {
            rig.app.queue_key(Keyed::Text(i.to_string()));
        }
        assert_eq!(rig.app.keys.len(), MAX_QUEUED_KEYS);
        assert!(matches!(rig.app.keys.front(), Some(Keyed::Text(t)) if t == "0"));
    }

    #[test]
    fn after_a_tap_the_first_key_only_brings_the_focus_in() {
        let mut rig = Rig::new(None);
        rig.tap(100.0, 100.0);
        rig.frame(&[]);
        assert!(rig.focused().is_none());
        rig.key(egui::Key::ArrowRight);
        let first = rig.focused();
        assert!(first.is_some());
        // On Pause, not a step on to Previous: Enter pauses.
        rig.key(egui::Key::Enter);
        assert!(rig.app.state.paused);
        assert_eq!(rig.show.prevs, 0);
    }

    #[test]
    fn escape_steps_back_a_level_and_the_focus_goes_back_with_it() {
        let mut rig = Rig::new(None);
        rig.open_by_key();
        // Pause, Previous, Next, Settings.
        for _ in 0..3 {
            rig.key(egui::Key::ArrowRight);
        }
        let settings_item = rig.focused();
        rig.key(egui::Key::Enter);
        rig.frame(&[]);
        assert_eq!(rig.app.state.screen, frame_ui::Screen::Settings);
        assert!(
            rig.focused().is_some(),
            "the selected section has the focus"
        );
        assert_ne!(rig.focused(), settings_item);
        rig.key(egui::Key::Escape);
        assert_eq!(rig.app.state.screen, frame_ui::Screen::Menu);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.focused(), settings_item, "back on the Settings item");
        rig.key(egui::Key::Escape);
        assert!(!rig.app.overlay_open());
    }

    #[test]
    fn a_physical_keyboard_types_into_a_field_with_the_on_screen_one_away() {
        let mut rig = Rig::new(None);
        rig.tap(100.0, 100.0);
        rig.app.state.screen = frame_ui::Screen::Settings;
        rig.app.state.section = frame_ui::Section::Server;
        rig.frame(&[]);
        assert!(rig.focused().is_none(), "a tap gives nothing the focus");
        // The first key only brings the focus in: the selected section.
        rig.key(egui::Key::Tab);
        assert!(rig.focused().is_some());
        // Server is the last section; the next control is the URL field.
        rig.key(egui::Key::Tab);
        assert!(rig.app.ctx.text_edit_focused());
        rig.frame(&[Event::Text("immich.lan".into())]);
        rig.frame(&[]);
        assert_eq!(rig.app.state.settings.server_url, "immich.lan");
        assert!(rig.app.state.keyboard.last_rect().is_none());
        // Escape leaves the field and nothing more.
        rig.key(egui::Key::Escape);
        assert!(!rig.app.ctx.text_edit_focused());
        assert_eq!(rig.app.state.screen, frame_ui::Screen::Settings);
    }

    /// Settings on `section`, opened by tap, then a key: the section has
    /// the focus.
    fn settings_by_key(rig: &mut Rig, section: frame_ui::Section) -> Option<egui::Id> {
        rig.tap(100.0, 100.0);
        rig.app.state.screen = frame_ui::Screen::Settings;
        rig.app.state.section = section;
        rig.redraw();
        rig.key(egui::Key::Tab);
        let nav = rig.focused();
        assert!(nav.is_some());
        nav
    }

    #[test]
    fn a_confirmation_opened_by_key_focuses_cancel_never_its_action() {
        let mut rig = Rig::new(None);
        let nav = settings_by_key(&mut rig, frame_ui::Section::Server);
        rig.app.state.dialog = frame_ui::Dialog::ClearCache;
        rig.redraw();
        rig.redraw();
        let out = rig.key(egui::Key::Enter);
        assert!(
            !out.effects.iter().any(|e| matches!(e, Effect::ClearCache)),
            "Enter on the first focus must not clear the cache"
        );
        assert_eq!(rig.app.state.dialog, frame_ui::Dialog::None);
        rig.redraw();
        assert_eq!(rig.focused(), nav, "back on what opened the dialog");
    }

    #[test]
    fn a_picker_opened_by_key_starts_on_its_choice_and_escape_keeps_it() {
        let mut rig = Rig::new(None);
        settings_by_key(&mut rig, frame_ui::Section::Slideshow);
        rig.app.state.settings.transition = TransitionChoice::Cube;
        rig.app.state.dialog = frame_ui::Dialog::Transition;
        rig.redraw();
        rig.redraw();
        // Cube, then down to the next one, and Enter picks it.
        rig.key(egui::Key::ArrowDown);
        rig.key(egui::Key::Enter);
        assert_eq!(
            rig.app.state.settings.transition,
            TransitionChoice::Crosswarp
        );
        assert_eq!(rig.app.state.dialog, frame_ui::Dialog::None);
        // Escape leaves a picker as it was.
        rig.app.state.dialog = frame_ui::Dialog::Transition;
        rig.redraw();
        rig.redraw();
        rig.key(egui::Key::ArrowDown);
        rig.key(egui::Key::Escape);
        assert_eq!(rig.app.state.dialog, frame_ui::Dialog::None);
        assert_eq!(
            rig.app.state.settings.transition,
            TransitionChoice::Crosswarp
        );
        assert_eq!(rig.app.state.screen, frame_ui::Screen::Settings);
    }

    #[test]
    fn a_number_dialog_starts_on_its_slider_whose_arrows_are_its_own() {
        let mut rig = Rig::new(None);
        settings_by_key(&mut rig, frame_ui::Section::Slideshow);
        // A fresh state's draft is 10 s.
        rig.app.state.dialog = frame_ui::Dialog::Interval;
        rig.redraw();
        rig.redraw();
        rig.key(egui::Key::ArrowRight);
        rig.key(egui::Key::ArrowRight);
        // Down from the slider is the next control drawn: OK (the actions
        // are drawn right to left).
        rig.key(egui::Key::ArrowDown);
        rig.key(egui::Key::Enter);
        assert_eq!(rig.app.state.dialog, frame_ui::Dialog::None);
        assert_eq!(rig.app.state.settings.interval_secs, 12.0);
    }

    #[test]
    fn keys_faster_than_the_passes_each_get_a_pass_of_their_own() {
        let mut rig = Rig::new(None);
        let k = |key, pressed| {
            Event::Key(KeyEvent {
                key,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        };
        // All in one pass: M opens the menu, then Right, Right and Enter.
        // Held back until the menu's first item has the focus, then one a
        // pass: Pause, Previous, Next, and Enter on Next.
        let mut batch = Vec::new();
        for key in [
            egui::Key::M,
            egui::Key::ArrowRight,
            egui::Key::ArrowRight,
            egui::Key::Enter,
        ] {
            batch.extend([k(key, true), k(key, false)]);
        }
        let mut out = rig.frame(&batch);
        let mut passes = 1;
        while out.wait == Some(Duration::ZERO) && passes < 20 {
            out = rig.frame(&[]);
            passes += 1;
        }
        assert!(rig.app.overlay_open());
        assert_eq!((rig.show.prevs, rig.show.nexts), (0, 1));
        assert!(!rig.app.state.paused);
    }

    #[test]
    fn a_dialog_closed_by_escape_hands_the_focus_back_to_its_row() {
        let mut rig = Rig::new(None);
        settings_by_key(&mut rig, frame_ui::Section::Slideshow);
        // Past Videos, Display, Sleep, Connectivity and Server to the
        // page's first row, Photo interval, which opens its dialog.
        for _ in 0..6 {
            rig.key(egui::Key::Tab);
        }
        let row = rig.focused();
        rig.key(egui::Key::Enter);
        rig.redraw();
        assert_eq!(rig.app.state.dialog, frame_ui::Dialog::Interval);
        assert_ne!(rig.focused(), row, "the dialog has the focus");
        rig.key(egui::Key::Escape);
        assert_eq!(rig.app.state.dialog, frame_ui::Dialog::None);
        rig.redraw();
        assert_eq!(rig.focused(), row, "back on Photo interval");
    }

    #[test]
    fn a_key_counts_as_a_touch_for_the_menu_timeout() {
        let mut rig = Rig::new(None);
        rig.open_by_key();
        advance(AUTO_DISMISS - Duration::from_secs(1));
        rig.key(egui::Key::ArrowRight);
        advance(Duration::from_secs(2));
        rig.frame(&[]);
        assert!(rig.app.overlay_open());
        advance(AUTO_DISMISS);
        rig.frame(&[]);
        assert!(!rig.app.overlay_open());
    }

    #[test]
    fn a_tap_on_the_open_settings_never_reads_as_outside() {
        let mut rig = Rig::new(None);
        rig.tap(100.0, 100.0);
        assert!(rig.app.overlay_open());
        // On the Settings screen (drawn as Panels in the root layer, which
        // is Background order), a tap anywhere is UI, not tap-outside.
        rig.app.state.screen = frame_ui::Screen::Settings;
        let out = rig.frame(&[]);
        assert!(out.chrome_opaque, "settings cover the slideshow");
        rig.tap(640.0, 400.0);
        rig.tap(180.0, 204.0);
        assert!(
            rig.app.overlay_open(),
            "settings taps must not dismiss the overlay"
        );
        // The idle timeout still applies there.
        advance(AUTO_DISMISS + Duration::from_secs(1));
        rig.frame(&[]);
        assert!(!rig.app.overlay_open());
    }

    #[test]
    fn the_menu_auto_dismisses_untouched() {
        let mut rig = Rig::new(None);
        rig.tap(100.0, 100.0);
        assert!(rig.app.overlay_open());
        // Still open just inside the window, and the wait aims at it.
        advance(AUTO_DISMISS - Duration::from_secs(1));
        let out = rig.frame(&[]);
        assert!(rig.app.overlay_open());
        assert!(out.wait.unwrap() <= Duration::from_secs(1));
        advance(Duration::from_secs(1));
        rig.frame(&[]);
        assert!(!rig.app.overlay_open());
    }

    /// Opens the menu, types the server and key into settings, and lets
    /// the menu time out: the `SetServer` effects its closing sent.
    fn close_with_server(rig: &mut Rig, url: &str, key: &str) -> Vec<(String, String)> {
        rig.tap(100.0, 100.0);
        assert!(rig.app.overlay_open());
        rig.app.state.settings.server_url = url.into();
        rig.app.state.settings.api_key = key.into();
        advance(AUTO_DISMISS + Duration::from_secs(1));
        let out = rig.frame(&[]);
        assert!(!rig.app.overlay_open());
        out.effects
            .iter()
            .filter_map(|e| match e {
                Effect::SetServer { url, key } => Some((url.clone(), key.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_emptied_server_url_removes_the_server_when_the_menu_closes() {
        let mut rig = Rig::new(None);
        let sent = |url: &str, key: &str| vec![(url.to_string(), key.to_string())];
        assert_eq!(
            close_with_server(&mut rig, "immich.lan", "key"),
            sent("http://immich.lan", "key")
        );
        // Unchanged: nothing is sent.
        assert_eq!(close_with_server(&mut rig, "immich.lan", "key"), []);
        // Not a web address: the old server stays until it's fixed.
        assert_eq!(close_with_server(&mut rig, "immich lan", "key"), []);
        // Emptied (spaces alone count as empty): the server goes.
        assert_eq!(close_with_server(&mut rig, " ", "key"), sent("", "key"));
        assert_eq!(close_with_server(&mut rig, "", "key"), []);
        // And a server typed again comes back.
        assert_eq!(
            close_with_server(&mut rig, "immich.lan", "key"),
            sent("http://immich.lan", "key")
        );
    }

    #[test]
    fn a_settings_change_saves_after_the_debounce() {
        let mut rig = Rig::new(None);
        rig.tap(100.0, 100.0);
        rig.app.state.settings.interval_secs = 20.0;
        let out = rig.frame(&[]);
        assert!(
            !out.effects
                .iter()
                .any(|e| matches!(e, Effect::SaveSettings { .. })),
            "nothing saves before the debounce"
        );
        advance(SAVE_DEBOUNCE);
        let out = rig.frame(&[]);
        let saved = out.effects.iter().find_map(|e| match e {
            Effect::SaveSettings { rows, .. } => Some(rows),
            _ => None,
        });
        let rows = saved.expect("the settled change saves");
        let interval = rows
            .iter()
            .find(|(k, _)| *k == "slideshow.interval_s")
            .unwrap();
        assert_eq!(interval.1, serde_json::json!(20.0f32));
        // Settled: nothing more to save.
        advance(SAVE_DEBOUNCE);
        let out = rig.frame(&[]);
        assert!(
            !out.effects
                .iter()
                .any(|e| matches!(e, Effect::SaveSettings { .. }))
        );
    }

    #[test]
    fn crossing_the_sleep_boundary_arms_the_alarm_and_sleeps() {
        let mut rig = Rig::new(Some(FakePower::default()));
        rig.app.state.settings.sleep_enabled = true;
        rig.app.state.settings.sleep_min = 23 * 60;
        rig.app.state.settings.wake_min = 5 * 60;
        set_wall_hm(22, 59);
        rig.frame(&[]);
        assert_eq!(rig.power.as_ref().unwrap().slept, 0);
        set_wall_hm(23, 1);
        rig.frame(&[]);
        let p = rig.power.as_ref().unwrap();
        assert_eq!(p.slept, 1, "crossed the boundary in front: sleeps");
        // The alarm aims at wake time: 05:00 tomorrow.
        assert_eq!(p.alarms, vec![((24 + 5) * 3600) * 1000]);
    }

    #[test]
    fn a_manual_wake_in_sleep_hours_goes_back_to_sleep_when_idle() {
        let mut rig = Rig::new(Some(FakePower::default()));
        rig.app.state.settings.sleep_enabled = true;
        rig.app.state.settings.sleep_min = 23 * 60;
        rig.app.state.settings.wake_min = 5 * 60;
        set_wall_hm(22, 59);
        rig.frame(&[]);
        set_wall_hm(23, 1);
        rig.frame(&[]);
        assert_eq!(rig.power.as_ref().unwrap().slept, 1);
        // The screen goes off: Pause, hidden. Someone turns it back on.
        rig.frame(&[Event::Pause]);
        advance(Duration::from_secs(60));
        set_wall_hm(23, 2);
        rig.frame(&[Event::Start, Event::Resume]);
        assert_eq!(
            rig.power.as_ref().unwrap().slept,
            1,
            "a manual wake stays up"
        );
        // Untouched for the manual idle: back to sleep, alarm re-armed.
        advance(DEFAULT_MANUAL_IDLE);
        set_wall_hm(23, 12);
        rig.frame(&[]);
        let p = rig.power.as_ref().unwrap();
        assert_eq!(p.slept, 2, "idle in sleep hours: back to sleep");
        assert_eq!(p.alarms.len(), 2);
    }

    #[test]
    fn a_failed_alarm_keeps_the_screen_on_and_retries() {
        let mut rig = Rig::new(Some(FakePower {
            fail_alarm: true,
            ..FakePower::default()
        }));
        rig.app.state.settings.sleep_enabled = true;
        rig.app.state.settings.sleep_min = 23 * 60;
        rig.app.state.settings.wake_min = 5 * 60;
        set_wall_hm(22, 59);
        rig.frame(&[]);
        set_wall_hm(23, 1);
        rig.frame(&[]);
        assert_eq!(rig.power.as_ref().unwrap().slept, 0, "no alarm, no sleep");
        // The failure heals. The crossing was spent, so the retry rides
        // the idle-in-sleep-hours path: one pass to notice, idle, sleep.
        rig.power.as_mut().unwrap().fail_alarm = false;
        rig.frame(&[]);
        advance(DEFAULT_MANUAL_IDLE);
        set_wall_hm(23, 11);
        rig.frame(&[]);
        assert_eq!(rig.power.as_ref().unwrap().slept, 1);
    }

    #[test]
    fn a_lasting_alarm_failure_retries_once_a_retry_interval() {
        let mut rig = Rig::new(Some(FakePower {
            fail_alarm: true,
            ..FakePower::default()
        }));
        rig.app.state.settings.sleep_enabled = true;
        rig.app.state.settings.sleep_min = 23 * 60;
        rig.app.state.settings.wake_min = 5 * 60;
        set_wall_hm(22, 59);
        rig.frame(&[]);
        set_wall_hm(23, 1);
        rig.frame(&[]);
        rig.frame(&[]);
        advance(DEFAULT_MANUAL_IDLE);
        set_wall_hm(23, 11);
        rig.frame(&[]);
        assert_eq!(rig.power.as_ref().unwrap().alarm_tries, 2, "crossing, idle");
        // Still failing, idle in sleep hours: the loop waits out the
        // retry interval, not 50 ms, and passes meanwhile don't retry.
        let out = rig.frame(&[]);
        assert!(
            out.wait.is_some_and(|w| w > ALARM_RETRY / 2),
            "{:?}",
            out.wait
        );
        for _ in 0..20 {
            advance(PAST_BOUNDARY);
            rig.frame(&[]);
        }
        assert_eq!(rig.power.as_ref().unwrap().alarm_tries, 2);
        advance(ALARM_RETRY);
        rig.frame(&[]);
        assert_eq!(rig.power.as_ref().unwrap().alarm_tries, 3);
        assert_eq!(rig.power.as_ref().unwrap().slept, 0);
    }

    #[test]
    fn waking_at_wake_time_clears_the_sleep_state() {
        let mut rig = Rig::new(Some(FakePower::default()));
        rig.app.state.settings.sleep_enabled = true;
        rig.app.state.settings.sleep_min = 23 * 60;
        rig.app.state.settings.wake_min = 5 * 60;
        set_wall_hm(22, 59);
        rig.frame(&[]);
        set_wall_hm(23, 1);
        rig.frame(&[]);
        rig.frame(&[Event::Pause]);
        // The alarm fires at 05:00: Start + Resume in wake hours.
        advance(Duration::from_secs(6 * 3600));
        set_wall_hm(5, 0);
        rig.frame(&[Event::Start, Event::Resume]);
        let p = rig.power.as_ref().unwrap();
        assert!(p.woken >= 1, "the wake lock lit the screen");
        // In front during wake hours: no new alarm, no sleep.
        advance(Duration::from_secs(60));
        set_wall_hm(5, 1);
        rig.frame(&[]);
        assert_eq!(rig.power.as_ref().unwrap().slept, 1);
    }

    #[test]
    fn a_debug_override_is_never_saved() {
        let mut rig = Rig::new(Some(FakePower::default()));
        rig.app.state.settings.sleep_enabled = true;
        rig.app.state.settings.sleep_min = 23 * 60;
        rig.app.state.settings.wake_min = 5 * 60;
        set_wall_hm(12, 0);
        rig.frame(&[]);
        // A test schedule lands over the saved one.
        rig.ov.sleep = Some(13 * 60);
        rig.ov.wake = Some(13 * 60 + 5);
        rig.frame(&[]);
        assert_eq!(rig.app.state.settings.sleep_min, 13 * 60);
        // Nudge a setting through the open menu so a save happens under
        // the override: the saved sleep times are the user's own.
        rig.tap(100.0, 100.0);
        rig.app.state.settings.interval_secs = 30.0;
        rig.frame(&[]);
        advance(SAVE_DEBOUNCE);
        let out = rig.frame(&[]);
        let sleep = out.effects.iter().find_map(|e| match e {
            Effect::SaveSettings { sleep, .. } => Some(*sleep),
            _ => None,
        });
        assert_eq!(
            sleep,
            Some(Schedule {
                enabled: true,
                sleep_min: 23 * 60,
                wake_min: 5 * 60,
            })
        );
        // Cleared again: the user's schedule comes back unharmed.
        rig.ov = Overrides::default();
        rig.frame(&[]);
        assert_eq!(rig.app.state.settings.sleep_min, 23 * 60);
        assert_eq!(rig.app.state.settings.wake_min, 5 * 60);
    }

    #[test]
    fn a_time_changed_under_a_debug_override_is_kept() {
        let mut rig = Rig::new(Some(FakePower::default()));
        rig.app.state.settings.sleep_enabled = true;
        rig.app.state.settings.sleep_min = 23 * 60;
        rig.app.state.settings.wake_min = 5 * 60;
        set_wall_hm(12, 0);
        rig.frame(&[]);
        rig.ov.sleep = Some(13 * 60);
        rig.frame(&[]);
        // The user moves the sleep time in Settings while the test runs.
        rig.tap(100.0, 100.0);
        rig.app.state.settings.sleep_min = 22 * 60;
        rig.frame(&[]);
        advance(SAVE_DEBOUNCE);
        let out = rig.frame(&[]);
        let sleep = out.effects.iter().find_map(|e| match e {
            Effect::SaveSettings { sleep, .. } => Some(*sleep),
            _ => None,
        });
        assert_eq!(
            sleep,
            Some(Schedule {
                enabled: true,
                sleep_min: 22 * 60,
                wake_min: 5 * 60,
            })
        );
        // The override's end keeps the edit, not the time before it.
        rig.ov = Overrides::default();
        rig.frame(&[]);
        assert_eq!(rig.app.state.settings.sleep_min, 22 * 60);
    }

    /// Settings → Connectivity → Wi-Fi networks, on a rig with a network
    /// worker.
    fn networks_open(rig: &mut Rig) {
        rig.net = Some(FakeNet::new());
        rig.tap(100.0, 100.0);
        rig.app.state.screen = frame_ui::Screen::Settings;
        rig.app.state.section = frame_ui::Section::Connectivity;
        rig.app.state.sub = frame_ui::Sub::Networks;
        rig.redraw();
    }

    fn sent(rig: &Rig) -> Vec<String> {
        rig.net.as_ref().unwrap().sent.borrow().clone()
    }

    fn scans(rig: &Rig) -> usize {
        sent(rig).iter().filter(|s| *s == "Scan").count()
    }

    #[test]
    fn connectivity_sits_between_sleep_and_server() {
        let all = frame_ui::Section::ALL;
        let at = |s| all.iter().position(|x| *x == s).unwrap();
        assert_eq!(
            at(frame_ui::Section::Connectivity),
            at(frame_ui::Section::Sleep) + 1
        );
        assert_eq!(all.last(), Some(&frame_ui::Section::Server));
    }

    #[test]
    fn the_network_list_scans_when_it_opens_then_every_so_often() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        assert_eq!(scans(&rig), 1, "a scan as the list opens");
        // Kept open by a touch now and then.
        advance(WIFI_RESCAN - Duration::from_secs(1));
        rig.app.last_input = clock::now();
        rig.frame(&[]);
        assert_eq!(scans(&rig), 1);
        advance(Duration::from_secs(1));
        let out = rig.frame(&[]);
        assert_eq!(scans(&rig), 2, "and again once it's due");
        assert!(out.wait.unwrap() <= WIFI_RESCAN);
        // Not while one is running.
        rig.net.as_ref().unwrap().update(|n| {
            n.wifi.as_mut().unwrap().scanning = true;
        });
        advance(WIFI_RESCAN);
        rig.app.last_input = clock::now();
        rig.frame(&[]);
        assert_eq!(scans(&rig), 2);
    }

    #[test]
    fn leaving_the_list_and_coming_back_scans_at_once() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        rig.app.state.sub = frame_ui::Sub::None;
        rig.redraw();
        rig.app.state.sub = frame_ui::Sub::Networks;
        rig.redraw();
        assert_eq!(scans(&rig), 2);
    }

    #[test]
    fn the_pages_commands_reach_the_network_worker() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        rig.app
            .state
            .actions
            .net
            .push(NetCommand::Forget(crate::network::Ssid::new("Home")));
        rig.redraw();
        assert!(
            sent(&rig).contains(&"Forget(\"Home\")".to_string()),
            "{:?}",
            sent(&rig)
        );
    }

    #[test]
    fn the_snapshot_follows_the_workers_version() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        let enabled = |rig: &Rig| {
            rig.app
                .state
                .network
                .as_ref()
                .unwrap()
                .wifi
                .as_ref()
                .unwrap()
                .enabled
        };
        assert!(enabled(&rig));
        rig.net.as_ref().unwrap().update(|n| {
            n.wifi.as_mut().unwrap().enabled = false;
        });
        rig.frame(&[]);
        assert!(!enabled(&rig));
    }

    #[test]
    fn a_host_without_a_network_worker_has_no_snapshot() {
        let mut rig = Rig::new(None);
        rig.tap(100.0, 100.0);
        assert!(rig.app.overlay_open());
        assert!(rig.app.state.network.is_none());
    }

    #[test]
    fn a_join_under_way_keeps_the_menu_open() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        let join = |stage| {
            move |n: &mut crate::network::NetSnapshot| {
                n.wifi.as_mut().unwrap().join = Some(crate::network::Join {
                    ssid: crate::network::Ssid::new("Home IoT"),
                    stage,
                });
            }
        };
        rig.net
            .as_ref()
            .unwrap()
            .update(join(crate::network::JoinStage::Connecting));
        rig.frame(&[]);
        advance(AUTO_DISMISS + Duration::from_secs(1));
        rig.frame(&[]);
        assert!(rig.app.overlay_open(), "open while the join runs");
        rig.net
            .as_ref()
            .unwrap()
            .update(join(crate::network::JoinStage::Joined { remembered: true }));
        rig.frame(&[]);
        advance(AUTO_DISMISS + Duration::from_secs(1));
        rig.frame(&[]);
        assert!(!rig.app.overlay_open(), "the usual timeout after it");
    }

    #[test]
    fn a_scan_filling_the_open_list_keeps_the_menu_open() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        rig.net.as_ref().unwrap().update(|n| {
            n.wifi.as_mut().unwrap().scanning = true;
        });
        rig.frame(&[]);
        advance(AUTO_DISMISS + Duration::from_secs(1));
        rig.frame(&[]);
        assert!(rig.app.overlay_open(), "open while the list fills in");
        rig.net.as_ref().unwrap().update(|n| {
            n.wifi.as_mut().unwrap().scanning = false;
        });
        rig.frame(&[]);
        advance(AUTO_DISMISS + Duration::from_secs(1));
        rig.frame(&[]);
        assert!(!rig.app.overlay_open(), "the usual timeout after it");
    }

    #[test]
    fn escape_from_a_join_steps_back_to_the_list() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        rig.app.state.sub = frame_ui::Sub::Join;
        rig.redraw();
        rig.key(egui::Key::Escape);
        assert_eq!(rig.app.state.sub, frame_ui::Sub::Networks);
        rig.key(egui::Key::Escape);
        assert_eq!(rig.app.state.sub, frame_ui::Sub::None);
        assert_eq!(rig.app.state.screen, frame_ui::Screen::Settings);
    }

    /// A key, then the passes a host runs at once after it.
    fn key_settled(rig: &mut Rig, key: egui::Key) {
        let mut out = rig.key(key);
        let mut passes = 0;
        while out.wait == Some(Duration::ZERO) && passes < 5 {
            out = rig.frame(&[]);
            passes += 1;
        }
    }

    #[test]
    fn a_network_joined_by_key_focuses_its_password_field() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        // In on the first nearby row, Home IoT (WPA2).
        key_settled(&mut rig, egui::Key::Tab);
        key_settled(&mut rig, egui::Key::Enter);
        assert_eq!(rig.app.state.sub, frame_ui::Sub::Join);
        assert!(
            rig.app.ctx.text_edit_focused(),
            "the password has the focus"
        );
        rig.frame(&[Event::Text("correct horse".into())]);
        assert_eq!(rig.app.state.join.key, "correct horse");
        key_settled(&mut rig, egui::Key::Enter);
        assert!(
            sent(&rig)
                .iter()
                .any(|s| s.starts_with("Join(\"Home IoT\"")),
            "Enter in the field joins: {:?}",
            sent(&rig)
        );
    }

    #[test]
    fn enter_in_a_password_too_short_to_join_keeps_the_focus() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        key_settled(&mut rig, egui::Key::Tab);
        key_settled(&mut rig, egui::Key::Enter);
        assert_eq!(rig.app.state.sub, frame_ui::Sub::Join);
        rig.frame(&[Event::Text("short".into())]);
        key_settled(&mut rig, egui::Key::Enter);
        assert!(sent(&rig).iter().all(|s| !s.starts_with("Join(")));
        assert!(
            rig.app.ctx.text_edit_focused(),
            "the password keeps the focus"
        );
        rig.frame(&[Event::Text("-and-more".into())]);
        assert_eq!(rig.app.state.join.key, "short-and-more");
    }

    #[test]
    fn a_networks_page_opened_by_key_never_focuses_forget_first() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        // The connected network's Forget is drawn first, but a destructive
        // button never takes the first focus: the first nearby row does.
        key_settled(&mut rig, egui::Key::Tab);
        key_settled(&mut rig, egui::Key::Enter);
        assert_ne!(rig.app.state.dialog, frame_ui::Dialog::Forget);
        assert_eq!(rig.app.state.sub, frame_ui::Sub::Join);
        assert_eq!(rig.app.state.join.ssid.show(), "Home IoT");
    }

    #[test]
    fn a_hidden_network_opened_by_key_focuses_its_name_field() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        key_settled(&mut rig, egui::Key::Tab);
        // Down to the list's last row, Add a hidden network.
        for _ in 0..20 {
            let was = rig.focused();
            key_settled(&mut rig, egui::Key::ArrowDown);
            if rig.focused() == was {
                break;
            }
        }
        key_settled(&mut rig, egui::Key::Enter);
        assert_eq!(rig.app.state.sub, frame_ui::Sub::Join);
        assert!(rig.app.ctx.text_edit_focused(), "the name has the focus");
        rig.frame(&[Event::Text("Attic".into())]);
        assert_eq!(rig.app.state.join.name, "Attic");
    }

    #[test]
    fn a_sub_pages_arrow_goes_up_one_and_the_top_bars_leaves_settings() {
        let mut rig = Rig::new(None);
        networks_open(&mut rig);
        // The arrow before the page title, on the list icons' column.
        rig.tap(412.0, 104.0);
        assert_eq!(rig.app.state.sub, frame_ui::Sub::None);
        assert_eq!(rig.app.state.screen, frame_ui::Screen::Settings);
        rig.app.state.sub = frame_ui::Sub::Networks;
        rig.redraw();
        // The top bar's "Settings" arrow, from a sub page too.
        rig.tap(40.0, 32.0);
        assert_eq!(rig.app.state.screen, frame_ui::Screen::Menu);
    }
}
