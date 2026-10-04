//! One clip's playback: the decode threads (MediaCodec video onto a
//! surface, AAC to PCM for OpenSL ES) and the SurfaceTexture bridge,
//! organised around what the slideshow needs:
//!
//! - **Frame 0 first, then wait.** The video thread renders the first frame
//!   at once and waits for `play`. The render thread latches it, which is
//!   how a video slide is composed before its transition (a "probe" player
//!   is stopped right there) and how live playback starts on exactly the
//!   frame the transition ended on.
//! - **Every rendered frame is stamped** (`releaseOutputBufferAtTime` with a
//!   sequence number), so after `updateTexImage` the render thread can read
//!   back which frame is on the texture (`getTimestamp`). The first and last
//!   frames are then composed only once they are really latched.
//! - **One media clock** (`Ctl`), started by `play` and stopped by pause,
//!   paces the video (a frame more than 60 ms late is dropped, not shown)
//!   and starts the audio. The audio thread reports how much OpenSL ES has
//!   played, for the drift measurement.
//! - **Stop** deletes the GL texture at once and hands the threads and the
//!   Java objects to a reaper thread, so a slide change never waits on a
//!   codec teardown. The codec is stopped before the `SurfaceTexture` is
//!   released, which needs the frame composed out of it first: the caller
//!   copies the latched frame into a GL_TEXTURE_2D before `stop`.
use crate::audio_out::AudioOut;
use crate::extractor::{Extractor, OpenError};
use crate::video_texture::VideoTexture;
use android_activity::AndroidAppWaker;
use jni::{JNIEnv, JavaVM};
use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
};
use ndk::native_window::NativeWindow;
use raam_core::clock;
use raam_core::gl::*;
use raam_core::video::{OpenClip, Phase, PlayerError};
use raam_model::ClipInfo;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

/// A frame later than this is dropped rather than shown late.
use raam_model::limits::DROP_LATE_US;
/// About 200 ms of 44.1 kHz stereo PCM per enqueue, so `Enqueue` runs a
/// few times a second rather than once per decoded buffer.
use raam_model::limits::PCM_BATCH_BYTES;

static VM: OnceLock<JavaVM> = OnceLock::new();

/// Video decoders created and not yet released, probes included. The RK
/// VPU maps its buffers from ion, and on this 493 MB frame a second
/// decoder opened while another's buffers are still held has run ion out
/// ("vpu_dmabuf_map: ion map failed"), so the core opens one only when
/// this is 0 (`VideoPlayer::decoders_open`).
static VIDEO_DECODERS: AtomicU32 = AtomicU32::new(0);

pub fn video_decoders() -> u32 {
    VIDEO_DECODERS.load(Ordering::Acquire)
}

/// Counts one decoder in `VIDEO_DECODERS` until dropped.
struct DecoderSlot;

impl DecoderSlot {
    fn take() -> Self {
        VIDEO_DECODERS.fetch_add(1, Ordering::AcqRel);
        DecoderSlot
    }
}

impl Drop for DecoderSlot {
    fn drop(&mut self) {
        VIDEO_DECODERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Keeps the process's JavaVM for the render thread's and the reaper's JNI.
pub fn init_jvm(vm_ptr: *mut std::ffi::c_void) {
    if VM.get().is_none()
        && let Ok(vm) = unsafe { JavaVM::from_raw(vm_ptr as *mut jni::sys::JavaVM) }
    {
        let _ = VM.set(vm);
    }
}

/// This thread's JNIEnv, attaching it for good if it isn't yet
/// (`android_main`'s thread already is).
pub fn env() -> Result<JNIEnv<'static>, String> {
    VM.get()
        .ok_or("no JavaVM")?
        .attach_current_thread_permanently()
        .map_err(|e| format!("attach: {e}"))
}

/// Where the decode thread is. The core sees `Phase`, which also asks
/// whether that frame is on the texture yet (`OpenClip::phase`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DecoderPhase {
    /// Opening the clip, configuring the decoder.
    Starting,
    /// Frame 0 is rendered; waiting for `play`.
    FirstFrame,
    Playing,
    /// The last frame is rendered and playback is over (not looping).
    Ended,
    Failed,
}

impl DecoderPhase {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => DecoderPhase::Starting,
            1 => DecoderPhase::FirstFrame,
            2 => DecoderPhase::Playing,
            3 => DecoderPhase::Ended,
            _ => DecoderPhase::Failed,
        }
    }
}

#[derive(Default)]
struct Ctl {
    play: bool,
    paused: bool,
    stop: bool,
    looping: bool,
    /// The media clock (µs, can start below 0: see `align`): time played
    /// so far, plus the running stretch.
    clock_acc_us: i64,
    clock_since: Option<Duration>,
    /// With sound, the clock doesn't run on `play` (or on a resume) but
    /// when OpenSL ES's position first moves, i.e. when the first sample
    /// has really left the mixer; the audio thread then sets it to that
    /// position less the output latency. Until then the picture holds.
    /// Gives up after `ALIGN_TIMEOUT`.
    align: bool,
    align_since: Option<Duration>,
}

