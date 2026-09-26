//! 027's video, out of the slideshow (the Raam migration's step 2): the OES
//! program that samples a decoded frame, the probe and live players, the
//! decoder backoff and the watch for a wedged decoder, and the video debug
//! props. Every `player::Player` lives behind `Video`; the slideshow keeps
//! planning, composing and drawing, and touches a decoded picture only as a
//! `ClipFrame` it draws into whatever framebuffer is bound.
use crate::player::{self, Phase, Player};
use android_activity::AndroidAppWaker;
use raam_core::clock;
use raam_core::gl::*;
use raam_core::video::{FinishedHandle, LiveCue, ProbeHandle, ProbeStatus, Tick, VideoSeam};
use raam_model::VideoClip;
use raam_model::limits::{
    DECODER_BACKOFF_BASE_SECS, DECODER_BACKOFF_CAP_SECS, DECODER_RELEASE_TIMEOUT,
    FIRST_FRAME_TIMEOUT, LIVE_DECODER_WAIT,
};
use std::ffi::c_void;
use std::time::Duration;

/// Samples a decoded frame (`GL_TEXTURE_EXTERNAL_OES`) through the
/// `SurfaceTexture`'s transform (011's shaders).
const VS_OES_SRC: &str = "attribute vec2 aPos; attribute vec2 aUV; \
     uniform vec2 uScale; uniform mat4 uTexMatrix; varying vec2 vUV; \
     void main() { \
         vUV = (uTexMatrix * vec4(aUV, 0.0, 1.0)).xy; \
         gl_Position = vec4(aPos * uScale, 0.0, 1.0); \
     }";

const FS_OES_SRC: &str = "#extension GL_OES_EGL_image_external : require\n\
     precision mediump float; varying vec2 vUV; uniform samplerExternalOES uTex; \
     void main() { gl_FragColor = texture2D(uTex, vUV); }";

#[derive(Clone, Copy)]
struct OesProgram {
    program: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    u_scale: GlInt,
    u_matrix: GlInt,
    u_tex: GlInt,
}

impl OesProgram {
    unsafe fn new() -> Self {
        unsafe {
            let program = link_program("oes", VS_OES_SRC, FS_OES_SRC);
            Self {
                program,
                a_pos: attrib_loc(program, "aPos"),
                a_uv: attrib_loc(program, "aUV"),
                u_scale: uniform_loc(program, "uScale"),
                u_matrix: uniform_loc(program, "uTexMatrix"),
                u_tex: uniform_loc(program, "uTex"),
            }
        }
    }

    /// `matrix` as `getTransformMatrix` gives it samples with (0,0) at the
    /// picture's bottom-left; the quad's UVs have v=1 at the bottom, so
    /// `upright` pre-flips them (`FLIP_V`) to draw it the right way up in the
    /// framebuffer. Without it the frame lands with row 0 = its top, which is
    /// the convention of an uploaded photo (and of a tile's `source`).
    unsafe fn draw(
        &self,
        quad_vbo: GlUint,
        quad_ibo: GlUint,
        oes: GlUint,
        scale: (f32, f32),
        matrix: &[f32; 16],
        upright: bool,
    ) {
        let m = if upright {
            player::mat_mul(matrix, &player::FLIP_V)
        } else {
            *matrix
        };
        unsafe {
            glUseProgram(self.program);
            glBindBuffer(GL_ARRAY_BUFFER, quad_vbo);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, quad_ibo);
            let stride = 4 * 4;
            glVertexAttribPointer(self.a_pos, 2, GL_FLOAT, 0, stride, std::ptr::null());
            glEnableVertexAttribArray(self.a_pos);
            glVertexAttribPointer(self.a_uv, 2, GL_FLOAT, 0, stride, (2 * 4) as *const c_void);
            glEnableVertexAttribArray(self.a_uv);
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_EXTERNAL_OES, oes);
            glUniform1i(self.u_tex, 0);
            glUniform2f(self.u_scale, scale.0, scale.1);
            glUniformMatrix4fv(self.u_matrix, 1, 0, m.as_ptr());
            glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, std::ptr::null());
        }
    }
}

