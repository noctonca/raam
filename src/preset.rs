//! The preset host (`--page`): the widget gallery and every frame_ui
//! screen by name, drawn by the core's own GLES2 painter over a stand-in
//! for the slideshow (backdrop.rs), so theme, kit and screen changes are
//! iterated and QA'd on the desktop before the frame. Input is egui's own
//! from winit, plus the scripted taps, holds and scroll of the flags in
//! main.rs, and `--screenshot` saves once egui is idle.
//!
//! A shot run (`--screenshot`, `--hash`) is hermetic: egui's clock is the
//! pass count, passes run back to back, and nothing from the machine
//! reaches egui but the window's size, so the golden suite's shots are
//! the same whatever the load, the display's refresh rate or scale, or
//! where the mouse rests. The web host's `shot=1` runs the same way.
use crate::{Args, Gl, PageArg, backdrop, golden, read_pixels};
use egui::Theme;
use glutin::surface::GlSurface;
use raam_core::frame_ui::{self, SHOT_PASS, SHOT_SETTLED};
use raam_core::gallery;
use raam_core::gl::*;
use raam_core::painter::Painter;
use raam_core::theme::{self, Options, TextMode};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

pub struct Preset {
    args: Args,
    ctx: egui::Context,
    schemes: theme::Schemes,
    opts: Options,
    ppp: f32,
    gallery: gallery::Gallery,
    /// The frame UI, when --page named one of its presets.
    frame: Option<frame_ui::AppState>,
    backdrop: Option<backdrop::Backdrop>,
    /// Whether the last frame drew the backdrop, to log when it's skipped.
    backdrop_drawn: Option<bool>,
    /// A shot run: the virtual clock, no input from the machine.
    hermetic: bool,
    start: Instant,
    info: gallery::ProbeInfo,
    gl: Option<Gl>,
    state: Option<egui_winit::State>,
    painter: Option<Painter>,
    next_run: Option<Instant>,
    /// F12: save the next frame here.
    shot: Option<PathBuf>,
    /// A shot run whose shot is still to come: taken once egui settles,
    /// then the host exits.
    shot_pending: bool,
    passes: u32,
    /// --scroll left the pointer over the pane: lift it once the scroll has
    /// played out, as the device does, or the shot shows a fake hover.
    park_pointer: bool,
    /// --click/--press/--hold: 0 waiting for idle, 1 press next, 2 release
    /// next, 3 done.
    tap_step: u8,
    /// Which of `args.taps` is under way.
    tap_idx: usize,
    /// --hold: when the touch lifts, on `now`'s clock.
    release_at: Option<Duration>,
    fps_frames: u32,
    fps_start: Instant,
}

impl Preset {
    pub fn new(args: Args) -> Self {
        let ctx = egui::Context::default();
        let opts = Options {
            text_mode: args.text,
            subpixel_binning: true,
        };
        let schemes = theme::Schemes::baked();
        theme::install_fonts(&ctx);
        theme::install(&ctx, schemes, opts);
        ctx.set_theme(args.theme);
        let mut gallery = gallery::Gallery::default();
        let frame = match &args.page {
            PageArg::Gallery(p) => {
                gallery.page = *p;
                None
            }
            PageArg::Frame(name) => {
                let mut st = frame_ui::preset(name);
                // The Display page's segmented control agrees with --theme.
                if let Some(st) = st.as_mut() {
                    st.settings.dark_theme = args.theme == Theme::Dark;
                }
                st
            }
        };
        let info = gallery::ProbeInfo {
            text_mode: Some(opts.text_mode),
            subpixel: true,
            ppp: args.ppp,
            ..Default::default()
        };
        let hermetic = args.screenshot.is_some() || args.hash;
        Self {
            ppp: args.ppp,
            shot_pending: hermetic,
            hermetic,
            args,
            ctx,
            schemes,
            opts,
            gallery,
            frame,
            backdrop: None,
            backdrop_drawn: None,
            start: Instant::now(),
            info,
            gl: None,
            state: None,
            painter: None,
            next_run: Some(Instant::now()),
            shot: None,
            passes: 0,
            park_pointer: false,
            tap_step: 0,
            tap_idx: 0,
            release_at: None,
            fps_frames: 0,
            fps_start: Instant::now(),
        }
    }

    /// egui's zoom for a device pixels_per_point. egui multiplies it by the
    /// window's scale factor, so `--exact` divides that back out.
    fn zoom(&self, ppp: f32) -> f32 {
        match (&self.gl, self.args.exact) {
            (Some(gl), true) => ppp / gl.window.scale_factor() as f32,
            _ => ppp,
        }
    }