/// If the sound hasn't started by then, the picture starts without it.
use raam_model::limits::AUDIO_ALIGN_TIMEOUT as ALIGN_TIMEOUT;

impl Ctl {
    fn now_us(&self) -> i64 {
        self.clock_acc_us
            + self
                .clock_since
                .map_or(0, |t| clock::elapsed(t).as_micros() as i64)
    }

    /// Playing: the sound plays (and the clock runs, unless aligning).
    fn running(&self) -> bool {
        self.play && !self.paused && !self.stop
    }

    fn clock_running(&self) -> bool {
        self.running() && !self.align
    }

    fn update_clock(&mut self) {
        if self.align
            && self
                .align_since
                .is_some_and(|t| clock::elapsed(t) > ALIGN_TIMEOUT)
        {
            log::warn!(
                "the sound didn't start within {ALIGN_TIMEOUT:?}, the picture goes on without waiting"
            );
            self.align = false;
        }
        match (self.clock_running(), self.clock_since) {
            (true, None) => self.clock_since = Some(clock::now()),
            (false, Some(t)) => {
                self.clock_acc_us += clock::elapsed(t).as_micros() as i64;
                self.clock_since = None;
            }
            _ => {}
        }
    }
}

struct Shared {
    ctl: Mutex<Ctl>,
    cond: Condvar,
    phase: AtomicU8,
    /// Stamp (ns) of the first and of the latest frame sent to the surface.
    first_stamp: AtomicI64,
    last_stamp: AtomicI64,
    rendered: AtomicU64,
    dropped: AtomicU64,
    loops: AtomicU32,
    /// Media time of the latest rendered frame, across loops (µs).
    position_us: AtomicI64,
    /// Where the current pass started on the media clock (µs).
    loop_base_us: AtomicI64,
    /// OpenSL ES's played time on the media clock (ms), -1 = no audio.
    audio_ms: AtomicI64,
    audio_ready: AtomicBool,
    audio_running: AtomicBool,
    volume_bits: AtomicU32,
    /// Largest |audio - video| seen while both played (ms).
    max_drift_ms: AtomicI64,
    /// Audio master: how far OpenSL ES's played position is from the
    /// media clock (µs, smoothed). The picture follows it, so it keeps with
    /// the sound when the sound runs slow (up to 1.6% on a 1080p clip here).
    av_offset_us: Mutex<Option<f64>>,
    /// Output latency after OpenSL ES's position (the HAL's buffers), from
    /// `AudioManager.getOutputLatency(STREAM_MUSIC)`; applied as a fixed
    /// offset: what is heard is the position less this.
    latency_us: i64,
    error: Mutex<Option<PlayerError>>,
    waker: AndroidAppWaker,
    label: String,
    /// A probe (frame 0 only), not the live clip: the fail injection
    /// tells them apart.
    probe: bool,
}

impl Shared {
    fn phase(&self) -> DecoderPhase {
        DecoderPhase::from_u8(self.phase.load(Ordering::Acquire))
    }

    fn set_phase(&self, p: DecoderPhase) {
        self.phase.store(p as u8, Ordering::Release);
        self.waker.wake();
    }

    fn fail(&self, e: PlayerError) {
        log::error!("{}: {e}", self.label);
        *self.error.lock().unwrap() = Some(e);
        self.set_phase(DecoderPhase::Failed);
    }

    fn stopped(&self) -> bool {
        self.ctl.lock().unwrap().stop
    }

    /// Waits until the media clock reaches `media_us`. False = stop.
    fn wait_until(&self, media_us: i64) -> bool {
        let mut ctl = self.ctl.lock().unwrap();
        loop {
            if ctl.stop {
                return false;
            }
            ctl.update_clock();
            if ctl.clock_running() {
                let now = self.media_now_us(&ctl);
                if now >= media_us {
                    return true;
                }
                let wait = Duration::from_micros((media_us - now).min(50_000) as u64);
                ctl = self.cond.wait_timeout(ctl, wait).unwrap().0;
            } else {
                ctl = self
                    .cond
                    .wait_timeout(ctl, Duration::from_millis(50))
                    .unwrap()
                    .0;
            }
        }
    }

    /// The time the picture should be at: the media clock, moved to where
    /// the sound is when there is sound.
    fn media_now_us(&self, ctl: &Ctl) -> i64 {
        ctl.now_us() + self.av_offset_us.lock().unwrap().map_or(0, |o| o as i64)
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }
}

/// What the render thread holds while a clip is open.
pub struct Player {
    oes: GlUint,
    texture: Option<VideoTexture>,
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    path: String,
    info: ClipInfo,
    latched: i64,
    matrix: [f32; 16],
    created: Duration,
    /// When `play` was called.
    played_at: Option<Duration>,
}

