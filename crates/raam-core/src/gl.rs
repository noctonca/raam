//! Raw GLES2 FFI: the painter's and pipeline's whole GL surface, plus
//! `RenderTarget`. This is the cfg-selected GL layer, one set of entry
//! points over three linkages: extern GLES2 on Android, and on Linux with
//! the `gles` feature (a GPU with no core profile, as a Raspberry Pi's
//! VideoCore); the same extern names on the desktop host (macOS/Linux GL
//! exports them, and a cfg
//! block below rewrites the GLSL ES 1.00 shaders to 1.50 for the core
//! contexts glutin makes there); and on wasm32 the WebGL1 shim in
//! gl/webgl.rs, which implements them over the canvas's context. The
//! helpers below the entry points are one copy for all three. EGL lives
//! with the Android host.
//!
//! # Safety
//!
//! Every GL entry point, here and in webgl.rs, needs the host's context
//! current on the calling thread; that is the one invariant behind the
//! crate's plain GL calls, so their `SAFETY:` notes only say where it
//! comes from (the caller's contract, or the type's). The types
//! that own GL objects (`Painter`, `ClockOverlay`, `FontAtlas`,
//! `TransitionProgram`, `Pipeline`, `RenderTarget`) are made only by an
//! `unsafe` constructor run under that context, and the host keeps it
//! current on its render thread for their whole life, so their safe
//! methods rely on it. A block that does more than pass plain values to
//! GL (a pointer with a length the driver will read or write, a C string
//! read back) says why its pointer holds in a `SAFETY:` note.
use std::ffi::{CStr, c_char, c_void};

pub type GlUint = u32;
pub type GlInt = i32;
pub type GlEnum = u32;
pub type GlSizei = i32;
pub type GlBitfield = u32;

/// A GL enum where GL takes it as a GLint (glTexParameteri's param,
/// glTexImage2D's internalformat). Every GL enum is below 2^31, so the
/// value is unchanged; in a `const` the check runs at compile time.
///
/// # Panics
/// On an enum past `GlInt::MAX`, which no GL enum is.
#[must_use]
pub const fn gl_enum_param(e: GlEnum) -> GlInt {
    let param = e.cast_signed();
    assert!(param >= 0, "GL enum past GLint::MAX");
    param
}

/// A size, count, stride or offset where GL takes a GLsizei (or a GLint
/// that can't be negative): texture sides and offsets, vertex counts. They
/// are bounded far below 2^31 (a texture side by GL_MAX_TEXTURE_SIZE, a
/// vertex count by the buffer it's drawn from), so the value is unchanged.
///
/// # Panics
/// On a value past `GlSizei::MAX`: a size that big is a bug upstream.
#[must_use]
#[track_caller]
pub fn gl_sizei<N>(n: N) -> GlSizei
where
    N: TryInto<GlSizei> + Copy + std::fmt::Display,
{
    n.try_into()
        .unwrap_or_else(|_| panic!("GL size {n} past GLsizei::MAX"))
}

/// A slice's length in bytes as GL's GLsizeiptr (glBufferData's size).
/// Rust caps an allocation at `isize::MAX` bytes, so this can't fail.
///
/// # Panics
/// Never: no slice is longer than `isize::MAX` bytes.
#[must_use]
pub fn gl_byte_len<T>(data: &[T]) -> isize {
    isize::try_from(size_of_val(data)).expect("a slice spans at most isize::MAX bytes")
}

/// `gl_sizei` the other way: a size, count or length GL hands back or
/// takes as a signed GLsizei/GLint (a target's side, an info-log length,
/// an index count), as the unsigned type Rust indexes and sizes with. A
/// GL size is never negative, so the value is unchanged; where GL's
/// contract allows a negative (an error the call ignores), the caller
/// clamps first.
///
/// # Panics
/// On a negative value: a negative size is a bug upstream.
#[must_use]
#[track_caller]
pub fn from_gl_size<N, U>(n: N) -> U
where
    N: TryInto<U> + Copy + std::fmt::Display,
{
    n.try_into()
        .unwrap_or_else(|_| panic!("GL size {n} negative"))
}

