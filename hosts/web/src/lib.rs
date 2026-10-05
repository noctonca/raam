//! The web host: the canvas demo (docs/ARCHITECTURE.md "The web demo").
//! The real core compiled to wasm — the App controller, the slideshow
//! pipeline, the clock overlay and the egui chrome, as the frame runs
//! them — over the bundled sample photos through its own `TileSource`
//! (source.rs), with `NoVideo` as its player. No engine on wasm.
//!
//! The canvas's drawing buffer is the frame's screen, 1280x800 device
//! pixels at pixels_per_point 1 (`?size=1024x600` for the second
//! certified panel); CSS only scales it to fit the page. The loop runs on
//! requestAnimationFrame and does the Android host's pass (its lib.rs)
//! only when it has to: an input event, the controller's wait running
//! out, or something landing from the source. A pointer is a finger, fed
//! to the controller as the Android host feeds touches, and the keyboard
//! is the frame's while the canvas has the focus.
//!
//! `?page=<name>` shows a gallery page or a frame_ui preset instead, as
//! the desktop preset host's `--page` does (preset.rs), for the pixel
//! diff and the page's design-system tab.
#![cfg(target_arch = "wasm32")]

mod platform;
mod preset;
mod source;

use platform::QuerySwitches;
use raam_core::app::{
    App, Deps, Effect, Event, Inputs, KeyEvent, LibraryInfo, Overrides, Stage, Touch,
};
use raam_core::frame_ui::AppState;
use raam_core::gl::*;
use raam_core::overlay::ClockOverlay;
use raam_core::painter::Painter;
use raam_core::seams::DebugSwitches;
use raam_core::slideshow::{Pipeline, SlideshowSettings};
use raam_core::video::NoVideo;
use raam_core::{clock, collage, switches};
use raam_model::{FitBackground, GapColour, HiddenItem, SourceKind, Stats};
use source::WebSource;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, KeyboardEvent, PointerEvent};

/// The frame's density: pixels_per_point 1, 1 dp = 1 px.
const DENSITY_DPI: u32 = 160;

/// The library's side of the status line and settings: the samples play
/// as the on-device folder would.
struct WebLibrary {
    version: Cell<u64>,
    stats: RefCell<Stats>,
}

impl LibraryInfo for WebLibrary {
    fn version(&self) -> u64 {
        self.version.get()
    }

    fn stats(&self) -> Stats {
        self.stats.borrow().clone()
    }

    fn online(&self) -> bool {
        false
    }
}

/// The slideshow as the frame shows it.
struct Live {
    controller: App,
    pipeline: Pipeline<NoVideo>,
    painter: Painter,
    overlay: ClockOverlay,
    source: WebSource,
    library: WebLibrary,
    screen: (i32, i32),
    events: Vec<Event>,
    /// When the controller asked to run again (monotonic).
    due: Duration,
    frames: u32,
    passes: u32,
    last_log: Duration,
}

