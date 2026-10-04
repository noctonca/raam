//! The Android host: NativeActivity, EGL, input, the video stack
//! (MediaCodec probe/live players, the OES program, SurfaceTexture,
//! OpenSL audio, decoder backoff and the wedge watch), power (wake alarm,
//! wake lock, screen off through root), storage paths and the debug
//! props. The product: applicationId io.github.noctonca.raam, the
//! frame's home app.
//!
//! The product's behaviour lives in raam-core's App controller:
//! each loop pass feeds it the lifecycle and touch events, calls
//! `frame`, executes the effects it returns, and draws around it — the
//! slideshow, the clock overlay, the egui chrome, the swap. Everything
//! portable lives in raam-core/raam-model, and the data side in
//! raam-engine, reached only through the seams built in `Host`.
#![cfg(target_os = "android")]

mod audio_out;
mod egl;
mod extractor;
mod player;
mod power;
mod props;
mod sles;
mod video;
mod video_texture;

use crate::egl::*;
use crate::props::WakeMech;
use android_activity::input::{InputEvent, MotionAction};
use android_activity::{
    AndroidApp, AndroidAppWaker, InputStatus, MainEvent, PollEvent, WindowManagerFlags,
};
use ndk::native_window::NativeWindow;
use raam_core::app::{App, Deps, Event, Inputs, Overrides, Stage, Touch};
use raam_core::frame_ui::{AppState, Screen};
use raam_core::gl::{GL_RENDERER, GL_VENDOR, GL_VERSION, gl_string, glDisableVertexAttribArray};
use raam_core::overlay::ClockOverlay;
use raam_core::painter::Painter;
use raam_core::slideshow::{Pipeline, SlideshowSettings};
use raam_core::{clock, collage, switches};
use raam_engine::{db, fetch, library, weather};
use raam_model::limits::LARGEST_LAYOUT;
use raam_model::{FitBackground, GapColour, SourceKind};
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;

/// The package, as the one-time storage grant needs to name it.
const PACKAGE: &str = "io.github.noctonca.raam";

// ---- the engine's seams (raam-core seams.rs), implemented over Android --

/// Send and Sync through `AndroidAppWaker`, which holds its own reference
/// to the looper, and whose `ALooper_wake` any thread may call.
struct AppWaker(AndroidAppWaker);

impl raam_core::seams::Waker for AppWaker {
    fn wake(&self) {
        self.0.wake();
    }
}

/// `debug.video.*` switches read from Android system properties.
struct PropSwitches;

impl raam_core::seams::DebugSwitches for PropSwitches {
    fn get(&self, name: &str) -> String {
        props::prop(name)
    }
}

/// Clip probing over AMediaExtractor, and this device's decoder ceiling.
struct ExtractorProbe;

impl raam_core::seams::MediaProbe for ExtractorProbe {
    fn probe(&self, path: &str) -> Result<raam_model::ClipInfo, String> {
        extractor::probe(path)
    }

    fn unplayable(&self, info: &raam_model::ClipInfo) -> Option<String> {
        extractor::unplayable(info)
    }
}