/// A decoded frame on a player's OES texture, drawable into whatever
/// framebuffer is bound. A copy of the transform at latch time: valid until
/// that player is latched again or stopped.
#[derive(Clone, Copy)]
pub struct ClipFrame {
    program: OesProgram,
    texture: GlUint,
    matrix: [f32; 16],
}

impl raam_core::video::ClipFrame for ClipFrame {
    /// See `OesProgram::draw` for `upright`.
    unsafe fn draw(&self, quad_vbo: GlUint, quad_ibo: GlUint, scale: (f32, f32), upright: bool) {
        unsafe {
            self.program.draw(
                quad_vbo,
                quad_ibo,
                self.texture,
                scale,
                &self.matrix,
                upright,
            )
        };
    }
}

/// A short-lived decoder bringing up a clip's first frame for the tile of a
/// plan being built. The plan bookkeeping (slot, rect, meta) stays with the
/// slideshow's `Building`.
pub struct ProbePlayer {
    player: Player,
}

impl ProbeHandle for ProbePlayer {
    /// How long ago the decoder was opened.
    fn age(&self) -> Duration {
        clock::elapsed(self.player.created)
    }

    fn stop(self) {
        self.player.stop();
    }
}

/// The clip on screen, playing.
struct Live {
    player: Player,
    started: Duration,
}

/// A live player taken back by `finish`, with the frame that was on screen
/// (if any) for the restill. `stop` it once that is composed.
pub struct FinishedLive {
    player: Player,
    frame: Option<ClipFrame>,
}

impl FinishedHandle for FinishedLive {
    type Frame = ClipFrame;

    fn frame(&self) -> Option<ClipFrame> {
        self.frame
    }

    fn stop(self) {
        self.player.stop();
    }
}

pub struct Video {
    oes: OesProgram,
    waker: AndroidAppWaker,
    /// The clip on screen, while it plays.
    live: Option<Live>,
    /// The clip on screen has finished (or couldn't play): its still holds.
    done: bool,
    /// Clips the decoder refused, for lib.rs to mark (asset id, why).
    unplayable: Vec<(i64, String)>,
    /// The clip on screen is to start once no other decoder is open (since).
    live_wanted: Option<Duration>,
    /// Decoder failures in a row, and until when clips are passed over
    /// because of them (`record_failure`).
    failures: u32,
    backoff_until: Option<Duration>,
    /// Since when a decoder has been open with no player of ours on it,
    /// and whether that was counted as a failure.
    orphan_since: Option<Duration>,
    orphan_counted: bool,
    /// The music output's latency (ms) as Android reports it, set by
    /// lib.rs, and the calibrated extra on top (the "Audio delay" setting,
    /// or `debug.video.audio_extra_ms`).
    pub audio_latency_ms: u32,
    pub audio_extra_ms: i32,
}

impl Video {
    pub unsafe fn new(waker: AndroidAppWaker) -> Self {
        Self {
            oes: unsafe { OesProgram::new() },
            waker,
            live: None,
            done: false,
            unplayable: Vec::new(),
            live_wanted: None,
            failures: 0,
            backoff_until: None,
            orphan_since: None,
            orphan_counted: false,
            audio_latency_ms: 0,
            audio_extra_ms: 0,
        }
    }

    /// The clip on screen has finished (or couldn't play): its still holds.
    pub fn done(&self) -> bool {
        self.done
    }

    /// Clips are being passed over after a decoder failure.
    pub fn backing_off(&self) -> bool {
        self.backoff_until.is_some()
    }

    /// A decoder is playing, wanted, or still winding down somewhere:
    /// don't open another (one video decoder at a time on this VPU).
    pub fn decoder_busy(&self) -> bool {
        self.live.is_some() || self.live_wanted.is_some() || player::video_decoders() > 0
    }