pub const GL_COLOR_BUFFER_BIT: GlBitfield = 0x4000;
pub const GL_VERTEX_SHADER: GlEnum = 0x8B31;
pub const GL_FRAGMENT_SHADER: GlEnum = 0x8B30;
pub const GL_COMPILE_STATUS: GlEnum = 0x8B81;
pub const GL_LINK_STATUS: GlEnum = 0x8B82;
pub const GL_TRIANGLES: GlEnum = 0x0004;
pub const GL_ARRAY_BUFFER: GlEnum = 0x8892;
pub const GL_ELEMENT_ARRAY_BUFFER: GlEnum = 0x8893;
pub const GL_STATIC_DRAW: GlEnum = 0x88E4;
pub const GL_FLOAT: GlEnum = 0x1406;
pub const GL_UNSIGNED_SHORT: GlEnum = 0x1403;
pub const GL_TEXTURE_2D: GlEnum = 0x0DE1;
pub const GL_RGBA: GlEnum = 0x1908;
pub const GL_UNSIGNED_BYTE: GlEnum = 0x1401;
pub const GL_TEXTURE_MIN_FILTER: GlEnum = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: GlEnum = 0x2800;
pub const GL_LINEAR: GlEnum = 0x2601;
pub const GL_TEXTURE_WRAP_S: GlEnum = 0x2802;
pub const GL_TEXTURE_WRAP_T: GlEnum = 0x2803;
pub const GL_CLAMP_TO_EDGE: GlEnum = 0x812F;
pub const GL_VENDOR: GlEnum = 0x1F00;
pub const GL_RENDERER: GlEnum = 0x1F01;
pub const GL_VERSION: GlEnum = 0x1F02;
pub const GL_TEXTURE0: GlEnum = 0x84C0;
pub const GL_TEXTURE1: GlEnum = 0x84C1;

pub const GL_FRAMEBUFFER: GlEnum = 0x8D40;
pub const GL_COLOR_ATTACHMENT0: GlEnum = 0x8CE0;
pub const GL_FRAMEBUFFER_COMPLETE: GlEnum = 0x8CD5;
pub const GL_NO_ERROR: GlEnum = 0;

pub const GL_DYNAMIC_DRAW: GlEnum = 0x88E8;
pub const GL_BLEND: GlEnum = 0x0BE2;
pub const GL_SCISSOR_TEST: GlEnum = 0x0C11;
pub const GL_ONE: GlEnum = 1;
pub const GL_ONE_MINUS_SRC_ALPHA: GlEnum = 0x0303;
// The clock overlay's single-channel shadow atlas.
pub const GL_SRC_ALPHA: GlEnum = 0x0302;
pub const GL_ALPHA: GlEnum = 0x1906;
// Decoded video frames (the Android host's SurfaceTexture bridge).
pub const GL_TEXTURE_EXTERNAL_OES: GlEnum = 0x8D65;
pub const GL_INFO_LOG_LENGTH: GlEnum = 0x8B84;

// The WebGL1 linkage: the same names and signatures, forwarded to the
// canvas's context (the host makes it current with `webgl::make_current`).
#[cfg(any(target_arch = "wasm32", test))]
mod raw;
#[cfg(any(target_arch = "wasm32", test))]
mod table;
#[cfg(target_arch = "wasm32")]
mod webgl;
#[cfg(target_arch = "wasm32")]
pub use webgl::*;

