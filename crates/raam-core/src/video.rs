//! The video orchestration, over the `VideoPlayer` seam. `Video` drives the
//! host's decoders: a short-lived probe brings up a clip's first frame for
//! its tile, the live clip starts when its slide lands, one decoder is open
//! at a time, a failure passes clips over for a while (longer after each in
//! a row), and a decoder whose release never finishes (a wedged VPU) counts
//! as a failure. The slideshow composes and draws; a decoded picture
//! reaches it only as a `ClipFrame`, an external texture and its transform.
//!
//! Tested on the frame's RK VPU, which decodes H.264 (up to 1920x1080 at
//! 15 Mbit/s, rotated or not) and has no HEVC decoder. The Android host
//! keeps the decoders themselves (MediaCodec, the SurfaceTexture bridge,
//! OpenSL audio, the reaper thread); photo-only hosts use `NoVideo`.

use crate::clock;
use crate::gl::GlUint;
use crate::switches;
use raam_model::limits::{
    AUDIO_PREROLL_WAIT, DECODER_BACKOFF_BASE_SECS, DECODER_BACKOFF_CAP_SECS, DECODER_RELEASE_POLL,
    DECODER_RELEASE_TIMEOUT, FIRST_FRAME_TIMEOUT, LIVE_DECODER_WAIT,
};
use raam_model::{AssetId, VideoClip, VideoPlayback};
use std::fmt;
use std::time::Duration;

/// Only a wait longer than this for the previous decoder's release is
/// logged; shorter ones are the normal hand-over.
const RELEASE_WAIT_LOG: Duration = Duration::from_millis(20);

// ---- the seam -------------------------------------------------------------

/// Why an open clip failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerError {
    /// The clip's file couldn't be opened (gone from the cache, say): not
    /// the decoder's doing, so no backoff and no verdict on the clip.
    File(String),
    /// The decoder refused the clip, or failed while playing it.
    Decoder(String),
}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerError::File(why) | PlayerError::Decoder(why) => f.write_str(why),
        }
    }
}

/// Where an open clip is, as the render thread sees it: what its texture
/// holds as of the last `latch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Opening, or frame 0 decoded but not on the texture yet.
    Starting,
    /// Frame 0 is on the texture, held there until `play`.
    FirstFrame,
    Playing,
    /// Playback is over and its last frame is on the texture.
    Ended,
    Failed(PlayerError),
}

/// What a clip is opened for: a probe only ever shows frame 0; a live clip
/// plays, with sound at this volume or none (no audio decoded at all).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Probe,
    Live { sound: Option<f32> },
}

/// The host's video decoders.
pub trait VideoPlayer {
    type Clip: OpenClip;

    /// Opens a clip onto a new external texture and starts decoding: frame
    /// 0 is rendered as soon as it's decoded, then held for `play`. On the
    /// GL thread. The error is only logged.
    fn open(&self, clip: &VideoClip, asset_id: AssetId, role: Role) -> Result<Self::Clip, String>;
    /// Video decoders created and not yet released, anywhere in the
    /// process: open clips, and stopped ones still being torn down. On the
    /// frame's VPU a second decoder beside another's buffers has run ion
    /// out of memory, so one opens only when this is 0.
    fn decoders_open(&self) -> u32;
}

/// One open clip. Its texture, threads and decoder are the host's; `stop`
/// hands them back.
pub trait OpenClip {
    /// Latches the newest decoded frame onto the texture, if there is one
    /// not latched yet. On the GL thread.
    fn latch(&mut self);
    fn phase(&self) -> Phase;
    /// Frame 0 is on screen: the clip plays from here.
    fn play(&mut self);
    fn set_paused(&self, paused: bool);
    fn set_looping(&self, looping: bool);
    /// Sound on at this volume, or off. On mid-clip starts the sound where
    /// the picture is.
    fn set_sound(&mut self, sound: Option<f32>);
    /// The sound is pre-rolled, or there's none to wait for.
    fn audio_ready(&self) -> bool;
    /// Sound is being decoded and played.
    fn audio_running(&self) -> bool;
    /// Media time of the newest decoded frame, across loops (µs).
    fn position_us(&self) -> i64;
    /// A frame has been latched.
    fn has_frame(&self) -> bool;
    /// The external texture (`GL_TEXTURE_EXTERNAL_OES`) and, as of the last
    /// latch, its sampling transform (column-major, (0,0) at the picture's
    /// bottom-left).
    fn oes(&self) -> GlUint;
    fn matrix(&self) -> [f32; 16];
    /// The host's view of the clip, for the periodic stats line.
    fn debug_line(&self) -> String;
    /// Ends the clip. The texture goes at once; the decoder is released
    /// off the render thread (and counted by `decoders_open` until then).
    /// Compose a latched frame out of the texture first.
    fn stop(self);
}