impl Player {
    /// Opens `path` onto a new external texture and starts decoding: frame 0
    /// is rendered as soon as it's decoded. `sound`: the volume, or `None`
    /// to decode no audio at all. On the GL thread.
    pub fn open(
        path: &str,
        info: ClipInfo,
        probe: bool,
        sound: Option<f32>,
        latency_ms: i32,
        waker: AndroidAppWaker,
        label: String,
    ) -> Result<Self, String> {
        let mut env = env()?;
        let mut oes = 0;
        unsafe {
            glGenTextures(1, &mut oes);
            glBindTexture(GL_TEXTURE_EXTERNAL_OES, oes);
            // External textures take only LINEAR/NEAREST and CLAMP_TO_EDGE.
            glTexParameteri(
                GL_TEXTURE_EXTERNAL_OES,
                GL_TEXTURE_MIN_FILTER,
                GL_LINEAR as i32,
            );
            glTexParameteri(
                GL_TEXTURE_EXTERNAL_OES,
                GL_TEXTURE_MAG_FILTER,
                GL_LINEAR as i32,
            );
            glTexParameteri(
                GL_TEXTURE_EXTERNAL_OES,
                GL_TEXTURE_WRAP_S,
                GL_CLAMP_TO_EDGE as i32,
            );
            glTexParameteri(
                GL_TEXTURE_EXTERNAL_OES,
                GL_TEXTURE_WRAP_T,
                GL_CLAMP_TO_EDGE as i32,
            );
        }
        let (texture, window) = match VideoTexture::new(&mut env, oes) {
            Ok(v) => v,
            Err(e) => {
                unsafe { glDeleteTextures(1, &oes) };
                return Err(e);
            }
        };
        let shared = Arc::new(Shared {
            ctl: Mutex::new(Ctl::default()),
            cond: Condvar::new(),
            phase: AtomicU8::new(DecoderPhase::Starting as u8),
            first_stamp: AtomicI64::new(0),
            last_stamp: AtomicI64::new(0),
            rendered: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            loops: AtomicU32::new(0),
            position_us: AtomicI64::new(0),
            loop_base_us: AtomicI64::new(0),
            audio_ms: AtomicI64::new(-1),
            audio_ready: AtomicBool::new(sound.is_none() || !info.has_audio),
            audio_running: AtomicBool::new(false),
            volume_bits: AtomicU32::new(sound.unwrap_or(0.0).to_bits()),
            max_drift_ms: AtomicI64::new(0),
            av_offset_us: Mutex::new(None),
            latency_us: latency_ms as i64 * 1000,
            error: Mutex::new(None),
            waker,
            label,
            probe,
        });
        let mut threads = Vec::new();
        {
            let (sh, p) = (shared.clone(), path.to_string());
            threads.push(std::thread::spawn(move || {
                if let Err(e) = video_thread(&p, window, &sh) {
                    sh.fail(e);
                }
            }));
        }
        let mut player = Self {
            oes,
            texture: Some(texture),
            shared,
            threads,
            path: path.to_string(),
            info,
            latched: 0,
            matrix: IDENTITY4,
            created: clock::now(),
            played_at: None,
        };
        if sound.is_some() && player.info.has_audio {
            player.start_audio(0);
        }
        Ok(player)
    }

    fn start_audio(&mut self, start_us: i64) {
        if self.shared.audio_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let (sh, p) = (self.shared.clone(), self.path.clone());
        self.threads.push(std::thread::spawn(move || {
            if let Err(e) = audio_thread(&p, &sh, start_us) {
                // The picture goes on without sound.
                log::error!("{}: audio: {e}", sh.label);
                sh.audio_ready.store(true, Ordering::Release);
            }
            sh.audio_running.store(false, Ordering::Release);
        }));
    }

    fn first_latched(&self) -> bool {
        self.latched != 0 && self.latched == self.shared.first_stamp.load(Ordering::Acquire)
    }

    /// The last frame the decoder rendered is the one on the texture.
    fn newest_latched(&self) -> bool {
        self.latched != 0 && self.latched == self.shared.last_stamp.load(Ordering::Acquire)
    }

    fn with_ctl(&self, f: impl FnOnce(&mut Ctl)) {
        let mut ctl = self.shared.ctl.lock().unwrap();
        f(&mut ctl);
        ctl.update_clock();
        drop(ctl);
        self.shared.cond.notify_all();
    }

    fn playing(&self) -> bool {
        self.played_at.is_some()
    }

    fn summary(&self) -> String {
        let s = &self.shared;
        format!(
            "{} frames shown, {} dropped, {} loops, position {:.2}s, audio {}, max A/V drift {} ms",
            s.rendered.load(Ordering::Relaxed),
            s.dropped.load(Ordering::Relaxed),
            s.loops.load(Ordering::Relaxed),
            self.position_us() as f64 / 1e6,
            match s.audio_ms.load(Ordering::Relaxed) {
                -1 => "none".to_string(),
                ms => format!("{:.2}s played", ms as f64 / 1000.0),
            },
            s.max_drift_ms.load(Ordering::Relaxed),
        )
    }
}