#[cfg(not(target_arch = "wasm32"))]
#[allow(non_snake_case, dead_code)]
unsafe extern "C" {
    pub fn glGetString(name: GlEnum) -> *const u8;
    pub fn glGetError() -> GlEnum;
    pub fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    pub fn glClear(mask: GlBitfield);
    pub fn glFinish();
    pub fn glFlush();
    pub fn glViewport(x: GlInt, y: GlInt, w: GlSizei, h: GlSizei);
    pub fn glCreateShader(shader_type: GlEnum) -> GlUint;
    pub fn glShaderSource(
        shader: GlUint,
        count: GlSizei,
        string: *const *const c_char,
        length: *const GlInt,
    );
    pub fn glCompileShader(shader: GlUint);
    pub fn glGetShaderiv(shader: GlUint, pname: GlEnum, params: *mut GlInt);
    pub fn glGetShaderInfoLog(
        shader: GlUint,
        max_len: GlSizei,
        len: *mut GlSizei,
        log: *mut c_char,
    );
    pub fn glCreateProgram() -> GlUint;
    pub fn glAttachShader(program: GlUint, shader: GlUint);
    pub fn glLinkProgram(program: GlUint);
    pub fn glGetProgramiv(program: GlUint, pname: GlEnum, params: *mut GlInt);
    pub fn glGetProgramInfoLog(
        program: GlUint,
        max_len: GlSizei,
        len: *mut GlSizei,
        log: *mut c_char,
    );
    pub fn glUseProgram(program: GlUint);
    pub fn glGenBuffers(n: GlSizei, buffers: *mut GlUint);
    pub fn glBindBuffer(target: GlEnum, buffer: GlUint);
    pub fn glBufferData(target: GlEnum, size: isize, data: *const c_void, usage: GlEnum);
    pub fn glVertexAttribPointer(
        index: GlUint,
        size: GlInt,
        type_: GlEnum,
        normalized: u8,
        stride: GlSizei,
        pointer: *const c_void,
    );
    pub fn glEnableVertexAttribArray(index: GlUint);
    pub fn glDisableVertexAttribArray(index: GlUint);
    pub fn glEnable(cap: GlEnum);
    pub fn glDisable(cap: GlEnum);
    pub fn glBlendFunc(sfactor: GlEnum, dfactor: GlEnum);
    pub fn glBlendFuncSeparate(src_rgb: GlEnum, dst_rgb: GlEnum, src_a: GlEnum, dst_a: GlEnum);
    pub fn glScissor(x: GlInt, y: GlInt, w: GlSizei, h: GlSizei);
    // The desktop linkage wraps these two (GL_ALPHA).
    #[cfg(any(target_os = "android", feature = "gles"))]
    pub fn glTexSubImage2D(
        target: GlEnum,
        level: GlInt,
        xoffset: GlInt,
        yoffset: GlInt,
        width: GlSizei,
        height: GlSizei,
        format: GlEnum,
        type_: GlEnum,
        pixels: *const c_void,
    );
    pub fn glGetAttribLocation(program: GlUint, name: *const c_char) -> GlInt;
    pub fn glGetUniformLocation(program: GlUint, name: *const c_char) -> GlInt;
    pub fn glUniform1i(location: GlInt, v0: GlInt);
    pub fn glUniform2i(location: GlInt, v0: GlInt, v1: GlInt);
    pub fn glUniform1f(location: GlInt, v0: f32);
    pub fn glUniform2f(location: GlInt, v0: f32, v1: f32);
    pub fn glUniform4f(location: GlInt, v0: f32, v1: f32, v2: f32, v3: f32);
    pub fn glUniformMatrix4fv(location: GlInt, count: GlSizei, transpose: u8, value: *const f32);
    pub fn glDrawArrays(mode: GlEnum, first: GlInt, count: GlSizei);
    pub fn glDrawElements(mode: GlEnum, count: GlSizei, type_: GlEnum, indices: *const c_void);
    pub fn glGenTextures(n: GlSizei, textures: *mut GlUint);
    pub fn glBindTexture(target: GlEnum, texture: GlUint);
    pub fn glActiveTexture(texture: GlEnum);
    // The desktop linkage wraps these two (GL_ALPHA).
    #[cfg(any(target_os = "android", feature = "gles"))]
    pub fn glTexImage2D(
        target: GlEnum,
        level: GlInt,
        internalformat: GlInt,
        width: GlSizei,
        height: GlSizei,
        border: GlInt,
        format: GlEnum,
        type_: GlEnum,
        pixels: *const c_void,
    );
    pub fn glTexParameteri(target: GlEnum, pname: GlEnum, param: GlInt);
    pub fn glGenFramebuffers(n: GlSizei, framebuffers: *mut GlUint);
    pub fn glBindFramebuffer(target: GlEnum, framebuffer: GlUint);
    pub fn glFramebufferTexture2D(
        target: GlEnum,
        attachment: GlEnum,
        textarget: GlEnum,
        texture: GlUint,
        level: GlInt,
    );
    pub fn glCheckFramebufferStatus(target: GlEnum) -> GlEnum;
    pub fn glDeleteTextures(n: GlSizei, textures: *const GlUint);
    pub fn glDeleteFramebuffers(n: GlSizei, framebuffers: *const GlUint);
    pub fn glGetIntegerv(pname: GlEnum, params: *mut GlInt);
}

