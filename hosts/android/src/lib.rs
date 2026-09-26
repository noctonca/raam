//! The Android host: NativeActivity, EGL, input, the video stack
//! (MediaCodec probe/live players, the OES program, SurfaceTexture,
//! OpenSL audio, decoder backoff and the wedge watch), power (wake alarm,
//! wake lock, screen off through root), storage paths and the debug
//! props. The product: applicationId io.github.noctonca.raam, the
//! frame's home app.
//!
//! `android_main` still holds the app controller (input routing, overlay
//! lifecycle, settings save, the sleep state machine); extracting it into
//! raam-core is migration step 5b. Everything portable already lives in
//! raam-core/raam-model, and the data side in raam-engine, reached only
//! through the seams built in `Host`.
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
use raam_core::gl::{GL_RENDERER, GL_VENDOR, GL_VERSION, gl_string, glDisableVertexAttribArray};
use raam_core::overlay::{self, ClockOverlay};
use raam_core::painter::Painter;
use raam_core::schedule::{self, Schedule};
use raam_core::slideshow::{Pipeline, SlideshowSettings};
use raam_core::ui::{self, AppState, Screen};
use raam_core::{clock, collage, switches, weather_icons};
use raam_engine::{db, fetch, immich, library, weather};
use raam_model::limits::{
    AUTO_DISMISS, DEFAULT_MANUAL_IDLE, LARGEST_LAYOUT, MAX_EGUI_WAIT, SAVE_DEBOUNCE, TAP_SLOP_PX,
    UNDO_HIDE,
};
use raam_model::{ClockStyle, FitBackground, GapColour, ScaleMode, SourceKind};
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;

/// The package, as the one-time storage grant needs to name it.
const PACKAGE: &str = "io.github.noctonca.raam";

// ---- the engine's seams (raam-core seams.rs), implemented over Android --

/// `ALooper_wake` is thread-safe by the NDK's contract, so sharing the
/// waker across the engine's threads is sound.
struct AppWaker(AndroidAppWaker);
unsafe impl Sync for AppWaker {}
unsafe impl Send for AppWaker {}

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
/// pipeline gets this as its injected reader — no I/O in the core).
fn mem_free_kb() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemFree:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