/// The player of a host without video (desktop, web). Nothing ever opens:
/// a clip that reaches its probe drops its plan and backs clips off as a
/// decoder failure would, so the slideshow goes on with photos. Such a
/// host should keep clips out of its queue in the first place (its
/// `MediaProbe` reports every clip unplayable, or its source offers none).
pub struct NoVideo;

/// `NoVideo`'s clip, which can't exist.
pub enum NoClip {}

impl VideoPlayer for NoVideo {
    type Clip = NoClip;

    fn open(&self, _: &VideoClip, _: AssetId, _: Role) -> Result<NoClip, String> {
        Err("no video on this host".into())
    }

    fn decoders_open(&self) -> u32 {
        0
    }
}

impl OpenClip for NoClip {
    fn latch(&mut self) {
        match *self {}
    }
    fn phase(&self) -> Phase {
        match *self {}
    }
    fn play(&mut self) {
        match *self {}
    }
    fn set_paused(&self, _: bool) {
        match *self {}
    }
    fn set_looping(&self, _: bool) {
        match *self {}
    }
    fn set_sound(&mut self, _: Option<f32>) {
        match *self {}
    }
    fn audio_ready(&self) -> bool {
        match *self {}
    }
    fn audio_running(&self) -> bool {
        match *self {}
    }
    fn position_us(&self) -> i64 {
        match *self {}
    }
    fn has_frame(&self) -> bool {
        match *self {}
    }
    fn oes(&self) -> GlUint {
        match *self {}
    }
    fn matrix(&self) -> [f32; 16] {
        match *self {}
    }
    fn debug_line(&self) -> String {
        match *self {}
    }
    fn stop(self) {
        match self {}
    }
}

// ---- what the slideshow sees ------------------------------------------------

/// A decoded frame on an open clip's texture, drawable by the slideshow's
/// OES program. Valid until that clip is latched again or stopped.
#[derive(Clone, Copy)]
pub struct ClipFrame {
    pub texture: GlUint,
    pub matrix: [f32; 16],
}

impl ClipFrame {
    fn of(clip: &impl OpenClip) -> Self {
        ClipFrame {
            texture: clip.oes(),
            matrix: clip.matrix(),
        }
    }
}

/// What the slideshow currently asks of the live clip: the current slide's
/// clip (if it is one) and the sound and playback settings.
pub struct LiveCue<'a> {
    /// The clip and its asset id.
    pub clip: Option<(&'a VideoClip, AssetId)>,
    /// The volume, or `None` for no audio decoding.
    pub sound: Option<f32>,
    pub playback: VideoPlayback,
}

/// What `poll_probe` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Still decoding: ask again next frame.
    Waiting,
    /// The first frame is on the texture (`probe_frame`): compose the tile,
    /// then `stop` the probe.
    Ready,
    /// The probe failed (logged, the refusal and backoff recorded): drop
    /// the plan being built.
    Failed,
}

/// `tick`'s verdict: `Finished` means the clip just ended or failed — the
/// slideshow restills its slide (`finish`) and restarts the dwell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Running,
    Finished,
}

/// A short-lived decoder bringing up a clip's first frame for a tile of the
/// plan being built. The plan bookkeeping (slot, rect, meta) stays with the
/// slideshow's `Building`.
#[must_use = "stop() it: a dropped probe leaks its decoder, and no clip opens again"]
pub struct ProbePlayer<C> {
    clip: C,
    opened: Duration,
}

impl<C: OpenClip> ProbePlayer<C> {
    /// How long ago the decoder was opened.
    pub fn age(&self) -> Duration {
        clock::elapsed(self.opened)
    }

    pub fn stop(self) {
        self.clip.stop();
    }
}