pub const GL_MAX_TEXTURE_SIZE: GlEnum = 0x0D33;

// The desktop linkage: OpenGL.framework / libGL export the same names,
// but glutin only makes core-profile contexts there (4.1 on macOS), so
// GLSL ES 1.00 is rewritten to 1.50 on the way into `link_program`, a
// core context needs one vertex array object bound before any draw, and
// GLES2's GL_ALPHA textures become swizzled one-channel ones.
#[cfg(all(
    not(target_os = "android"),
    not(feature = "gles"),
    not(target_arch = "wasm32")
))]
mod desktop {
    use super::*;

    const GL_RED: GlEnum = 0x1903;
    const GL_R8: GlEnum = 0x8229;
    const GL_TEXTURE_SWIZZLE_RGBA: GlEnum = 0x8E46;
    const GL_ZERO: GlInt = 0;

    unsafe extern "C" {
        fn glGenVertexArrays(n: GlSizei, arrays: *mut GlUint);
        fn glBindVertexArray(array: GlUint);
        fn glTexParameteriv(target: GlEnum, pname: GlEnum, params: *const GlInt);
        #[link_name = "glTexImage2D"]
        fn tex_image_2d(
            target: GlEnum,
            level: GlInt,
            internalformat: GlInt,
            width: GlSizei,
            height: GlSizei,
            border: GlInt,
            format: GlEnum,
            type_: GlEnum,
            pixels: *const c_void,
        );
        #[link_name = "glTexSubImage2D"]
        fn tex_sub_image_2d(
            target: GlEnum,
            level: GlInt,
            xoffset: GlInt,
            yoffset: GlInt,
            width: GlSizei,
            height: GlSizei,
            format: GlEnum,
            type_: GlEnum,
            pixels: *const c_void,
        );
        pub fn glReadPixels(
            x: GlInt,
            y: GlInt,
            w: GlSizei,
            h: GlSizei,
            format: GlEnum,
            type_: GlEnum,
            data: *mut c_void,
        );
    }