impl Live {
    /// # Safety
    /// Requires the current WebGL context.
    unsafe fn new(w: i32, h: i32) -> Result<Self, String> {
        let default_max = collage::screen_default_max(w, h, DENSITY_DPI);
        log::info!(
            "screen {w}x{h} @ {DENSITY_DPI}dpi = {:.2}\" diagonal -> default collage max {default_max}",
            collage::screen_diagonal_inches(w, h, DENSITY_DPI),
        );
        let mut state = AppState::new("", "");
        let s = &mut state.settings;
        s.collage_max = default_max.min(collage::LARGEST_LAYOUT);
        s.screen_default_max = default_max;
        // The samples are the on-device folder; there is no Immich here
        // (yet) and no power to sleep with.
        s.immich_enabled = false;
        s.local_enabled = true;
        s.sleep_enabled = false;
        let margin = raam_core::num::sat_i32((2.0 * DENSITY_DPI as f32 / 160.0) + 0.5);
        let source = WebSource::new(s.collage_max, (w, h, margin.max(1)))?;
        let samples = i64::try_from(source.len()).expect("the bundled samples, a handful, fit i64");
        let library = WebLibrary {
            version: Cell::new(1),
            stats: RefCell::new(Stats {
                local_assets: samples,
                local_ready: samples,
                local_dir: "Sample photos (CC0, Wikimedia Commons)".into(),
                local_note: "bundled with the demo".into(),
                ..Stats::default()
            }),
        };
        let settings = SlideshowSettings {
            dwell: Duration::from_secs(10),
            transition: None,
            ken_burns: true,
            fill_by_default: true,
            fit_background: FitBackground::Blurred,
            gap_colour: GapColour::Black,
            video_playback: state.settings.video_playback,
            video_sound: false,
        };
        let controller = App::new(state, || None);
        // SAFETY: the canvas's context is current (this fn's contract:
        // start made it so in make_context, on the page's one thread).
        let pipeline = unsafe { Pipeline::new(w, h, DENSITY_DPI, settings, NoVideo, || None) };
        // SAFETY: the canvas's context is current, as for the pipeline.
        let mut painter = unsafe { Painter::new() };
        // The theme's text mode is the shader boost, chosen on the frame:
        // right for light and dark text alike, and a theme switch never
        // rebuilds the font atlas.
        painter.text_boost = true;
        // SAFETY: the canvas's context is current, as for the pipeline.
        let overlay = unsafe { ClockOverlay::new() };
        let now = clock::now();
        log::info!("pipeline + painter ready, {w}x{h}");
        Ok(Self {
            controller,
            pipeline,
            painter,
            overlay,
            source,
            library,
            screen: (w, h),
            // The activity's start, as Android reports it at boot.
            events: vec![Event::Start, Event::Resume],
            due: now,
            frames: 0,
            passes: 0,
            last_log: now,
        })
    }

    fn touch(&mut self, phase: egui::TouchPhase, pos: egui::Pos2, id: i32, force: f32) {
        self.events.push(Event::Touch(Touch {
            phase,
            pos,
            device_id: 0,
            touch_id: u64::from(id.cast_unsigned()),
            force,
        }));
    }

    fn key(&mut self, key: Option<egui::Key>, k: Keystroke) {
        if let Some(key) = key {
            self.events.push(Event::Key(KeyEvent {
                key,
                pressed: k.pressed,
                repeat: k.repeat,
                modifiers: k.modifiers,
            }));
        }
        if let Some(t) = k.text {
            self.events.push(Event::Text(t));
        }
    }

    /// One animation frame: the Android host's loop pass, when due.
    fn frame(&mut self) {
        // The debug switches, snapshotted once per pass.
        switches::set_fail(&QuerySwitches.get("debug.video.fail"));
        self.source.pump();
        let now = clock::now();
        let woke = self.source.take_woke();
        if self.events.is_empty() && !woke && now < self.due {
            return;
        }
        self.passes += 1;
        let events = std::mem::take(&mut self.events);
        let out = self.controller.frame(
            &events,
            &Inputs {
                has_surface: true,
                screen: self.screen,
                wakelock_allowed: false,
                overrides: Overrides::default(),
                chooses_colour_depth: false,
            },
            &mut Deps {
                stage: Some(Stage {
                    slideshow: &mut self.pipeline,
                    source: &self.source,
                    library: &self.library,
                    // No weather on the web yet: the overlay shows the
                    // time and date.
                    weather: None,
                    // Nor Wi-Fi: Connectivity says it's not available here.
                    network: None,
                }),
                power: None,
            },
        );
        self.run_effects(out.effects);
        self.due = out
            .wait
            .and_then(|w| now.checked_add(w))
            .unwrap_or(Duration::MAX);
        if out.skip_draw {
            return;
        }
        let (w, h) = self.screen;
        // The full-screen settings cover everything: skip the slideshow
        // and overlay draws under them (the controller's opaque lever).
        if !out.chrome_opaque {
            self.pipeline.draw_frame();
            // SAFETY: the canvas's context, current since make_context, the
            // page's one thread; attribute indices 0..8 are within WebGL1's
            // minimum.
            unsafe {
                for i in 0..8 {
                    glDisableVertexAttribArray(i);
                }
            }
        }
        if let Some(r) = out.overlay
            && self.overlay.update(r.style, r.corner, &r.content, w, h)
        {
            log::info!(
                "overlay rebuilt ({}, {}): {:?}",
                r.style.label(),
                r.corner.label(),
                r.content
            );
        }
        if !out.chrome_opaque {
            self.overlay.draw(w, h);
        }
        if let Some(mut e) = out.egui {
            for (id, deltas) in &e.textures_delta.set {
                for delta in deltas {
                    self.painter.set_texture(*id, delta);
                }
            }
            self.painter.upload(&e.primitives, 1.0, w, h);
            for id in &e.textures_delta.free {
                self.painter.free_texture(*id);
            }
            // Applied above; epaint asserts on dropping an uncleared delta.
            e.textures_delta.clear();
        }
        if out.draw_egui {
            self.painter.draw(1.0, w, h);
        }
        self.frames += 1;
        let since = clock::elapsed(self.last_log);
        if since >= Duration::from_secs(10) {
            log::info!(
                "stats fps={:.1} passes={} overlay={} layout={}",
                f64::from(self.frames) / since.as_secs_f64(),
                self.passes,
                self.controller.overlay_open(),
                self.pipeline.shown_layout().as_deref().unwrap_or("-"),
            );
            self.frames = 0;
            self.passes = 0;
            self.last_log = clock::now();
        }
    }

