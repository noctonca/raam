//! The MediaCodec stack behind raam-core's `VideoPlayer` seam. The core
//! runs the orchestration (probe, live start, one decoder at a time,
//! backoff, the wedge watch) and draws the frames; each open clip here is
//! a `player::Player` (the decode and audio threads, the SurfaceTexture
//! bridge), and the process-wide count of decoders not yet released is
//! `player::video_decoders`.
use crate::player::{self, Player};
use android_activity::AndroidAppWaker;
use raam_core::video::{Role, VideoPlayer};
use raam_model::VideoClip;

pub struct Decoders {
    waker: AndroidAppWaker,
    /// The music output's latency (ms) as Android reports it, set by
    /// lib.rs, and the calibrated extra on top (the "Audio delay" setting,
    /// or `debug.video.audio_extra_ms`).
    pub audio_latency_ms: u32,
    pub audio_extra_ms: i32,
}

impl Decoders {
    pub fn new(waker: AndroidAppWaker) -> Self {
        Self {
            waker,
            audio_latency_ms: 0,
            audio_extra_ms: 0,
        }
    }
}

impl VideoPlayer for Decoders {
    type Clip = Player;

    fn open(&self, clip: &VideoClip, asset_id: i64, role: Role) -> Result<Player, String> {
        let (label, sound, latency) = match role {
            Role::Probe => (format!("clip {asset_id} (probe)"), None, 0),
            Role::Live { sound } => {
                let latency = self.audio_latency_ms as i32 + self.audio_extra_ms;
                if sound.is_some() {
                    log::info!(
                        "audio delay applied: {latency} ms ({} reported + {} calibrated)",
                        self.audio_latency_ms,
                        self.audio_extra_ms
                    );
                }
                (format!("clip {asset_id}"), sound, latency)
            }
        };
        Player::open(
            &clip.path,
            clip.info.clone(),
            matches!(role, Role::Probe),
            sound,
            latency,
            self.waker.clone(),
            label,
        )
    }

    fn decoders_open(&self) -> u32 {
        player::video_decoders()
    }
}
