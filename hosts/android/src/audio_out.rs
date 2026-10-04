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

use raam_model::limits::{AUDIO_OUT_BUFFERS as NUM_BUFFERS, PCM_BATCH_BYTES};

struct BufferSlots {
    free: Mutex<u32>,
    cond: Condvar,
}

unsafe extern "C" fn buffer_queue_callback(
    _caller: sles::SLAndroidSimpleBufferQueueItf,
    context: *mut c_void,
) {
    // SAFETY: `context` is the `slots` box `AudioOut::new` registered, and
    // `AudioOut`'s Drop frees it only after Destroy, when no callback runs.
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

// SAFETY: Android's OpenSL ES engine is thread-safe
// (SL_ENGINEOPTION_THREADSAFE is on by default), and the engine and output
// mix are never destroyed; each player is only touched by the thread that
// owns it.
unsafe impl Send for Engine {}
// SAFETY: as for Send.
unsafe impl Sync for Engine {}

static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

fn engine() -> Result<&'static Engine, String> {
    ENGINE
        // SAFETY: each object is checked created and realized before its
        // interfaces are used, the out-pointers are live locals of the right
        // interface type, and the OnceLock makes this run once per process.
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
    /// The PCM each queue slot plays. Android's buffer queue reads a
    /// buffer in place until its callback, so the queue gets these, never
    /// the caller's. Dropped after `Drop` has destroyed the player.
    bufs: Vec<Vec<u8>>,
    /// The slot `enqueue` fills next. The queue plays in order, so once a
    /// slot is free this one, the oldest, has played.
    next: usize,
    pub enqueued_bytes: u64,
}

// SAFETY: only the thread that owns it calls into it (it isn't Sync); the
// buffer-queue callback runs on an OpenSL ES thread but only touches
// `slots`, whose state is behind a Mutex.
unsafe impl Send for AudioOut {}

impl AudioOut {
    /// `sample_rate_hz`/`channels` must match the decoder's PCM (the track's
    /// `sample-rate`/`channel-count`): this path has no resampler. The
    /// player starts paused.
    pub fn new(sample_rate_hz: u32, channels: u32) -> Result<Self, String> {
        let engine = engine()?;
        // SAFETY: the engine and output mix live for the process; the
        // locator and format structs are locals that outlive
        // CreateAudioPlayer, which copies them; `player_obj` is used only
        // once created, and destroyed once, either here on error or in Drop
        // (via `out`); `slots` is registered only after `out` owns it.
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
                    u32::try_from(ids.len()).expect("a handful of interface ids"),
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
                bufs: (0..NUM_BUFFERS)
                    .map(|_| Vec::with_capacity(PCM_BATCH_BYTES * 2))
                    .collect(),
                next: 0,
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
            // SAFETY: `play_itf` comes from the player, which lives until Drop.
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
            // In -6000..=0: the volume is in (0.001, 1] here.
            sles::SLmillibel::try_from(raam_core::num::sat_i32(
                (2000.0 * volume.min(1.0).log10()).round().max(-6000.0),
            ))
            .expect("-6000..=0 millibels fit i16")
        };
        check(
            // SAFETY: `volume_itf` comes from the player, which lives until Drop.
            unsafe { ((**self.volume_itf).SetVolumeLevel.unwrap())(self.volume_itf, mb) },
            "SetVolumeLevel",
        )
    }

    /// How much the player has played since it was made, in ms.
    pub fn position_ms(&self) -> Option<u32> {
        let mut ms: sles::SLmillisecond = 0;
        // SAFETY: `play_itf` lives until Drop, and `ms` is a live local.
        let r = unsafe { ((**self.play_itf).GetPosition.unwrap())(self.play_itf, &mut ms) };
        (r == sles::SL_RESULT_SUCCESS).then_some(ms)
    }

    /// Waits (up to `timeout`) for a free queue slot, i.e. for playback to
    /// have consumed an earlier buffer, then queues a copy of `pcm`, so the
    /// caller may reuse it at once. Ok(false) = no slot yet: the caller
    /// checks for stop or pause and tries again.
    pub fn enqueue(&mut self, pcm: &[u8], timeout: Duration) -> Result<bool, String> {
        // SAFETY: `slots` is a live box until Drop, and only shared borrows
        // of it are ever made (its state is behind a Mutex).
        let slots = unsafe { &*self.slots };
        {
            let mut free = slots.free.lock().unwrap();
            if *free == 0 {
                free = slots.cond.wait_timeout(free, timeout).unwrap().0;
                if *free == 0 {
                    return Ok(false);
                }
            }
            *free -= 1;
        }
        let buf = &mut self.bufs[self.next];
        buf.clear();
        buf.extend_from_slice(pcm);
        // SAFETY: `bq_itf` lives until Drop. The queue reads `buf` until
        // this slot's callback: `buf` is only written again once the queue
        // has played it (see `next`), and lives until Drop has destroyed
        // the player.
        let status = unsafe {
            ((**self.bq_itf).Enqueue.unwrap())(
                self.bq_itf,
                buf.as_ptr() as *const c_void,
                sles::SLuint32::try_from(buf.len()).expect("a PCM batch fits u32"),
            )
        };
        if let Err(e) = check(status, "bufferqueue Enqueue") {
            // Not queued, so no callback will hand the slot back.
            *slots.free.lock().unwrap() += 1;
            return Err(e);
        }
        self.next = (self.next + 1) % self.bufs.len();
        self.enqueued_bytes += pcm.len() as u64;
        Ok(true)
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        // SAFETY: `player_obj` and `slots` were made in `new` and are freed
        // only here, once; Destroy blocks until no callback is running, so
        // nothing reads `slots` after it is freed, nor `bufs`, which drop
        // after this body.
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