    /// The controller's effects, on the web's stand-ins for the engine.
    fn run_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::SetMaxGroup(max) => self.source.set_max_group(max),
                Effect::SetHidden(key, hidden) => {
                    self.source.set_hidden(key.as_str(), hidden);
                    let mut stats = self.library.stats.borrow_mut();
                    stats.hidden.retain(|h| h.key != key);
                    if hidden {
                        stats.hidden.push(HiddenItem {
                            label: self
                                .source
                                .title(key.as_str())
                                .unwrap_or_else(|| key.to_string()),
                            key,
                            source: None,
                        });
                    }
                    self.library.version.set(self.library.version.get() + 1);
                }
                Effect::SetSourceEnabled(SourceKind::Local, on) => self.source.set_enabled(on),
                Effect::SaveSettings { .. } => {
                    log::info!("settings changed (the demo keeps them for this page only)")
                }
                // Fill/Fit is the pipeline's own; there is nothing to save
                // it to.
                Effect::SetScale(..) => {}
                other => log::info!("not in the web demo: {}", effect_name(&other)),
            }
        }
    }

    /// What a test or the page may want to know.
    fn status(&self) -> String {
        serde_json::json!({
            "mode": "live",
            "layout": self.pipeline.shown_layout(),
            "overlay": self.controller.overlay_open(),
            "transitioning": self.pipeline.is_transitioning(),
            "paused": self.controller.state.paused,
            "shown": self.pipeline.shown_photo().map(|(key, _)| key.into_string()),
        })
        .to_string()
    }
}

fn effect_name(e: &Effect) -> &'static str {
    match e {
        Effect::SaveSettings { .. } => "save settings",
        Effect::SetScale(..) => "Fill/Fit",
        Effect::SetHidden(..) => "hide",
        Effect::SetSourceEnabled(..) => "the Immich source",
        Effect::SetServer { .. } => "a server",
        Effect::ExportCuration => "the curation export",
        Effect::SelectAlbum(..) => "albums",
        Effect::SetCap(_) => "the cache cap",
        Effect::ClearCache => "the cache",
        Effect::Rescan => "rescanning",
        Effect::SyncNow => "syncing",
        Effect::SetMaxGroup(_) => "the collage max",
        Effect::SetWeather(_) => "the weather",
    }
}

enum Host {
    Live(Box<Live>),
    Preset(Box<preset::Preset>),
}

/// A key as the page saw it, beyond its egui name.
pub struct Keystroke {
    pressed: bool,
    repeat: bool,
    modifiers: egui::Modifiers,
    /// What it typed, if it's a character key going down.
    text: Option<String>,
}

impl Host {
    fn pointer(&mut self, phase: egui::TouchPhase, pos: egui::Pos2, id: i32, force: f32) {
        match self {
            Host::Live(l) => l.touch(phase, pos, id, force),
            Host::Preset(p) => p.touch(phase, pos, id),
        }
    }