/// The clip on screen, playing.
struct Live<C> {
    clip: C,
    started: Duration,
    has_audio: bool,
    /// `play` was called.
    played: bool,
}

/// A live clip taken back by `finish`, with the frame that was on screen
/// (if any) for the restill. `stop` it once that is composed.
#[must_use = "stop() it: a dropped clip leaks its decoder, and no clip opens again"]
pub struct Finished<C> {
    clip: C,
    pub frame: Option<ClipFrame>,
}

impl<C: OpenClip> Finished<C> {
    pub fn stop(self) {
        self.clip.stop();
    }
}

// ---- the orchestration ------------------------------------------------------

pub struct Video<P: VideoPlayer> {
    player: P,
    /// The clip on screen, while it plays.
    live: Option<Live<P::Clip>>,
    /// The clip on screen has finished (or couldn't play): its still holds.
    done: bool,
    /// Clips the decoder refused, for the host to mark (asset id, why).
    unplayable: Vec<(AssetId, String)>,
    /// The clip on screen is to start once no other decoder is open (since).
    live_wanted: Option<Duration>,
    /// Decoder failures in a row, and until when clips are passed over
    /// because of them (`record_failure`).
    failures: u32,
    backoff_until: Option<Duration>,
    /// Since when a decoder has been open with no clip of ours on it, and
    /// whether that was counted as a failure.
    orphan_since: Option<Duration>,
    orphan_counted: bool,
}

impl<P: VideoPlayer> Video<P> {
    pub fn new(player: P) -> Self {
        Self {
            player,
            live: None,
            done: false,
            unplayable: Vec::new(),
            live_wanted: None,
            failures: 0,
            backoff_until: None,
            orphan_since: None,
            orphan_counted: false,
        }
    }

