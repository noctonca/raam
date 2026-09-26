//! The video seam: how the slideshow drives the host's video stack (probe
//! and live players, decoder backoff, the wedge watch) without seeing any
//! of it. This trait is the surface migration step 2 carved out in the lab
//! repo, verbatim; narrowing it to ARCHITECTURE.md's finer-grained
//! `VideoPlayer` (open/play/pause/stop/phase/has_frame/oes/matrix) is step
//! 5b/6 work, once the App controller crosses. Photo-only hosts implement
//! it as a stub whose `open_probe` never succeeds.

use crate::gl::GlUint;
use raam_model::{VideoClip, VideoPlayback};
use std::time::Duration;

/// What the slideshow currently asks of the live player: the current
/// slide's clip (if it is one) and the sound and playback settings.
pub struct LiveCue<'a> {
    /// The clip and its asset id.
    pub clip: Option<(&'a VideoClip, i64)>,
    /// The volume, or `None` for no audio decoding.
    pub sound: Option<f32>,
    pub playback: VideoPlayback,
}

/// What `poll_probe` found.
pub enum ProbeStatus {
    /// Still decoding: ask again next frame.
    Waiting,
    /// The first frame is on the texture (`probe_frame`): compose the tile,
    /// then `stop` the probe.
    Ready,
    /// The probe failed (logged, the refusal and backoff recorded host-side):
    /// drop the plan being built.
    Failed,
}

/// `tick`'s verdict: `Finished` means the clip just ended or failed — the
/// slideshow restills its slide (`finish`) and restarts the dwell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    Running,
    Finished,
}

/// A decoded frame, drawable into whatever framebuffer is bound.
pub trait ClipFrame: Copy {
    /// `upright` pre-flips for drawing to the screen; a tile's `source`
    /// copy draws with `false` (row 0 = the picture's top).
    ///
    /// # Safety
    /// Requires a current GL context and the quad buffers it is given.
    unsafe fn draw(&self, quad_vbo: GlUint, quad_ibo: GlUint, scale: (f32, f32), upright: bool);
}

/// A short-lived decoder bringing up a clip's first frame for the tile of
/// a plan being built. The plan bookkeeping (slot, rect, meta) stays with
/// the slideshow's `Building`.
pub trait ProbeHandle {
    /// How long ago the decoder was opened.
    fn age(&self) -> Duration;
    fn stop(self);
}

/// A live player taken back by `finish`, with the frame that was on screen
/// (if any) for the restill. `stop` it once that is composed.
pub trait FinishedHandle {
    type Frame;
    fn frame(&self) -> Option<Self::Frame>;
    fn stop(self);
}

pub trait VideoSeam {
    type Frame: ClipFrame;
    type Probe: ProbeHandle;
    type Finished: FinishedHandle<Frame = Self::Frame>;

    /// The clip on screen has finished (or couldn't play): its still holds.
    fn done(&self) -> bool;
    /// Clips are being passed over after a decoder failure.
    fn backing_off(&self) -> bool;
    /// A decoder is playing, wanted, or still winding down somewhere:
    /// don't open another (one video decoder at a time).
    fn decoder_busy(&self) -> bool;
    /// A clip is animating unless it's playing and paused.
    fn animating(&self, paused: bool) -> bool;
    /// The video machinery's polling deadline, and a backoff's end.
    fn deadline(&self) -> Option<Duration>;
    /// The playing clip's (position µs, has audio), for the status line.
    fn live_progress(&self) -> Option<(i64, bool)>;

    /// Opens a probing decoder on a clip's first frame.
    fn open_probe(&self, clip: &VideoClip, asset_id: i64) -> Result<Self::Probe, String>;
    /// Polls a probing decoder. A failure is logged and recorded host-side
    /// (the refused clip, the backoff); the caller drops the plan being
    /// built.
    fn poll_probe(&mut self, probe: &mut Self::Probe, asset_id: i64) -> ProbeStatus;
    /// The probe's first frame, for composing its tile.
    fn probe_frame(&self, probe: &Self::Probe) -> Self::Frame;

    /// The playing clip's newest frame, unless none is up yet (or a debug
    /// switch asks for the composed still).
    fn live_frame(&self) -> Option<Self::Frame>;
    /// The slide that just landed starts playing if it's a clip.
    fn start(&mut self, cue: &LiveCue);
    /// Keeps the playing clip in step: pause, sound, the loop flag, the
    /// first frame's hand-over, and its end.
    fn tick(&mut self, cue: &LiveCue, paused: bool, looping: bool) -> Tick;
    /// The clip's slide is going (its end, Next, a hide): takes the live
    /// player back for the restill, its still holding from here.
    fn finish(&mut self) -> Option<Self::Finished>;

    /// A decoder failed or hung: clips are passed over for a while, longer
    /// after each failure in a row.
    fn record_failure(&mut self, why: &str);
    /// Ends a backoff that's over, and watches for a decoder whose release
    /// never finishes (a wedged VPU). `probing`: a probe of ours is up.
    fn watch_decoders(&mut self, probing: bool);
}
