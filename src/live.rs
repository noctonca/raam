//! The live slideshow, a bare `raam`: the product on the desktop, as the
//! frame runs it (docs/ARCHITECTURE.md "Hosts"). raam-core's App
//! controller, `Pipeline<NoVideo>`, clock overlay and egui chrome; the
//! engine's library, fetch and weather workers over SQLite in the app-data
//! dir and a local photos folder, with Immich from settings. Each pass is
//! the Android host's loop body (hosts/android/src/lib.rs): feed the
//! controller its events, run its effects, draw around it, swap. A pass
//! runs when the controller's wait runs out, on input, or when an engine
//! worker wakes the loop through the event-loop proxy.
//!
//! The window is the frame's screen in device pixels, since the controller
//! runs egui at one pixel per point: a retina window is half size, as
//! `--exact` makes it for the presets. With `--fullscreen` it is the whole
//! monitor, as on a Linux frame. The mouse is a finger: a press, a drag
//! and a release are one touch, fed as the Android host feeds one. A
//! touchscreen's first finger is fed the same way.
use crate::platform::{self, EnvSwitches};
use crate::{Args, Gl, Step, save_png};
use glutin::surface::GlSurface;
use raam_core::app::{App, Deps, Effect, Event, Inputs, Overrides, Stage, Touch};
use raam_core::frame_ui::AppState;
use raam_core::gl::glDisableVertexAttribArray;
use raam_core::overlay::ClockOverlay;
use raam_core::painter::Painter;
use raam_core::seams::{DebugSwitches, MediaProbe};
use raam_core::slideshow::{Pipeline, SlideshowSettings};
use raam_core::video::NoVideo;
use raam_core::{clock, collage, switches};
use raam_engine::{db, fetch, immich, library, weather};
use raam_model::limits::LARGEST_LAYOUT;
use raam_model::{ClipInfo, FitBackground, GapColour, ScaleMode, SourceKind};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

/// The frame's density: pixels_per_point 1, 1 dp = 1 px.
const DENSITY_DPI: u32 = 160;

/// A scripted tap is held down this long, as a finger's is.
const TAP_HOLD: Duration = Duration::from_millis(80);

/// Before a scripted step: time for the menu or a settings page to open
/// and settle.
const SCRIPT_STEP: Duration = Duration::from_millis(1500);

/// A scripted run whose first collage hasn't come by now (an empty
/// folder, say) fails rather than wait forever.
const SCRIPT_START_LIMIT: Duration = Duration::from_secs(60);

/// A full-screen window's size holds still this long before the pipeline
/// is made for it: a window system may carry out the request after the
/// window opens, resizing it in steps.
const FULLSCREEN_SETTLE: Duration = Duration::from_millis(500);

/// An engine worker woke the loop.
pub struct Wake;

/// The engine's threads wake the winit loop through its proxy.
struct ProxyWaker(EventLoopProxy<Wake>);

impl raam_core::seams::Waker for ProxyWaker {
    fn wake(&self) {
        // Fails only once the loop has gone, when nothing needs waking.
        let _ = self.0.send_event(Wake);
    }
}

const NO_PLAYER: &str = "this host has no video player";

/// No decoder here: the engine keeps every clip out before it fetches one
/// (`MediaProbe::no_player`), and a clip in the photos folder is skipped
/// at the scan.
struct NoPlayer;

impl MediaProbe for NoPlayer {
    fn probe(&self, _path: &str) -> Result<ClipInfo, String> {
        Err(NO_PLAYER.into())
    }

    fn unplayable(&self, _info: &ClipInfo) -> Option<String> {
        Some(NO_PLAYER.into())
    }

    fn no_player(&self) -> Option<String> {
        Some(NO_PLAYER.into())
    }
}

/// What exists once the window does.
struct Running {
    gl: Gl,
    pipeline: Pipeline<NoVideo>,
    painter: Painter,
    overlay: ClockOverlay,
    fetch: Arc<fetch::FetchShared>,
    weather: Arc<weather::WeatherShared>,
    screen: (i32, i32),
}

