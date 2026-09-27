//! The `raam` binary: the desktop/Linux host. Today it is the preset
//! host - the widget gallery and every frame_ui screen by name, rendered
//! by the core's own GLES2 painter (gl.rs bridges the shaders to the 4.1
//! core context), so theme, kit and screen changes are iterated and
//! QA'd on the Mac before the frame. It adopts the whole slideshow
//! pipeline at migration step 8.
//!
//! Options:
//! - `--theme dark|light`, `--text egui|off|shader|boost`
//! - `--page <name>`: a gallery page (settings | components | colours |
//!   type | icons | targets | probe) or a frame_ui preset
//!   (`frame_ui::PAGES`):
//!   - the menu bar over the slideshow: menu | menu-undo | menu-paused
//!   - settings: set-photos | set-albums | set-hidden | set-slideshow |
//!     set-videos | set-display | set-sleep | set-server
//!   - with a dialog open: set-slideshow-interval |
//!     set-slideshow-transition | set-videos-playback | set-videos-delay |
//!     set-sleep-at | set-sleep-wake | set-server-cache | set-server-clear;
//!     and set-server-keyboard (the URL field focused, keyboard up)
//!
//!   Any preset takes a fixture suffix: `-empty` (a first run: no server,
//!   nothing synced), `-nopick` (albums listed, none picked) or `-full`
//!   (the picked albums hold more than the cache), e.g. `menu-empty`,
//!   `set-albums-nopick`.
//! - `--backdrop still|<transition>|white|black|none`: what the menu bar
//!   floats over. `still` (the default) is a generated photo-like texture
//!   held still; a transition name (fade | directionalwipe | cube |
//!   crosswarp | swap) loops it; white and black are flat, for the bar's
//!   worst-case contrast. Settings are opaque, so nothing is drawn under
//!   them whatever this says.
//! - `--ppp <f>`: the device's pixels_per_point (default 1)
//! - `--size WxH`: the device screen in pixels (default 1280x800)
//! - `--exact`: one device pixel per screen pixel, so a retina window is
//!   half size but its pixels match `adb screencap`
//! - `--screenshot <file.png>`: draw until egui is idle (nothing due
//!   within 200 ms), save, and exit
//! - `--scroll <px>`: scroll the detail pane down this far first, so a
//!   screenshot can reach what's below the fold
//! - `--click X,Y` / `--press X,Y`: a tap (or a press held down) at that
//!   point after the scroll. Repeatable: taps run in order, each once
//!   egui is idle, so `--click` a field then keys of the on-screen
//!   keyboard types into it
//! - `--hold X,Y,MS`: a touch held still for MS milliseconds, then
//!   lifted, fed as the Android host feeds a finger, with frames run as
//!   egui asks meanwhile, so its press-and-hold timer fires as on the
//!   frame
//!
//! Keys: F1 theme, F2 next text mode, F12 screenshot into `shots/`.
mod backdrop;

use egui::Theme;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, Version,
};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raam_core::gl::*;
use raam_core::painter::Painter;
use raam_core::theme::{self, Options, TextMode};
use raam_core::{frame_ui, gallery};
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::raw_window_handle::HasWindowHandle;
use winit::window::{Window, WindowId};

/// What `--page` named: a gallery page or a frame_ui preset.
#[derive(Clone)]
enum PageArg {
    Gallery(gallery::Page),
    Frame(String),
}

struct Args {
    theme: Theme,
    text: TextMode,
    page: PageArg,
    backdrop: String,
    ppp: f32,
    size: [u32; 2],
    exact: bool,
    screenshot: Option<PathBuf>,
    scroll: f32,
    /// (point, release): --click releases, --press holds. In order.
    taps: Vec<(egui::Pos2, bool)>,
    /// --hold: the touch's point and how long it stays down.
    hold: Option<(egui::Pos2, Duration)>,
}

