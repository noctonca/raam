//! OpenSL ES audio output: engine -> output mix -> buffer-queue player, for
//! a slideshow that plays many clips. The engine and the output mix are
//! made once and kept (OpenSL ES wants one engine per process), and each
//! clip gets its own player, destroyed when the clip ends. The player also
//! takes a volume, pauses, and reports how much it has played (the A/V
//! drift measurement).
use crate::sles;
use std::ffi::c_void;
use std::ptr;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use raam_model::limits::AUDIO_OUT_BUFFERS as NUM_BUFFERS;

struct BufferSlots {
    free: Mutex<u32>,
    cond: Condvar,
}

unsafe extern "C" fn buffer_queue_callback(
    _caller: sles::SLAndroidSimpleBufferQueueItf,
    context: *mut c_void,
) {
    let slots = unsafe { &*(context as *const BufferSlots) };
    let mut free = slots.free.lock().unwrap();
    *free += 1;
    slots.cond.notify_one();
}

/// The engine and output mix, made on first use and never destroyed.
struct Engine {
    engine_itf: sles::SLEngineItf,
    output_mix: sles::SLObjectItf,
}

// Android's OpenSL ES engine is thread-safe (SL_ENGINEOPTION_THREADSAFE is
// on by default); each player is only touched by the thread that owns it.
unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

fn engine() -> Result<&'static Engine, String> {
    ENGINE
        .get_or_init(|| unsafe {
            let mut engine_obj: sles::SLObjectItf = ptr::null();
            check(
                sles::slCreateEngine(&mut engine_obj, 0, ptr::null(), 0, ptr::null(), ptr::null()),
                "slCreateEngine",
            )?;
            check(
                ((**engine_obj).Realize.unwrap())(engine_obj, sles::SL_BOOLEAN_FALSE),
                "engine Realize",
            )?;
            let mut engine_itf: sles::SLEngineItf = ptr::null();
            check(
                ((**engine_obj).GetInterface.unwrap())(
                    engine_obj,
                    sles::SL_IID_ENGINE,
                    &mut engine_itf as *mut _ as *mut c_void,
                ),
                "engine GetInterface(SL_IID_ENGINE)",
            )?;
            let mut output_mix: sles::SLObjectItf = ptr::null();
            check(
                ((**engine_itf).CreateOutputMix.unwrap())(
                    engine_itf,
                    &mut output_mix,
                    0,
                    ptr::null(),
                    ptr::null(),
                ),
                "CreateOutputMix",
            )?;
            check(
                ((**output_mix).Realize.unwrap())(output_mix, sles::SL_BOOLEAN_FALSE),
                "output mix Realize",
            )?;
            Ok(Engine {
                engine_itf,
                output_mix,
            })
        })
        .as_ref()
        .map_err(|e| e.clone())
}

pub struct AudioOut {
    player_obj: sles::SLObjectItf,
    play_itf: sles::SLPlayItf,
    bq_itf: sles::SLAndroidSimpleBufferQueueItf,
    volume_itf: sles::SLVolumeItf,
    /// Freed in `Drop`, after the player (and so its callback) is gone.
    slots: *mut BufferSlots,
    pub enqueued_bytes: u64,
}

// Only the thread that owns it calls into it; the buffer-queue callback runs
// on an OpenSL ES thread but only touches `slots`.
unsafe impl Send for AudioOut {}

