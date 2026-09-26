//! 011's JNI bridge letting `MediaCodec` decode video straight into a GL
//! texture: this device is API 23, so there's no `AImageReader` (API 24+),
//! and the only route is a Java `android.graphics.SurfaceTexture` on a
//! `GL_TEXTURE_EXTERNAL_OES` texture, wrapped in an `android.view.Surface`
//! for the decoder, with `updateTexImage()` on the GL thread.
//!
//! 027 makes one per clip rather than one per process, so it also has to go
//! away cleanly: every JNI object is made inside a local frame (the render
//! thread is attached for good, so a stray local ref per clip would pile up
//! towards the 512 limit), the `Surface` is kept as a global ref so it can
//! be released, and `release` frees both Java objects. `timestamp` reads
//! back which frame is latched: the decoder stamps each frame it renders
//! (`releaseOutputBufferAtTime`), so the render thread can tell "the frame
//! I want is on the texture" from "an older one, or none yet".
use jni::JNIEnv;
use jni::objects::{GlobalRef, JFloatArray, JValue};
use jni::sys::jint;
use ndk::native_window::NativeWindow;

pub struct VideoTexture {
    surface_texture: GlobalRef,
    surface: GlobalRef,
    /// The `float[16]` for `getTransformMatrix`, allocated once and reused
    /// (011: a fresh array per frame would leak a local ref per frame).
    matrix_array: GlobalRef,
}

impl VideoTexture {
    /// `tex_id` must be a live `GL_TEXTURE_EXTERNAL_OES` texture made with the
    /// EGL context current on this thread (the constructor binds to it).
    /// Returns the decoder-facing window with it.
    pub fn new(env: &mut JNIEnv, tex_id: u32) -> Result<(Self, NativeWindow), String> {
        env.with_local_frame(
            8,
            |env| -> Result<Result<(Self, NativeWindow), String>, jni::errors::Error> {
                let st = env.new_object(
                    "android/graphics/SurfaceTexture",
                    "(I)V",
                    &[JValue::Int(tex_id as jint)],
                )?;
                let surface = env.new_object(
                    "android/view/Surface",
                    "(Landroid/graphics/SurfaceTexture;)V",
                    &[JValue::Object(&st)],
                )?;
                let array = env.new_float_array(16)?;
                let raw_env = env.get_native_interface();
                let Some(window) =
                    (unsafe { NativeWindow::from_surface(raw_env, surface.as_raw()) })
                else {
                    return Ok(Err("ANativeWindow_fromSurface returned null".into()));
                };
                Ok(Ok((
                    Self {
                        surface_texture: env.new_global_ref(st)?,
                        surface: env.new_global_ref(surface)?,
                        matrix_array: env.new_global_ref(array)?,
                    },
                    window,
                )))
            },
        )
        .map_err(|e| describe(env, "SurfaceTexture setup", e))?
    }

    /// Latches the most recently queued decoded frame onto the texture. On
    /// the GL thread; harmless if nothing new has arrived.
    pub fn update_tex_image(&self, env: &mut JNIEnv) -> Result<(), String> {
        env.call_method(&self.surface_texture, "updateTexImage", "()V", &[])
            .map(|_| ())
            .map_err(|e| describe(env, "updateTexImage", e))
    }

    /// The latched frame's timestamp in ns (0 before the first).
    pub fn timestamp(&self, env: &mut JNIEnv) -> Result<i64, String> {
        env.call_method(&self.surface_texture, "getTimestamp", "()J", &[])
            .and_then(|v| v.j())
            .map_err(|e| describe(env, "getTimestamp", e))
    }

    /// The 4x4 (column-major) transform to apply to sampling UVs: the
    /// producer's crop, flip and rotation, per `getTransformMatrix`'s
    /// contract, with (0,0) the bottom-left of the picture.
    pub fn transform_matrix(&self, env: &mut JNIEnv) -> Result<[f32; 16], String> {
        env.call_method(
            &self.surface_texture,
            "getTransformMatrix",
            "([F)V",
            &[JValue::Object(self.matrix_array.as_obj())],
        )
        .map_err(|e| describe(env, "getTransformMatrix", e))?;
        let array_ref: &JFloatArray = self.matrix_array.as_obj().into();
        let mut out = [0f32; 16];
        env.get_float_array_region(array_ref, 0, &mut out)
            .map_err(|e| describe(env, "get_float_array_region", e))?;
        Ok(out)
    }

    /// Releases the `Surface` and the `SurfaceTexture` (the GL texture is the
    /// caller's). The decoder must be gone first.
    pub fn release(self, env: &mut JNIEnv) {
        for (obj, what) in [
            (&self.surface, "Surface.release"),
            (&self.surface_texture, "SurfaceTexture.release"),
        ] {
            if let Err(e) = env.call_method(obj, "release", "()V", &[]) {
                log::warn!("{}", describe(env, what, e));
            }
        }
    }
}

fn describe(env: &mut JNIEnv, what: &str, e: jni::errors::Error) -> String {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    format!("{what}: {e}")
}