pub struct Live {
    args: Args,
    switches: Arc<EnvSwitches>,
    host: raam_engine::Host,
    lib: Arc<library::Library>,
    /// The settings rows the DB had, so a saved collage max beats the
    /// screen's default.
    saved_keys: Vec<String>,
    /// The saved Fill/Fit choices, for the pipeline once it exists.
    overrides: Option<HashMap<String, ScaleMode>>,
    controller: App,
    /// A full-screen window whose size hasn't held still yet, and when it
    /// will have if nothing resizes it before.
    opening: Option<(Gl, Instant)>,
    run: Option<Running>,
    events: Vec<Event>,
    next_run: Option<Instant>,
    pointer: egui::Pos2,
    down: bool,
    /// The touchscreen's finger being fed, while one is down.
    finger: Option<u64>,
    /// --click/--press/--screenshot: they start once the first collage is
    /// up and still, then each step waits for the one before to play out.
    script_started: bool,
    script_at: Option<Instant>,
    script_step: usize,
    /// The scripted tap under way is down.
    tap_down: bool,
    /// Save this pass's frame here (F12, or the script's last step).
    shot: Option<PathBuf>,
    frames: u32,
    passes: u32,
    last_log: Duration,
}

impl Live {
    /// Opens the database and starts the library, before any window.
    pub fn new(args: Args, proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        // The clock and the fault flag before anything can read either
        // (raam-core clock.rs, switches.rs).
        clock::set_source(platform::clock_source());
        let switches = Arc::new(EnvSwitches::default());
        switches::set_fail(&switches.get("debug.video.fail"));
        let host = raam_engine::Host {
            waker: Arc::new(ProxyWaker(proxy)),
            switches: switches.clone(),
            probe: Arc::new(NoPlayer),
            // Nothing to grant here: a folder it can't read shows in the
            // scan's note.
            grant_storage: Arc::new(|| true),
        };
        let files_dir = match &args.data {
            Some(d) => d.clone(),
            None => platform::data_dir()?,
        };
        std::fs::create_dir_all(&files_dir)
            .map_err(|e| format!("data dir {}: {e}", files_dir.display()))?;
        let photos = match &args.photos {
            Some(p) => std::path::absolute(p).map_err(|e| format!("--photos: {e}"))?,
            None => platform::photos_dir()?,
        };
        let paths = raam_engine::Paths {
            files_dir: files_dir.clone(),
            local_dir_default: photos.to_string_lossy().into_owned(),
            // In the data dir rather than next to the folder, so a folder
            // picked with --photos (a checkout's samples) stays as it was.
            curation_export: files_dir.join("curation.json"),
        };
        let t = clock::now();
        let database = db::open(&files_dir.join("raam.db"), &paths.local_dir_default)?;
        let mut state = AppState::new("", "");
        let (saved_keys, overrides) = {
            let conn = database.lock().unwrap();
            if args.photos.is_some() {
                db::set_local_dir(&conn, &paths.local_dir_default)
                    .map_err(|e| format!("db: setting the photos folder: {e}"))?;
            }
            state.settings.cache_cap_mb = library::default_cap_mb(&files_dir);
            (
                db::load_settings(&conn, &mut state.settings),
                db::load_overrides(&conn),
            )
        };
        log::info!(
            "db: loaded {} saved settings and {} Fill/Fit overrides in {:?}",
            saved_keys.len(),
            overrides.len(),
            clock::elapsed(t)
        );
        let lib = library::spawn(database, paths, state.settings.cache_cap_mb, host.clone());
        state.settings.immich_enabled = lib.enabled(SourceKind::Immich);
        state.settings.local_enabled = lib.enabled(SourceKind::Local);
        // No power here, so no schedule runs: the menu mustn't say the
        // frame sleeps at 23:00 (the web demo does the same).
        state.settings.sleep_enabled = false;
        // The controller takes the loaded state; its saved-settings
        // snapshots start from it, so nothing writes at boot.
        let controller = App::new(state, || None);
        let now = clock::now();
        let scripted = !args.script.is_empty() || args.screenshot.is_some();
        Ok(Self {
            args,
            switches,
            host,
            lib,
            saved_keys,
            overrides: Some(overrides),
            controller,
            opening: None,
            run: None,
            // The activity's start, as Android reports it at boot.
            events: vec![Event::Start, Event::Resume],
            next_run: Some(Instant::now()),
            pointer: egui::Pos2::ZERO,
            down: false,
            finger: None,
            script_started: false,
            // The start limit, so the loop wakes to check it.
            script_at: scripted.then(|| Instant::now() + SCRIPT_START_LIMIT),
            script_step: 0,
            tap_down: false,
            shot: None,
            frames: 0,
            passes: 0,
            last_log: now,
        })
    }