impl AudioOut {
    /// `sample_rate_hz`/`channels` must match the decoder's PCM (the track's
    /// `sample-rate`/`channel-count`): this path has no resampler. The
    /// player starts paused.
    pub fn new(sample_rate_hz: u32, channels: u32) -> Result<Self, String> {
        let engine = engine()?;
        unsafe {
            let mut loc_bufq = sles::SLDataLocator_AndroidSimpleBufferQueue {
                locatorType: sles::SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE,
                numBuffers: NUM_BUFFERS,
            };
            let channel_mask = match channels {
                1 => sles::SL_SPEAKER_FRONT_CENTER,
                _ => sles::SL_SPEAKER_FRONT_LEFT | sles::SL_SPEAKER_FRONT_RIGHT,
            };
            let mut format_pcm = sles::SLDataFormat_PCM {
                formatType: sles::SL_DATAFORMAT_PCM,
                numChannels: channels,
                samplesPerSec: sample_rate_hz * 1000, // millihertz
                bitsPerSample: sles::SL_PCMSAMPLEFORMAT_FIXED_16,
                containerSize: sles::SL_PCMSAMPLEFORMAT_FIXED_16,
                channelMask: channel_mask,
                endianness: sles::SL_BYTEORDER_LITTLEENDIAN,
            };
            let mut audio_src = sles::SLDataSource {
                pLocator: &mut loc_bufq as *mut _ as *mut c_void,
                pFormat: &mut format_pcm as *mut _ as *mut c_void,
            };
            let mut loc_outmix = sles::SLDataLocator_OutputMix {
                locatorType: sles::SL_DATALOCATOR_OUTPUTMIX,
                outputMix: engine.output_mix,
            };
            let mut audio_snk = sles::SLDataSink {
                pLocator: &mut loc_outmix as *mut _ as *mut c_void,
                pFormat: ptr::null_mut(),
            };

            let mut player_obj: sles::SLObjectItf = ptr::null();
            let ids = [sles::SL_IID_ANDROIDSIMPLEBUFFERQUEUE, sles::SL_IID_VOLUME];
            let req = [sles::SL_BOOLEAN_TRUE, sles::SL_BOOLEAN_TRUE];
            check(
                ((**engine.engine_itf).CreateAudioPlayer.unwrap())(
                    engine.engine_itf,
                    &mut player_obj,
                    &mut audio_src,
                    &mut audio_snk,
                    ids.len() as u32,
                    ids.as_ptr(),
                    req.as_ptr(),
                ),
                "CreateAudioPlayer",
            )?;
            let destroy = |obj: sles::SLObjectItf| ((**obj).Destroy.unwrap())(obj);
            let result = (|| {
                check(
                    ((**player_obj).Realize.unwrap())(player_obj, sles::SL_BOOLEAN_FALSE),
                    "player Realize",
                )?;
                let get = |iid, out: *mut c_void, what| {
                    check(
                        ((**player_obj).GetInterface.unwrap())(player_obj, iid, out),
                        what,
                    )
                };
                let mut play_itf: sles::SLPlayItf = ptr::null();
                get(
                    sles::SL_IID_PLAY,
                    &mut play_itf as *mut _ as *mut c_void,
                    "GetInterface(SL_IID_PLAY)",
                )?;
                let mut bq_itf: sles::SLAndroidSimpleBufferQueueItf = ptr::null();
                get(
                    sles::SL_IID_ANDROIDSIMPLEBUFFERQUEUE,
                    &mut bq_itf as *mut _ as *mut c_void,
                    "GetInterface(BUFFERQUEUE)",
                )?;
                let mut volume_itf: sles::SLVolumeItf = ptr::null();
                get(
                    sles::SL_IID_VOLUME,
                    &mut volume_itf as *mut _ as *mut c_void,
                    "GetInterface(SL_IID_VOLUME)",
                )?;
                Ok((play_itf, bq_itf, volume_itf))
            })();
            let (play_itf, bq_itf, volume_itf) = match result {
                Ok(v) => v,
                Err(e) => {
                    destroy(player_obj);
                    return Err(e);
                }
            };
            let slots = Box::into_raw(Box::new(BufferSlots {
                free: Mutex::new(NUM_BUFFERS),
                cond: Condvar::new(),
            }));
            let out = Self {
                player_obj,
                play_itf,
                bq_itf,
                volume_itf,
                slots,
                enqueued_bytes: 0,
            };
            check(
                ((**bq_itf).RegisterCallback.unwrap())(
                    bq_itf,
                    Some(buffer_queue_callback),
                    slots as *mut c_void,
                ),
                "bufferqueue RegisterCallback",
            )?;
            out.set_state(sles::SL_PLAYSTATE_PAUSED)?;
            Ok(out)
        }
    }

    fn set_state(&self, state: sles::SLuint32) -> Result<(), String> {
        check(
            unsafe { ((**self.play_itf).SetPlayState.unwrap())(self.play_itf, state) },
            "SetPlayState",
        )
    }

    pub fn set_playing(&self, playing: bool) -> Result<(), String> {
        self.set_state(if playing {
            sles::SL_PLAYSTATE_PLAYING
        } else {
            sles::SL_PLAYSTATE_PAUSED
        })
    }

    /// 0..1, as a level in millibels (-60 dB at 1%, silence at 0).
    pub fn set_volume(&self, volume: f32) -> Result<(), String> {
        let mb: sles::SLmillibel = if volume <= 0.001 {
            sles::SL_MILLIBEL_MIN
        } else {
            (2000.0 * volume.min(1.0).log10()).round().max(-6000.0) as sles::SLmillibel
        };
        check(
            unsafe { ((**self.volume_itf).SetVolumeLevel.unwrap())(self.volume_itf, mb) },
            "SetVolumeLevel",
        )
    }

    /// How much the player has played since it was made, in ms.
    pub fn position_ms(&self) -> Option<u32> {
        let mut ms: sles::SLmillisecond = 0;
        let r = unsafe { ((**self.play_itf).GetPosition.unwrap())(self.play_itf, &mut ms) };
        (r == sles::SL_RESULT_SUCCESS).then_some(ms)
    }

    /// Waits (up to `timeout`) for a free queue slot, i.e. for playback to
    /// have consumed an earlier buffer, then enqueues `pcm`. Ok(false) = no
    /// slot yet: the caller checks for stop or pause and tries again.
    pub fn enqueue(&mut self, pcm: &[u8], timeout: Duration) -> Result<bool, String> {
        {
            let slots = unsafe { &*self.slots };
            let mut free = slots.free.lock().unwrap();
            if *free == 0 {
                free = slots.cond.wait_timeout(free, timeout).unwrap().0;
                if *free == 0 {
                    return Ok(false);
                }
            }
            *free -= 1;
        }
        let status = unsafe {
            ((**self.bq_itf).Enqueue.unwrap())(
                self.bq_itf,
                pcm.as_ptr() as *const c_void,
                pcm.len() as sles::SLuint32,
            )
        };
        check(status, "bufferqueue Enqueue")?;
        self.enqueued_bytes += pcm.len() as u64;
        Ok(true)
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        unsafe {
            let _ = self.set_state(sles::SL_PLAYSTATE_STOPPED);
            // Destroy blocks until no callback is running, so the slots can go.
            ((**self.player_obj).Destroy.unwrap())(self.player_obj);
            drop(Box::from_raw(self.slots));
        }
    }
}

fn check(result: sles::SLresult, what: &str) -> Result<(), String> {
    if result == sles::SL_RESULT_SUCCESS {
        Ok(())
    } else {
        Err(format!("{what} failed: SLresult 0x{result:08x}"))
    }
}
