//! The `raam` binary: the desktop/Linux host (docs/ARCHITECTURE.md
//! "Hosts"), drawing with the core's own GLES2 renderer (gl.rs bridges
//! the shaders to the 4.1 core context, or with the `gles` feature runs
//! them in a GLES 2.0 one). Two modes:
//!
//! - **The slideshow** (no `--page`; live.rs): the product as the frame
//!   runs it, on the engine, with no video player. The mouse is a finger,
//!   and so is a touchscreen's first.
//! - **The preset host** (`--page`; preset.rs): the widget gallery and
//!   every frame_ui screen by name, over a stand-in for the slideshow, so
//!   theme, kit and screen changes are iterated and QA'd on the
//!   desktop before the frame.
//!
//! The slideshow's options:
//! - `--data <dir>`: the DB and caches (default: the app-data dir,
//!   `~/Library/Application Support/raam` on macOS, else
//!   `$XDG_DATA_HOME/raam`)
//! - `--photos <dir>`: the photos folder, remembered in the data dir (a
//!   fresh one watches `~/Pictures/Raam`, made if missing). Immich is set
//!   up in the settings, as on the frame
//! - `--size WxH`: the screen in device pixels (default 1280x800); the
//!   window is always exact, as `--exact` makes the presets
//! - `--fullscreen`: fill the monitor instead, borderless and with the
//!   pointer hidden, as a Linux frame runs. The primary monitor, or the
//!   first where none is (Wayland); the screen is the window's size once
//!   it has held still for half a second
//! - `--click X,Y` / `--press X,Y`: scripted taps, the first once the
//!   first collage is up and still, each 1.5 s after the step before
//! - `--set NAME=VALUE`: a scripted step that sets a debug switch, as F5
//!   does (`--set debug.video.fail=rt`; `NAME=` clears it)
//! - `--wait S`: a scripted pause of S seconds more before the next step
//! - `--screenshot <file.png>`: save 1.5 s after the last step (or the
//!   first collage) and exit
//! - the frame's `debug.*` props as env vars, upper-cased with dots as
//!   underscores: `RAAM_DEBUG_VIDEO_FAIL=rt`
//!
//! Keys: F5 GPU-failure injection on/off (`debug.video.fail=rt`), F12
//! screenshot into `shots/`.
//!
//! The preset host's options:
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
//! - `--screenshot <file.png>`: draw until egui has settled (nothing due
//!   within 200 ms), save, and exit. The run is hermetic: a virtual clock
//!   steps 1/60 s a pass, and the window neither takes the focus nor
//!   passes the real mouse or keyboard to egui, so the shot depends on the
//!   flags alone, not on load, the display or where the pointer rests
//! - `--hash`: as `--screenshot`, but print the shot's size and a hash of
//!   its pixels (the golden suite's check); both flags may be given
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
//!
//! And two with no window, for scripts/goldens.sh:
//! - `--pages`: print every `--page` name, fixture variants included
//! - `--diff A.png B.png [--tolerance N] [--out D.png]`: compare two shots,
//!   print how B differs from A, exit 1 if any pixel differs by more than
//!   N levels (default 0); D.png gets B in grey with those pixels magenta
mod backdrop;
mod golden;
mod live;
mod platform;
mod preset;

use egui::Theme;
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext, Version,
};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raam_core::gl::*;
use raam_core::theme::TextMode;
use raam_core::{frame_ui, gallery};
use std::ffi::c_void;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::monitor::MonitorHandle;
use winit::raw_window_handle::HasWindowHandle;
#[cfg(not(target_os = "macos"))]
use winit::window::Fullscreen;
use winit::window::Window;

/// A scripted step of the slideshow's, run in the order given.
#[derive(Clone)]
enum Step {
    /// --click (released) or --press (held down).
    Tap(egui::Pos2, bool),
    /// --set: a debug switch, as F5 flips `debug.video.fail`.
    Set(String, String),
    /// --wait: an extra pause before the next step.
    Wait(Duration),
}

/// What `--page` named: a gallery page or a frame_ui preset.
#[derive(Clone)]
enum PageArg {
    Gallery(gallery::Page),
    Frame(String),
}