    /// Once after the context is made current: core profiles refuse to
    /// draw with the default VAO.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn bind_vao() {
        // SAFETY: the caller's contract: a current GL context; `vao` is a
        // local GLuint for glGenVertexArrays's one name.
        unsafe {
            let mut vao = 0;
            glGenVertexArrays(1, &mut vao);
            glBindVertexArray(vao);
        }
    }

    /// GLES2's glTexImage2D. A core profile has no GL_ALPHA (the clock
    /// overlay's glyph atlases): one byte a texel is GL_R8 there, swizzled
    /// so a shader samples (0, 0, 0, a) as it does from GL_ALPHA.
    ///
    /// # Safety
    /// Requires a current GL context, and `pixels` as glTexImage2D does.
    #[allow(non_snake_case, clippy::too_many_arguments)]
    pub unsafe fn glTexImage2D(
        target: GlEnum,
        level: GlInt,
        internalformat: GlInt,
        width: GlSizei,
        height: GlSizei,
        border: GlInt,
        format: GlEnum,
        type_: GlEnum,
        pixels: *const c_void,
    ) {
        // SAFETY: the caller's contract: a current GL context, and `pixels`
        // valid for what glTexImage2D reads. GL_R8 + GL_RED is one byte a
        // texel, as GL_ALPHA is, so the swap reads no more of it.
        unsafe {
            if format != GL_ALPHA {
                tex_image_2d(
                    target,
                    level,
                    internalformat,
                    width,
                    height,
                    border,
                    format,
                    type_,
                    pixels,
                );
                return;
            }
            tex_image_2d(
                target,
                level,
                gl_enum_param(GL_R8),
                width,
                height,
                border,
                GL_RED,
                type_,
                pixels,
            );
            let swizzle = [GL_ZERO, GL_ZERO, GL_ZERO, gl_enum_param(GL_RED)];
            // SAFETY: SWIZZLE_RGBA reads four GLints, and `swizzle` is four.
            glTexParameteriv(target, GL_TEXTURE_SWIZZLE_RGBA, swizzle.as_ptr());
        }
    }

    /// GLES2's glTexSubImage2D, GL_ALPHA uploads going to the red channel
    /// (see `glTexImage2D`).
    ///
    /// # Safety
    /// Requires a current GL context, and `pixels` as glTexSubImage2D does.
    #[allow(non_snake_case, clippy::too_many_arguments)]
    pub unsafe fn glTexSubImage2D(
        target: GlEnum,
        level: GlInt,
        xoffset: GlInt,
        yoffset: GlInt,
        width: GlSizei,
        height: GlSizei,
        format: GlEnum,
        type_: GlEnum,
        pixels: *const c_void,
    ) {
        let format = if format == GL_ALPHA { GL_RED } else { format };
        // SAFETY: the caller's contract: a current GL context, and `pixels`
        // valid for what glTexSubImage2D reads; GL_RED reads one byte a
        // texel, as the GL_ALPHA it replaces does.
        unsafe {
            tex_sub_image_2d(
                target, level, xoffset, yoffset, width, height, format, type_, pixels,
            )
        }
    }

    /// GLSL ES 1.00 -> 1.50. `precision` statements are legal (and
    /// ignored) in 1.50, so only the removed keywords and built-ins need
    /// replacing.
    pub fn to_150(src: &str, fragment: bool) -> String {
        if fragment {
            let body = src
                .replace("gl_FragColor", "fragColor")
                .replace("texture2D(", "texture(")
                .replace("varying ", "in ");
            format!("#version 150\nout vec4 fragColor;\n{body}")
        } else {
            let body = src.replace("attribute ", "in ").replace("varying ", "out ");
            format!("#version 150\n{body}")
        }
    }
}

#[cfg(all(
    not(target_os = "android"),
    not(feature = "gles"),
    not(target_arch = "wasm32")
))]
pub use desktop::{bind_vao, glReadPixels, glTexImage2D, glTexSubImage2D};

// The GLES2 linkage on a desktop host: the Android one, plus the two
// entry points the host takes from the desktop linkage.
#[cfg(all(feature = "gles", not(target_os = "android")))]
mod gles {
    use super::*;

    unsafe extern "C" {
        pub fn glReadPixels(
            x: GlInt,
            y: GlInt,
            w: GlSizei,
            h: GlSizei,
            format: GlEnum,
            type_: GlEnum,
            data: *mut c_void,
        );
    }

    /// Nothing to bind: GLES2 draws with the default vertex array.
    ///
    /// # Safety
    /// None; unsafe to match the desktop linkage's.
    pub unsafe fn bind_vao() {}
}