/// READ/WRITE_EXTERNAL_STORAGE are runtime permissions here (target SDK 34
/// on API 23). A permission dialog would push us out of the foreground,
/// which the frame's low-memory filter punishes with a kill, so the app
/// grants them to itself through su, as it already turns the screen off
/// (the root map in docs/ARCHITECTURE.md). Android M remounts the running
/// process's storage view on a grant, so no restart is needed. `adb shell
/// pm grant` does the same by hand.
fn grant_storage() -> bool {
    let cmd = format!(
        "pm grant {PACKAGE} android.permission.READ_EXTERNAL_STORAGE; pm grant {PACKAGE} android.permission.WRITE_EXTERNAL_STORAGE"
    );
    match std::process::Command::new("su").args(["-c", &cmd]).output() {
        Ok(o) => {
            log::info!(
                "local: su pm grant storage -> {} {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
            o.status.success()
        }
        Err(e) => {
            log::error!("local: could not run su for pm grant: {e}");
            false
        }
    }
}

/// Free memory as /proc/meminfo reports it, for the log lines (the
/// pipeline and the controller get this as their injected reader — no I/O
/// in the core).
fn mem_free_kb() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemFree:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[derive(Default)]
struct Stats {
    frames: u32,
    egui_frames: u32,
    egui_runs: u32,
    trans_frames: u32,
    advance: Duration,
    slide: Duration,
    run: Duration,
    tess: Duration,
    upload: Duration,
    paint: Duration,
    swap: Duration,
    total: Duration,
    verts: usize,
    clock_draw: Duration,
    clock_rebuild: Duration,
    clock_rebuilds: u32,
}

fn ms(d: Duration, n: u32) -> f64 {
    if n == 0 {
        0.0
    } else {
        d.as_secs_f64() * 1000.0 / n as f64
    }
}

/// The music stream's volume, when the controller asks for it. The
/// stream is the system's (see power.rs), so the host sets it, not the
/// engine.
fn set_music_volume(v: Option<f32>, power: Option<&power::Power>) {
    let (Some(v), Some(p)) = (v, power) else {
        return;
    };
    match p.set_music_volume(v) {
        Ok((i, max)) => log::info!("music stream volume {i}/{max} ({:.0}%)", v * 100.0),
        Err(e) => log::error!("setting the music stream volume: {e}"),
    }
}

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("raam"),
    );
    // The host installs the clock and snapshots the fault flag before
    // anything can read either (raam-core clock.rs, switches.rs).
    clock::set_source(props::clock_source());
    switches::set_fail(&props::prop("debug.video.fail"));
    // A panic on any thread ends the process. android-activity would catch
    // one in android_main and leave the process up with a frozen window and
    // the worker threads running (seen at boot); as the home app, a dead
    // process is started again by the system. `exit`, not `abort`: no crash
    // dialog, and nothing waits on this process's state.
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        log::error!(
            "panic on thread {}: {info}; exiting so the system restarts us",
            thread.name().unwrap_or("?")
        );
        std::process::exit(70);
    }));
    // Test-only: `debug.video.fail=panic` panics right after startup.
    if switches::fail() == switches::Fail::Panic {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            panic!("test panic (debug.video.fail=panic)");
        });
    }
    log::info!("android_main started");
    // The JavaVM for the video texture's JNI (player.rs).
    player::init_jvm(app.vm_as_ptr());

    let mut fetch: Option<std::sync::Arc<fetch::FetchShared>> = None;
    // Nothing is baked in: a fresh install starts with no server and no
    // key, both entered in settings.
    let mut state = AppState::new("", "");

    // The host's side of the engine's seams.
    let host = raam_engine::Host {
        waker: Arc::new(AppWaker(app.create_waker())),
        switches: Arc::new(PropSwitches),
        probe: Arc::new(ExtractorProbe),
        grant_storage: Arc::new(grant_storage),
    };

    // The database, its saved settings, and the library threads.
    let files_dir = app
        .internal_data_path()
        .unwrap_or_else(|| std::path::PathBuf::from(format!("/data/data/{PACKAGE}/files")));
    let _ = std::fs::create_dir_all(&files_dir);
    let paths = raam_engine::Paths {
        files_dir: files_dir.clone(),
        local_dir_default: "/sdcard/Pictures/Frame".to_string(),
        // Next to the folder, not in it, so it is never scanned. The same
        // place the prototype exported to: a fresh install imports the
        // curation that is already there.
        curation_export: std::path::PathBuf::from("/sdcard/Pictures/frame-curation.json"),
    };
    let t = clock::now();
    let database =
        db::open(&files_dir.join("raam.db"), &paths.local_dir_default).unwrap_or_else(|e| {
            log::error!("db: {e}; running on an in-memory database, nothing will be saved");
            db::open(std::path::Path::new(":memory:"), &paths.local_dir_default)
                .expect("in-memory database")
        });
    let (saved_keys, overrides) = {
        let conn = database.lock().unwrap();
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
    let mut overrides = Some(overrides);
    let lib = library::spawn(database, paths, state.settings.cache_cap_mb, host.clone());
    state.settings.immich_enabled = lib.enabled(SourceKind::Immich);
    state.settings.local_enabled = lib.enabled(SourceKind::Local);
    // The controller takes the loaded state; its saved-settings snapshots
    // start from it, so nothing writes at boot.
    let mut controller = App::new(state, mem_free_kb);

    let mut egl: Option<EglState> = None;
    let mut painter: Option<Painter> = None;
    let mut pipeline: Option<Pipeline<video::Decoders>> = None;
    let mut clock_overlay: Option<ClockOverlay> = None;
    let mut weather: Option<std::sync::Arc<weather::WeatherShared>> = None;

    let mut next_wait: Option<Duration> = Some(Duration::ZERO);
    let mut stats = Stats::default();
    let mut last_log = clock::now();

    // Lifecycle and schedule plumbing.
    let mut power = match power::Power::new(&app) {
        Ok(p) => Some(p),
        Err(e) => {
            log::error!("power: JNI setup failed, no sleep schedule: {e}");
            None
        }
    };
    let mut low_memory = false;
    let mut debug = props::DebugProps::default();
    let mut mech = WakeMech::Both;
    let mut flags_applied: Option<WakeMech> = None;

    loop {
        let mut quit = false;
        let mut events: Vec<Event> = Vec::new();
        app.poll_events(next_wait, |event| {
            let PollEvent::Main(event) = event else {
                return;
            };
            let name = match &event {
                MainEvent::InputAvailable => return,
                MainEvent::InitWindow { .. } => "InitWindow",
                MainEvent::TerminateWindow { .. } => "TerminateWindow",
                MainEvent::WindowResized { .. } => "WindowResized",
                MainEvent::RedrawNeeded { .. } => "RedrawNeeded",
                MainEvent::ContentRectChanged { .. } => "ContentRectChanged",
                MainEvent::GainedFocus => "GainedFocus",
                MainEvent::LostFocus => "LostFocus",
                MainEvent::ConfigChanged { .. } => "ConfigChanged",
                MainEvent::LowMemory => "LowMemory",
                MainEvent::Start => "Start",
                MainEvent::Resume { .. } => "Resume",
                MainEvent::SaveState { .. } => "SaveState",
                MainEvent::Pause => "Pause",
                MainEvent::Stop => "Stop",
                MainEvent::Destroy => "Destroy",
                MainEvent::InsetsChanged { .. } => "InsetsChanged",
                _ => "other",
            };
            log::info!("lifecycle: {name}");
            match event {
                MainEvent::Destroy => quit = true,
                // The glue releases the ANativeWindow once this callback
                // returns, so the EGL surface must go now, not next pass.
                MainEvent::TerminateWindow { .. } => {
                    if let Some(e) = egl.as_mut() {
                        // SAFETY: on the render thread, which owns the EGL
                        // state, while the glue's window is still alive.
                        unsafe { e.release_window() };
                    }
                }
                MainEvent::Start => events.push(Event::Start),
                MainEvent::Resume { .. } => events.push(Event::Resume),
                MainEvent::Pause => events.push(Event::Pause),
                MainEvent::LowMemory => low_memory = true,
                _ => {}
            }
        });
        if quit {
            break;
        }
        if low_memory {
            low_memory = false;
            log::warn!(
                "LowMemory, MemFree={:?}KB (nothing to trim yet)",
                mem_free_kb()
            );
        }

        // The test-only property overrides. The schedule ones go to the
        // controller as inputs; the wake mechanism is host business.
        switches::set_fail(&props::prop("debug.video.fail"));
        switches::set_video_holds(
            props::prop("debug.video.hold_first").trim() == "1",
            props::prop("debug.video.show_still").trim() == "1",
        );
        let props = props::debug_props();
        if props != debug {
            log::info!("debug props {props:?}");
            mech = WakeMech::from_prop(&props.mech);
            debug = props;
        }
        if flags_applied != Some(mech) {
            // KEEP_SCREEN_ON while in front; TURN_SCREEN_ON so the window
            // lights the screen when the alarm brings it back.
            let screen_on =
                WindowManagerFlags::TURN_SCREEN_ON | WindowManagerFlags::SHOW_WHEN_LOCKED;
            if mech.flags() {
                app.set_window_flags(
                    WindowManagerFlags::KEEP_SCREEN_ON | screen_on,
                    WindowManagerFlags::empty(),
                );
            } else {
                app.set_window_flags(WindowManagerFlags::KEEP_SCREEN_ON, screen_on);
            }
            log::info!("wake mechanism {mech:?}");
            flags_applied = Some(mech);
        }
        for t in read_touches(&app) {
            events.push(Event::Touch(t));
        }
        let overrides_in = Overrides {
            sleep: debug.sleep,
            wake: debug.wake,
            idle: debug.idle,
        };

        // A new window after TerminateWindow: same config and context, so
        // every GL object made before is still there.
        if let Some(e) = egl.as_mut()
            && !e.has_window()
            && let Some(window) = app.native_window()
        {
            let (old_w, old_h) = (e.width, e.height);
            // SAFETY: on the render thread that owns the EGL state, and the
            // old surface was released at TerminateWindow; `window` is a
            // live window the glue just handed out.
            match unsafe { e.attach_window(&window) } {
                Ok(()) => {
                    log::info!("window surface re-created {}x{}", e.width, e.height);
                    if (e.width, e.height) != (old_w, old_h) {
                        // Never seen on this fixed-landscape frame; the pipeline
                        // keeps its original size, so this is only logged.
                        log::warn!(
                            "window size changed {old_w}x{old_h} -> {}x{}",
                            e.width,
                            e.height
                        );
                    }
                }
                Err(err) => log::error!("window surface failed: {err}"),
            }
        }

        if egl.is_none() {
            // The controller runs before the first window too, so a boot
            // in wake hours lights the screen.
            let out = controller.frame(
                &events,
                &Inputs {
                    has_surface: false,
                    screen: (0, 0),
                    wakelock_allowed: mech.wakelock(),
                    overrides: overrides_in,
                },
                &mut Deps {
                    stage: None,
                    power: power
                        .as_mut()
                        .map(|p| p as &mut dyn raam_core::seams::Power),
                },
            );
            raam_engine::run_effects(out.effects, &lib, None, None);
            set_music_volume(out.music_volume, power.as_ref());
            let Some(window) = app.native_window() else {
                next_wait = out.wait;
                continue;
            };
            // SAFETY: on the render thread, which keeps the context made
            // here current for the rest of the process; `window` is live.
            let e = match unsafe { EglState::new(&window) } {
                Ok(e) => e,
                Err(err) => {
                    log::error!("EGL setup failed: {err}");
                    next_wait = Some(Duration::from_millis(500));
                    continue;
                }
            };
            // The default collage max comes from the screen diagonal
            // (Frameo's rule): DisplayMetrics' pixel size over densityDpi.
            let density = app.config().density().unwrap_or(160);
            let default_max = collage::screen_default_max(e.width, e.height, density);
            log::info!(
                "screen {}x{} @ {density}dpi = {:.2}\" diagonal -> default collage max {default_max}",
                e.width,
                e.height,
                collage::screen_diagonal_inches(e.width, e.height, density),
            );
            if !saved_keys.iter().any(|k| k == "collage.max") {
                controller.state.settings.collage_max = default_max.min(LARGEST_LAYOUT);
            }
            controller.state.settings.screen_default_max = default_max;
            let margin = ((2.0 * density as f32 / 160.0) + 0.5) as i32;
            fetch = Some(fetch::spawn(
                host.clone(),
                controller.state.settings.collage_max,
                fetch::Screen {
                    width: e.width,
                    height: e.height,
                    margin: margin.max(1),
                },
                lib.clone(),
            ));
            // SAFETY: `EglState::new` just made the context current on this
            // thread, and it stays current here for the painter's life.
            painter = Some(unsafe { Painter::new() });
            // The theme's text mode is the shader boost, chosen on the frame:
            // right for light and dark text alike, and a theme switch never
            // rebuilds the font atlas.
            painter.as_mut().unwrap().text_boost = true;
            // SAFETY: the same current context, kept for the overlay's life.
            clock_overlay = Some(unsafe { ClockOverlay::new() });
            weather = Some(weather::spawn(host.waker.clone()));
            // SAFETY: the same current context, kept for the pipeline's life.
            pipeline = Some(unsafe {
                Pipeline::new(
                    e.width,
                    e.height,
                    density,
                    SlideshowSettings {
                        dwell: Duration::from_secs(10),
                        transition: None,
                        ken_burns: true,
                        fill_by_default: true,
                        fit_background: FitBackground::Blurred,
                        gap_colour: GapColour::Black,
                        video_playback: controller.state.settings.video_playback,
                        video_sound: controller.state.settings.video_sound,
                    },
                    video::Decoders::new(app.create_waker()),
                    mem_free_kb,
                )
            });
            if let (Some(p), Some(o)) = (pipeline.as_mut(), overrides.take()) {
                p.set_overrides(o);
            }
            // The output latency the picture waits for.
            if let (Some(p), Some(pw)) = (pipeline.as_mut(), power.as_ref()) {
                match pw.output_latency_ms() {
                    Ok(ms) => {
                        log::info!("music output latency {ms} ms (AudioManager.getOutputLatency)");
                        p.video.player_mut().audio_latency_ms = ms.max(0) as u32;
                    }
                    Err(e) => log::warn!("no output latency ({e}), using 0"),
                }
            }
            log::info!("EGL + pipeline + painter ready, {}x{}", e.width, e.height);
            egl = Some(e);
            next_wait = Some(Duration::ZERO);
            continue;
        }

        let egl_state = egl.as_ref().unwrap();
        let frame_start = clock::now();
        // Test-only: `debug.video.audio_extra_ms` overrides the calibration.
        let p = pipeline.as_mut().unwrap();
        p.video.player_mut().audio_extra_ms = props::prop("debug.video.audio_extra_ms")
            .trim()
            .parse()
            .unwrap_or(controller.state.settings.audio_delay_ms);

        let fetch_ref: &fetch::FetchShared = fetch.as_ref().unwrap();
        let out = controller.frame(
            &events,
            &Inputs {
                has_surface: egl_state.has_window(),
                screen: (egl_state.width, egl_state.height),
                wakelock_allowed: mech.wakelock(),
                overrides: overrides_in,
            },
            &mut Deps {
                stage: Some(Stage {
                    slideshow: p,
                    source: fetch_ref,
                    library: lib.as_ref(),
                    weather: Some(weather.as_ref().unwrap().as_ref()),
                    // Wi-Fi setup on Android is still to come.
                    network: None,
                }),
                power: power
                    .as_mut()
                    .map(|p| p as &mut dyn raam_core::seams::Power),
            },
        );
        raam_engine::run_effects(out.effects, &lib, fetch.as_deref(), weather.as_deref());
        set_music_volume(out.music_volume, power.as_ref());
        if out.became_visible {
            stats = Stats::default();
            last_log = clock::now();
        }
        if out.skip_draw {
            next_wait = out.wait;
            continue;
        }
        stats.advance += out.advance;
        let pipeline = pipeline.as_mut().unwrap();
        for (asset, why) in pipeline.video.take_unplayable() {
            lib.send(library::Cmd::SetUnplayable(asset, why));
        }

        // The full-screen settings cover everything: skip the slideshow
        // and overlay draws under them (the controller's opaque lever).
        if !out.chrome_opaque {
            let t = clock::now();
            pipeline.draw_frame();
            // SAFETY: plain values to GL with the context current on this
            // render thread (on the pbuffer if the window is gone).
            unsafe {
                for i in 0..8 {
                    glDisableVertexAttribArray(i);
                }
            }
            stats.slide += clock::elapsed(t);
            if pipeline.is_transitioning() {
                stats.trans_frames += 1;
            }
        }

        // Clock overlay: over the slideshow (transitions included), under
        // the egui chrome. The controller says when the text changed; the
        // rebuild is applied even under opaque settings so the text is
        // current when they close.
        let clock_overlay = clock_overlay.as_mut().unwrap();
        if let Some(r) = out.overlay {
            let t = clock::now();
            if clock_overlay.update(
                r.style,
                r.corner,
                &r.content,
                egl_state.width,
                egl_state.height,
            ) {
                stats.clock_rebuild += clock::elapsed(t);
                stats.clock_rebuilds += 1;
                log::info!(
                    "overlay rebuilt ({}, {}): {:?} in {:.2}ms",
                    r.style.label(),
                    r.corner.label(),
                    r.content,
                    clock::elapsed(t).as_secs_f64() * 1000.0
                );
            }
        }
        if !out.chrome_opaque {
            let t = clock::now();
            clock_overlay.draw(egl_state.width, egl_state.height);
            stats.clock_draw += clock::elapsed(t);
        }

        // The egui chrome: upload a fresh run's output, then draw from the
        // painter's persistent buffers while the menu is up.
        let painter = painter.as_mut().unwrap();
        if let Some(mut e) = out.egui {
            for (id, deltas) in &e.textures_delta.set {
                for delta in deltas {
                    painter.set_texture(*id, delta);
                }
            }
            stats.run += e.run;
            stats.tess += e.tess;
            stats.verts += e
                .primitives
                .iter()
                .map(|p| match &p.primitive {
                    egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                    _ => 0,
                })
                .sum::<usize>();
            let t = clock::now();
            // The frame runs at pixels_per_point 1.0 (160 dpi: 1 dp = 1 px).
            painter.upload(&e.primitives, 1.0, egl_state.width, egl_state.height);
            stats.upload += clock::elapsed(t);
            // Textures freed this pass are no longer referenced by the
            // primitives just uploaded, so freeing now is safe.
            for id in &e.textures_delta.free {
                painter.free_texture(*id);
            }
            e.textures_delta.clear();
            stats.egui_runs += 1;
        }
        if out.draw_egui {
            let t = clock::now();
            painter.draw(1.0, egl_state.width, egl_state.height);
            stats.paint += clock::elapsed(t);
            stats.egui_frames += 1;
        }

        let t = clock::now();
        // SAFETY: on the render thread that owns the EGL state; `swap`
        // skips a released (null) window surface.
        unsafe { egl_state.swap() };
        stats.swap += clock::elapsed(t);
        stats.total += clock::elapsed(frame_start);
        stats.frames += 1;
        next_wait = out.wait;

        let since = clock::elapsed(last_log);
        if since >= Duration::from_secs(2) {
            let overlay = match (controller.overlay_open(), controller.state.screen) {
                (false, _) => "closed",
                (true, Screen::Menu) => "menu",
                (true, Screen::Settings) => "settings",
            };
            let n = stats.frames;
            let e = stats.egui_frames;
            let r = stats.egui_runs;
            log::info!(
                "stats fps={:.1} overlay={overlay} kb={} paused={} trans_frames={} egui_frames={e} egui_runs={r} \
                 ms/frame: advance={:.2} slide={:.2} swap={:.2} total={:.2} | ms/egui_run: run_ui={:.2} tess={:.2} \
                 upload={:.2} | ms/egui_frame: draw={:.2} | verts/run={} | clock: draw={:.3}ms/frame rebuilds={} \
                 rebuild={:.2}ms MemFree={:?}KB",
                n as f64 / since.as_secs_f64(),
                controller.state.keyboard.last_rect().is_some(),
                controller.state.paused,
                stats.trans_frames,
                ms(stats.advance, n),
                ms(stats.slide, n),
                ms(stats.swap, n),
                ms(stats.total, n),
                ms(stats.run, r),
                ms(stats.tess, r),
                ms(stats.upload, r),
                ms(stats.paint, e),
                if r == 0 { 0 } else { stats.verts / r as usize },
                ms(stats.clock_draw, n),
                stats.clock_rebuilds,
                ms(stats.clock_rebuild, stats.clock_rebuilds),
                mem_free_kb(),
            );
            if let Some(d) = pipeline.video.debug_line() {
                log::info!("live clip: {d}");
            }
            stats = Stats::default();
            last_log = clock::now();
        }
    }
    log::info!("exiting");
}