    /// The window. A full-screen one waits in `opening` until its size
    /// holds still (`settle`); any other starts at once.
    fn open(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        if !self.args.fullscreen {
            let gl = crate::create_gl(el, self.args.size, true, true, None)?;
            // The pipeline's targets are made once for the screen, which on
            // a frame never changes size.
            gl.window.set_resizable(false);
            return self.start(gl, self.args.size);
        }
        // Wayland names no primary monitor.
        let m = el
            .primary_monitor()
            .or_else(|| el.available_monitors().next())
            .ok_or("--fullscreen: no monitor")?;
        let s = m.size();
        log::info!(
            "full screen on {} ({}x{})",
            m.name().unwrap_or_else(|| "the monitor".into()),
            s.width,
            s.height
        );
        let gl = crate::create_gl(el, [s.width, s.height], true, true, Some(m))?;
        // A frame has no mouse; one plugged in still works, unseen.
        gl.window.set_cursor_visible(false);
        self.opening = Some((gl, Instant::now() + FULLSCREEN_SETTLE));
        Ok(())
    }

    /// A full-screen window whose size has held still starts, at that
    /// size.
    fn settle(&mut self) -> Result<(), String> {
        let Some((gl, at)) = self.opening.take() else {
            return Ok(());
        };
        if Instant::now() < at {
            self.opening = Some((gl, at));
            return Ok(());
        }
        let s = gl.window.inner_size();
        log::info!(
            "full screen: the window holds still at {}x{}",
            s.width,
            s.height
        );
        self.start(gl, [s.width, s.height])
    }

    /// Everything that needs the window's GL context or the screen's size.
    fn start(&mut self, gl: Gl, screen: [u32; 2]) -> Result<(), String> {
        let [w, h] = screen.map(|v| v as i32);
        let size = gl.window.inner_size();
        if (size.width, size.height) != (w as u32, h as u32) {
            log::warn!(
                "the window came up {}x{}, not {w}x{h}; drawing {w}x{h}",
                size.width,
                size.height
            );
        }
        let default_max = collage::screen_default_max(w, h, DENSITY_DPI);
        log::info!(
            "screen {w}x{h} @ {DENSITY_DPI}dpi = {:.2}\" diagonal -> default collage max {default_max}",
            collage::screen_diagonal_inches(w, h, DENSITY_DPI),
        );
        let s = &mut self.controller.state.settings;
        if !self.saved_keys.iter().any(|k| k == "collage.max") {
            s.collage_max = default_max.min(LARGEST_LAYOUT);
        }
        s.screen_default_max = default_max;
        let margin = ((2.0 * DENSITY_DPI as f32 / 160.0) + 0.5) as i32;
        let fetch = fetch::spawn(
            self.host.clone(),
            s.collage_max,
            fetch::Screen {
                width: w,
                height: h,
                margin: margin.max(1),
            },
            self.lib.clone(),
        );
        let mut painter = unsafe { Painter::new() };
        // The theme's text mode is the shader boost
        // (`theme::Options::default`).
        painter.text_boost = true;
        let overlay = unsafe { ClockOverlay::new() };
        let weather = weather::spawn(self.host.waker.clone());
        let settings = SlideshowSettings {
            dwell: Duration::from_secs(10),
            transition: None,
            ken_burns: true,
            fill_by_default: true,
            fit_background: FitBackground::Blurred,
            gap_colour: GapColour::Black,
            video_playback: s.video_playback,
            video_sound: false,
            video_volume: s.video_volume,
        };
        let mut pipeline = unsafe { Pipeline::new(w, h, DENSITY_DPI, settings, NoVideo, || None) };
        if let Some(o) = self.overrides.take() {
            pipeline.set_overrides(o);
        }
        log::info!("pipeline + painter ready, {w}x{h}");
        self.run = Some(Running {
            gl,
            pipeline,
            painter,
            overlay,
            fetch,
            weather,
            screen: (w, h),
        });
        self.next_run = Some(Instant::now());
        Ok(())
    }