#[cfg(all(feature = "gles", not(target_os = "android")))]
pub use gles::{bind_vao, glReadPixels};

/// Linear filtering and clamp-to-edge wrap on the bound 2D texture: what
/// every texture raam makes uses (photos, render targets, egui's, the
/// glyph atlas).
///
/// # Safety
/// Requires a current GL context with a texture bound to `GL_TEXTURE_2D`.
pub unsafe fn set_linear_clamp() {
    const LINEAR: GlInt = gl_enum_param(GL_LINEAR);
    const CLAMP: GlInt = gl_enum_param(GL_CLAMP_TO_EDGE);
    // SAFETY: the caller's contract: a current GL context with a texture
    // bound.
    unsafe {
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, CLAMP);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, CLAMP);
    }
}

/// One of the driver's identification strings (`GL_VENDOR`,
/// `GL_RENDERER`, `GL_VERSION`), or "?" when it has none.
///
/// # Safety
/// Requires a current GL context.
pub unsafe fn gl_string(name: GlEnum) -> String {
    // SAFETY: the caller's contract: a current GL context; the string read
    // back has its own note below.
    unsafe {
        let ptr = glGetString(name);
        if ptr.is_null() {
            return "?".to_string();
        }
        // SAFETY: a non-null glGetString answer is a NUL-terminated string
        // the driver keeps for the context's life (webgl.rs keeps its own
        // the same way), and it is copied out before returning.
        CStr::from_ptr(ptr as *const c_char)
            .to_string_lossy()
            .into_owned()
    }
}

/// Compiles and links a vertex+fragment shader pair.
///
/// # Panics
/// If a stage fails to compile or the program to link, with the driver's
/// own info log: a bad shader is a build defect, not something to
/// survive (TigerStyle: crash, don't limp). `label` is included in the
/// panic message so a bad ported gl-transitions shader is identifiable
/// immediately from logcat among the many programs the pipeline links.
/// Also if a source holds a NUL byte.
///
/// # Safety
/// Requires a current GL context.
pub unsafe fn link_program(label: &str, vs_src: &str, fs_src: &str) -> GlUint {
    #[cfg(all(
        not(target_os = "android"),
        not(feature = "gles"),
        not(target_arch = "wasm32")
    ))]
    let (vs_src, fs_src) = (
        &desktop::to_150(vs_src, false),
        &desktop::to_150(fs_src, true),
    );
    // SAFETY: the caller's contract: a current GL context; the info log
    // read has its own note below.
    unsafe {
        let vs = compile(label, GL_VERTEX_SHADER, vs_src);
        let fs = compile(label, GL_FRAGMENT_SHADER, fs_src);
        let program = glCreateProgram();
        glAttachShader(program, vs);
        glAttachShader(program, fs);
        glLinkProgram(program);
        let mut status = 0;
        glGetProgramiv(program, GL_LINK_STATUS, &mut status);
        if status == 0 {
            let mut len = 0i32;
            glGetProgramiv(program, GL_INFO_LOG_LENGTH, &mut len);
            let mut buf = vec![0u8; from_gl_size(len.max(1))];
            let mut written = 0i32;
            // SAFETY: `buf` holds at least `len` bytes, the most the driver
            // writes (log and NUL), and `written` is a valid i32 to fill.
            glGetProgramInfoLog(program, len, &mut written, buf.as_mut_ptr() as *mut c_char);
            panic!(
                "[{label}] program link failed: {}",
                String::from_utf8_lossy(&buf[..from_gl_size::<_, usize>(written.max(0))])
            );
        }
        program
    }
}

