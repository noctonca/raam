//! Screen power, through JNI against the NativeActivity (`android_main`'s
//! thread is already attached by android-activity):
//! - the wake alarm: `AlarmManager.setExactAndAllowWhileIdle(RTC_WAKEUP, t,
//!   PendingIntent.getActivity(<this activity>))`. The PendingIntent lives in
//!   system_server, so it fires and relaunches us even if this process was
//!   killed in the night.
//! - turning the screen on: a 3s `SCREEN_BRIGHT | ACQUIRE_CAUSES_WAKEUP |
//!   ON_AFTER_RELEASE` wake lock (normal `WAKE_LOCK` permission).
//! - turning it off: there is no non-root route on API 23 (`goToSleep` needs
//!   the signature-level DEVICE_POWER, `input` needs INJECT_EVENTS,
//!   `DevicePolicyManager.lockNow` a device-admin receiver in Java), so
//!   `su -c 'input keyevent 223'`, as Frameo's own schedule does.
use jni::objects::{JObject, JValue};
use jni::{JNIEnv, JavaVM};
use raam_core::clock;

const RTC_WAKEUP: i32 = 0;
const FLAG_UPDATE_CURRENT: i32 = 0x0800_0000;
const SCREEN_BRIGHT_WAKE_LOCK: i32 = 0x0000_000a;
const ACQUIRE_CAUSES_WAKEUP: i32 = 0x1000_0000;
const ON_AFTER_RELEASE: i32 = 0x2000_0000;
const WAKE_REQUEST_CODE: i32 = 23;

pub struct Power {
    vm: JavaVM,
    activity: jni::sys::jobject,
}

type JResult<T> = Result<T, String>;

fn check(env: &mut JNIEnv, what: &str, e: jni::errors::Error) -> String {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    format!("{what}: {e}")
}

impl Power {
    pub fn new(app: &android_activity::AndroidApp) -> JResult<Self> {
        // SAFETY: the glue's JavaVM pointer is non-null and lives as long as
        // the process.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM) }
            .map_err(|e| e.to_string())?;
        Ok(Self {
            vm,
            activity: app.activity_as_ptr() as jni::sys::jobject,
        })
    }

    fn with_env<T>(&self, f: impl FnOnce(&mut JNIEnv, &JObject) -> JResult<T>) -> JResult<T> {
        let mut env = self.vm.attach_current_thread().map_err(|e| e.to_string())?;
        // SAFETY: `activity` is the glue's global ref to the NativeActivity,
        // valid for the process's life; it is only borrowed here.
        let activity = unsafe { JObject::from_raw(self.activity) };
        let r = env.with_local_frame(16, |env| -> Result<JResult<T>, jni::errors::Error> {
            Ok(f(env, &activity))
        });
        // `activity` is a borrowed global ref owned by the glue: never delete it.
        r.map_err(|e| e.to_string())?
    }

