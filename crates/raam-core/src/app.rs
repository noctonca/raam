//! The App controller: the product's behaviour, shared by every host.
//! Events in, effects out: the host feeds lifecycle and touch events plus
//! a few per-pass inputs; the controller routes input (tap-slop, menu
//! open/close, auto-dismiss, undo-hide), runs the egui chrome, applies
//! settings to the slideshow, drives the sleep/wake state machine,
//! debounces settings saves, and computes the next wake deadline.
//!
//! It draws nothing. The host draws around `frame`: the slideshow, then
//! the clock overlay (rebuilt when `FrameOut::overlay` says so), then the
//! egui chrome (`FrameOut::egui` uploaded first when present), then the
//! swap. The outside world enters through the deps traits and leaves as
//! `Effect`s the host maps onto its engine and power plumbing.

use crate::frame_ui::{self, AppState, Status};
use crate::overlay;
use crate::schedule::{self, Schedule};
use crate::seams::Power;
use crate::slideshow::SlideshowSettings;
use crate::source::TileSource;
use crate::{clock, store, theme, weather_icons};
use raam_model::limits::{
    AUTO_DISMISS, DEFAULT_MANUAL_IDLE, MAX_EGUI_WAIT, SAVE_DEBOUNCE, TAP_SLOP_PX, UNDO_HIDE,
};
use raam_model::{ClockStyle, Corner, ScaleMode, SourceKind, Stats};
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
}

/// One pointer event, already in egui terms.
pub struct Touch {
    pub phase: egui::TouchPhase,
    pub pos: egui::Pos2,
    pub device_id: u64,
    pub touch_id: u64,
    pub force: f32,
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
    fn shown_layout(&self) -> String;
    fn is_animating(&self) -> bool;
    fn recompose_pending(&self) -> bool;
    fn next_deadline(&self) -> Option<Duration>;
}

/// The library's numbers for the status line and settings panel.
pub trait LibraryInfo {
    /// A change counter and the stats snapshot.
    fn stats(&self) -> (u64, Stats);
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
}

pub struct Deps<'a> {
    pub stage: Option<Stage<'a>>,
    /// No power means no sleep schedule (desktop, web, a failed JNI setup).
    pub power: Option<&'a mut dyn Power>,
}

/// What the host executes after `frame`: engine commands and host-side
/// setters, in order.
pub enum Effect {
    SaveSettings {
        rows: Vec<(&'static str, serde_json::Value)>,
        sleep: (bool, u32, u32),
    },
    SetScale(String, Option<ScaleMode>),
    SetHidden(String, bool),
    SetSourceEnabled(SourceKind, bool),
    /// The server and key apply when the menu closes, not per keystroke.
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
    /// The music stream's volume, set when it changes while sound is on.
    SetMusicVolume(f32),
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
    last_input: Duration,
    last_touch: Duration,
    undo: Option<(String, Duration)>,
    stats_version: u64,
    // Frame-reuse state: egui only runs when it has input, asked for a
    // repaint that is now due, or the status line changed.
    egui_due: Duration,
    egui_uploaded: bool,
    last_status: String,
    // Re-derive the overlay's text only when one of these changes.
    clock_inputs: Option<(u64, u64, ClockStyle, Corner, bool)>,
    weather_status: String,
    // What was last sent for saving, so only changes are written.
    saved_rows: Vec<(&'static str, serde_json::Value)>,
    saved_sleep: (bool, u32, u32),
    saved_server: (String, String),
    saved_cap: u32,
    settings_dirty: Option<Duration>,
    // The schedule as the user set it, kept while a debug override is on.
    schedule_base: (u32, u32),
    // Change-driven effects.
    sent_max_group: usize,
    sent_enabled: (bool, bool),
    music_volume: Option<f32>,
    // Lifecycle and the sleep state machine.
    resumed: bool,
    check_wake: bool,
    asleep_since: Option<Duration>,
    manual_wake: Option<Duration>,
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
            saved_sleep: (
                state.settings.sleep_enabled,
                state.settings.sleep_min,
                state.settings.wake_min,
            ),
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
            last_input: now,
            last_touch: now,
            undo: None,
            stats_version: 0,
            egui_due: now,
            egui_uploaded: false,
            last_status: String::new(),
            clock_inputs: None,
            weather_status: String::from("weather: waiting"),
            settings_dirty: None,
            music_volume: None,
            resumed: false,
            check_wake: false,
            asleep_since: None,
            manual_wake: None,
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
            advance: Duration::ZERO,
            overlay: None,
            egui: None,
            draw_egui: false,
        };

        let mut touches: Vec<&Touch> = Vec::new();
        for e in events {
            match e {
                Event::Start => self.check_wake = true,
                Event::Resume => {
                    self.resumed = true;
                    self.check_wake = true;
                }
                Event::Pause => self.resumed = false,
                Event::Touch(t) => touches.push(t),
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
            // Touches were dropped, not routed, so they don't replay on
            // return.
            out.skip_draw = true;
            out.wait = boundary_wait.map(|w| w + Duration::from_millis(50));
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
        }
        if let Some(since) = self.asleep_since
            && clock::elapsed(since) >= Duration::from_secs(30)
        {
            log::error!(
                "schedule: screen still on 30s after sleeping, treating it as a manual wake"
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
            if (crossed || idle_done)
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
                        p.sleep_screen();
                    }
                    Err(e) => log::error!("schedule: wake alarm failed, staying awake: {e}"),
                }
            }
        }
        self.prev_in_sleep = Some(in_sleep);