/// Compiles one shader stage, panicking with the driver's log on failure
/// (see `link_program`).
///
/// # Safety
/// Requires a current GL context.
unsafe fn compile(label: &str, kind: GlEnum, src: &str) -> GlUint {
    // SAFETY: the caller's contract: a current GL context; the source and
    // info log pointers have their own notes below.
    unsafe {
        let shader = glCreateShader(kind);
        let c_src = std::ffi::CString::new(src).unwrap();
        let ptr = c_src.as_ptr();
        // SAFETY: one NUL-terminated string (the null lengths say so) that
        // outlives the call; GL copies the source.
        glShaderSource(shader, 1, &ptr, std::ptr::null());
        glCompileShader(shader);
        let mut status = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut status);
        if status == 0 {
            let mut len = 0i32;
            glGetShaderiv(shader, GL_INFO_LOG_LENGTH, &mut len);
            let mut buf = vec![0u8; from_gl_size(len.max(1))];
            let mut written = 0i32;
            // SAFETY: `buf` holds at least `len` bytes, the most the driver
            // writes (log and NUL), and `written` is a valid i32 to fill.
            glGetShaderInfoLog(shader, len, &mut written, buf.as_mut_ptr() as *mut c_char);
            panic!(
                "[{label}] shader compile failed (type 0x{kind:x}): {}\n--- source ---\n{src}",
                String::from_utf8_lossy(&buf[..from_gl_size::<_, usize>(written.max(0))])
            );
        }
        shader
    }
}

/// An active attribute's location in `program`.
///
/// # Panics
/// If the linker dropped or never saw `name` (a build defect), or `name`
/// holds a NUL byte.
///
/// # Safety
/// Requires a current GL context and a linked `program`.
pub unsafe fn attrib_loc(program: GlUint, name: &str) -> GlUint {
    // SAFETY: the caller's contract: a current GL context and a linked
    // `program`; `c` is NUL-terminated and outlives the call.
    unsafe {
        let c = std::ffi::CString::new(name).unwrap();
        let loc = glGetAttribLocation(program, c.as_ptr());
        GlUint::try_from(loc).unwrap_or_else(|_| panic!("attribute {name} not found/active"))
    }
}

/// A uniform's location in `program`, or -1 (which GL ignores) when the
/// linker dropped it. Call it once per program, at startup: on wasm each
/// call takes a slot in webgl.rs's uniform table.
///
/// # Panics
/// If `name` holds a NUL byte.
///
/// # Safety
/// Requires a current GL context and a linked `program`.
pub unsafe fn uniform_loc(program: GlUint, name: &str) -> GlInt {
    // SAFETY: the caller's contract: a current GL context and a linked
    // `program`; `c` is NUL-terminated and outlives the call.
    unsafe {
        let c = std::ffi::CString::new(name).unwrap();
        glGetUniformLocation(program, c.as_ptr())
    }
}

/// A small offscreen render target: a texture plus the FBO that renders into
/// it. NPOT-safe (CLAMP_TO_EDGE + non-mipmapped LINEAR filtering, matching
/// GLES2's NPOT texture rules). Used both at the tiny blur working
/// resolution (`BLUR_WIDTH_PX` wide) and at up to full screen resolution,
/// for each tile's `source` and composed `target` and a transition's
/// scratch pair.
pub struct RenderTarget {
    pub texture: GlUint,
    pub fbo: GlUint,
    pub width: i32,
    pub height: i32,
}