impl OpenClip for Player {
    /// Latches the newest rendered frame onto the texture if there is one
    /// not latched yet.
    fn latch(&mut self) {
        let want = self.shared.last_stamp.load(Ordering::Acquire);
        if want == 0 || want == self.latched {
            return;
        }
        let Some(tex) = self.texture.as_ref() else {
            return;
        };
        let Ok(mut env) = env() else { return };
        // `updateTexImage` takes the oldest queued frame, not the newest, so
        // if more than one is waiting (a 60 fps clip on a ~50 fps loop) take
        // them until the newest is on, rather than falling behind.
        let before = self.latched;
        for _ in 0..4 {
            if let Err(e) = tex.update_tex_image(&mut env) {
                log::error!("{}", e);
                break;
            }
            match tex.timestamp(&mut env) {
                Ok(ts) if ts != self.latched && ts != 0 => self.latched = ts,
                _ => break,
            }
            if self.latched == want {
                break;
            }
        }
        if self.latched == before {
            return;
        }
        self.matrix = tex.transform_matrix(&mut env).unwrap_or(IDENTITY4);
    }

    /// The decode thread's phase, held back until its frame is latched:
    /// frame 0 counts once it's on the texture, and the end once the last
    /// frame is (stopping the codec before that would drop it).
    fn phase(&self) -> Phase {
        match self.shared.phase() {
            DecoderPhase::Starting => Phase::Starting,
            DecoderPhase::FirstFrame if self.first_latched() => Phase::FirstFrame,
            DecoderPhase::FirstFrame => Phase::Starting,
            DecoderPhase::Playing => Phase::Playing,
            DecoderPhase::Ended if self.newest_latched() => Phase::Ended,
            DecoderPhase::Ended => Phase::Playing,
            DecoderPhase::Failed => Phase::Failed(
                self.shared
                    .error
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("a failed decode thread records its error first"),
            ),
        }
    }

    fn play(&mut self) {
        self.played_at = Some(clock::now());
        let audio = self.shared.audio_running.load(Ordering::Acquire);
        self.with_ctl(|c| {
            c.play = true;
            if audio {
                c.align = true;
                c.align_since = Some(clock::now());
            }
        });
    }

    fn set_paused(&self, paused: bool) {
        if self.shared.ctl.lock().unwrap().paused != paused {
            log::info!(
                "{} {}",
                self.shared.label,
                if paused { "paused" } else { "resumed" }
            );
            let audio = self.shared.audio_running.load(Ordering::Acquire);
            self.with_ctl(|c| {
                c.paused = paused;
                // A resume waits for the sound to move again, as a start does.
                if !paused && c.play && audio {
                    c.align = true;
                    c.align_since = Some(clock::now());
                }
            });
        }
    }

    fn set_looping(&self, looping: bool) {
        if self.shared.ctl.lock().unwrap().looping != looping {
            log::info!("{} looping {looping}", self.shared.label);
            self.with_ctl(|c| c.looping = looping);
        }
    }

    /// Sound on (with this volume) or off. Off mutes; on starts the audio
    /// thread at the current position if there wasn't one.
    fn set_sound(&mut self, sound: Option<f32>) {
        let v = sound.unwrap_or(0.0);
        if self.shared.volume() != v {
            self.shared
                .volume_bits
                .store(v.to_bits(), Ordering::Relaxed);
        }
        if sound.is_some()
            && self.info.has_audio
            && !self.shared.audio_running.load(Ordering::Acquire)
            && self.playing()
        {
            let pos = self.shared.ctl.lock().unwrap().now_us()
                - self.shared.loop_base_us.load(Ordering::Relaxed);
            log::info!(
                "{} sound on mid-clip, audio from {:.2}s",
                self.shared.label,
                pos as f64 / 1e6
            );
            self.start_audio(pos.max(0));
        }
    }

    fn audio_ready(&self) -> bool {
        self.shared.audio_ready.load(Ordering::Acquire)
    }

    fn audio_running(&self) -> bool {
        self.shared.audio_running.load(Ordering::Acquire)
    }

    /// Media time of the latest rendered frame (µs).
    fn position_us(&self) -> i64 {
        self.shared.position_us.load(Ordering::Relaxed)
    }

    fn has_frame(&self) -> bool {
        self.latched != 0
    }

    fn oes(&self) -> GlUint {
        self.oes
    }

    fn matrix(&self) -> [f32; 16] {
        self.matrix
    }

    /// Render-side view, for the periodic stats line.
    fn debug_line(&self) -> String {
        format!(
            "{:?} latched {} last {} rendered {} dropped {} pos {:.2}s",
            self.shared.phase(),
            self.latched,
            self.shared.last_stamp.load(Ordering::Relaxed),
            self.shared.rendered.load(Ordering::Relaxed),
            self.shared.dropped.load(Ordering::Relaxed),
            self.position_us() as f64 / 1e6
        )
    }