fn text_mode(s: &str) -> Option<TextMode> {
    [
        TextMode::EguiDefault,
        TextMode::Off,
        TextMode::Shader,
        TextMode::Boost,
    ]
    .into_iter()
    .find(|m| m.name() == s)
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        theme: Theme::Dark,
        text: TextMode::Shader,
        page: PageArg::Gallery(gallery::Page::Settings),
        backdrop: "still".into(),
        ppp: 1.0,
        size: [1280, 800],
        exact: false,
        screenshot: None,
        scroll: 0.0,
        taps: Vec::new(),
        hold: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--version" | "-V" => {
                println!("raam {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--theme" => {
                a.theme = match val()?.as_str() {
                    "dark" => Theme::Dark,
                    "light" => Theme::Light,
                    v => return Err(format!("unknown theme {v:?}")),
                }
            }
            "--text" => {
                let v = val()?;
                a.text = text_mode(&v).ok_or(format!("unknown text mode {v:?}"))?;
            }
            "--page" => {
                let v = val()?;
                a.page = match gallery::Page::from_name(&v) {
                    Some(p) => PageArg::Gallery(p),
                    None if frame_ui::preset(&v).is_some() => PageArg::Frame(v),
                    None => return Err(format!("unknown page {v:?}")),
                };
            }
            "--backdrop" => {
                let v = val()?;
                let known = [
                    "still",
                    "white",
                    "black",
                    "none",
                    "fade",
                    "directionalwipe",
                    "cube",
                    "crosswarp",
                    "swap",
                ];
                if !known.contains(&v.as_str()) {
                    return Err(format!("unknown backdrop {v:?}"));
                }
                a.backdrop = v;
            }
            "--ppp" => a.ppp = val()?.parse().map_err(|e| format!("--ppp: {e}"))?,
            "--size" => {
                let v = val()?;
                let (w, h) = v
                    .split_once('x')
                    .ok_or(format!("--size wants WxH, got {v:?}"))?;
                a.size = [
                    w.parse().map_err(|e| format!("--size: {e}"))?,
                    h.parse().map_err(|e| format!("--size: {e}"))?,
                ];
            }
            "--exact" => a.exact = true,
            "--screenshot" => a.screenshot = Some(val()?.into()),
            "--click" | "--press" => {
                let v = val()?;
                let (x, y) = v
                    .split_once(',')
                    .ok_or(format!("{flag} wants X,Y, got {v:?}"))?;
                let p = egui::pos2(
                    x.parse().map_err(|e| format!("{flag}: {e}"))?,
                    y.parse().map_err(|e| format!("{flag}: {e}"))?,
                );
                a.taps.push((p, flag == "--click"));
            }
            "--hold" => {
                let v = val()?;
                let bad = || format!("--hold wants X,Y,MS, got {v:?}");
                let mut n = v.split(',').map(|s| s.parse::<f32>());
                let (Some(Ok(x)), Some(Ok(y)), Some(Ok(ms)), None) =
                    (n.next(), n.next(), n.next(), n.next())
                else {
                    return Err(bad());
                };
                a.hold = Some((egui::pos2(x, y), Duration::from_millis(ms as u64)));
            }
            "--scroll" => a.scroll = val()?.parse().map_err(|e| format!("--scroll: {e}"))?,
            _ => {
                return Err(format!(
                    "unknown option {flag:?} (see the top of src/main.rs)"
                ));
            }
        }
    }
    Ok(a)
}

struct Gl {
    window: Window,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
}

struct App {
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
    start: Instant,
    info: gallery::ProbeInfo,
    gl: Option<Gl>,
    state: Option<egui_winit::State>,
    painter: Option<Painter>,
    next_run: Option<Instant>,
    /// Save the next frame here (F12, or --screenshot once egui is idle).
    shot: Option<PathBuf>,
    exit_after_shot: bool,
    passes: u32,
    /// --scroll left the pointer over the pane: lift it once the scroll has
    /// played out, as the device does, or the shot shows a fake hover.
    park_pointer: bool,
    /// --click/--press/--hold: 0 waiting for idle, 1 press next, 2 release
    /// next, 3 done.
    tap_step: u8,
    /// Which of `args.taps` is under way.
    tap_idx: usize,
    /// --hold: when the touch lifts.
    release_at: Option<Instant>,
    fps_frames: u32,
    fps_start: Instant,
}