struct Args {
    /// No `--page`: the slideshow.
    live: bool,
    /// --data and --photos (the slideshow).
    data: Option<PathBuf>,
    photos: Option<PathBuf>,
    theme: Theme,
    text: TextMode,
    page: PageArg,
    backdrop: String,
    ppp: f32,
    size: [u32; 2],
    /// --fullscreen (the slideshow): the screen is the full-screen
    /// window's size, not `size`.
    fullscreen: bool,
    exact: bool,
    screenshot: Option<PathBuf>,
    /// --hash: print the shot's hash (and exit, as --screenshot does).
    hash: bool,
    scroll: f32,
    /// (point, release): --click releases, --press holds. In order.
    taps: Vec<(egui::Pos2, bool)>,
    /// The slideshow's taps, --set and --wait, in order.
    script: Vec<Step>,
    /// --hold: the touch's point and how long it stays down.
    hold: Option<(egui::Pos2, Duration)>,
    /// --diff A B, --tolerance and --out: no window.
    diff: Option<(PathBuf, PathBuf)>,
    tolerance: u8,
    out: Option<PathBuf>,
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
        live: true,
        data: None,
        photos: None,
        theme: Theme::Dark,
        text: TextMode::Shader,
        page: PageArg::Gallery(gallery::Page::Settings),
        backdrop: "still".into(),
        ppp: 1.0,
        size: [1280, 800],
        fullscreen: false,
        exact: false,
        screenshot: None,
        hash: false,
        scroll: 0.0,
        taps: Vec::new(),
        script: Vec::new(),
        hold: None,
        diff: None,
        tolerance: 0,
        out: None,
    };
    // The first flag given that the other mode has no use for.
    let (mut preset_only, mut live_only) = (None, None);
    // --diff's own flags, and the first flag it has no use for.
    let (mut diff_only, mut not_diff) = (None, None);
    let mut size_given = false;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--theme" | "--text" | "--backdrop" | "--ppp" | "--scroll" | "--hold" | "--hash" => {
                preset_only.get_or_insert(flag.clone());
            }
            "--data" | "--photos" | "--set" | "--wait" | "--fullscreen" => {
                live_only.get_or_insert(flag.clone());
            }
            _ => {}
        }
        match flag.as_str() {
            "--diff" => {}
            "--tolerance" | "--out" => {
                diff_only.get_or_insert(flag.clone());
            }
            _ => {
                not_diff.get_or_insert(flag.clone());
            }
        }
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--version" | "-V" => {
                println!("raam {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--pages" => {
                for p in gallery::Page::ALL {
                    println!("{}", p.name());
                }
                for p in frame_ui::PAGES {
                    println!("{p}");
                    for f in frame_ui::FIXTURES {
                        println!("{p}{f}");
                    }
                }
                std::process::exit(0);
            }
            "--diff" => a.diff = Some((val()?.into(), val()?.into())),
            "--tolerance" => {
                a.tolerance = val()?.parse().map_err(|e| format!("--tolerance: {e}"))?
            }
            "--out" => a.out = Some(val()?.into()),
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
            "--data" => a.data = Some(val()?.into()),
            "--photos" => a.photos = Some(val()?.into()),
            "--page" => {
                a.live = false;
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
                size_given = true;
            }
            "--fullscreen" => a.fullscreen = true,
            "--exact" => a.exact = true,
            "--screenshot" => a.screenshot = Some(val()?.into()),
            "--hash" => a.hash = true,
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
                a.script.push(Step::Tap(p, flag == "--click"));
            }
            "--set" => {
                let v = val()?;
                let (name, value) = v
                    .split_once('=')
                    .ok_or(format!("--set wants NAME=VALUE, got {v:?}"))?;
                a.script.push(Step::Set(name.into(), value.into()));
            }
            "--wait" => {
                let v = val()?;
                let secs: f32 = v.parse().map_err(|e| format!("--wait: {e}"))?;
                a.script
                    .push(Step::Wait(Duration::from_secs_f32(secs.max(0.0))));
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
    match (&a.diff, diff_only, not_diff) {
        (Some(_), _, Some(f)) => return Err(format!("{f} is no use to --diff")),
        (None, Some(f), _) => return Err(format!("{f} goes with --diff")),
        (Some(_), _, None) => return Ok(a),
        _ => {}
    }
    match (a.live, preset_only, live_only) {
        (true, Some(f), _) => Err(format!(
            "{f} is the preset host's (--page); the slideshow takes its look from its settings"
        )),
        (false, _, Some(f)) => Err(format!("{f} is the slideshow's (no --page)")),
        _ if a.fullscreen && size_given => {
            Err("--size is no use to --fullscreen: the screen is the monitor's".into())
        }
        _ => Ok(a),
    }
}

/// The window and its GL context, current on this thread.
pub struct Gl {
    pub window: Window,
    pub surface: Surface<WindowSurface>,
    pub context: PossiblyCurrentContext,
    pub max_texture: i32,
}