    /// Ends playback. The GL texture goes now (on the GL thread); the
    /// threads are joined and the Java objects released on a reaper thread.
    /// Compose the latched frame out of the texture first.
    fn stop(mut self) {
        log::info!(
            "{} stopped after {:.1}s: {}",
            self.shared.label,
            clock::elapsed(self.created).as_secs_f64(),
            self.summary()
        );
        self.with_ctl(|c| c.stop = true);
        unsafe { glDeleteTextures(1, &self.oes) };
        let threads = std::mem::take(&mut self.threads);
        let texture = self.texture.take();
        let label = self.shared.label.clone();
        std::thread::spawn(move || {
            let t = clock::now();
            for th in threads {
                let _ = th.join();
            }
            let joined = clock::elapsed(t);
            if let Some(tex) = texture {
                // Attached only for this (the guard detaches on drop).
                match VM.get().map(|vm| vm.attach_current_thread()) {
                    Some(Ok(mut guard)) => tex.release(&mut guard),
                    _ => log::error!("{label}: can't attach to release the SurfaceTexture"),
                }
            }
            log::info!(
                "{label} torn down (threads joined in {joined:?}, all in {:?})",
                clock::elapsed(t)
            );
        });
    }
}

#[rustfmt::skip]
const IDENTITY4: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0,
];

// ---- the decode threads ----------------------------------------------------