    /// Whether the frame takes this key from the page. Tab and Escape stay
    /// the page's while the menu is closed, so the canvas never traps the
    /// keyboard; on the design-system page Tab always does.
    fn takes(&self, key: Option<egui::Key>) -> bool {
        let open = match self {
            Host::Live(l) => l.controller.overlay_open(),
            Host::Preset(_) => false,
        };
        open || !matches!(key, Some(egui::Key::Tab | egui::Key::Escape))
    }

    fn key(&mut self, key: Option<egui::Key>, k: Keystroke) {
        match self {
            Host::Live(l) => l.key(key, k),
            Host::Preset(p) => {
                let key = key.map(|key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: k.pressed,
                    repeat: k.repeat,
                    modifiers: k.modifiers,
                });
                p.input(key.into_iter().chain(k.text.map(egui::Event::Text)));
            }
        }
    }

    fn frame(&mut self) {
        match self {
            Host::Live(l) => l.frame(),
            Host::Preset(p) => p.frame(),
        }
    }
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

fn with_host(f: impl FnOnce(&mut Host)) {
    HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            f(host)
        }
    })
}

fn listen<E: wasm_bindgen::convert::FromWasmAbi + 'static>(
    target: &web_sys::EventTarget,
    name: &str,
    f: impl FnMut(E) + 'static,
) -> Result<(), JsValue> {
    let cb = Closure::<dyn FnMut(E)>::new(f);
    target.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref())?;
    cb.forget();
    Ok(())
}