impl RenderTarget {
    /// For targets made once at startup.
    ///
    /// # Panics
    /// If the target can't be made (`alloc`'s error): a failure there is
    /// fatal. Also on a side that isn't positive.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn new(width: i32, height: i32) -> Self {
        // SAFETY: the caller's contract: a current GL context.
        unsafe { Self::alloc(width, height) }.unwrap_or_else(|e| panic!("{e}"))
    }

    /// A texture-backed FBO, or why not. On this frame the usual reason is
    /// Mali out of memory (seen at boot, with MemFree about 7 MB): the
    /// texture gets no storage and the FBO reports INCOMPLETE_ATTACHMENT.
    /// Whatever was created is deleted again before returning the error.
    ///
    /// # Panics
    /// On a side that isn't positive: a bug upstream, not a GPU failure.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn try_new(width: i32, height: i32) -> Result<Self, String> {
        // Test-only: `debug.video.fail=rt` fails every render target made
        // after startup. The flag, not the Android property: the host
        // snapshots it once per loop pass (switches.rs).
        if crate::switches::fail() == crate::switches::Fail::Rt {
            return Err(format!(
                "{width}x{height} render target: test failure (debug.video.fail=rt)"
            ));
        }
        // SAFETY: the caller's contract: a current GL context.
        unsafe { Self::alloc(width, height) }
    }

    /// # Panics
    /// On a side that isn't positive. The callers' sizes never are (the
    /// screen's, a tile's, at least 1 px when scaled), so one would be a
    /// bug upstream; returned as an error, the pipeline would take it for
    /// the GPU running out of memory and retry it every `GPU_RETRY`.
    unsafe fn alloc(width: i32, height: i32) -> Result<Self, String> {
        assert!(width > 0, "render target {width}x{height}: width not positive");
        assert!(height > 0, "render target {width}x{height}: height not positive");
        // GLES2 has five error codes and WebGL a sixth (context lost), and
        // GL keeps at most one flag for each, so this many reads clear them.
        const GL_ERROR_CODES: usize = 6;
        // SAFETY: `new` and `try_new`, its only callers, pass on their
        // contract: a current GL context. The null pixels are GL's "storage,
        // no upload", so the driver reads nothing through them.
        unsafe {
            // Nothing else reads GL's error flags, so one raised by any
            // earlier call surfaces here first: say so, rather than clear
            // it unseen, and keep it from being blamed on this target.
            for _ in 0..GL_ERROR_CODES {
                let stale = glGetError();
                if stale == GL_NO_ERROR {
                    break;
                }
                log::warn!(
                    "GL error 0x{stale:x} raised by an earlier call, found before making a {width}x{height} render target"
                );
            }
            let mut texture = 0;
            glGenTextures(1, &mut texture);
            glBindTexture(GL_TEXTURE_2D, texture);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                gl_enum_param(GL_RGBA),
                width,
                height,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                std::ptr::null(),
            );
            let tex_error = glGetError();
            set_linear_clamp();

            let mut fbo = 0;
            glGenFramebuffers(1, &mut fbo);
            glBindFramebuffer(GL_FRAMEBUFFER, fbo);
            glFramebufferTexture2D(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                texture,
                0,
            );
            let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
            glBindFramebuffer(GL_FRAMEBUFFER, 0);

            let target = Self {
                texture,
                fbo,
                width,
                height,
            };
            if tex_error != GL_NO_ERROR || status != GL_FRAMEBUFFER_COMPLETE {
                target.destroy();
                return Err(format!(
                    "{width}x{height} render target: framebuffer status 0x{status:x}, glTexImage2D error 0x{tex_error:x}"
                ));
            }
            Ok(target)
        }
    }

    /// Makes this target the draw framebuffer, with the viewport over all
    /// of it.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn bind_and_viewport(&self) {
        // SAFETY: the caller's contract: a current GL context.
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, self.fbo);
            glViewport(0, 0, self.width, self.height);
        }
    }

    /// Frees the GPU-side texture+FBO. The pipeline frees targets as soon
    /// as it is done with them (a transition's scratch pair right after it
    /// finishes, a collage's tiles when it leaves the screen) rather than
    /// holding them at rest - the frame's thin `MemFree` headroom (under
    /// 10 MB at its lowest, measured) makes every ~4MB screen-sized
    /// texture worth not paying for.
    ///
    /// # Safety
    /// Requires a current GL context; the texture and FBO must not be bound or drawn after this.
    pub unsafe fn destroy(self) {
        // SAFETY: the caller's contract: a current GL context, and nothing
        // uses the names after; each delete reads one GLuint from `self`.
        unsafe {
            glDeleteFramebuffers(1, &self.fbo);
            glDeleteTextures(1, &self.texture);
        }
    }
}