fn video_thread(path: &str, window: NativeWindow, sh: &Shared) -> Result<(), PlayerError> {
    let t0 = clock::now();
    let ex = Extractor::open(path).map_err(|e| match e {
        OpenError::File(why) => PlayerError::File(why),
        OpenError::Media(why) => PlayerError::Decoder(why),
    })?;
    let (track, mut format) = ex
        .find_track("video/")
        .ok_or_else(|| PlayerError::Decoder("no video track".into()))?;
    ex.select_track(track).map_err(PlayerError::Decoder)?;
    let mime = format.str("mime").unwrap_or("video/avc").to_string();
    // Declared before the codec, so it's dropped (uncounted) after it.
    let _slot = DecoderSlot::take();
    let codec = MediaCodec::from_decoder_type(&mime)
        .ok_or_else(|| PlayerError::Decoder(format!("no decoder for {mime}")))?;

    let mut input_eos = false;
    let mut seq: i64 = 0;
    let mut first_pts: Option<i64> = None;
    let mut last_rel: i64 = 0;
    let mut frame_us: i64 = 33_333;
    let mut next_drift_log = clock::now();
    let mut last_output = clock::now();
    let mut queued_in = 0u64;
    // Samples fed and frames out in this pass: `OMX.rk.video_decoder.avc`
    // doesn't always send an end-of-stream output buffer (seen on two of
    // the first four clips: every frame came out, then nothing), so the
    // pass also ends when every sample fed has come out as a frame.
    let mut in_pass = 0u64;
    let mut out_pass = 0u64;
    let mut at_end = false;
    let result = (|| -> Result<(), String> {
        codec
            .configure(&format, Some(&window), MediaCodecDirection::Decoder)
            .map_err(|e| format!("configure: {e:?}"))?;
        codec.start().map_err(|e| format!("start: {e:?}"))?;
        let configured = clock::elapsed(t0);
        // Test-only: `debug.video.fail=probe|live|hang` fails the probe or
        // the live decoder at once; `hang` fails the live one and holds its
        // release 15 s, as a wedged stop does.
        let fail = raam_core::switches::fail();
        use raam_core::switches::Fail;
        if (fail == Fail::Probe && sh.probe)
            || ((fail == Fail::Live || fail == Fail::Hang) && !sh.probe)
        {
            return Err(format!("test failure (debug.video.fail={fail:?})"));
        }
        loop {
            if sh.stopped() {
                return Ok(());
            }
            // Seen once on the frame (a 60 fps clip, while two 17-27 MB
            // downloads were being probed): the decoder simply stopped
            // producing. A stall ends the clip, rather than the slideshow
            // freezing on it.
            let running = sh.ctl.lock().unwrap().clock_running();
            if !running {
                last_output = clock::now();
            }
            if !input_eos
                && clock::elapsed(last_output) > Duration::from_secs(3)
                && sh.phase() == DecoderPhase::Playing
            {
                return Err(format!(
                    "decoder stalled: no output for {:.1}s (input eos {input_eos}, {queued_in} samples in, {} rendered, {} dropped, last stamp {})",
                    clock::elapsed(last_output).as_secs_f64(),
                    sh.rendered.load(Ordering::Relaxed),
                    sh.dropped.load(Ordering::Relaxed),
                    sh.last_stamp.load(Ordering::Relaxed)
                ));
            }
            if !input_eos {
                match codec.dequeue_input_buffer(Duration::from_millis(10)) {
                    Ok(DequeuedInputBufferResult::Buffer(mut input)) => {
                        let n = ex.read_sample_data(input.buffer_mut());
                        if n < 0 {
                            codec
                                .queue_input_buffer(
                                    input,
                                    0,
                                    0,
                                    0,
                                    ndk_sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
                                )
                                .map_err(|e| format!("queue EOS: {e:?}"))?;
                            input_eos = true;
                        } else {
                            let pts = ex.sample_time_us().max(0) as u64;
                            codec
                                .queue_input_buffer(input, 0, n as usize, pts, 0)
                                .map_err(|e| format!("queue input: {e:?}"))?;
                            ex.advance();
                            queued_in += 1;
                            in_pass += 1;
                        }
                    }
                    Ok(DequeuedInputBufferResult::TryAgainLater) => {}
                    Err(e) => return Err(format!("dequeue input: {e:?}")),
                }
            }
            match codec.dequeue_output_buffer(Duration::from_millis(10)) {
                Ok(DequeuedOutputBufferInfoResult::Buffer(out)) => {
                    last_output = clock::now();
                    let info = *out.info();
                    let eos = info.flags() & (ndk_sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM) != 0;
                    let pts = info.presentation_time_us();
                    if eos && info.size() == 0 {
                        let _ = codec.release_output_buffer(out, false);
                        at_end = true;
                    } else {
                        out_pass += 1;
                        let first = *first_pts.get_or_insert(pts);
                        let rel = pts - first;
                        if rel > last_rel {
                            frame_us = (rel - last_rel).clamp(5_000, 100_000);
                        }
                        last_rel = rel;
                        let media = sh.loop_base_us.load(Ordering::Relaxed) + rel;
                        let phase = sh.phase();
                        if phase == DecoderPhase::Starting {
                            seq += 1;
                            let stamp = seq * 1000;
                            codec
                                .release_output_buffer_at_time(out, stamp)
                                .map_err(|e| format!("render: {e:?}"))?;
                            sh.first_stamp.store(stamp, Ordering::Release);
                            sh.last_stamp.store(stamp, Ordering::Release);
                            sh.rendered.fetch_add(1, Ordering::Relaxed);
                            log::info!(
                                "{}: frame 0 rendered {:.0} ms after open (decoder configured in {configured:?})",
                                sh.label,
                                clock::elapsed(t0).as_secs_f64() * 1000.0
                            );
                            sh.set_phase(DecoderPhase::FirstFrame);
                            // Hold frame 0 until the slideshow plays it.
                            {
                                let mut ctl = sh.ctl.lock().unwrap();
                                while !ctl.play && !ctl.stop {
                                    ctl = sh
                                        .cond
                                        .wait_timeout(ctl, Duration::from_millis(100))
                                        .unwrap()
                                        .0;
                                }
                                if ctl.stop {
                                    return Ok(());
                                }
                            }
                            // However long frame 0 was held (audio pre-roll,
                            // the window hidden), the stall watch starts now.
                            last_output = clock::now();
                            sh.set_phase(DecoderPhase::Playing);
                        } else {
                            if !sh.wait_until(media) {
                                let _ = codec.release_output_buffer(out, false);
                                return Ok(());
                            }
                            // Waiting out a pause (the menu) isn't a stall.
                            last_output = clock::now();
                            let late = {
                                let ctl = sh.ctl.lock().unwrap();
                                sh.media_now_us(&ctl) - media
                            };
                            if late > DROP_LATE_US && !eos {
                                let _ = codec.release_output_buffer(out, false);
                                sh.dropped.fetch_add(1, Ordering::Relaxed);
                            } else {
                                seq += 1;
                                let stamp = seq * 1000;
                                codec
                                    .release_output_buffer_at_time(out, stamp)
                                    .map_err(|e| format!("render: {e:?}"))?;
                                sh.last_stamp.store(stamp, Ordering::Release);
                                sh.rendered.fetch_add(1, Ordering::Relaxed);
                            }
                            sh.position_us.store(media, Ordering::Relaxed);
                            let audio = sh.audio_ms.load(Ordering::Relaxed);
                            if audio >= 0 {
                                let drift = audio - sh.latency_us / 1000 - media / 1000;
                                sh.max_drift_ms.fetch_max(drift.abs(), Ordering::Relaxed);
                                if next_drift_log <= clock::now() {
                                    log::info!(
                                        "{}: A/V at {:.2}s: audio {:+} ms against the picture (late frames dropped so far: {})",
                                        sh.label,
                                        media as f64 / 1e6,
                                        drift,
                                        sh.dropped.load(Ordering::Relaxed)
                                    );
                                    next_drift_log = clock::now() + Duration::from_secs(2);
                                }
                            }
                        }
                        if eos {
                            at_end = true;
                        }
                    }
                }
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                    log::info!("{}: output format {:?}", sh.label, codec.output_format());
                }
                Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged) => {}
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => {}
                Err(e) => return Err(format!("dequeue output: {e:?}")),
            }
            if !at_end && input_eos && sh.phase() == DecoderPhase::Playing {
                let all_out = out_pass >= in_pass;
                let quiet = clock::elapsed(last_output) > Duration::from_millis(600);
                if all_out || quiet {
                    log::info!(
                        "{}: end of pass without an EOS buffer from the decoder ({out_pass} of {in_pass} frames out{})",
                        sh.label,
                        if all_out {
                            ""
                        } else {
                            ", then 600 ms of nothing"
                        }
                    );
                    at_end = true;
                }
            }
            if at_end {
                at_end = false;
                if sh.ctl.lock().unwrap().looping {
                    ex.seek_to(0)?;
                    codec.flush().map_err(|e| format!("flush: {e:?}"))?;
                    input_eos = false;
                    first_pts = None;
                    in_pass = 0;
                    out_pass = 0;
                    last_output = clock::now();
                    sh.loop_base_us
                        .fetch_add(last_rel + frame_us, Ordering::Relaxed);
                    last_rel = 0;
                    let n = sh.loops.fetch_add(1, Ordering::Relaxed) + 1;
                    log::info!(
                        "{}: loop {n} starts at {:.2}s",
                        sh.label,
                        sh.loop_base_us.load(Ordering::Relaxed) as f64 / 1e6
                    );
                    continue;
                }
                sh.set_phase(DecoderPhase::Ended);
                // Keep the codec until the slideshow has composed the last
                // frame: stopping it disconnects the surface, which drops
                // any frame not yet latched.
                let mut ctl = sh.ctl.lock().unwrap();
                while !ctl.stop {
                    ctl = sh
                        .cond
                        .wait_timeout(ctl, Duration::from_millis(100))
                        .unwrap()
                        .0;
                }
                return Ok(());
            }
        }
    })();
    // Failed is reported before the codec is stopped: after an OMX error
    // (seen: ion out of memory, then OMX.rk ERROR 0x80001000) the stop can
    // take seconds or never return, and the slideshow must not wait on it.
    if let Err(e) = result {
        sh.fail(PlayerError::Decoder(e));
    }
    let t = clock::now();
    if raam_core::switches::fail() == raam_core::switches::Fail::Hang && !sh.probe {
        std::thread::sleep(Duration::from_secs(15));
    }
    let _ = codec.stop();
    drop(codec);
    drop(window);
    if clock::elapsed(t) > Duration::from_secs(1) {
        log::warn!(
            "{}: the decoder took {:?} to stop and release",
            sh.label,
            clock::elapsed(t)
        );
    }
    Ok(())
}