fn read_touches(app: &AndroidApp) -> Vec<Touch> {
    let mut out = Vec::new();
    match app.input_events_iter() {
        Ok(mut iter) => loop {
            let read = iter.next(|event| {
                let InputEvent::MotionEvent(motion) = event else {
                    return InputStatus::Unhandled;
                };
                if motion.pointer_count() > 0 {
                    let p = motion.pointer_at_index(0);
                    let phase = match motion.action() {
                        MotionAction::Down => Some(egui::TouchPhase::Start),
                        MotionAction::Move => Some(egui::TouchPhase::Move),
                        MotionAction::Up => Some(egui::TouchPhase::End),
                        MotionAction::Cancel => Some(egui::TouchPhase::Cancel),
                        _ => None,
                    };
                    if let Some(phase) = phase {
                        out.push(Touch {
                            phase,
                            pos: egui::pos2(p.x(), p.y()),
                            device_id: motion.device_id() as u32 as u64,
                            touch_id: p.pointer_id() as u32 as u64,
                            force: p.pressure().clamp(0.0, 1.0),
                        });
                    }
                }
                InputStatus::Handled
            });
            if !read {
                break;
            }
        },
        Err(err) => log::error!("input_events_iter failed: {err:?}"),
    }
    out
}

struct EglState {
    display: EglDisplay,
    config: EglConfig,
    context: EglContext,
    /// Current while there is no window, so the context and its GL objects
    /// outlive the window surface (surfaceless contexts aren't assumed on
    /// the Mali-400).
    pbuffer: EglSurface,
    /// Null between TerminateWindow and the next InitWindow.
    surface: EglSurface,
    width: i32,
    height: i32,
}