    /// egui's clock: since the start, or in a shot run the virtual one,
    /// a `SHOT_PASS` a pass.
    fn now(&self) -> Duration {
        if self.hermetic {
            SHOT_PASS * self.passes
        } else {
            self.start.elapsed()
        }
    }

    fn create_window(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let gl = crate::create_gl(el, self.args.size, self.args.exact, !self.hermetic)?;
        self.info.gl_max_texture = gl.max_texture;
        let mut painter = unsafe { Painter::new() };
        painter.text_boost = self.opts.text_mode == TextMode::Shader;
        self.painter = Some(painter);
        self.state = Some(egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &gl.window,
            Some(gl.window.scale_factor() as f32),
            None,
            // Not a desktop GPU's 16384: the device leaves egui at its default,
            // and the atlas's shape should match.
            None,
        ));
        self.gl = Some(gl);
        self.ctx.set_zoom_factor(self.zoom(self.ppp));
        Ok(())
    }

    fn apply(&mut self, reqs: &[gallery::Request]) {
        for r in reqs {
            log::info!("request {r:?}");
            match *r {
                gallery::Request::Theme(t) => self.ctx.set_theme(t),
                gallery::Request::TextMode(m) => {
                    self.opts.text_mode = m;
                    theme::install(&self.ctx, self.schemes, self.opts);
                    if let Some(p) = self.painter.as_mut() {
                        p.text_boost = m == TextMode::Shader;
                    }
                    self.info.text_mode = Some(m);
                }
                gallery::Request::Subpixel(b) => {
                    self.opts.subpixel_binning = b;
                    theme::install(&self.ctx, self.schemes, self.opts);
                    self.info.subpixel = b;
                }
                gallery::Request::Ppp(p) => {
                    self.ppp = p;
                    self.ctx.set_zoom_factor(self.zoom(p));
                    self.info.ppp = p;
                }
            }
        }
        if !reqs.is_empty() {
            self.next_run = Some(Instant::now());
        }
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let now = self.now();
        let (Some(gl), Some(state), Some(painter)) =
            (self.gl.as_ref(), self.state.as_mut(), self.painter.as_mut())
        else {
            return;
        };
        let mut raw = state.take_egui_input(&gl.window);
        // As on the frame, whose app always has the window: egui draws a
        // focused field as focused only while the window is, and a
        // screenshot run's window is usually behind the terminal.
        raw.focused = true;
        if self.hermetic {
            raw.events.clear();
            raw.hovered_files.clear();
            raw.system_theme = None;
            raw.time = Some(now.as_secs_f64());
            raw.predicted_dt = SHOT_PASS.as_secs_f32();
        }
        // --scroll: a wheel over the detail pane once the first pass has
        // laid it out.
        if self.passes == 1 && self.args.scroll != 0.0 {
            raw.events
                .push(egui::Event::PointerMoved(egui::pos2(800.0, 400.0)));
            raw.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -self.args.scroll),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
            self.park_pointer = true;
        } else if self.park_pointer && !self.ctx.input(|i| i.is_scrolling()) {
            raw.events.push(egui::Event::PointerGone);
            self.park_pointer = false;
        }
        // --click / --press, once egui is idle (the scroll has settled), so
        // the target is where the screenshot will show it.
        if let Some(&(pos, release)) = self.args.taps.get(self.tap_idx) {
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            match self.tap_step {
                1 => {
                    raw.events.push(egui::Event::PointerMoved(pos));
                    raw.events.push(button(true));
                    self.tap_step = 2;
                    self.next_run = Some(Instant::now());
                }
                2 => {
                    if release {
                        raw.events.push(button(false));
                    }
                    self.tap_step = 3;
                    self.next_run = Some(Instant::now());
                }
                _ => {}
            }
        }
        // --hold: the events a finger becomes on the frame (raam-core's
        // app.rs, push_egui_touch), the lift once the hold is up.
        if let Some((pos, hold)) = self.args.hold {
            // The synthetic finger owns the pointer: the real cursor over
            // the window would read as the finger jumping (a drag, a
            // scroll).
            if self.tap_step != 0 {
                raw.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::PointerMoved(_)
                            | egui::Event::PointerButton { .. }
                            | egui::Event::PointerGone
                    )
                });
            }
            let touch = |phase| egui::Event::Touch {
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(0),
                phase,
                pos,
                force: Some(0.5),
            };
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            match self.tap_step {
                1 => {
                    raw.events.extend([
                        egui::Event::PointerMoved(pos),
                        button(true),
                        touch(egui::TouchPhase::Start),
                    ]);
                    self.release_at = Some(now + hold);
                    log::info!("hold: down at {pos:?} for {} ms", hold.as_millis());
                    self.tap_step = 2;
                }
                2 if self.release_at.is_some_and(|t| now >= t) => {
                    raw.events.extend([
                        egui::Event::PointerMoved(pos),
                        button(false),
                        touch(egui::TouchPhase::End),
                        egui::Event::PointerGone,
                    ]);
                    log::info!("hold: up after {} ms", hold.as_millis());
                    self.release_at = None;
                    self.tap_step = 3;
                    self.next_run = Some(Instant::now());
                }
                _ => {}
            }
        }
        let mut reqs = Vec::new();
        let (g, info, frame) = (&mut self.gallery, &self.info, &mut self.frame);
        let mut out = self.ctx.run_ui(raw, |ui| match frame.as_mut() {
            Some(st) => frame_ui::draw(ui, st),
            None => reqs = gallery::draw(ui, g, info),
        });
        if let Some(st) = self.frame.as_mut() {
            frame_ui::stand_in(st);
            // The Display page's theme choice, applied as the controller
            // applies it on the frame.
            let want = if st.settings.dark_theme {
                Theme::Dark
            } else {
                Theme::Light
            };
            if self.ctx.theme() != want {
                self.ctx.set_theme(want);
                self.next_run = Some(Instant::now());
            }
        }
        state.handle_platform_output(&gl.window, out.platform_output);
        self.passes += 1;

        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                if *id == egui::TextureId::default() && delta.pos.is_none() {
                    let [w, h] = delta.image.size();
                    self.info.atlas = [w, h];
                    self.info.atlas_uploads += 1;
                    log::info!("FULL atlas {w}x{h} (upload {})", self.info.atlas_uploads);
                }
                painter.set_texture(*id, delta);
            }
        }
        let size = gl.window.inner_size();
        let (w, h) = (size.width as i32, size.height as i32);
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        painter.upload(&prims, out.pixels_per_point, w, h);
        for id in &out.textures_delta.free {
            painter.free_texture(*id);
        }
        // Applied above; epaint asserts on dropping an uncleared delta.
        out.textures_delta.clear();
        self.info.atlas_fill = self.ctx.fonts(|f| f.font_atlas_fill_ratio());
        // The slideshow's stand-in under the menu bar. Settings are opaque:
        // nothing is drawn under them (the controller's lever on the frame).
        let under = self
            .frame
            .as_ref()
            .filter(|st| !st.opaque())
            .map(|_| self.args.backdrop.as_str());
        let clear = match under {
            Some("white") => 1.0,
            _ => 0.0,
        };
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glViewport(0, 0, w, h);
            glClearColor(clear, clear, clear, 1.0);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        let mut animating = false;
        let drawn = match under {
            Some("white" | "black" | "none") | None => false,
            Some(name) => {
                let b = self
                    .backdrop
                    .get_or_insert_with(|| unsafe { backdrop::Backdrop::new(w, h) });
                let (prog, t) = if name == "still" {
                    ("fade", 0.0)
                } else {
                    (name, now.as_secs_f32())
                };
                animating = name != "still";
                unsafe { b.draw(prog, t, w, h) };
                true
            }
        };
        if self.frame.is_some() && self.backdrop_drawn != Some(drawn) {
            log::info!(
                "backdrop pass {}",
                if drawn {
                    "drawn"
                } else {
                    "skipped (settings are opaque, or --backdrop is flat)"
                }
            );
            self.backdrop_drawn = Some(drawn);
        }
        painter.draw(out.pixels_per_point, w, h);

        let delay = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::MAX);
        // --screenshot: once egui has nothing more to animate.
        if self.passes > 2 && delay >= SHOT_SETTLED && !self.park_pointer && self.shot.is_none() {
            if (!self.args.taps.is_empty() || self.args.hold.is_some()) && self.tap_step == 0 {
                self.tap_step = 1;
                self.next_run = Some(Instant::now());
            } else if self.tap_step != 0 && self.tap_step != 3 {
                // Still tapping.
            } else if self.tap_step == 3 && self.tap_idx + 1 < self.args.taps.len() {
                // The next tap, now that the last one has played out.
                self.tap_idx += 1;
                self.tap_step = 1;
                self.next_run = Some(Instant::now());
            } else if self.shot_pending {
                // Once: a pass already queued still runs after exit().
                self.shot_pending = false;
                let img = read_pixels(w, h);
                if self.args.hash {
                    println!("{w}x{h} {:016x}", golden::hash(&img));
                }
                if let Some(path) = &self.args.screenshot {
                    match golden::write_png(path, &img) {
                        Ok(()) => log::info!("saved {} ({w}x{h})", path.display()),
                        Err(e) => log::error!("screenshot {}: {e}", path.display()),
                    }
                }
                log::info!("shot after {} passes", self.passes);
                el.exit();
            }
        }
        if let Some(path) = self.shot.take() {
            match golden::write_png(&path, &read_pixels(w, h)) {
                Ok(()) => log::info!("saved {} ({w}x{h})", path.display()),
                Err(e) => log::error!("screenshot {}: {e}", path.display()),
            }
        }
        if let Err(e) = gl.surface.swap_buffers(&gl.context) {
            log::error!("swap_buffers: {e}");
        }

        self.fps_frames += 1;
        let secs = self.fps_start.elapsed().as_secs_f32();
        if secs >= 2.0 {
            self.info.fps = self.fps_frames as f32 / secs;
            self.fps_frames = 0;
            self.fps_start = Instant::now();
        }
        self.next_run = Instant::now().checked_add(delay);
        if self.tap_step == 1 {
            // Idle, and the tap is due: don't wait for some other event.
            self.next_run = Some(Instant::now());
        }
        if self.park_pointer {
            let soon = Instant::now() + Duration::from_millis(50);
            self.next_run = Some(self.next_run.map_or(soon, |t| t.min(soon)));
        }
        if let Some(up) = self.release_at {
            let up = self.start + up;
            self.next_run = Some(self.next_run.map_or(up, |t| t.min(up)));
        }
        if animating {
            let soon = Instant::now() + Duration::from_millis(16);
            self.next_run = Some(self.next_run.map_or(soon, |t| t.min(soon)));
        }
        if self.hermetic {
            // The clock is the pass count: the next pass at once.
            self.next_run = Some(Instant::now());
        }
        self.apply(&reqs);
    }

    fn key(&mut self, key: &Key) {
        match key {
            Key::Named(NamedKey::F1) => {
                let t = if self.ctx.theme() == Theme::Dark {
                    Theme::Light
                } else {
                    Theme::Dark
                };
                // A preset's Display page owns the theme there.
                if let Some(st) = self.frame.as_mut() {
                    st.settings.dark_theme = t == Theme::Dark;
                }
                self.apply(&[gallery::Request::Theme(t)]);
            }
            Key::Named(NamedKey::F2) => {
                let next = match self.opts.text_mode {
                    TextMode::EguiDefault => TextMode::Off,
                    TextMode::Off => TextMode::Shader,
                    TextMode::Shader => TextMode::Boost,
                    TextMode::Boost => TextMode::EguiDefault,
                };
                self.apply(&[gallery::Request::TextMode(next)]);
            }
            Key::Named(NamedKey::F12) => {
                let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shots");
                let _ = std::fs::create_dir_all(&dir);
                let theme = if self.ctx.theme() == Theme::Dark {
                    "dark"
                } else {
                    "light"
                };
                let page = match &self.args.page {
                    PageArg::Frame(name) => name.as_str(),
                    PageArg::Gallery(_) => self.gallery.page.name(),
                };
                let name = format!("{page}-{theme}-{}.png", self.opts.text_mode.name());
                self.shot = Some(dir.join(name));
                self.next_run = Some(Instant::now());
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for Preset {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gl.is_none()
            && let Err(e) = self.create_window(el)
        {
            log::error!("{e}");
            el.exit();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => {
                el.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw(el);
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(gl), Some(w), Some(h)) = (
                    &self.gl,
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                ) {
                    gl.surface.resize(&gl.context, w, h);
                }
                self.next_run = Some(Instant::now());
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // A window moved to another display: keep --exact exact.
                self.ctx.set_zoom_factor(self.zoom(self.ppp));
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
            } if !self.hermetic => {
                self.key(&logical_key.clone());
            }
            _ => {}
        }
        if let (Some(gl), Some(state)) = (&self.gl, self.state.as_mut())
            && state.on_window_event(&gl.window, &event).repaint
        {
            self.next_run = Some(Instant::now());
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        match self.next_run {
            Some(t) if t <= Instant::now() => {
                if let Some(gl) = &self.gl {
                    gl.window.request_redraw();
                }
                self.next_run = None;
                el.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }
}