    /// The host's player, for its own settings (the Android audio latency).
    pub fn player_mut(&mut self) -> &mut P {
        &mut self.player
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
    /// don't open another (one video decoder at a time).
    pub fn decoder_busy(&self) -> bool {
        self.live.is_some() || self.live_wanted.is_some() || self.player.decoders_open() > 0
    }

    /// Clips the decoder refused since the last call.
    #[must_use = "a refused clip not recorded is planned again"]
    pub fn take_unplayable(&mut self) -> Vec<(AssetId, String)> {
        std::mem::take(&mut self.unplayable)
    }

    /// The app went hidden: the clip pauses at once (`tick`, which applies
    /// the pause otherwise, doesn't run while hidden).
    pub fn pause_now(&self) {
        if let Some(l) = &self.live {
            l.clip.set_paused(true);
        }
    }

    pub fn debug_line(&self) -> Option<String> {
        self.live.as_ref().map(|l| l.clip.debug_line())
    }

    /// The playing clip's (position µs, has audio), for the status line.
    pub fn live_progress(&self) -> Option<(i64, bool)> {
        self.live
            .as_ref()
            .map(|l| (l.clip.position_us(), l.clip.audio_running()))
    }

    /// A clip is animating unless it's playing and paused.
    pub fn animating(&self, paused: bool) -> bool {
        self.live.as_ref().is_some_and(|l| !(l.played && paused))
    }

    /// A decoder's release is polled (the host's reaper doesn't wake the
    /// loop), for the clip waiting on it and for the wedge watch; and a
    /// backoff's end.
    pub fn deadline(&self) -> Option<Duration> {
        if self.live_wanted.is_some() || (self.live.is_none() && self.player.decoders_open() > 0) {
            return Some(DECODER_RELEASE_POLL);
        }
        self.backoff_until
            .map(|t| t.saturating_sub(clock::now()).max(Duration::from_millis(1)))
    }

    /// Opens a probing decoder on a clip's first frame.
    pub fn open_probe(
        &self,
        clip: &VideoClip,
        asset_id: AssetId,
    ) -> Result<ProbePlayer<P::Clip>, String> {
        let clip = self.player.open(clip, asset_id, Role::Probe)?;
        Ok(ProbePlayer {
            clip,
            opened: clock::now(),
        })
    }

    /// Polls a probing decoder. A failure is logged and recorded here (the
    /// refused clip, the backoff) as a live failure would be; the caller
    /// drops the plan being built.
    pub fn poll_probe(
        &mut self,
        probe: &mut ProbePlayer<P::Clip>,
        asset_id: AssetId,
    ) -> ProbeStatus {
        probe.clip.latch();
        let phase = probe.clip.phase();
        if let Phase::Failed(e) = &phase {
            match e {
                PlayerError::File(why) => log::error!("clip {asset_id}: {why}"),
                PlayerError::Decoder(why) => {
                    // The decoder refused it: not something a retry fixes.
                    // Only when it was the one decoder open (a second
                    // instance failing next to a playing clip may be
                    // contention, not the clip).
                    if self.live.is_none() {
                        self.unplayable.push((asset_id, why.clone()));
                    }
                    self.record_failure(&format!("clip {asset_id} probe: {why}"));
                }
            }
            return ProbeStatus::Failed;
        }
        if probe.age() > FIRST_FRAME_TIMEOUT {
            self.record_failure(&format!(
                "clip {asset_id} probe: no first frame within {FIRST_FRAME_TIMEOUT:?}"
            ));
            return ProbeStatus::Failed;
        }
        if phase == Phase::FirstFrame {
            ProbeStatus::Ready
        } else {
            ProbeStatus::Waiting
        }
    }

    /// The probe's first frame, for composing its tile.
    pub fn probe_frame(&self, probe: &ProbePlayer<P::Clip>) -> ClipFrame {
        ClipFrame::of(&probe.clip)
    }

    /// The playing clip's newest frame, unless none is up yet or the
    /// `show_still` switch asks for the composed still.
    pub fn live_frame(&self) -> Option<ClipFrame> {
        let live = self.live.as_ref().filter(|l| l.clip.has_frame())?;
        if switches::show_still() {
            return None;
        }
        Some(ClipFrame::of(&live.clip))
    }

    /// The slide that just landed starts playing if it's a clip (its still
    /// holds meanwhile).
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
        let open = self.player.decoders_open();
        if open > 0 {
            if clock::elapsed(since) < LIVE_DECODER_WAIT {
                return;
            }
            self.live_wanted = None;
            self.done = true;
            self.record_failure(&format!(
                "{open} decoder(s) still not released after {LIVE_DECODER_WAIT:?}, the clip's still holds"
            ));
            return;
        }
        self.live_wanted = None;
        if clock::elapsed(since) > RELEASE_WAIT_LOG {
            log::info!(
                "waited {:.0} ms for the previous decoder's release",
                clock::elapsed(since).as_secs_f64() * 1000.0
            );
        }
        let Some((clip, asset_id)) = cue.clip else {
            return;
        };
        match self
            .player
            .open(clip, asset_id, Role::Live { sound: cue.sound })
        {
            Ok(opened) => {
                log::info!(
                    "clip {asset_id} starting ({:?}, sound {})",
                    cue.playback,
                    if cue.sound.is_some() { "on" } else { "off" }
                );
                self.live = Some(Live {
                    clip: opened,
                    started: clock::now(),
                    has_audio: clip.info.has_audio,
                    played: false,
                });
            }
            Err(e) => {
                // As for a probe that can't open: the backoff keeps a
                // decoder under pressure from being asked clip after clip.
                self.done = true;
                self.record_failure(&format!("clip {asset_id}: can't start playback: {e}"));
            }
        }
    }

    /// Keeps the playing clip in step: pause, sound, the loop flag, the
    /// first frame's hand-over (play), and its end.
    #[must_use = "a Finished clip is taken back with `finish`, or it never ends"]
    pub fn tick(&mut self, cue: &LiveCue, paused: bool, looping: bool) -> Tick {
        self.open_live(cue);
        let Some(live) = self.live.as_mut() else {
            return Tick::Running;
        };
        live.clip.set_paused(paused);
        live.clip.set_sound(cue.sound);
        live.clip.latch();
        let waited = clock::elapsed(live.started);
        let failed = match live.clip.phase() {
            Phase::Ended => {
                log::info!("clip ended ({:?})", cue.playback);
                self.failures = 0;
                return Tick::Finished;
            }
            Phase::Failed(e) => e.to_string(),
            Phase::Starting if waited > FIRST_FRAME_TIMEOUT => {
                format!("no first frame within {FIRST_FRAME_TIMEOUT:?}")
            }
            Phase::Starting => return Tick::Running,
            Phase::FirstFrame if !live.played => {
                if !switches::hold_first()
                    && (live.clip.audio_ready() || waited > AUDIO_PREROLL_WAIT)
                {
                    log::info!(
                        "clip playing, {:.0} ms after the slide landed (audio {})",
                        waited.as_secs_f64() * 1000.0,
                        if !live.has_audio {
                            "none in the clip"
                        } else if cue.sound.is_none() {
                            "muted, not decoded"
                        } else if live.clip.audio_ready() {
                            "pre-rolled"
                        } else {
                            "not ready, starting without it"
                        }
                    );
                    live.clip.play();
                    live.played = true;
                }
                return Tick::Running;
            }
            Phase::FirstFrame | Phase::Playing => {
                live.clip.set_looping(looping);
                return Tick::Running;
            }
        };
        self.record_failure(&format!("clip failed: {failed}"));
        Tick::Finished
    }