impl EglState {
    unsafe fn new(window: &NativeWindow) -> Result<Self, String> {
        // SAFETY: each EGL handle is checked non-null (or the call checked
        // for success) before the next call uses it; the attribute lists are
        // EGL_NONE-terminated locals and the out-pointers live locals, all
        // outliving their calls. GL strings are read only once
        // `attach_window` has made the context current.
        unsafe {
            let display = eglGetDisplay(std::ptr::null_mut());
            if display.is_null() {
                return Err("eglGetDisplay returned null".into());
            }
            let (mut major, mut minor) = (0i32, 0i32);
            if eglInitialize(display, &mut major, &mut minor) == 0 {
                return Err(format!("eglInitialize failed: 0x{:x}", eglGetError()));
            }

            let attribs = [
                EGL_SURFACE_TYPE,
                EGL_WINDOW_BIT | EGL_PBUFFER_BIT,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES2_BIT,
                EGL_RED_SIZE,
                8,
                EGL_GREEN_SIZE,
                8,
                EGL_BLUE_SIZE,
                8,
                EGL_NONE,
            ];
            let mut config: EglConfig = std::ptr::null_mut();
            let mut num_config: EglInt = 0;
            if eglChooseConfig(display, attribs.as_ptr(), &mut config, 1, &mut num_config) == 0
                || num_config == 0
            {
                return Err("eglChooseConfig found no window+pbuffer config".into());
            }
            let ctx_attribs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            let context =
                eglCreateContext(display, config, std::ptr::null_mut(), ctx_attribs.as_ptr());
            if context.is_null() {
                return Err(format!("eglCreateContext failed: 0x{:x}", eglGetError()));
            }
            let pb_attribs = [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE];
            let pbuffer = eglCreatePbufferSurface(display, config, pb_attribs.as_ptr());
            if pbuffer.is_null() {
                return Err(format!(
                    "eglCreatePbufferSurface failed: 0x{:x}",
                    eglGetError()
                ));
            }
            let mut s = Self {
                display,
                config,
                context,
                pbuffer,
                surface: std::ptr::null_mut(),
                width: 0,
                height: 0,
            };
            s.attach_window(window)?;
            log::info!(
                "EGL {major}.{minor} | vendor={} renderer={} version={}",
                gl_string(GL_VENDOR),
                gl_string(GL_RENDERER),
                gl_string(GL_VERSION),
            );
            Ok(s)
        }
    }

