//! Minimal OpenSL ES bindings. There's no AAudio at API 23 and no Rust
//! crate for OpenSL ES, so this is the audio output path. sles_sys.rs is
//! checked in, pre-generated (docs/ARCHITECTURE.md's dependency register:
//! "no bindgen, no libclang at build time"): bindgen 0.70 over
//! `SLES/OpenSLES.h` + `SLES/OpenSLES_Android.h` from NDK 28.2.13676358,
//! targeting armv7-linux-androideabi23, `extern "C"` rewritten `unsafe
//! extern` for edition 2024. Regenerate the same way if the NDK's headers
//! ever change (they are a frozen Khronos API; they won't).
#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    unsafe_op_in_unsafe_fn,
    clippy::all
)]

include!("sles_sys.rs");

// The handful of constants below are simple cast-expression #defines in the
// NDK headers (e.g. `#define SL_BOOLEAN_TRUE ((SLboolean) 0x00000001)`).
// bindgen's macro-constant translation didn't pick these particular ones up
// (only the `extern const SLInterfaceID` symbols like SL_IID_ENGINE came
// through as real linked statics) - rather than fight bindgen's macro
// allowlisting further, they're hardcoded here directly from the header
// values checked in $ANDROID_NDK_ROOT/.../include/SLES/OpenSLES.h and
// OpenSLES_Android.h.
pub const SL_BOOLEAN_FALSE: SLboolean = 0x0000_0000;
pub const SL_BOOLEAN_TRUE: SLboolean = 0x0000_0001;
pub const SL_DATAFORMAT_PCM: SLuint32 = 0x0000_0002;
pub const SL_PCMSAMPLEFORMAT_FIXED_16: SLuint32 = 0x0010;
pub const SL_SPEAKER_FRONT_CENTER: SLuint32 = 0x0000_0004;
pub const SL_SPEAKER_FRONT_LEFT: SLuint32 = 0x0000_0001;
pub const SL_SPEAKER_FRONT_RIGHT: SLuint32 = 0x0000_0002;
pub const SL_BYTEORDER_LITTLEENDIAN: SLuint32 = 0x0000_0002;
pub const SL_DATALOCATOR_OUTPUTMIX: SLuint32 = 0x0000_0004;
pub const SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE: SLuint32 = 0x800007BD;
pub const SL_PLAYSTATE_PLAYING: SLuint32 = 0x0000_0003;
// Pause/stop and volume.
pub const SL_PLAYSTATE_PAUSED: SLuint32 = 0x0000_0002;
pub const SL_PLAYSTATE_STOPPED: SLuint32 = 0x0000_0001;
pub const SL_MILLIBEL_MIN: SLmillibel = -0x7FFF - 1;
pub const SL_RESULT_SUCCESS: SLresult = 0;