    /// The window's new size. The GL surface follows it (Wayland's EGL
    /// surface doesn't on its own); a full-screen window still settling
    /// waits again; the pipeline keeps the size it was made for.
    fn resized(&mut self, size: PhysicalSize<u32>) {
        let gl = match (&self.run, &mut self.opening) {
            (Some(run), _) => &run.gl,
            (None, Some((gl, at))) => {
                *at = Instant::now() + FULLSCREEN_SETTLE;
                &*gl
            }
            (None, None) => return,
        };
        if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
            gl.surface.resize(&gl.context, w, h);
        }
        if let Some(run) = &self.run {
            let (w, h) = run.screen;
            if (size.width, size.height) != (w as u32, h as u32) {
                log::warn!(
                    "the window is now {}x{}; still drawing {w}x{h}",
                    size.width,
                    size.height
                );
            }
        }
    }

    /// A touchscreen's touch, fed only while it is the first finger down,
    /// as the Android host reads only a motion's first pointer.
    fn finger(&mut self, t: winit::event::Touch) {
        use winit::event::TouchPhase as P;
        match self.finger {
            None if t.phase == P::Started => self.finger = Some(t.id),
            Some(id) if id == t.id => {}
            _ => return,
        }
        let phase = match t.phase {
            P::Started => egui::TouchPhase::Start,
            P::Moved => egui::TouchPhase::Move,
            P::Ended => egui::TouchPhase::End,
            P::Cancelled => egui::TouchPhase::Cancel,
        };
        if matches!(t.phase, P::Ended | P::Cancelled) {
            self.finger = None;
        }
        let pos = egui::pos2(t.location.x as f32, t.location.y as f32);
        self.touch(phase, pos);
    }

    fn touch(&mut self, phase: egui::TouchPhase, pos: egui::Pos2) {
        self.events.push(Event::Touch(Touch {
            phase,
            pos,
            device_id: 0,
            touch_id: 0,
            force: 0.5,
        }));
        self.next_run = Some(Instant::now());
    }

    /// Taps, switches or a screenshot were scripted.
    fn scripted(&self) -> bool {
        !self.args.script.is_empty() || self.args.screenshot.is_some()
    }

    /// The next scripted step (--click, --press, --set, --wait, then
    /// --screenshot), when it's due.
    fn script(&mut self) {
        if !self.scripted() {
            return;
        }
        let now = Instant::now();
        if !self.script_started {
            if let Some(run) = &self.run
                && run.pipeline.has_slide()
                && !run.pipeline.is_transitioning()
            {
                self.script_started = true;
                self.script_at = Some(now + SCRIPT_STEP);
            } else if self.script_at.is_some_and(|limit| now >= limit) {
                log::error!(
                    "script: no collage within {} s, giving up",
                    SCRIPT_START_LIMIT.as_secs()
                );
                std::process::exit(1);
            }
            return;
        }
        if self.script_at.is_none_or(|at| now < at) {
            return;
        }
        let Some(step) = self.args.script.get(self.script_step).cloned() else {
            self.shot = self.args.screenshot.clone();
            self.script_at = None;
            return;
        };
        let mut next = SCRIPT_STEP;
        match step {
            Step::Tap(pos, _) if !self.tap_down => {
                log::info!("script: tap at {pos:?}");
                self.touch(egui::TouchPhase::Start, pos);
                self.tap_down = true;
                self.script_at = Some(now + TAP_HOLD);
                return;
            }
            Step::Tap(pos, release) => {
                self.tap_down = false;
                if release {
                    self.touch(egui::TouchPhase::End, pos);
                }
            }
            Step::Set(name, value) => {
                log::info!("script: {name}={value:?}");
                self.switches.set(&name, &value);
            }
            Step::Wait(d) => next += d,
        }
        self.script_step += 1;
        self.script_at = Some(now + next);
    }

    /// One pass of the Android host's loop.
    fn pass(&mut self, el: &ActiveEventLoop) {
        // The debug switches, snapshotted once per pass.
        switches::set_fail(&self.switches.get("debug.video.fail"));
        self.script();
        let Some(run) = self.run.as_mut() else {
            return;
        };
        self.passes += 1;
        let events = std::mem::take(&mut self.events);
        let out = self.controller.frame(
            &events,
            &Inputs {
                has_surface: true,
                screen: run.screen,
                wakelock_allowed: false,
                overrides: Overrides::default(),
            },
            &mut Deps {
                stage: Some(Stage {
                    slideshow: &mut run.pipeline,
                    source: run.fetch.as_ref(),
                    library: self.lib.as_ref(),
                    weather: Some(run.weather.as_ref()),
                }),
                // No power: no sleep schedule.
                power: None,
            },
        );
        run_effects(out.effects, &self.lib, &run.fetch, &run.weather);
        self.next_run = out.wait.and_then(|w| Instant::now().checked_add(w));
        if out.skip_draw {
            return;
        }
        for (asset, why) in run.pipeline.video.take_unplayable() {
            self.lib.send(library::Cmd::SetUnplayable(asset, why));
        }
        let (w, h) = run.screen;
        // The full-screen settings cover everything: skip the slideshow
        // and overlay draws under them (the controller's opaque lever).
        if !out.chrome_opaque {
            run.pipeline.draw_frame();
            unsafe {
                for i in 0..8 {
                    glDisableVertexAttribArray(i);
                }
            }
        }
        if let Some(r) = out.overlay
            && run.overlay.update(r.style, r.corner, &r.content, w, h)
        {
            log::info!(
                "overlay rebuilt ({}, {}): {:?}",
                r.style.label(),
                r.corner.label(),
                r.content
            );
        }
        if !out.chrome_opaque {
            run.overlay.draw(w, h);
        }
        if let Some(mut e) = out.egui {
            for (id, deltas) in &e.textures_delta.set {
                for delta in deltas {
                    run.painter.set_texture(*id, delta);
                }
            }
            run.painter.upload(&e.primitives, 1.0, w, h);
            for id in &e.textures_delta.free {
                run.painter.free_texture(*id);
            }
            // Applied above; epaint asserts on dropping an uncleared delta.
            e.textures_delta.clear();
        }
        if out.draw_egui {
            run.painter.draw(1.0, w, h);
        }
        if let Some(path) = self.shot.take() {
            match save_png(&path, w, h) {
                Ok(()) => log::info!("saved {} ({w}x{h})", path.display()),
                Err(e) => log::error!("screenshot {}: {e}", path.display()),
            }
            if self.args.screenshot.as_ref() == Some(&path) {
                el.exit();
            }
        }
        if let Err(e) = run.gl.surface.swap_buffers(&run.gl.context) {
            log::error!("swap_buffers: {e}");
        }
        self.frames += 1;
        let since = clock::elapsed(self.last_log);
        if since >= Duration::from_secs(10) {
            log::info!(
                "stats fps={:.1} passes={} overlay={} layout={}",
                self.frames as f64 / since.as_secs_f64(),
                self.passes,
                self.controller.overlay_open(),
                run.pipeline.shown_layout(),
            );
            self.frames = 0;
            self.passes = 0;
            self.last_log = clock::now();
        }
    }

    fn key(&mut self, key: &Key) {
        match key {
            // The GPU-failure injection, on and off without a restart.
            Key::Named(NamedKey::F5) => {
                let on = self.switches.get("debug.video.fail") != "rt";
                self.switches
                    .set("debug.video.fail", if on { "rt" } else { "" });
                log::info!("debug.video.fail {}", if on { "rt" } else { "cleared" });
                self.next_run = Some(Instant::now());
            }
            Key::Named(NamedKey::F12) => {
                let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shots");
                let _ = std::fs::create_dir_all(&dir);
                let secs = clock::wall().as_secs();
                self.shot = Some(dir.join(format!("live-{secs}.png")));
                self.next_run = Some(Instant::now());
            }
            _ => {}
        }
    }
}