    fn has_window(&self) -> bool {
        !self.surface.is_null()
    }

    unsafe fn attach_window(&mut self, window: &NativeWindow) -> Result<(), String> {
        // SAFETY: `display`, `config` and `context` were made and checked in
        // `new` and are never destroyed; `window` is a live ANativeWindow
        // (EGL takes its own reference); a surface that fails to become
        // current is destroyed once and never stored.
        unsafe {
            let surface = eglCreateWindowSurface(
                self.display,
                self.config,
                window.ptr().as_ptr() as *mut c_void,
                std::ptr::null(),
            );
            if surface.is_null() {
                return Err(format!(
                    "eglCreateWindowSurface failed: 0x{:x}",
                    eglGetError()
                ));
            }
            if eglMakeCurrent(self.display, surface, surface, self.context) == 0 {
                let err = eglGetError();
                eglDestroySurface(self.display, surface);
                return Err(format!("eglMakeCurrent(window) failed: 0x{err:x}"));
            }
            self.surface = surface;
            self.width = window.width();
            self.height = window.height();
            Ok(())
        }
    }

    /// Before the ANativeWindow goes away: park the context on the pbuffer
    /// and drop the window surface. GL objects are untouched.
    unsafe fn release_window(&mut self) {
        if self.surface.is_null() {
            return;
        }
        // SAFETY: `display`, `pbuffer` and `context` live for the process;
        // `surface` is non-null (checked above) and ours, destroyed once
        // here and then nulled, after the context has moved off it.
        unsafe {
            if eglMakeCurrent(self.display, self.pbuffer, self.pbuffer, self.context) == 0 {
                log::error!("eglMakeCurrent(pbuffer) failed: 0x{:x}", eglGetError());
            }
            if eglDestroySurface(self.display, self.surface) == 0 {
                log::error!("eglDestroySurface failed: 0x{:x}", eglGetError());
            }
        }
        self.surface = std::ptr::null_mut();
        log::info!("window surface released, context kept on the pbuffer");
    }

    /// Swap, logging failures (e.g. EGL_BAD_SURFACE) instead of panicking.
    unsafe fn swap(&self) {
        if self.surface.is_null() {
            return;
        }
        // SAFETY: `display` lives for the process and `surface` is non-null
        // (checked above), so the live window surface made current earlier.
        if unsafe { eglSwapBuffers(self.display, self.surface) } == 0 {
            // SAFETY: no arguments; reads this thread's last EGL error.
            log::error!("eglSwapBuffers failed: 0x{:x}", unsafe { eglGetError() });
        }
    }
}