/// The page's canvas as the frame's EGL window: an opaque WebGL1 back
/// buffer with no multisampling, depth or stencil, as on the frame. The
/// drawing buffer is preserved so a frame can be read back after it's
/// shown (the pixel diff).
fn make_context(canvas: &HtmlCanvasElement) -> Result<(), String> {
    let attrs = web_sys::WebGlContextAttributes::new();
    attrs.set_alpha(false);
    attrs.set_antialias(false);
    attrs.set_depth(false);
    attrs.set_stencil(false);
    attrs.set_preserve_drawing_buffer(true);
    let gl = canvas
        .get_context_with_context_options("webgl", &attrs)
        .map_err(|e| format!("getContext: {e:?}"))?
        .ok_or("no WebGL1 in this browser")?
        .dyn_into::<web_sys::WebGlRenderingContext>()
        .map_err(|_| "not a WebGL1 context")?;
    make_current(gl);
    let mut max_tex = 0;
    // SAFETY: the context made current just above; GL_MAX_TEXTURE_SIZE is
    // single-valued, so one GlInt is written to `max_tex`.
    unsafe { glGetIntegerv(GL_MAX_TEXTURE_SIZE, &mut max_tex) };
    log::info!(
        "{} | {} | GL_MAX_TEXTURE_SIZE {max_tex}",
        // SAFETY: the context made current above.
        unsafe { gl_string(GL_VERSION) },
        // SAFETY: as for GL_VERSION.
        unsafe { gl_string(GL_RENDERER) }
    );
    Ok(())
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    platform::init_log();
    // The host installs the clock before anything can read it.
    clock::set_source(platform::clock_source());
    let window = web_sys::window().ok_or("no window")?;
    let doc = window.document().ok_or("no document")?;
    let query = web_sys::UrlSearchParams::new_with_str(&window.location().search()?)?;
    platform::load_switches(&query);
    let (w, h) = query
        .get("size")
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?))
        })
        .filter(|&(w, h)| (320..=4096).contains(&w) && (240..=4096).contains(&h))
        .unwrap_or((1280, 800));
    let canvas: HtmlCanvasElement = doc
        .get_element_by_id("frame")
        .ok_or("no #frame canvas")?
        .dyn_into()?;
    canvas.set_width(from_gl_size(w));
    canvas.set_height(from_gl_size(h));
    make_context(&canvas).map_err(|e| JsValue::from_str(&e))?;
    switches::set_fail(&QuerySwitches.get("debug.video.fail"));

    let host = match query.get("page") {
        Some(page) => {
            let p = preset::Preset::new(&page, &query, w, h).map_err(|e| JsValue::from_str(&e))?;
            Host::Preset(Box::new(p))
        }
        None => {
            // SAFETY: make_context above made the canvas's context current,
            // and the page has no other thread.
            let live = unsafe { Live::new(w, h) }.map_err(|e| JsValue::from_str(&e))?;
            Host::Live(Box::new(live))
        }
    };
    HOST.with(|slot| *slot.borrow_mut() = Some(host));

    // A pointer is a finger: its position in the canvas's own pixels, the
    // primary pointer only, moves only while it's down.
    let down = Rc::new(Cell::new(None::<i32>));
    let target: &web_sys::EventTarget = canvas.as_ref();
    for (name, phase) in [
        ("pointerdown", egui::TouchPhase::Start),
        ("pointermove", egui::TouchPhase::Move),
        ("pointerup", egui::TouchPhase::End),
        ("pointercancel", egui::TouchPhase::Cancel),
    ] {
        let (c, down) = (canvas.clone(), down.clone());
        listen(target, name, move |e: PointerEvent| {
            if !e.is_primary() {
                return;
            }
            e.prevent_default();
            let id = e.pointer_id();
            match phase {
                egui::TouchPhase::Start => {
                    let _ = c.set_pointer_capture(id);
                    // Held off by preventing the default: the keys follow
                    // a tap or click.
                    let _ = c.focus();
                    down.set(Some(id));
                }
                _ if down.get() != Some(id) => return,
                egui::TouchPhase::Move => {}
                _ => down.set(None),
            }
            let (cw, ch) = (
                c.client_width().max(1) as f32,
                c.client_height().max(1) as f32,
            );
            let pos = egui::pos2(
                e.offset_x() as f32 * c.width() as f32 / cw,
                e.offset_y() as f32 * c.height() as f32 / ch,
            );
            let force = e.pressure().clamp(0.0, 1.0);
            with_host(|host| host.pointer(phase, pos, id, force));
        })?;
    }
    listen(target, "contextmenu", |e: web_sys::Event| {
        e.prevent_default()
    })?;
    // Keys while the canvas has the focus. The browser keeps its shortcuts
    // (Ctrl, Alt, Command held), and a key with no egui name still types
    // its character.
    for (name, pressed) in [("keydown", true), ("keyup", false)] {
        listen(target, name, move |e: KeyboardEvent| {
            if e.ctrl_key() || e.alt_key() || e.meta_key() {
                return;
            }
            let name = e.key();
            let key = egui::Key::from_name(&name);
            let text = (pressed && name.chars().count() == 1).then_some(name);
            if key.is_none() && text.is_none() {
                return;
            }
            let mut taken = false;
            with_host(|host| taken = host.takes(key));
            if !taken {
                return;
            }
            e.prevent_default();
            let k = Keystroke {
                pressed,
                repeat: e.repeat(),
                modifiers: egui::Modifiers {
                    shift: e.shift_key(),
                    ..Default::default()
                },
                text,
            };
            with_host(|host| host.key(key, k));
        })?;
    }
    // The tab going to the background is the activity pausing.
    let d = doc.clone();
    listen(
        doc.as_ref(),
        "visibilitychange",
        move |_: web_sys::Event| {
            let visible = d.visibility_state() == web_sys::VisibilityState::Visible;
            with_host(|host| {
                if let Host::Live(l) = host {
                    l.events
                        .push(if visible { Event::Resume } else { Event::Pause });
                }
            });
        },
    )?;

    // The loop: every animation frame, doing the pass only when due.
    let f = Rc::new(RefCell::new(None::<Closure<dyn FnMut()>>));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::new(move || {
        with_host(Host::frame);
        if let (Some(w), Some(cb)) = (web_sys::window(), f.borrow().as_ref()) {
            let _ = w.request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }));
    if let Some(cb) = g.borrow().as_ref() {
        window.request_animation_frame(cb.as_ref().unchecked_ref())?;
    }
    Ok(())
}

/// Sets a debug switch live (`set_switch("debug.video.fail", "rt")` from
/// the console or a test); "" clears it.
#[wasm_bindgen]
pub fn set_switch(name: &str, value: &str) {
    platform::set_switch(name, value);
}

/// The host's state as JSON, for a test or the page.
#[wasm_bindgen]
pub fn status() -> String {
    HOST.with(|h| match h.borrow().as_ref() {
        Some(Host::Live(l)) => l.status(),
        Some(Host::Preset(p)) => p.status(),
        None => "{}".into(),
    })
}