/// The controller's effects, mapped onto the engine as the Android host
/// maps them.
fn run_effects(
    effects: Vec<Effect>,
    lib: &library::Library,
    fetch: &fetch::FetchShared,
    weather: &weather::WeatherShared,
) {
    for effect in effects {
        match effect {
            Effect::SaveSettings { rows, sleep } => {
                lib.send(library::Cmd::SaveSettings { rows, sleep })
            }
            Effect::SetScale(key, mode) => lib.send(library::Cmd::SetScale(key, mode)),
            Effect::SetHidden(key, hidden) => lib.send(library::Cmd::SetHidden(key, hidden)),
            Effect::SetSourceEnabled(kind, on) => lib.set_enabled(kind, on),
            Effect::SetServer { url, key } => {
                lib.send(library::Cmd::SetServer(immich::Config { url, key }))
            }
            Effect::ExportCuration => lib.send(library::Cmd::ExportCuration),
            Effect::SelectAlbum(album, on) => lib.send(library::Cmd::SelectAlbum(album, on)),
            Effect::SetCap(cap) => lib.send(library::Cmd::SetCap(cap)),
            Effect::ClearCache => lib.send(library::Cmd::ClearCache),
            Effect::Rescan => lib.send(library::Cmd::Rescan),
            Effect::SyncNow => lib.send(library::Cmd::SyncNow),
            Effect::SetMaxGroup(max) => fetch.set_max_group(max),
            // No clip plays here, so there is no sound to turn up.
            Effect::SetMusicVolume(_) => {}
            Effect::SetWeather(on) => weather.set_enabled(on),
        }
    }
}