    /// Clips the decoder refused since the last call.
    pub fn take_unplayable(&mut self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.unplayable)
    }

    /// 027: pause the clip at once (the window is going away; `tick`,
    /// which applies the pause otherwise, doesn't run while hidden).
    pub fn pause_now(&self) {
        if let Some(l) = &self.live {
            l.player.set_paused(true);
        }
    }

    pub fn debug_line(&self) -> Option<String> {
        self.live.as_ref().map(|l| l.player.debug_line())
    }

    /// The playing clip's (position µs, has audio), for the status line.
    pub fn live_progress(&self) -> Option<(i64, bool)> {
        self.live
            .as_ref()
            .map(|l| (l.player.position_us(), l.player.has_audio()))
    }

    /// A clip is animating unless it's playing and paused.
    pub fn animating(&self, paused: bool) -> bool {
        self.live
            .as_ref()
            .is_some_and(|l| !(l.player.playing() && paused))
    }

    /// 027: a decoder's release is polled (the reaper thread doesn't wake
    /// the loop), for the clip waiting on it and for the stuck-release
    /// watch; and a backoff's end.
    pub fn deadline(&self) -> Option<Duration> {
        if self.live_wanted.is_some() || (self.live.is_none() && player::video_decoders() > 0) {
            return Some(Duration::from_millis(100));
        }
        self.backoff_until
            .map(|t| t.saturating_sub(clock::now()).max(Duration::from_millis(1)))
    }

    /// Opens a probing decoder on a clip's first frame.
    pub fn open_probe(&self, clip: &VideoClip, asset_id: i64) -> Result<ProbePlayer, String> {
        let label = format!("clip {asset_id} (probe)");
        Player::open(
            &clip.path,
            clip.info.clone(),
            None,
            0,
            self.waker.clone(),
            label,
        )
        .map(|player| ProbePlayer { player })
    }

    /// Polls a probing decoder. A failure is logged and recorded here (the
    /// refused clip, the backoff) as a live failure would be; the caller
    /// drops the plan being built.
    pub fn poll_probe(&mut self, probe: &mut ProbePlayer, asset_id: i64) -> ProbeStatus {
        probe.player.latch();
        let phase = probe.player.phase();
        if phase == Phase::Failed || clock::elapsed(probe.player.created) > FIRST_FRAME_TIMEOUT {
            let why = probe
                .player
                .error()
                .unwrap_or_else(|| format!("no first frame within {FIRST_FRAME_TIMEOUT:?}"));
            log::error!("clip {asset_id}: {why}");
            // The decoder refused it: not something a retry fixes.
            // Only when it was the one decoder open (a second instance
            // failing next to a playing clip may be contention, not the clip).
            if phase == Phase::Failed && !why.starts_with("open") && self.live.is_none() {
                self.unplayable.push((asset_id, why.clone()));
            }
            if !why.starts_with("open") {
                self.record_failure(&format!("clip {asset_id} probe: {why}"));
            }
            return ProbeStatus::Failed;
        }
        if probe.player.first_latched() {
            ProbeStatus::Ready
        } else {
            ProbeStatus::Waiting
        }
    }

    /// The probe's first frame, for composing its tile (drawn with
    /// `upright: false`, so row 0 = the picture's top, like an uploaded
    /// photo).
    pub fn probe_frame(&self, probe: &ProbePlayer) -> ClipFrame {
        ClipFrame {
            program: self.oes,
            texture: probe.player.oes(),
            matrix: *probe.player.matrix(),
        }
    }

    /// The playing clip's newest frame, unless none is up yet or the
    /// `debug.video.show_still` prop asks for the composed still.
    pub fn live_frame(&self) -> Option<ClipFrame> {
        let live = self.live.as_ref().filter(|l| l.player.has_frame())?;
        // Test-only: `debug.video.show_still=1` draws the composed still
        // instead of the live picture.
        if crate::props::prop("debug.video.show_still").trim() == "1" {
            return None;
        }
        Some(ClipFrame {
            program: self.oes,
            texture: live.player.oes(),
            matrix: *live.player.matrix(),
        })
    }

    /// 027: the slide that just landed starts playing if it's a clip (its
    /// still holds meanwhile).
    pub fn start(&mut self, cue: &LiveCue) {
        self.done = false;
        self.live_wanted = cue.clip.is_some().then(clock::now);
        self.open_live(cue);
    }

    /// Opens the wanted clip once no other video decoder is open (the
    /// probe's may still be being released).
    fn open_live(&mut self, cue: &LiveCue) {
        let Some(since) = self.live_wanted else {
            return;
        };
        if self.backoff_until.is_some() {
            log::warn!("backing off after a decoder failure, the clip's still holds");
            self.live_wanted = None;
            self.done = true;
            return;
        }
        let open = player::video_decoders();
        if open > 0 {
            if clock::elapsed(since) < LIVE_DECODER_WAIT {
                return;
            }
            log::error!(
                "{open} decoder(s) still not released after {LIVE_DECODER_WAIT:?}, the clip's still holds"
            );
            self.live_wanted = None;
            self.done = true;
            self.record_failure("the previous decoder wasn't released");
            return;
        }
        self.live_wanted = None;
        if clock::elapsed(since) > Duration::from_millis(20) {
            log::info!(
                "waited {:.0} ms for the previous decoder's release",
                clock::elapsed(since).as_secs_f64() * 1000.0
            );
        }
        let Some((clip, asset_id)) = cue.clip else {
            return;
        };
        let label = format!("clip {asset_id}");
        let latency = self.audio_latency_ms as i32 + self.audio_extra_ms;
        if cue.sound.is_some() {
            log::info!(
                "audio delay applied: {latency} ms ({} reported + {} calibrated)",
                self.audio_latency_ms,
                self.audio_extra_ms
            );
        }
        match Player::open(
            &clip.path,
            clip.info.clone(),
            cue.sound,
            latency,
            self.waker.clone(),
            label,
        ) {
            Ok(player) => {
                log::info!(
                    "clip {asset_id} starting ({:?}, sound {})",
                    cue.playback,
                    if cue.sound.is_some() { "on" } else { "off" }
                );
                self.live = Some(Live {
                    player,
                    started: clock::now(),
                });
            }
            Err(e) => {
                log::error!("clip {asset_id}: can't start playback: {e}");
                self.done = true;
            }
        }
    }

    /// Keeps the playing clip in step: pause, sound, the loop flag, the
    /// first frame's hand-over (play), and its end.
    pub fn tick(&mut self, cue: &LiveCue, paused: bool, looping: bool) -> Tick {
        self.open_live(cue);
        let Some(live) = self.live.as_mut() else {
            return Tick::Running;
        };
        live.player.set_paused(paused);
        live.player.set_sound(cue.sound);
        live.player.latch();
        let phase = live.player.phase();
        let ended = phase == Phase::Ended && live.player.newest_latched();
        let no_first =
            phase == Phase::Starting && clock::elapsed(live.started) > FIRST_FRAME_TIMEOUT;
        if ended || phase == Phase::Failed || no_first {
            if ended {
                log::info!("clip ended ({:?})", cue.playback);
                self.failures = 0;
            } else {
                let why = match live.player.error() {
                    Some(e) => e,
                    None => format!("no first frame within {FIRST_FRAME_TIMEOUT:?}"),
                };
                log::error!("clip failed: {why}");
                self.record_failure(&format!("playback: {why}"));
            }
            return Tick::Finished;
        }
        match phase {
            Phase::FirstFrame if !live.player.playing() => {
                let waited = clock::elapsed(live.started);
                // Test-only: `debug.video.hold_first=1` keeps the clip on
                // frame 0 (to screenshot the slide it landed on).
                let hold = crate::props::prop("debug.video.hold_first").trim() == "1";
                if !hold
                    && live.player.first_latched()
                    && (live.player.audio_ready() || waited > Duration::from_millis(1500))
                {
                    log::info!(
                        "clip playing, {:.0} ms after the slide landed (audio {})",
                        waited.as_secs_f64() * 1000.0,
                        if !live.player.info.has_audio {
                            "none in the clip"
                        } else if cue.sound.is_none() {
                            "muted, not decoded"
                        } else if live.player.audio_ready() {
                            "pre-rolled"
                        } else {
                            "not ready, starting without it"
                        }
                    );
                    live.player.play();
                }
            }
            Phase::FirstFrame | Phase::Playing => live.player.set_looping(looping),
            _ => {}
        }
        Tick::Running
    }

    /// The clip's slide is going (its end, Next, a hide): takes the live
    /// player back for the restill, its still holding from here.
    pub fn finish(&mut self) -> Option<FinishedLive> {
        self.live_wanted = None;
        let mut live = self.live.take()?;
        self.done = true;
        live.player.latch();
        let frame = live.player.has_frame().then(|| ClipFrame {
            program: self.oes,
            texture: live.player.oes(),
            matrix: *live.player.matrix(),
        });
        Some(FinishedLive {
            player: live.player,
            frame,
        })
    }

    /// 027: a decoder failed or hung (on this frame, typically the RK VPU
    /// out of ion memory). Clips are passed over for a while, longer after
    /// each failure in a row: 30 s, 1, 2, 4, 8, then 10 min. A clip that
    /// plays to its end ends the run.
    pub fn record_failure(&mut self, why: &str) {
        self.failures += 1;
        let secs =
            (DECODER_BACKOFF_BASE_SECS << (self.failures - 1).min(5)).min(DECODER_BACKOFF_CAP_SECS);
        self.backoff_until = Some(clock::now() + Duration::from_secs(secs));
        log::warn!(
            "decoder failure {} in a row ({why}): clips passed over for {secs} s",
            self.failures
        );
    }

    /// Ends a backoff that's over, and watches for a decoder whose release
    /// never finishes (a wedged VPU), which counts as a failure once.
    /// `probing`: a probe player of ours is up (the live one is known here).
    pub fn watch_decoders(&mut self, probing: bool) {
        if self.backoff_until.is_some_and(|t| clock::now() >= t) {
            log::info!("backoff over, clips are tried again");
            self.backoff_until = None;
        }
        let ours = self.live.is_some() || probing;
        if ours || player::video_decoders() == 0 {
            self.orphan_since = None;
            self.orphan_counted = false;
            return;
        }
        let since = *self.orphan_since.get_or_insert_with(clock::now);
        if !self.orphan_counted && clock::elapsed(since) > DECODER_RELEASE_TIMEOUT {
            self.orphan_counted = true;
            self.record_failure(&format!(
                "a stopped decoder not released after {DECODER_RELEASE_TIMEOUT:?}"
            ));
        }
    }
}