        let mut egui_events = Vec::new();
        let mut presses = Vec::new();
        for t in touches {
            self.last_touch = clock::now();
            if !self.overlay_open {
                match t.phase {
                    egui::TouchPhase::Start => self.closed_down = Some(t.pos),
                    egui::TouchPhase::End => {
                        if let Some(d) = self.closed_down.take()
                            && d.distance(t.pos) <= TAP_SLOP_PX
                        {
                            self.overlay_open = true;
                            self.last_input = clock::now();
                            let tile = stage.slideshow.select_at(t.pos.x, t.pos.y);
                            log::info!("overlay opened by tap at {:?} on tile {tile:?}", t.pos);
                        }
                    }
                    egui::TouchPhase::Cancel => self.closed_down = None,
                    egui::TouchPhase::Move => {}
                }
                continue;
            }
            self.last_input = clock::now();
            push_egui_touch(t, &mut self.egui_down, &mut egui_events, &mut presses);
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
            if let Some(v) = want_volume {
                out.effects.push(Effect::SetMusicVolume(v));
            }
            self.music_volume = want_volume;
        }
        stage.slideshow.set_menu_open(self.overlay_open);
        let t = clock::now();
        stage.slideshow.update(stage.source);
        out.advance = clock::elapsed(t);

        // Clock overlay: over the slideshow (transitions included), under
        // the egui chrome. The host rebuilds and draws; the text and its
        // change detection live here.
        let minute = clock::wall().as_secs() / 60;
        let clock_inputs = (
            minute,
            stage.weather.map_or(0, |w| w.version()),
            self.state.settings.clock_style,
            self.state.settings.clock_corner,
            self.state.settings.clock_24h,
        );
        if self.clock_inputs != Some(clock_inputs) {
            self.clock_inputs = Some(clock_inputs);
            let (city, current) = stage.weather.map_or((None, None), |w| w.snapshot());
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
                _ if stage.weather.is_none() => String::new(),
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
            let (version, lib_stats) = stage.library.stats();
            let layout = stage.slideshow.shown_layout();
            self.state.status = Status {
                layout: (layout != "-").then_some(layout),
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
                predicted_dt: 1.0 / 30.0,
                events: egui_events,
                ..Default::default()
            };
            // The screen these presses landed on (run_ui below may flip
            // it, e.g. Back: Settings -> Menu in the same pass).
            let menu_at_input = self.state.screen == frame_ui::Screen::Menu;
            let mut actions = frame_ui::Actions::default();
            let need_run = !self.egui_uploaded
                || !raw_input.events.is_empty()
                || clock::now() >= self.egui_due
                || status != self.last_status
                || version != self.stats_version;
            if need_run {
                self.stats_version = version;
                self.state.library = lib_stats;
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
                let rows = store::settings_rows(&self.state.settings);
                let sleep = (
                    self.state.settings.sleep_enabled,
                    self.state.settings.sleep_min,
                    self.state.settings.wake_min,
                );
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
                    .unwrap_or_else(|| clock::now() + Duration::from_secs(3600));
                out.egui = Some(EguiOut {
                    textures_delta: std::mem::take(&mut full_output.textures_delta),
                    primitives,
                    run,
                    tess,
                });
            }
            egui_delay = self.egui_due.saturating_sub(clock::now());

            let typing = self.ctx.egui_wants_keyboard_input();
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
                // settings page said it would ("Will use http://…").
                if let Some(url) = frame_ui::normalise_url(&self.state.settings.server_url) {
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
            let sleep = (
                self.state.settings.sleep_enabled,
                if self.overrides.sleep.is_some() {
                    self.schedule_base.0
                } else {
                    self.state.settings.sleep_min
                },
                if self.overrides.wake.is_some() {
                    self.schedule_base.1
                } else {
                    self.state.settings.wake_min
                },
            );
            if rows != self.saved_rows || sleep != self.saved_sleep {
                self.saved_rows = rows.clone();
                self.saved_sleep = sleep;
                out.effects.push(Effect::SaveSettings { rows, sleep });
            }
        }

        // A scaling change lands in `update` next pass, so run one more.
        out.wait = if force_redraw
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
                w = min_wait(w, b + Duration::from_millis(50));
            }
            if let Some(m) = self.manual_wake {
                let idle = clock::elapsed(m).min(clock::elapsed(self.last_touch));
                w = min_wait(
                    w,
                    self.manual_idle.saturating_sub(idle) + Duration::from_millis(50),
                );
            }
            if let Some(since) = self.settings_dirty {
                w = min_wait(
                    w,
                    SAVE_DEBOUNCE.saturating_sub(clock::elapsed(since)) + Duration::from_millis(10),
                );
            }
            if self.overlay_open {
                if egui_delay < MAX_EGUI_WAIT {
                    w = min_wait(w, egui_delay);
                }
                if self.undo.is_some() {
                    w = min_wait(w, Duration::from_millis(250));
                }
                if !self.ctx.egui_wants_keyboard_input() {
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
        s.video_volume = ss.video_volume;
        // Hidden time never counts: the controller pauses the clock while
        // there is nothing to draw on, and this only runs while drawing.
        slideshow.set_clock_paused(self.state.paused);
    }
}

fn min_wait(a: Option<Duration>, b: Duration) -> Option<Duration> {
    Some(a.map_or(b, |a| a.min(b)))
}

/// Time to the next wall-clock minute, plus a few ms so the wake lands
/// after the boundary rather than just before it.
fn until_next_minute() -> Duration {
    let now = clock::wall();
    let into = Duration::from_millis((now.as_millis() % 60_000) as u64);
    Duration::from_secs(60) - into + Duration::from_millis(20)
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
    use raam_model::Plan;
    use std::cell::Cell;

    #[derive(Default)]
    struct FakeShow {
        settings: Option<SlideshowSettings>,
        selected: Cell<u32>,
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
                    video_volume: 0.5,
                }),
                selected: Cell::new(0),
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
        fn request_next(&mut self) {}
        fn request_prev(&mut self, _source: &dyn TileSource) {}
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
        fn shown_layout(&self) -> String {
            "1 (single)".into()
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

    struct FakeLib;
    impl LibraryInfo for FakeLib {
        fn stats(&self) -> (u64, Stats) {
            (1, Stats::default())
        }
        fn online(&self) -> bool {
            true
        }
    }

    struct FakeWeather;
    impl WeatherInfo for FakeWeather {
        fn version(&self) -> u64 {
            1
        }
        fn snapshot(&self) -> (Option<String>, Option<WeatherNow>) {
            (None, None)
        }
    }

    #[derive(Default)]
    struct FakePower {
        alarms: Vec<i64>,
        slept: u32,
        woken: u32,
        fail_alarm: bool,
    }

    impl Power for FakePower {
        fn set_wake_alarm(&mut self, epoch_ms: i64) -> Result<(), String> {
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
                lib: FakeLib,
                weather: Some(FakeWeather),
                power,
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
    }

    #[test]
    fn a_host_without_weather_leaves_it_out_of_the_status_line() {
        // The status line is published while the menu is up.
        let mut rig = Rig::new(None);
        rig.tap(640.0, 400.0);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.app.state.status.weather, "locating...");
        let mut rig = Rig::new(None);
        rig.weather = None;
        rig.tap(640.0, 400.0);
        assert!(rig.app.overlay_open());
        assert_eq!(rig.app.state.status.weather, "");
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
        assert_eq!(sleep, Some((true, 23 * 60, 5 * 60)));
        // Cleared again: the user's schedule comes back unharmed.
        rig.ov = Overrides::default();
        rig.frame(&[]);
        assert_eq!(rig.app.state.settings.sleep_min, 23 * 60);
        assert_eq!(rig.app.state.settings.wake_min, 5 * 60);
    }
}