impl ApplicationHandler<Wake> for Live {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.run.is_none()
            && self.opening.is_none()
            && let Err(e) = self.open(el)
        {
            log::error!("{e}");
            el.exit();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _wake: Wake) {
        self.next_run = Some(Instant::now());
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => self.pass(el),
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = egui::pos2(position.x as f32, position.y as f32);
                if self.down {
                    self.touch(egui::TouchPhase::Move, self.pointer);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    self.down = true;
                    self.touch(egui::TouchPhase::Start, self.pointer);
                }
                ElementState::Released if self.down => {
                    self.down = false;
                    self.touch(egui::TouchPhase::End, self.pointer);
                }
                ElementState::Released => {}
            },
            // X11 sends no button presses for a touch (winit asks for the
            // touches themselves), and Wayland keeps the two apart.
            WindowEvent::Touch(t) => self.finger(t),
            WindowEvent::Resized(size) => self.resized(size),
            // A scripted run draws regardless, or a window stacked behind
            // another would never take its screenshot.
            WindowEvent::Occluded(hidden) if self.scripted() => log::info!(
                "window {} (a scripted run keeps drawing)",
                if hidden { "covered" } else { "uncovered" }
            ),
            // Covered or minimised is the activity leaving the front.
            WindowEvent::Occluded(hidden) => {
                self.events
                    .push(if hidden { Event::Pause } else { Event::Resume });
                self.next_run = Some(Instant::now());
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        repeat: false,
                        ..
                    },
                ..
            } => self.key(&logical_key),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if let Err(e) = self.settle() {
            log::error!("{e}");
            el.exit();
            return;
        }
        let next = match (self.next_run, self.script_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // A full-screen window still settling starts then.
        let settle_at = self.opening.as_ref().map(|(_, at)| *at);
        let until = |t: Option<Instant>| match (t, settle_at) {
            (Some(a), Some(b)) => ControlFlow::WaitUntil(a.min(b)),
            (Some(t), None) | (None, Some(t)) => ControlFlow::WaitUntil(t),
            (None, None) => ControlFlow::Wait,
        };
        match next {
            Some(t) if t <= Instant::now() => {
                if let Some(run) = &self.run {
                    run.gl.window.request_redraw();
                }
                self.next_run = None;
                el.set_control_flow(until(None));
            }
            t => el.set_control_flow(until(t)),
        }
    }
}