struct Touch {
    phase: egui::TouchPhase,
    pos: egui::Pos2,
    device_id: u64,
    touch_id: u64,
    force: f32,
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

fn apply_ui(state: &AppState, pipeline: &mut Pipeline<video::Video>, fetch: &fetch::FetchShared) {
    fetch.set_max_group(state.settings.collage_max);
    pipeline.settings.gap_colour = state.settings.gap_colour;
    // Hidden time never counts: lib.rs pauses the clock itself while there
    // is nothing to draw on, and this only runs while drawing.
    pipeline.clock.set_paused(state.paused);
    pipeline.settings.dwell = Duration::from_secs_f32(state.settings.interval_secs);
    pipeline.settings.transition = state.settings.transition.shader_name();
    pipeline.settings.ken_burns = state.settings.ken_burns_enabled;
    pipeline.settings.fill_by_default = state.settings.fill_by_default;
    pipeline.settings.fit_background = state.settings.fit_background;
    pipeline.settings.video_playback = state.settings.video_playback;
    pipeline.settings.video_sound = state.settings.video_sound;
    pipeline.settings.video_volume = state.settings.video_volume;
    // Test-only: `debug.video.audio_extra_ms` overrides the calibration.
    pipeline.video.audio_extra_ms = props::prop("debug.video.audio_extra_ms")
        .trim()
        .parse()
        .unwrap_or(state.settings.audio_delay_ms);
}

fn min_wait(a: Option<Duration>, b: Duration) -> Option<Duration> {
    Some(a.map_or(b, |a| a.min(b)))
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
    // the worker threads running (seen at boot, 2026-09-26); as the home app,
    // a dead process is started again by the system. `exit`, not `abort`:
    // no crash dialog, and nothing waits on this process's state.
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
    let ctx = egui::Context::default();
    ctx.set_pixels_per_point(1.0);
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
        // place the experiments exported to: a fresh install imports the
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
    // What was last sent for saving, so only changes are written.
    let mut saved_rows = db::settings_rows(&state.settings);
    let mut saved_sleep = (
        state.settings.sleep_enabled,
        state.settings.sleep_min,
        state.settings.wake_min,
    );
    let mut saved_server = (
        state.settings.server_url.clone(),
        state.settings.api_key.clone(),
    );
    let mut saved_cap = state.settings.cache_cap_mb;
    let mut settings_dirty: Option<Duration> = None;
    // The schedule as the user set it, kept while a debug override is on.
    let mut schedule_base = (state.settings.sleep_min, state.settings.wake_min);
    let mut undo: Option<(String, Duration)> = None;
    let mut stats_version = 0u64;

    let mut egl: Option<EglState> = None;
    let mut painter: Option<Painter> = None;
    let mut pipeline: Option<Pipeline<video::Video>> = None;
    let mut clock_overlay: Option<ClockOverlay> = None;
    let mut weather: Option<std::sync::Arc<weather::WeatherShared>> = None;
    // Re-derive the overlay's text only when one of these changes.
    let mut clock_inputs: Option<(u64, u64, ClockStyle, bool)> = None;
    let mut weather_status = String::from("weather: waiting");

    let start = clock::now();
    let mut overlay_open = false;
    let mut closed_down: Option<egui::Pos2> = None;
    let mut egui_down = false;
    let mut last_input = clock::now();
    let mut next_wait: Option<Duration> = Some(Duration::ZERO);
    let mut stats = Stats::default();
    let mut last_log = clock::now();
    // Frame-reuse state: the overlay's geometry lives in the painter's
    // persistent buffers; egui itself only runs when it has input, asked
    // for a repaint that is now due, or the status line changed.
    let mut egui_due = clock::now();
    let mut egui_uploaded = false;
    let mut last_status = String::new();

    // 023: lifecycle and schedule state.
    let power = match power::Power::new(&app) {
        Ok(p) => Some(p),
        Err(e) => {
            log::error!("power: JNI setup failed, no sleep schedule: {e}");
            None
        }
    };
    let mut resumed = false;
    let mut low_memory = false;
    // A Start/Resume arrived: if it is wake time, make sure the screen is on.
    let mut check_wake = false;
    // We turned the screen off and haven't seen it come back yet.
    let mut asleep_since: Option<Duration> = None;
    // Awake by hand in sleep hours: back to sleep once untouched this long.
    let mut manual_wake: Option<Duration> = None;
    // Sleep hours at the last visible evaluation; `None` after being hidden,
    // so only a boundary crossed while in front sends the screen to sleep.
    let mut prev_in_sleep: Option<bool> = None;
    let mut last_touch = clock::now();
    let mut hidden_since: Option<Duration> = Some(clock::now());
    let mut hidden_wakes = 0u32;
    let mut debug = props::DebugProps::default();
    let mut mech = WakeMech::Both;
    let mut manual_idle = DEFAULT_MANUAL_IDLE;
    let mut flags_applied: Option<WakeMech> = None;
    let mut music_volume: Option<f32> = None;

    loop {
        let mut quit = false;
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
                        unsafe { e.release_window() };
                    }
                }
                MainEvent::Start => check_wake = true,
                MainEvent::Resume { .. } => {
                    resumed = true;
                    check_wake = true;
                }
                MainEvent::Pause => resumed = false,
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