impl App {
    fn new(args: Args) -> Self {
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
        Self {
            ppp: args.ppp,
            exit_after_shot: args.screenshot.is_some(),
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

    fn create_window(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let [w, h] = self.args.size;
        let attrs = Window::default_attributes()
            .with_title("raam")
            .with_inner_size(LogicalSize::new(w, h));
        let template = ConfigTemplateBuilder::new();
        let (window, config) = DisplayBuilder::new()
            .with_window_attributes(Some(attrs))
            // No MSAA: egui feathers its own edges, and the frame has none.
            .build(el, template, |configs| {
                configs.min_by_key(|c| c.num_samples()).unwrap()
            })
            .map_err(|e| format!("no GL config: {e}"))?;
        let window = window.ok_or("no window")?;
        if self.args.exact {
            let _ = window.request_inner_size(PhysicalSize::new(w, h));
        }
        let display = config.display();
        let raw = window.window_handle().ok().map(|h| h.as_raw());
        // glutin makes a 4.1 core context on macOS whatever is asked; gl.rs
        // bridges the gap to GLES2.
        let ctx_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 2))))
            .build(raw);
        let not_current = unsafe { display.create_context(&config, &ctx_attrs) }
            .map_err(|e| format!("create_context: {e}"))?;
        let surf_attrs = window
            .build_surface_attributes(Default::default())
            .map_err(|e| format!("surface attributes: {e}"))?;
        let surface = unsafe { display.create_window_surface(&config, &surf_attrs) }
            .map_err(|e| format!("create_window_surface: {e}"))?;
        let context = not_current
            .make_current(&surface)
            .map_err(|e| format!("make_current: {e}"))?;
        let _ = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::MIN));

        unsafe { bind_vao() };
        let mut max_tex = 0;
        unsafe { glGetIntegerv(GL_MAX_TEXTURE_SIZE, &mut max_tex) };
        self.info.gl_max_texture = max_tex;
        log::info!(
            "GL {} | {} | window scale {} | GL_MAX_TEXTURE_SIZE {max_tex}",
            unsafe { gl_string(GL_VERSION) },
            unsafe { gl_string(GL_RENDERER) },
            window.scale_factor()
        );
        let mut painter = unsafe { Painter::new() };
        painter.text_boost = self.opts.text_mode == TextMode::Shader;
        self.painter = Some(painter);
        self.state = Some(egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            // Not the Mac's 16384: the device leaves egui at its default,
            // and the atlas's shape should match.
            None,
        ));
        self.gl = Some(Gl {
            window,
            surface,
            context,
        });
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
        // --hold: the Android host's events for a finger (its lib.rs,
        // push_egui_touch), the lift once the hold is up.
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
                    self.release_at = Some(Instant::now() + hold);
                    log::info!("hold: down at {pos:?} for {} ms", hold.as_millis());
                    self.tap_step = 2;
                }
                2 if self.release_at.is_some_and(|t| Instant::now() >= t) => {
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
                    (name, self.start.elapsed().as_secs_f32())
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
        // --screenshot: once egui has nothing more to animate. A slow
        // repaint counts as idle too: a focused field's cursor blinks
        // forever.
        if self.passes > 2
            && delay >= Duration::from_millis(200)
            && !self.park_pointer
            && self.shot.is_none()
        {
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
            } else if let Some(p) = self.args.screenshot.take() {
                self.shot = Some(p);
                self.next_run = Some(Instant::now());
            }
        }
        if let Some(path) = self.shot.take() {
            match save_png(&path, w, h) {
                Ok(()) => log::info!("saved {} ({w}x{h})", path.display()),
                Err(e) => log::error!("screenshot {}: {e}", path.display()),
            }
            if self.exit_after_shot {
                el.exit();
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
            self.next_run = Some(self.next_run.map_or(up, |t| t.min(up)));
        }
        if animating {
            let soon = Instant::now() + Duration::from_millis(16);
            self.next_run = Some(self.next_run.map_or(soon, |t| t.min(soon)));
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

impl ApplicationHandler for App {
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
            } => {
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

/// The back buffer, bottom-up in GL, written top-down as RGB.
fn save_png(path: &std::path::Path, w: i32, h: i32) -> Result<(), String> {
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    unsafe {
        glReadPixels(
            0,
            0,
            w,
            h,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            rgba.as_mut_ptr() as *mut c_void,
        )
    };
    let row = (w * 4) as usize;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in (0..h as usize).rev() {
        rgb.extend(
            rgba[y * row..(y + 1) * row]
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]]),
        );
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&rgb))
        .map_err(|e| e.to_string())
}

/// A stderr logger: everything at info and up, no dependency.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }

    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[{}] {}", r.level(), r.args());
        }
    }

    fn flush(&self) {}
}

fn main() {
    let _ = log::set_logger(&StderrLog);
    log::set_max_level(log::LevelFilter::Info);
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let event_loop = EventLoop::new().expect("event loop");
    let mut app = App::new(args);
    if let Err(e) = event_loop.run_app(&mut app) {
        log::error!("event loop: {e}");
    }
}