fn audio_thread(path: &str, sh: &Shared, start_us: i64) -> Result<(), String> {
    // THREAD_PRIORITY_AUDIO (-16), so decoding the sound keeps up while a
    // 1080p clip and the render loop load the CPU. Android lets an app
    // raise its own threads' priority this far (RLIMIT_NICE).
    let prio = unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, -16) };
    log::info!(
        "{}: audio thread priority -16: {}",
        sh.label,
        if prio == 0 { "set" } else { "refused" }
    );
    let ex = Extractor::open(path)?;
    let (track, mut format) = ex.find_track("audio/").ok_or("no audio track")?;
    ex.select_track(track)?;
    if start_us > 0 {
        ex.seek_to(start_us)?;
    }
    let mime = format.str("mime").unwrap_or("?").to_string();
    let mut rate = format.i32("sample-rate").unwrap_or(44100) as u32;
    let mut channels = format.i32("channel-count").unwrap_or(1) as u32;
    let codec =
        MediaCodec::from_decoder_type(&mime).ok_or_else(|| format!("no decoder for {mime}"))?;
    codec
        .configure(&format, None, MediaCodecDirection::Decoder)
        .map_err(|e| format!("configure: {e:?}"))?;
    codec.start().map_err(|e| format!("start: {e:?}"))?;

    let mut out: Option<AudioOut> = None;
    let mut volume = f32::NAN;
    let mut playing = false;
    let mut input_eos = false;
    let mut pending: Vec<u8> = Vec::with_capacity(PCM_BATCH_BYTES * 2);
    let mut queued_batches = 0u32;
    // Media time the player's position 0 stands for.
    let offset_ms = start_us / 1000;
    let mut skip_before_us = start_us;
    let mut ended = false;
    let mut align_from: Option<u32> = None;
    let result = (|| -> Result<(), String> {
        loop {
            let (stop, running, looping) = {
                let c = sh.ctl.lock().unwrap();
                (c.stop, c.running(), c.looping)
            };
            if stop {
                return Ok(());
            }
            if let Some(o) = out.as_ref() {
                if running != playing {
                    o.set_playing(running)?;
                    playing = running;
                }
                let v = sh.volume();
                if v != volume {
                    o.set_volume(v)?;
                    volume = v;
                }
                if let Some(ms) = o.position_ms() {
                    let audio_ms = offset_ms + ms as i64;
                    // GetPosition went back to 0 once a drained player
                    // stopped: keep the furthest point reached.
                    let before = sh.audio_ms.fetch_max(audio_ms, Ordering::Relaxed);
                    // What is heard now, on the media clock.
                    let heard_us = audio_ms * 1000 - sh.latency_us;
                    let mut ctl = sh.ctl.lock().unwrap();
                    if running && ctl.align {
                        if align_from.is_none() {
                            align_from = Some(ms);
                        }
                        if align_from.is_some_and(|from| ms > from) {
                            // The first sample has left the mixer: the picture
                            // starts (or resumes) here, exactly.
                            let was = ctl.now_us();
                            ctl.clock_acc_us = heard_us;
                            ctl.clock_since = None;
                            ctl.align = false;
                            ctl.update_clock();
                            drop(ctl);
                            sh.cond.notify_all();
                            *sh.av_offset_us.lock().unwrap() = Some(0.0);
                            log::info!(
                                "{}: sound started (position {} ms, {} ms output latency): picture clock {:.3}s -> {:.3}s",
                                sh.label,
                                audio_ms,
                                sh.latency_us / 1000,
                                was as f64 / 1e6,
                                heard_us as f64 / 1e6
                            );
                            align_from = None;
                        }
                    } else if running && !ctl.align && !ended && audio_ms > before {
                        // Long-run drift only (a starved mixer runs slow):
                        // slow, starting from 0 at the alignment above.
                        let off = (heard_us - ctl.now_us()) as f64;
                        drop(ctl);
                        let mut o = sh.av_offset_us.lock().unwrap();
                        *o = Some(o.map_or(0.0, |prev| prev * 0.98 + off * 0.02));
                    }
                }
            }
            if ended {
                // Drained: let what is queued play out (the video thread
                // decides when the clip ends).
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            // A full batch waits for a free buffer before decoding more.
            if pending.len() >= PCM_BATCH_BYTES {
                let o = match out.as_mut() {
                    Some(o) => o,
                    None => {
                        let o = AudioOut::new(rate, channels)?;
                        o.set_volume(sh.volume())?;
                        volume = sh.volume();
                        log::info!(
                            "{}: audio {mime} {rate} Hz x{channels}, OpenSL ES player ready",
                            sh.label
                        );
                        out.insert(o)
                    }
                };
                if o.enqueue(&pending, Duration::from_millis(50))? {
                    pending.clear();
                    queued_batches += 1;
                    if queued_batches == 2 {
                        sh.audio_ready.store(true, Ordering::Release);
                        sh.waker.wake();
                    }
                }
                continue;
            }
            if !input_eos {
                match codec.dequeue_input_buffer(Duration::from_millis(10)) {
                    Ok(DequeuedInputBufferResult::Buffer(mut input)) => {
                        let n = ex.read_sample_data(input.buffer_mut());
                        if n < 0 {
                            codec
                                .queue_input_buffer(
                                    input,
                                    0,
                                    0,
                                    0,
                                    ndk_sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
                                )
                                .map_err(|e| format!("queue EOS: {e:?}"))?;
                            input_eos = true;
                        } else {
                            let pts = ex.sample_time_us().max(0) as u64;
                            codec
                                .queue_input_buffer(input, 0, n as usize, pts, 0)
                                .map_err(|e| format!("queue input: {e:?}"))?;
                            ex.advance();
                        }
                    }
                    Ok(DequeuedInputBufferResult::TryAgainLater) => {}
                    Err(e) => return Err(format!("dequeue input: {e:?}")),
                }
            }
            match codec.dequeue_output_buffer(Duration::from_millis(10)) {
                Ok(DequeuedOutputBufferInfoResult::Buffer(output)) => {
                    let info = *output.info();
                    let eos = info.flags() & (ndk_sys::AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM) != 0;
                    // buffer() is the whole allocation; the PCM is
                    // [offset, offset + size).
                    let off = info.offset().max(0) as usize;
                    let sz = info.size().max(0) as usize;
                    let raw = output.buffer();
                    if info.presentation_time_us() >= skip_before_us {
                        pending
                            .extend_from_slice(&raw[off.min(raw.len())..(off + sz).min(raw.len())]);
                    }
                    let _ = codec.release_output_buffer(output, false);
                    if eos {
                        if looping {
                            ex.seek_to(0)?;
                            codec.flush().map_err(|e| format!("flush: {e:?}"))?;
                            input_eos = false;
                            skip_before_us = 0;
                        } else {
                            // Whatever is left, then nothing more.
                            if !pending.is_empty() {
                                if out.is_none() {
                                    let o = AudioOut::new(rate, channels)?;
                                    o.set_volume(sh.volume())?;
                                    out = Some(o);
                                }
                                let o = out.as_mut().unwrap();
                                while !o.enqueue(&pending, Duration::from_millis(50))? {
                                    if sh.stopped() {
                                        return Ok(());
                                    }
                                }
                                pending.clear();
                            }
                            sh.audio_ready.store(true, Ordering::Release);
                            ended = true;
                        }
                    }
                }
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                    let f = codec.output_format();
                    rate = f.i32("sample-rate").unwrap_or(rate as i32) as u32;
                    channels = f.i32("channel-count").unwrap_or(channels as i32) as u32;
                }
                Ok(_) => {}
                Err(e) => return Err(format!("dequeue output: {e:?}")),
            }
        }
    })();
    let _ = codec.stop();
    drop(codec);
    drop(out);
    result
}