    fn service<'a>(env: &mut JNIEnv<'a>, ctx: &JObject, name: &str) -> JResult<JObject<'a>> {
        let name = env
            .new_string(name)
            .map_err(|e| check(env, "new_string", e))?;
        env.call_method(
            ctx,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[JValue::Object(&name)],
        )
        .and_then(|v| v.l())
        .map_err(|e| check(env, "getSystemService", e))
    }

    fn wake_intent<'a>(env: &mut JNIEnv<'a>, ctx: &JObject) -> JResult<JObject<'a>> {
        let cls = env
            .call_method(ctx, "getClass", "()Ljava/lang/Class;", &[])
            .and_then(|v| v.l())
            .map_err(|e| check(env, "getClass", e))?;
        let intent = env
            .new_object(
                "android/content/Intent",
                "(Landroid/content/Context;Ljava/lang/Class;)V",
                &[JValue::Object(ctx), JValue::Object(&cls)],
            )
            .map_err(|e| check(env, "new Intent", e))?;
        env.call_static_method(
            "android/app/PendingIntent",
            "getActivity",
            "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
            &[
                JValue::Object(ctx),
                JValue::Int(WAKE_REQUEST_CODE),
                JValue::Object(&intent),
                JValue::Int(FLAG_UPDATE_CURRENT),
            ],
        )
        .and_then(|v| v.l())
        .map_err(|e| check(env, "PendingIntent.getActivity", e))
    }

    /// Relaunch this activity at `epoch_ms` (wall clock), replacing any
    /// earlier wake alarm (same PendingIntent, FLAG_UPDATE_CURRENT).
    pub fn set_wake_alarm(&self, epoch_ms: i64) -> JResult<()> {
        self.with_env(|env, ctx| {
            let pi = Self::wake_intent(env, ctx)?;
            let am = Self::service(env, ctx, "alarm")?;
            env.call_method(
                &am,
                "setExactAndAllowWhileIdle",
                "(IJLandroid/app/PendingIntent;)V",
                &[
                    JValue::Int(RTC_WAKEUP),
                    JValue::Long(epoch_ms),
                    JValue::Object(&pi),
                ],
            )
            .map_err(|e| check(env, "setExactAndAllowWhileIdle", e))?;
            Ok(())
        })
    }

    /// Turn the screen on now (no-op if it already is).
    pub fn wake_screen(&self) -> JResult<()> {
        self.with_env(|env, ctx| {
            let pm = Self::service(env, ctx, "power")?;
            let tag = env
                .new_string("video:wake")
                .map_err(|e| check(env, "new_string", e))?;
            let wl = env
                .call_method(
                    &pm,
                    "newWakeLock",
                    "(ILjava/lang/String;)Landroid/os/PowerManager$WakeLock;",
                    &[
                        JValue::Int(
                            SCREEN_BRIGHT_WAKE_LOCK | ACQUIRE_CAUSES_WAKEUP | ON_AFTER_RELEASE,
                        ),
                        JValue::Object(&tag),
                    ],
                )
                .and_then(|v| v.l())
                .map_err(|e| check(env, "newWakeLock", e))?;
            // Timed: it releases itself, and ON_AFTER_RELEASE restarts the
            // user-activity timer so the screen stays on after.
            env.call_method(&wl, "acquire", "(J)V", &[JValue::Long(3000)])
                .map_err(|e| check(env, "WakeLock.acquire", e))?;
            Ok(())
        })
    }

    /// The clip volume, set the way Frameo sets it:
    /// `AudioManager.setStreamVolume(STREAM_MUSIC, index, 0)`, `v` of the
    /// stream's range. System-wide (Frameo shares the stream), and the only
    /// knob that matters: this frame's music stream sat at -50 dB, so a
    /// player-level volume alone was all but silent. Returns (index, max).
    pub fn set_music_volume(&self, v: f32) -> JResult<(i32, i32)> {
        self.with_env(|env, ctx| {
            let am = Self::service(env, ctx, "audio")?;
            let max = env
                .call_method(&am, "getStreamMaxVolume", "(I)I", &[JValue::Int(3)])
                .and_then(|r| r.i())
                .map_err(|e| check(env, "getStreamMaxVolume", e))?;
            let index = (v.clamp(0.0, 1.0) * max as f32).round() as i32;
            env.call_method(
                &am,
                "setStreamVolume",
                "(III)V",
                &[JValue::Int(3), JValue::Int(index), JValue::Int(0)],
            )
            .map_err(|e| check(env, "setStreamVolume", e))?;
            Ok((index, max))
        })
    }

    /// The music output's latency in ms, which OpenSL ES on API 23 doesn't
    /// report: `AudioManager.getOutputLatency(STREAM_MUSIC)`, @hide but
    /// callable over JNI (hidden-API checks start at API 28). It is
    /// AudioFlinger's figure for the output (its HAL buffering).
    pub fn output_latency_ms(&self) -> JResult<i32> {
        self.with_env(|env, ctx| {
            let am = Self::service(env, ctx, "audio")?;
            env.call_method(&am, "getOutputLatency", "(I)I", &[JValue::Int(3)])
                .and_then(|r| r.i())
                .map_err(|e| check(env, "getOutputLatency", e))
        })
    }

    /// `PowerManager.isInteractive()`: whether the screen is on.
    pub fn is_interactive(&self) -> JResult<bool> {
        self.with_env(|env, ctx| {
            let pm = Self::service(env, ctx, "power")?;
            env.call_method(&pm, "isInteractive", "()Z", &[])
                .and_then(|v| v.z())
                .map_err(|e| check(env, "isInteractive", e))
        })
    }
}

/// Turn the screen off through root, off-thread (`input` takes ~1s to start
/// on this device). The result is only logged.
pub fn sleep_screen() {
    std::thread::spawn(|| {
        let t = clock::now();
        match std::process::Command::new("su")
            .args(["-c", "input keyevent 223"])
            .output()
        {
            Ok(out) => log::info!(
                "power: su input keyevent 223 -> {} in {:.2}s{}",
                out.status,
                clock::elapsed(t).as_secs_f64(),
                if out.stderr.is_empty() {
                    String::new()
                } else {
                    format!(" stderr={:?}", String::from_utf8_lossy(&out.stderr))
                }
            ),
            Err(e) => log::error!("power: could not run su: {e}"),
        }
    });
}

/// The controller's Power seam (raam-core seams.rs) over this JNI plumbing.
/// The wake mechanism's flags-only mode is the controller's business (it
/// gets `wakelock_allowed` per pass); here every call does the real thing.
impl raam_core::seams::Power for Power {
    fn set_wake_alarm(&mut self, epoch_ms: i64) -> Result<(), String> {
        Power::set_wake_alarm(self, epoch_ms)
    }

    fn sleep_screen(&mut self) {
        sleep_screen();
    }

    fn wake_screen(&mut self) -> Result<(), String> {
        Power::wake_screen(self)
    }

    fn is_interactive(&self) -> Result<bool, String> {
        Power::is_interactive(self)
    }
}
