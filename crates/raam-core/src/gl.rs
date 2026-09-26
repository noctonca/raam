//! Raw GLES2 FFI: the GLES2 half of the experiments' gl.rs (the painter's
//! and pipeline's whole GL surface), plus `RenderTarget`. This is the
//! cfg-selected GL layer's Android/extern linkage; the desktop rewrite and
//! the WebGL1 shim replace this file by name when those hosts adopt the
//! core (migration steps 7-8). The EGL half lives with the Android host.
use std::ffi::{CStr, c_char, c_void};

pub type GlUint = u32;
pub type GlInt = i32;
pub type GlEnum = u32;
pub type GlSizei = i32;
pub type GlBitfield = u32;

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
// 022: the clock overlay's single-channel shadow atlas (012).
pub const GL_SRC_ALPHA: GlEnum = 0x0302;
pub const GL_ALPHA: GlEnum = 0x1906;
// 027: decoded video frames (011's SurfaceTexture bridge).
pub const GL_TEXTURE_EXTERNAL_OES: GlEnum = 0x8D65;

#[allow(non_snake_case, dead_code)]
unsafe extern "C" {
    pub fn glGetString(name: GlEnum) -> *const u8;
    pub fn glGetError() -> GlEnum;
    pub fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    pub fn glClear(mask: GlBitfield);
    pub fn glFinish();
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
}

///
/// # Safety
/// Requires a current GL context.
pub unsafe fn gl_string(name: GlEnum) -> String {
    unsafe {
        let ptr = glGetString(name);
        if ptr.is_null() {
            return "?".to_string();
        }
        CStr::from_ptr(ptr as *const c_char)
            .to_string_lossy()
            .into_owned()
    }
}

/// Compiles and links a vertex+fragment shader pair, panicking with the
/// driver's own info log on failure - a bad shader is a build defect, not
/// something to survive (TigerStyle: crash, don't limp). `label` is included in
/// the panic message so a bad ported gl-transitions shader is identifiable
/// immediately from logcat, since 014 links many more programs than 008 did.
///
/// # Safety
/// Requires a current GL context.
pub unsafe fn link_program(label: &str, vs_src: &str, fs_src: &str) -> GlUint {
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
            glGetProgramiv(program, 0x8B84 /* GL_INFO_LOG_LENGTH */, &mut len);
            let mut buf = vec![0u8; len.max(1) as usize];
            let mut written = 0i32;
            glGetProgramInfoLog(program, len, &mut written, buf.as_mut_ptr() as *mut c_char);
            panic!(
                "[{label}] program link failed: {}",
                String::from_utf8_lossy(&buf[..written.max(0) as usize])
            );
        }
        program
    }
}

unsafe fn compile(label: &str, kind: GlEnum, src: &str) -> GlUint {
    unsafe {
        let shader = glCreateShader(kind);
        let c_src = std::ffi::CString::new(src).unwrap();
        let ptr = c_src.as_ptr();
        glShaderSource(shader, 1, &ptr, std::ptr::null());
        glCompileShader(shader);
        let mut status = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut status);
        if status == 0 {
            let mut len = 0i32;
            glGetShaderiv(shader, 0x8B84 /* GL_INFO_LOG_LENGTH */, &mut len);
            let mut buf = vec![0u8; len.max(1) as usize];
            let mut written = 0i32;
            glGetShaderInfoLog(shader, len, &mut written, buf.as_mut_ptr() as *mut c_char);
            panic!(
                "[{label}] shader compile failed (type 0x{kind:x}): {}\n--- source ---\n{src}",
                String::from_utf8_lossy(&buf[..written.max(0) as usize])
            );
        }
        shader
    }
}

///
/// # Safety
/// Requires a current GL context and a linked `program`.
pub unsafe fn attrib_loc(program: GlUint, name: &str) -> GlUint {
    unsafe {
        let c = std::ffi::CString::new(name).unwrap();
        let loc = glGetAttribLocation(program, c.as_ptr());
        assert!(loc >= 0, "attribute {name} not found/active");
        loc as GlUint
    }
}

///
/// # Safety
/// Requires a current GL context and a linked `program`.
pub unsafe fn uniform_loc(program: GlUint, name: &str) -> GlInt {
    unsafe {
        let c = std::ffi::CString::new(name).unwrap();
        glGetUniformLocation(program, c.as_ptr())
    }
}

/// A small offscreen render target: a texture plus the FBO that renders into
/// it. NPOT-safe (CLAMP_TO_EDGE + non-mipmapped LINEAR filtering, matching
/// GLES2's NPOT texture rules). Unlike 008 (which only ever used this at a
/// tiny 128x80 blur working resolution), 014 also uses this at full screen
/// resolution to hold each slide's fully-composited (blur+photo) texture.
pub struct RenderTarget {
    pub texture: GlUint,
    pub fbo: GlUint,
    pub width: i32,
    pub height: i32,
}

impl RenderTarget {
    /// For targets made once at startup: a failure there is fatal.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn new(width: i32, height: i32) -> Self {
        unsafe { Self::alloc(width, height) }.unwrap_or_else(|e| panic!("{e}"))
    }

    /// A texture-backed FBO, or why not. On this frame the usual reason is
    /// Mali out of memory (seen at boot, with MemFree about 7 MB): the
    /// texture gets no storage and the FBO reports INCOMPLETE_ATTACHMENT.
    /// Whatever was created is deleted again before returning the error.
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
        unsafe { Self::alloc(width, height) }
    }

    unsafe fn alloc(width: i32, height: i32) -> Result<Self, String> {
        unsafe {
            for _ in 0..8 {
                if glGetError() == GL_NO_ERROR {
                    break;
                }
            }
            let mut texture = 0;
            glGenTextures(1, &mut texture);
            glBindTexture(GL_TEXTURE_2D, texture);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                width,
                height,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                std::ptr::null(),
            );
            let tex_error = glGetError();
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);

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

    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn bind_and_viewport(&self) {
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, self.fbo);
            glViewport(0, 0, self.width, self.height);
        }
    }

    /// Frees the GPU-side texture+FBO. Used by 014's transition pipeline to
    /// keep only one extra full-screen-resolution render target alive at a
    /// time (created right as a transition starts, destroyed right after it
    /// finishes) rather than permanently holding two - this device's own
    /// near-zero `MemFree` headroom (004, 008) makes an extra ~4MB screen-
    /// sized texture worth not paying for at rest.
    ///
    /// # Safety
    /// Requires a current GL context; the texture and FBO must not be bound or drawn after this.
    pub unsafe fn destroy(self) {
        unsafe {
            glDeleteFramebuffers(1, &self.fbo);
            glDeleteTextures(1, &self.texture);
        }
    }
}