    /// The clip's slide is going (its end, Next, a hide): takes the live
    /// clip back for the restill, its still holding from here.
    #[must_use = "stop() the clip: a dropped one leaks its decoder"]
    pub fn finish(&mut self) -> Option<Finished<P::Clip>> {
        self.live_wanted = None;
        let mut live = self.live.take()?;
        self.done = true;
        live.clip.latch();
        let frame = live.clip.has_frame().then(|| ClipFrame::of(&live.clip));
        Some(Finished {
            clip: live.clip,
            frame,
        })
    }

    /// A decoder failed or hung (on the frame, typically the RK VPU out of
    /// ion memory). Clips are passed over for a while, longer after each
    /// failure in a row: 30 s, 1, 2, 4, 8, then 10 min. A clip that plays
    /// to its end ends the run. The failure's one log line.
    pub fn record_failure(&mut self, why: &str) {
        self.failures += 1;
        let secs =
            (DECODER_BACKOFF_BASE_SECS << (self.failures - 1).min(5)).min(DECODER_BACKOFF_CAP_SECS);
        self.backoff_until = Some(clock::now() + Duration::from_secs(secs));
        log::error!(
            "decoder failure {} in a row ({why}): clips passed over for {secs} s",
            self.failures
        );
    }

    /// Ends a backoff that's over, and watches for a decoder whose release
    /// never finishes (a wedged VPU), which counts as a failure, and again
    /// each time its backoff ends while it is still open: a clip planned
    /// after the backoff would otherwise wait on its release for good, and
    /// the slideshow with it (raam#107).
    /// `probing`: a probe of ours is up (the live clip is known here).
    pub fn watch_decoders(&mut self, probing: bool) {
        let ours = self.live.is_some() || probing;
        if self.backoff_until.is_some_and(|t| clock::now() >= t) {
            self.backoff_until = None;
            if self.orphan_counted && !ours && self.player.decoders_open() > 0 {
                let open_for = self.orphan_since.map(clock::elapsed).unwrap_or_default();
                self.record_failure(&format!(
                    "a stopped decoder still not released after {open_for:?}"
                ));
                return;
            }
            log::info!("backoff over, clips are tried again");
        }
        if ours || self.player.decoders_open() == 0 {
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

// ---- simulation ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::fake::{advance, install};
    use raam_model::ClipInfo;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The decoders as the tests script them: the phase each role's clip
    /// reports, the process's decoder count, and a log of what was asked.
    struct Script {
        probe_phase: Phase,
        live_phase: Phase,
        audio_ready: bool,
        /// Decoders created and not released. A stopped clip releases its
        /// own at once unless `hold_release` (a slow or wedged teardown).
        decoders: u32,
        hold_release: bool,
        /// Every `open` is refused (no decoder created).
        fail_open: bool,
        opens: Vec<(AssetId, Role)>,
        plays: u32,
        looping: bool,
    }

    #[derive(Clone)]
    struct Fake(Rc<RefCell<Script>>);

    struct FakeClip {
        role: Role,
        script: Rc<RefCell<Script>>,
        latched: bool,
    }

    impl Fake {
        fn new() -> Self {
            install();
            Fake(Rc::new(RefCell::new(Script {
                probe_phase: Phase::Starting,
                live_phase: Phase::Starting,
                audio_ready: false,
                decoders: 0,
                hold_release: false,
                fail_open: false,
                opens: Vec::new(),
                plays: 0,
                looping: false,
            })))
        }

        fn with<R>(&self, f: impl FnOnce(&mut Script) -> R) -> R {
            f(&mut self.0.borrow_mut())
        }
    }

    impl VideoPlayer for Fake {
        type Clip = FakeClip;

        fn open(&self, _: &VideoClip, asset_id: AssetId, role: Role) -> Result<FakeClip, String> {
            let refused = self.with(|s| {
                s.opens.push((asset_id, role));
                if !s.fail_open {
                    s.decoders += 1;
                }
                s.fail_open
            });
            if refused {
                return Err("no decoder".into());
            }
            Ok(FakeClip {
                role,
                script: self.0.clone(),
                latched: false,
            })
        }

        fn decoders_open(&self) -> u32 {
            self.with(|s| s.decoders)
        }
    }

    impl OpenClip for FakeClip {
        fn latch(&mut self) {
            self.latched = self.latched || !matches!(self.phase(), Phase::Starting);
        }
        fn phase(&self) -> Phase {
            let s = self.script.borrow();
            match self.role {
                Role::Probe => s.probe_phase.clone(),
                Role::Live { .. } => s.live_phase.clone(),
            }
        }
        fn play(&mut self) {
            self.script.borrow_mut().plays += 1;
        }
        fn set_paused(&self, _: bool) {}
        fn set_looping(&self, looping: bool) {
            self.script.borrow_mut().looping = looping;
        }
        fn set_sound(&mut self, _: Option<f32>) {}
        fn audio_ready(&self) -> bool {
            self.script.borrow().audio_ready
        }
        fn audio_running(&self) -> bool {
            false
        }
        fn position_us(&self) -> i64 {
            0
        }
        fn has_frame(&self) -> bool {
            self.latched
        }
        fn oes(&self) -> GlUint {
            7
        }
        fn matrix(&self) -> [f32; 16] {
            [0.0; 16]
        }
        fn debug_line(&self) -> String {
            String::new()
        }
        fn stop(self) {
            let mut s = self.script.borrow_mut();
            if !s.hold_release {
                s.decoders -= 1;
            }
        }
    }

    fn clip() -> VideoClip {
        VideoClip {
            path: "clip.mp4".into(),
            info: ClipInfo {
                mime: "video/avc".into(),
                coded_w: 1280,
                coded_h: 720,
                rotation: 0,
                duration_us: 10_000_000,
                has_audio: true,
                audio: Some("audio/mp4a-latm".into()),
            },
        }
    }

    fn cue(clip: &VideoClip) -> LiveCue<'_> {
        LiveCue {
            clip: Some((clip, AssetId::new(42))),
            sound: None,
            playback: VideoPlayback::Continue,
        }
    }

    /// The backoff's length, read off the deadline while nothing else is
    /// polled.
    fn backoff_left(video: &Video<Fake>) -> Option<Duration> {
        assert!(video.backing_off());
        video.deadline()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_live_clip_waits_for_the_probes_release() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        let probe = video.open_probe(&clip, AssetId::new(42)).unwrap();
        fake.with(|s| s.hold_release = true);
        probe.stop();
        assert!(video.decoder_busy());
        video.start(&cue(&clip));
        assert_eq!(fake.with(|s| s.opens.len()), 1, "only the probe so far");
        assert_eq!(video.deadline(), Some(DECODER_RELEASE_POLL));
        advance(ms(800));
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Running);
        assert_eq!(fake.with(|s| s.opens.len()), 1);
        fake.with(|s| s.decoders = 0);
        advance(DECODER_RELEASE_POLL);
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Running);
        assert_eq!(
            fake.with(|s| s.opens.clone()),
            vec![
                (AssetId::new(42), Role::Probe),
                (AssetId::new(42), Role::Live { sound: None })
            ]
        );
        assert!(video.decoder_busy() && !video.done());
    }

    #[test]
    fn a_live_clip_that_cant_open_holds_the_still_and_backs_off() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        fake.with(|s| s.fail_open = true);
        video.start(&cue(&clip));
        assert_eq!(fake.with(|s| s.opens.len()), 1);
        assert!(video.done());
        assert_eq!(
            backoff_left(&video),
            Some(Duration::from_secs(DECODER_BACKOFF_BASE_SECS))
        );
    }

    #[test]
    fn a_release_that_never_comes_holds_the_still_and_backs_off() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        fake.with(|s| s.decoders = 1);
        video.start(&cue(&clip));
        advance(LIVE_DECODER_WAIT + ms(1));
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Running);
        assert!(video.done());
        assert_eq!(fake.with(|s| s.opens.len()), 0, "no second decoder");
        fake.with(|s| s.decoders = 0);
        assert_eq!(
            backoff_left(&video),
            Some(Duration::from_secs(DECODER_BACKOFF_BASE_SECS))
        );
    }

    #[test]
    fn frame_zero_plays_once_the_sound_is_ready_or_after_the_preroll_wait() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        video.start(&cue(&clip));
        fake.with(|s| s.live_phase = Phase::FirstFrame);
        assert_eq!(video.tick(&cue(&clip), false, true), Tick::Running);
        assert_eq!(fake.with(|s| (s.plays, s.looping)), (0, false));
        assert!(video.animating(true), "a clip not yet played animates");
        advance(AUDIO_PREROLL_WAIT + ms(1));
        assert_eq!(video.tick(&cue(&clip), false, true), Tick::Running);
        assert_eq!(fake.with(|s| s.plays), 1);
        assert!(!video.animating(true), "played and paused holds still");
        assert_eq!(video.tick(&cue(&clip), false, true), Tick::Running);
        assert_eq!(fake.with(|s| (s.plays, s.looping)), (1, true));

        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        video.start(&cue(&clip));
        fake.with(|s| {
            s.live_phase = Phase::FirstFrame;
            s.audio_ready = true;
        });
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Running);
        assert_eq!(fake.with(|s| s.plays), 1, "pre-rolled: plays at once");
    }

    #[test]
    fn an_ended_clip_finishes_and_ends_the_failure_run() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        video.record_failure("test");
        advance(Duration::from_secs(DECODER_BACKOFF_BASE_SECS));
        video.watch_decoders(false);
        assert!(!video.backing_off());
        video.start(&cue(&clip));
        fake.with(|s| s.live_phase = Phase::Playing);
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Running);
        fake.with(|s| s.live_phase = Phase::Ended);
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Finished);
        let finished = video.finish().unwrap();
        assert_eq!(finished.frame.map(|f| f.texture), Some(7));
        finished.stop();
        assert!(video.done() && !video.decoder_busy());
        video.record_failure("test");
        assert_eq!(
            backoff_left(&video),
            Some(Duration::from_secs(DECODER_BACKOFF_BASE_SECS)),
            "the run started over"
        );
    }

    #[test]
    fn the_backoff_doubles_up_to_its_cap() {
        let fake = Fake::new();
        let mut video = Video::new(fake);
        let mut lengths = Vec::new();
        for _ in 0..7 {
            video.record_failure("test");
            lengths.push(backoff_left(&video).unwrap().as_secs());
        }
        assert_eq!(lengths, [30, 60, 120, 240, 480, 600, 600]);
    }

    #[test]
    fn a_probe_the_decoder_refuses_marks_the_clip_and_backs_off() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let mut probe = video.open_probe(&clip(), AssetId::new(42)).unwrap();
        fake.with(|s| s.probe_phase = Phase::Failed(PlayerError::Decoder("refused".into())));
        assert_eq!(
            video.poll_probe(&mut probe, AssetId::new(42)),
            ProbeStatus::Failed
        );
        probe.stop();
        assert_eq!(
            video.take_unplayable(),
            vec![(AssetId::new(42), "refused".to_string())]
        );
        assert!(video.backing_off());
    }

    #[test]
    fn a_probe_whose_file_is_gone_neither_marks_nor_backs_off() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let mut probe = video.open_probe(&clip(), AssetId::new(42)).unwrap();
        fake.with(|s| s.probe_phase = Phase::Failed(PlayerError::File("open: gone".into())));
        assert_eq!(
            video.poll_probe(&mut probe, AssetId::new(42)),
            ProbeStatus::Failed
        );
        assert!(video.take_unplayable().is_empty());
        assert!(!video.backing_off());
    }

    #[test]
    fn a_probe_failing_beside_a_live_clip_backs_off_without_marking() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        video.start(&cue(&clip));
        let mut probe = video.open_probe(&clip, AssetId::new(43)).unwrap();
        fake.with(|s| s.probe_phase = Phase::Failed(PlayerError::Decoder("ion".into())));
        assert_eq!(
            video.poll_probe(&mut probe, AssetId::new(43)),
            ProbeStatus::Failed
        );
        assert!(video.take_unplayable().is_empty(), "maybe contention");
        assert!(video.backing_off());
    }

    #[test]
    fn a_probe_without_a_first_frame_times_out() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let mut probe = video.open_probe(&clip(), AssetId::new(42)).unwrap();
        assert_eq!(
            video.poll_probe(&mut probe, AssetId::new(42)),
            ProbeStatus::Waiting
        );
        advance(FIRST_FRAME_TIMEOUT + ms(1));
        assert_eq!(
            video.poll_probe(&mut probe, AssetId::new(42)),
            ProbeStatus::Failed
        );
        assert!(video.take_unplayable().is_empty());
        assert!(video.backing_off());

        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let mut probe = video.open_probe(&clip(), AssetId::new(42)).unwrap();
        fake.with(|s| s.probe_phase = Phase::FirstFrame);
        assert_eq!(
            video.poll_probe(&mut probe, AssetId::new(42)),
            ProbeStatus::Ready
        );
        assert_eq!(video.probe_frame(&probe).texture, 7);
    }

    #[test]
    fn a_live_clip_that_fails_or_never_shows_a_frame_backs_off() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        video.start(&cue(&clip));
        fake.with(|s| s.live_phase = Phase::Failed(PlayerError::Decoder("OMX".into())));
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Finished);
        assert!(video.backing_off());
        assert!(
            video.take_unplayable().is_empty(),
            "a live failure marks nothing"
        );

        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        video.start(&cue(&clip));
        advance(FIRST_FRAME_TIMEOUT);
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Running);
        advance(ms(1));
        assert_eq!(video.tick(&cue(&clip), false, false), Tick::Finished);
        assert!(video.backing_off());
    }

    #[test]
    fn a_clip_starting_during_a_backoff_holds_its_still() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        let clip = clip();
        video.record_failure("test");
        video.start(&cue(&clip));
        assert!(video.done());
        assert!(fake.with(|s| s.opens.is_empty()));
        assert!(!video.decoder_busy());
    }

    #[test]
    fn a_stopped_decoder_never_released_keeps_clips_backed_off() {
        let fake = Fake::new();
        let mut video = Video::new(fake.clone());
        fake.with(|s| s.decoders = 1);
        video.watch_decoders(true);
        advance(DECODER_RELEASE_TIMEOUT + ms(1));
        video.watch_decoders(true);
        assert!(!video.backing_off(), "a probe of ours is on it");
        video.watch_decoders(false);
        advance(DECODER_RELEASE_TIMEOUT);
        video.watch_decoders(false);
        assert!(!video.backing_off());
        advance(ms(1));
        video.watch_decoders(false);
        assert!(video.backing_off(), "wedged: counted");
        // Still open when the backoff ends: counted again, for twice as
        // long, so a clip planned now isn't parked on its release for good.
        advance(Duration::from_secs(DECODER_BACKOFF_BASE_SECS));
        video.watch_decoders(false);
        assert!(video.backing_off(), "still wedged: counted again");
        advance(Duration::from_secs(DECODER_BACKOFF_BASE_SECS));
        video.watch_decoders(false);
        assert!(video.backing_off(), "the second backoff is twice as long");
        // Released at last: the backoff runs out and clips come back.
        fake.with(|s| s.decoders = 0);
        advance(Duration::from_secs(DECODER_BACKOFF_BASE_SECS));
        video.watch_decoders(false);
        assert!(!video.backing_off(), "released: the backoff ends");
        assert_eq!(video.deadline(), None, "nothing left to poll");
    }

    #[test]
    fn no_video_never_opens() {
        install();
        let video = Video::new(NoVideo);
        assert!(video.open_probe(&clip(), AssetId::new(42)).is_err());
        assert!(!video.decoder_busy());
        assert_eq!(video.deadline(), None);
    }
}