/// The core's side of this stack: the slideshow drives it only through
/// the seam (raam-core's video.rs), so these forward to the inherent
/// methods above. The host-only surface (pause on hide, the unplayable
/// list, audio latency, `new`) stays inherent and concrete.
impl VideoSeam for Video {
    type Frame = ClipFrame;
    type Probe = ProbePlayer;
    type Finished = FinishedLive;

    fn done(&self) -> bool {
        Video::done(self)
    }

    fn backing_off(&self) -> bool {
        Video::backing_off(self)
    }

    fn decoder_busy(&self) -> bool {
        Video::decoder_busy(self)
    }

    fn animating(&self, paused: bool) -> bool {
        Video::animating(self, paused)
    }

    fn deadline(&self) -> Option<Duration> {
        Video::deadline(self)
    }

    fn live_progress(&self) -> Option<(i64, bool)> {
        Video::live_progress(self)
    }

    fn open_probe(&self, clip: &VideoClip, asset_id: i64) -> Result<ProbePlayer, String> {
        Video::open_probe(self, clip, asset_id)
    }

    fn poll_probe(&mut self, probe: &mut ProbePlayer, asset_id: i64) -> ProbeStatus {
        Video::poll_probe(self, probe, asset_id)
    }

    fn probe_frame(&self, probe: &ProbePlayer) -> ClipFrame {
        Video::probe_frame(self, probe)
    }

    fn live_frame(&self) -> Option<ClipFrame> {
        Video::live_frame(self)
    }

    fn start(&mut self, cue: &LiveCue) {
        Video::start(self, cue)
    }

    fn tick(&mut self, cue: &LiveCue, paused: bool, looping: bool) -> Tick {
        Video::tick(self, cue, paused, looping)
    }

    fn finish(&mut self) -> Option<FinishedLive> {
        Video::finish(self)
    }

    fn record_failure(&mut self, why: &str) {
        Video::record_failure(self, why)
    }

    fn watch_decoders(&mut self, probing: bool) {
        Video::watch_decoders(self, probing)
    }
}