/// A `size` window with a current GL context and the core's desktop
/// linkage ready (gl.rs). `exact` asks for that many device pixels, so a
/// retina window is half size but its pixels match `adb screencap`.
/// `focus`: the window takes the keyboard focus as it opens.
/// `fullscreen`: the window fills that monitor instead, `size` being the
/// monitor's in pixels.
fn create_gl(
    el: &ActiveEventLoop,
    size: [u32; 2],
    exact: bool,
    focus: bool,
    fullscreen: Option<MonitorHandle>,
) -> Result<Gl, String> {
    let [w, h] = size;
    let mut attrs = Window::default_attributes()
        .with_title("raam")
        .with_inner_size(LogicalSize::new(w, h))
        .with_active(focus);
    if let Some(m) = &fullscreen {
        // On that monitor: a Mac's simple full screen (below) fills the
        // screen the window is on.
        attrs = attrs.with_position(m.position());
        // The monitor's size to start with too: an X server with no window
        // manager ignores the full-screen request.
        #[cfg(not(target_os = "macos"))]
        {
            attrs = attrs
                .with_inner_size(PhysicalSize::new(w, h))
                .with_fullscreen(Some(Fullscreen::Borderless(Some(m.clone()))));
        }
    }
    let template = ConfigTemplateBuilder::new();
    let builder = DisplayBuilder::new();
    // GLES2-capable configs, over EGL (GLX makes GLES contexts only by an
    // extension).
    #[cfg(feature = "gles")]
    let (template, builder) = (
        template.with_api(glutin::config::Api::GLES2),
        builder.with_preference(glutin_winit::ApiPreference::PreferEgl),
    );
    let (window, config) = builder
        .with_window_attributes(Some(attrs))
        // No MSAA: egui feathers its own edges, and the frame has none.
        .build(el, template, |configs| {
            configs.min_by_key(|c| c.num_samples()).unwrap()
        })
        .map_err(|e| format!("no GL config: {e}"))?;
    let window = window.ok_or("no window")?;
    if fullscreen.is_some() {
        // A Mac's own full screen animates into a Space of its own; the
        // simple kind covers the screen in place.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowExtMacOS;
            window.set_simple_fullscreen(true);
        }
    } else if exact {
        let _ = window.request_inner_size(PhysicalSize::new(w, h));
    }
    let display = config.display();
    let raw = window.window_handle().ok().map(|h| h.as_raw());
    // glutin makes a 4.1 core context on macOS whatever is asked; gl.rs
    // bridges the gap to GLES2. With `gles`, GLES2 itself.
    #[cfg(not(feature = "gles"))]
    let api = ContextApi::OpenGl(Some(Version::new(3, 2)));
    #[cfg(feature = "gles")]
    let api = ContextApi::Gles(Some(Version::new(2, 0)));
    let ctx_attrs = ContextAttributesBuilder::new()
        .with_context_api(api)
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
    let mut max_texture = 0;
    unsafe { glGetIntegerv(GL_MAX_TEXTURE_SIZE, &mut max_texture) };
    log::info!(
        "GL {} | {} | window scale {} | GL_MAX_TEXTURE_SIZE {max_texture}",
        unsafe { gl_string(GL_VERSION) },
        unsafe { gl_string(GL_RENDERER) },
        window.scale_factor()
    );
    Ok(Gl {
        window,
        surface,
        context,
        max_texture,
    })
}

/// The back buffer, bottom-up in GL, as a top-down RGB image.
fn read_pixels(w: i32, h: i32) -> golden::Image {
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
    golden::Image {
        w: w as u32,
        h: h as u32,
        rgb,
    }
}

/// The back buffer into a PNG file.
fn save_png(path: &std::path::Path, w: i32, h: i32) -> Result<(), String> {
    golden::write_png(path, &read_pixels(w, h))
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
    if let Some((a, b)) = &args.diff {
        std::process::exit(golden::run_diff(a, b, args.tolerance, args.out.as_deref()));
    }
    let result = if args.live {
        let event_loop = EventLoop::<live::Wake>::with_user_event()
            .build()
            .expect("event loop");
        let mut live = match live::Live::new(args, event_loop.create_proxy()) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        event_loop.run_app(&mut live)
    } else {
        let event_loop = EventLoop::new().expect("event loop");
        event_loop.run_app(&mut preset::Preset::new(args))
    };
    if let Err(e) = result {
        log::error!("event loop: {e}");
    }
}