        // Settings and the test-only property overrides.
        switches::set_fail(&props::prop("debug.video.fail"));
        let props = props::debug_props();
        if props != debug {
            log::info!("debug props {props:?}");
            // A cleared override puts the default back, so a test schedule
            // can't outlive its test.
            // A cleared override puts the saved schedule back.
            if props.sleep != debug.sleep {
                if debug.sleep.is_none() {
                    schedule_base.0 = state.settings.sleep_min;
                }
                state.settings.sleep_min = props.sleep.unwrap_or(schedule_base.0);
            }
            if props.wake != debug.wake {
                if debug.wake.is_none() {
                    schedule_base.1 = state.settings.wake_min;
                }
                state.settings.wake_min = props.wake.unwrap_or(schedule_base.1);
            }
            manual_idle = props.idle.unwrap_or(DEFAULT_MANUAL_IDLE);
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
        let sched = Schedule {
            enabled: state.settings.sleep_enabled && power.is_some(),
            sleep_min: state.settings.sleep_min,
            wake_min: state.settings.wake_min,
        };
        let (now_sod, now_epoch) = schedule::local_now();
        let in_sleep = sched.asleep_at(now_sod / 60);
        // Time to the next sleep/wake boundary, the loop's longest wait.
        let boundary_wait = sched.enabled.then(|| {
            schedule::until(sched.sleep_min, now_sod).min(schedule::until(sched.wake_min, now_sod))
        });

        if std::mem::take(&mut check_wake) && !in_sleep {
            if let Some(p) = &power {
                let before = p.is_interactive();
                if mech.wakelock() {
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
            if asleep_since.take().is_some() {
                log::info!(
                    "schedule: woken at wake time ({})",
                    schedule::fmt_hm(now_sod / 60)
                );
            }
        }

        // A new window after TerminateWindow: same config and context, so
        // every GL object made before is still there.
        if let Some(e) = egl.as_mut()
            && !e.has_window()
            && let Some(window) = app.native_window()
        {
            let (old_w, old_h) = (e.width, e.height);
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

        // Nothing to draw on: no window, or not resumed (screen off, another
        // activity in front). The slideshow's time stops and the loop blocks
        // until the next event or schedule boundary.
        let has_surface = egl.as_ref().is_some_and(|e| e.has_window());
        if egl.is_some() && !(has_surface && resumed) {
            if hidden_since.is_none() {
                log::info!("hidden (surface={has_surface} resumed={resumed}), slideshow paused");
                hidden_since = Some(clock::now());
                hidden_wakes = 0;
                prev_in_sleep = None;
                if let Some(p) = pipeline.as_mut() {
                    p.clock.set_paused(true);
                    // 027: a playing clip (and its sound) stops with it.
                    p.video.pause_now();
                }
                if overlay_open {
                    overlay_open = false;
                    if let Some(p) = pipeline.as_mut() {
                        p.clear_selection();
                        p.set_menu_open(false);
                    }
                    egui_down = false;
                    egui_uploaded = false;
                    state.reset_on_close();
                    ctx.memory_mut(|m| m.stop_text_input());
                }
                closed_down = None;
            }
            hidden_wakes += 1;
            // Backstop for the alarm: if the process is alive and the CPU
            // awake at wake time, light the screen from here too (whatever is
            // in front then shows; the alarm brings us to the front).
            if asleep_since.is_some() && !in_sleep {
                log::info!("schedule: wake time reached in-process while hidden");
                asleep_since = None;
                if let Some(p) = &power
                    && let Err(e) = p.wake_screen()
                {
                    log::error!("wake: wake lock failed: {e}");
                }
            }
            // Drain touches so they don't replay on return.
            let _ = read_touches(&app);
            next_wait = boundary_wait.map(|w| w + Duration::from_millis(50));
            continue;
        }

        if egl.is_none() {
            let Some(window) = app.native_window() else {
                next_wait = boundary_wait;
                continue;
            };
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
                state.settings.collage_max = default_max.min(LARGEST_LAYOUT);
            }
            state.settings.screen_default_max = default_max;
            let margin = ((2.0 * density as f32 / 160.0) + 0.5) as i32;
            fetch = Some(fetch::spawn(
                host.clone(),
                state.settings.collage_max,
                fetch::Screen {
                    width: e.width,
                    height: e.height,
                    margin: margin.max(1),
                },
                lib.clone(),
            ));
            painter = Some(unsafe { Painter::new() });
            clock_overlay = Some(unsafe { ClockOverlay::new() });
            weather = Some(weather::spawn(host.waker.clone()));
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
                        video_playback: state.settings.video_playback,
                        video_sound: state.settings.video_sound,
                        video_volume: state.settings.video_volume,
                    },
                    video::Video::new(app.create_waker()),
                    mem_free_kb,
                )
            });
            if let (Some(p), Some(o)) = (pipeline.as_mut(), overrides.take()) {
                p.set_overrides(o);
            }
            // Follow-up: the output latency the picture waits for.
            if let (Some(p), Some(pw)) = (pipeline.as_mut(), power.as_ref()) {
                match pw.output_latency_ms() {
                    Ok(ms) => {
                        log::info!("music output latency {ms} ms (AudioManager.getOutputLatency)");
                        p.video.audio_latency_ms = ms.max(0) as u32;
                    }
                    Err(e) => log::warn!("no output latency ({e}), using 0"),
                }
            }
            log::info!("EGL + pipeline + painter ready, {}x{}", e.width, e.height);
            egl = Some(e);
            next_wait = Some(Duration::ZERO);
            continue;
        }
        if let Some(since) = hidden_since.take() {
            log::info!(
                "visible again after {:.1}s hidden ({hidden_wakes} loop wakes while hidden), MemFree={:?}KB",
                clock::elapsed(since).as_secs_f64(),
                mem_free_kb()
            );
            egui_uploaded = false;
            last_log = clock::now();
            stats = Stats::default();
            last_touch = clock::now();
            if asleep_since.take().is_some() && in_sleep {
                log::info!(
                    "schedule: woken by hand in sleep hours, back to sleep after {manual_idle:?} untouched"
                );
                manual_wake = Some(clock::now());
            }
        }

        // The schedule, evaluated only while in front.
        if !in_sleep {
            manual_wake = None;
        }
        if let Some(since) = asleep_since
            && clock::elapsed(since) >= Duration::from_secs(30)
        {
            log::error!(
                "schedule: screen still on 30s after sleeping, treating it as a manual wake"
            );
            asleep_since = None;
            manual_wake = Some(clock::now());
        }
        if asleep_since.is_none() && in_sleep {
            let crossed = prev_in_sleep == Some(false);
            if !crossed && manual_wake.is_none() {
                log::info!(
                    "schedule: in front during sleep hours, sleeping after {manual_idle:?} untouched"
                );
                manual_wake = Some(clock::now());
            }
            let idle_done = manual_wake
                .is_some_and(|m| clock::elapsed(m).min(clock::elapsed(last_touch)) >= manual_idle);
            if (crossed || idle_done)
                && !overlay_open
                && let Some(p) = &power
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
                        asleep_since = Some(clock::now());
                        manual_wake = None;
                        power::sleep_screen();
                    }
                    Err(e) => log::error!("schedule: wake alarm failed, staying awake: {e}"),
                }
            }
        }
        prev_in_sleep = Some(in_sleep);

        let egl_state = egl.as_ref().unwrap();
        let painter = painter.as_mut().unwrap();
        let pipeline = pipeline.as_mut().unwrap();
        let fetch: &fetch::FetchShared = fetch.as_ref().unwrap();
        let clock_overlay = clock_overlay.as_mut().unwrap();
        let weather = weather.as_ref().unwrap();
        let frame_start = clock::now();

        let mut egui_events = Vec::new();
        let mut presses = Vec::new();
        for t in read_touches(&app) {
            last_touch = clock::now();
            if !overlay_open {
                match t.phase {
                    egui::TouchPhase::Start => closed_down = Some(t.pos),
                    egui::TouchPhase::End => {
                        if let Some(d) = closed_down.take()
                            && d.distance(t.pos) <= TAP_SLOP_PX
                        {
                            overlay_open = true;
                            last_input = clock::now();
                            let tile = pipeline.select_at(t.pos.x, t.pos.y);
                            log::info!("overlay opened by tap at {:?} on tile {tile:?}", t.pos);
                        }
                    }
                    egui::TouchPhase::Cancel => closed_down = None,
                    egui::TouchPhase::Move => {}
                }
                continue;
            }
            last_input = clock::now();
            push_egui_touch(&t, &mut egui_down, &mut egui_events, &mut presses);
        }

        apply_ui(&state, pipeline, fetch);
        // 027: the volume is the music stream's (see power.rs), set when it
        // changes while sound is on (and once at start).
        let want_volume = state
            .settings
            .video_sound
            .then_some(state.settings.video_volume);
        if want_volume.is_some() && want_volume != music_volume {
            if let (Some(p), Some(v)) = (&power, want_volume) {
                match p.set_music_volume(v) {
                    Ok((i, max)) => log::info!("music stream volume {i}/{max} ({:.0}%)", v * 100.0),
                    Err(e) => log::error!("setting the music stream volume: {e}"),
                }
            }
            music_volume = want_volume;
        }
        pipeline.set_menu_open(overlay_open);
        let t = clock::now();
        pipeline.update(fetch);
        stats.advance += clock::elapsed(t);
        for (asset, why) in pipeline.video.take_unplayable() {
            lib.send(library::Cmd::SetUnplayable(asset, why));
        }

        let t = clock::now();
        pipeline.draw_frame();
        unsafe {
            for i in 0..8 {
                glDisableVertexAttribArray(i);
            }
        }
        stats.slide += clock::elapsed(t);
        if pipeline.is_transitioning() {
            stats.trans_frames += 1;
        }

        // Clock overlay: over the slideshow (transitions included), under
        // the egui chrome.
        let t = clock::now();
        let minute = epoch_secs() / 60;
        let inputs = (
            minute,
            weather.version(),
            state.settings.clock_style,
            state.settings.clock_24h,
        );
        if clock_inputs != Some(inputs) {
            clock_inputs = Some(inputs);
            let (city, current) = weather.snapshot();
            let w = current.map(|c| {
                let (description, icon) = weather_icons::describe(c.code, c.is_day);
                overlay::Weather {
                    icon: icon.ch(),
                    temp: format!("{}\u{b0}", c.temp_c.round() as i64),
                    description,
                    city: city.clone(),
                }
            });
            weather_status = match (&city, current) {
                (Some(city), Some(c)) => format!(
                    "{city} {:.1}\u{b0}C {}",
                    c.temp_c,
                    weather_icons::describe(c.code, c.is_day).0
                ),
                (Some(city), None) => format!("{city}, weather pending"),
                _ => "locating...".to_string(),
            };
            let now = props::local_time((minute * 60) as i64);
            let content = overlay::content(
                state.settings.clock_style,
                state.settings.clock_24h,
                &now,
                w,
            );
            if clock_overlay.update(
                state.settings.clock_style,
                &content,
                egl_state.width,
                egl_state.height,
            ) {
                stats.clock_rebuild += clock::elapsed(t);
                stats.clock_rebuilds += 1;
                log::info!(
                    "overlay rebuilt ({}): {:?} in {:.2}ms",
                    state.settings.clock_style.label(),
                    content,
                    clock::elapsed(t).as_secs_f64() * 1000.0
                );
            }
        }
        let t = clock::now();
        clock_overlay.draw(egl_state.width, egl_state.height);
        stats.clock_draw += clock::elapsed(t);

        let mut force_redraw = false;
        let mut egui_delay = Duration::MAX;
        if undo
            .as_ref()
            .is_some_and(|u| clock::elapsed(u.1) >= UNDO_HIDE)
        {
            undo = None;
        }
        if overlay_open {
            state.shown_scale = pipeline.shown_scale_mode();
            state.shown_video = pipeline.shown_is_video();
            state.undo_secs = undo
                .as_ref()
                .map(|u| UNDO_HIDE.saturating_sub(clock::elapsed(u.1)).as_secs() + 1);
            let (version, lib_stats) = lib.stats();
            let status = format!(
                "{}  ·  {}  ·  {}  ·  {}  ·  every {:.0}s  ·  back history: {}  ·  {}  ·  {}  ·  {}",
                if state.paused { "Paused" } else { "Playing" },
                pipeline.shown_layout(),
                match state.shown_scale {
                    Some(ScaleMode::Fill) => "Fill frame",
                    Some(ScaleMode::Fit) => "Fit to frame",
                    None => "-",
                },
                state.settings.transition.label(),
                state.settings.interval_secs,
                pipeline.history_len(),
                weather_status,
                if sched.enabled {
                    format!(
                        "sleeps {}-{}",
                        schedule::fmt_hm(sched.sleep_min),
                        schedule::fmt_hm(sched.wake_min)
                    )
                } else {
                    "no sleep schedule".to_string()
                },
                sources_status(&state.settings, &lib_stats, lib.online()),
            );
            let raw_input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(egl_state.width as f32, egl_state.height as f32),
                )),
                time: Some(clock::elapsed(start).as_secs_f64()),
                predicted_dt: 1.0 / 30.0,
                events: egui_events,
                ..Default::default()
            };
            let mut actions = ui::Actions::default();
            let need_run = !egui_uploaded
                || !raw_input.events.is_empty()
                || clock::now() >= egui_due
                || status != last_status
                || version != stats_version;
            if need_run {
                stats_version = version;
                state.library = lib_stats;
                let t = clock::now();
                let mut full_output =
                    ctx.run_ui(raw_input, |ui| ui::draw(ui.ctx(), &mut state, &status));
                stats.run += clock::elapsed(t);

                actions = std::mem::take(&mut state.actions);
                apply_ui(&state, pipeline, fetch);
                if actions.next {
                    pipeline.request_next();
                }
                if actions.prev {
                    pipeline.request_prev(fetch);
                }
                if actions.toggle_scale
                    && let Some((key, mode)) = pipeline.toggle_shown_scale()
                {
                    lib.send(library::Cmd::SetScale(key, mode));
                }
                if actions.hide
                    && let Some((key, asset)) = pipeline.shown_photo()
                {
                    log::info!("hide asset {asset} ({key})");
                    lib.send(library::Cmd::SetHidden(key.clone(), true));
                    pipeline.forget(&key, fetch);
                    undo = Some((key, clock::now()));
                }
                if actions.undo_hide
                    && let Some((key, _)) = undo.take()
                {
                    log::info!("undo hide of {key}");
                    lib.send(library::Cmd::SetHidden(key.clone(), false));
                    pipeline.unforget(&key);
                }
                if let Some(key) = actions.unhide.take() {
                    lib.send(library::Cmd::SetHidden(key.clone(), false));
                    pipeline.unforget(&key);
                }
                for (flag, cmd) in [
                    (actions.clear_cache, library::Cmd::ClearCache),
                    (actions.rescan, library::Cmd::Rescan),
                    (actions.sync_now, library::Cmd::SyncNow),
                    (actions.export, library::Cmd::ExportCuration),
                ] {
                    if flag {
                        lib.send(cmd);
                    }
                }
                for (album, on) in actions.select_album.drain(..) {
                    lib.send(library::Cmd::SelectAlbum(album, on));
                }
                lib.set_enabled(SourceKind::Immich, state.settings.immich_enabled);
                lib.set_enabled(SourceKind::Local, state.settings.local_enabled);
                if state.settings.cache_cap_mb != saved_cap {
                    saved_cap = state.settings.cache_cap_mb;
                    lib.send(library::Cmd::SetCap(saved_cap));
                }
                let rows = db::settings_rows(&state.settings);
                let sleep = (
                    state.settings.sleep_enabled,
                    state.settings.sleep_min,
                    state.settings.wake_min,
                );
                if rows != saved_rows || sleep != saved_sleep {
                    settings_dirty.get_or_insert_with(clock::now);
                }
                if actions.next || actions.prev {
                    pipeline.update(fetch);
                }

                for (id, deltas) in &full_output.textures_delta.set {
                    for delta in deltas {
                        painter.set_texture(*id, delta);
                    }
                }
                let t = clock::now();
                let primitives = ctx.tessellate(
                    std::mem::take(&mut full_output.shapes),
                    full_output.pixels_per_point,
                );
                stats.tess += clock::elapsed(t);
                stats.verts += primitives
                    .iter()
                    .map(|p| match &p.primitive {
                        egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                        _ => 0,
                    })
                    .sum::<usize>();
                let t = clock::now();
                painter.upload(&primitives, egl_state.width, egl_state.height);
                stats.upload += clock::elapsed(t);
                // Textures freed this pass are no longer referenced by the
                // primitives just uploaded, so freeing now is safe.
                for id in &full_output.textures_delta.free {
                    painter.free_texture(*id);
                }
                full_output.textures_delta.clear();
                egui_uploaded = true;
                last_status = status;
                stats.egui_runs += 1;

                let delay = full_output
                    .viewport_output
                    .get(&egui::ViewportId::ROOT)
                    .map_or(Duration::MAX, |v| v.repaint_delay);
                egui_due = clock::now()
                    .checked_add(delay)
                    .unwrap_or_else(|| clock::now() + Duration::from_secs(3600));
            }
            let t = clock::now();
            painter.draw(egl_state.width, egl_state.height);
            stats.paint += clock::elapsed(t);
            stats.egui_frames += 1;
            egui_delay = egui_due.saturating_sub(clock::now());

            let typing = ctx.egui_wants_keyboard_input();
            // `run_ui`'s root Ui is itself a full-screen Background-order
            // layer, so "outside the chrome" means a hit on nothing above it.
            let tapped_outside = presses.iter().any(|p| {
                ctx.layer_id_at(*p)
                    .is_none_or(|l| l.order == egui::Order::Background)
            });
            let timed_out = !typing && clock::elapsed(last_input) >= AUTO_DISMISS;
            if actions.close || tapped_outside || timed_out {
                log::info!(
                    "overlay closed (button={} tap_outside={tapped_outside} timeout={timed_out})",
                    actions.close
                );
                overlay_open = false;
                pipeline.clear_selection();
                pipeline.set_menu_open(false);
                egui_down = false;
                egui_uploaded = false;
                state.reset_on_close();
                ctx.memory_mut(|m| m.stop_text_input());
                force_redraw = true;
                // The server and key apply (and save) when the menu closes,
                // not per keystroke.
                let server = (
                    state.settings.server_url.trim().to_string(),
                    state.settings.api_key.trim().to_string(),
                );
                if server != saved_server && !server.0.is_empty() {
                    saved_server = server.clone();
                    lib.send(library::Cmd::SetServer(immich::Config {
                        url: server.0,
                        key: server.1,
                    }));
                }
            }
        }
        // Save changed settings once they settle, or at once when the menu
        // has closed. A debug schedule override is never saved.
        if let Some(since) = settings_dirty
            && (!overlay_open || clock::elapsed(since) >= SAVE_DEBOUNCE)
        {
            settings_dirty = None;
            let rows = db::settings_rows(&state.settings);
            let sleep = (
                state.settings.sleep_enabled,
                if debug.sleep.is_some() {
                    schedule_base.0
                } else {
                    state.settings.sleep_min
                },
                if debug.wake.is_some() {
                    schedule_base.1
                } else {
                    state.settings.wake_min
                },
            );
            if rows != saved_rows || sleep != saved_sleep {
                saved_rows = rows.clone();
                saved_sleep = sleep;
                lib.send(library::Cmd::SaveSettings { rows, sleep });
            }
        }

        let t = clock::now();
        unsafe { egl_state.swap() };
        stats.swap += clock::elapsed(t);
        stats.total += clock::elapsed(frame_start);
        stats.frames += 1;

        // A scaling change lands in `update` next iteration, so run one more.
        next_wait = if force_redraw || pipeline.is_animating() || pipeline.recompose_pending() {
            Some(Duration::ZERO)
        } else {
            let mut w = pipeline.next_deadline();
            if state.settings.clock_style != ClockStyle::Off {
                w = min_wait(w, until_next_minute());
            }
            if let Some(b) = boundary_wait {
                w = min_wait(w, b + Duration::from_millis(50));
            }
            if let Some(m) = manual_wake {
                let idle = clock::elapsed(m).min(clock::elapsed(last_touch));
                w = min_wait(
                    w,
                    manual_idle.saturating_sub(idle) + Duration::from_millis(50),
                );
            }
            if let Some(since) = settings_dirty {
                w = min_wait(
                    w,
                    SAVE_DEBOUNCE.saturating_sub(clock::elapsed(since)) + Duration::from_millis(10),
                );
            }
            if overlay_open {
                if egui_delay < MAX_EGUI_WAIT {
                    w = min_wait(w, egui_delay);
                }
                if undo.is_some() {
                    w = min_wait(w, Duration::from_millis(250));
                }
                if !ctx.egui_wants_keyboard_input() {
                    w = min_wait(w, AUTO_DISMISS.saturating_sub(clock::elapsed(last_input)));
                }
            }
            w
        };

        let since = clock::elapsed(last_log);
        if since >= Duration::from_secs(2) {
            let overlay = match (overlay_open, state.screen) {
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
                state.keyboard.last_rect().is_some(),
                state.paused,
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

/// e.g. "Immich 213 + folder 6, offline: cached only".
fn sources_status(s: &raam_model::Settings, lib: &raam_model::Stats, online: bool) -> String {
    let mut parts = Vec::new();
    if s.immich_enabled {
        let picked = lib
            .albums
            .iter()
            .filter(|a| a.selected && !a.missing)
            .count();
        parts.push(format!(
            "Immich {} ({picked} album{})",
            lib.immich_assets,
            if picked == 1 { "" } else { "s" }
        ));
    }
    if s.local_enabled {
        parts.push(format!("folder {}", lib.local_ready));
    }
    let mut out = parts.join(" + ");
    if s.immich_enabled && !online {
        out.push_str(", offline: cached only");
    }
    out
}

fn epoch_secs() -> u64 {
    clock::wall().as_secs()
}

/// Time to the next wall-clock minute, plus a few ms so the wake lands after
/// the boundary rather than just before it.
fn until_next_minute() -> Duration {
    let now = clock::wall();
    let into = Duration::from_millis((now.as_millis() % 60_000) as u64);
    Duration::from_secs(60) - into + Duration::from_millis(20)
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

/// 018's synthesis: mouse-style pointer events for buttons/sliders/keyboard
/// plus real `Event::Touch` for `ScrollArea`'s touch-drag-to-scroll.
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
        if unsafe { eglSwapBuffers(self.display, self.surface) } == 0 {
            log::error!("eglSwapBuffers failed: 0x{:x}", unsafe { eglGetError() });
        }
    }
}
