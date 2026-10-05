//! The WebGL1 linkage: the GLES2 entry points the rest of the core calls,
//! with the same names and signatures, forwarded to the canvas's WebGL1
//! context. GLES2 names objects with integers and WebGL with JS objects,
//! so each kind gets a table indexed by the integer handed out (0 stays
//! "none", as in GL). Pointer arguments are wasm linear memory: buffer and
//! texture data become slices sized from the call's own arguments, and
//! attribute/index "pointers" are buffer offsets, as they are on the
//! frame. The shaders are GLSL ES 1.00, WebGL1's own language, so they go
//! in unchanged.
//!
//! The context is the host's to make (its attributes are the web host's
//! EGL config): `make_current` hands it over once, before any GL call.
//!
//! Each entry point's safety contract is its GL function's, as on the
//! other linkages (hence no `# Safety` section on each): a pointer
//! argument must be valid for what GL reads or writes through it. The
//! blocks that dereference one say which argument sizes it.
#![allow(non_snake_case, clippy::missing_safety_doc, clippy::too_many_arguments)]

use super::raw::{bytes, mat4s, write_log};
use super::table::Table;
use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use web_sys::{
    WebGlBuffer, WebGlFramebuffer, WebGlProgram, WebGlRenderingContext as Wgl, WebGlShader,
    WebGlTexture, WebGlUniformLocation,
};

struct State {
    gl: Wgl,
    buffers: Table<WebGlBuffer>,
    textures: Table<WebGlTexture>,
    programs: Table<WebGlProgram>,
    shaders: Table<WebGlShader>,
    framebuffers: Table<WebGlFramebuffer>,
    uniforms: Table<WebGlUniformLocation>,
    /// `glGetString`'s answers, kept (a context's never change) so the
    /// returned pointers stay valid, as GL's do.
    strings: HashMap<GlEnum, CString>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| {
        f(s.borrow_mut()
            .as_mut()
            .expect("gl: no current WebGL context"))
    })
}

/// Host only, once, before any GL call: the canvas's WebGL1 context
/// becomes the one every entry point draws with.
///
/// # Panics
/// If a context is already current.
pub fn make_current(gl: Wgl) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        assert!(s.is_none(), "gl: a WebGL context is already current");
        *s = Some(State {
            gl,
            buffers: Table::new(),
            textures: Table::new(),
            programs: Table::new(),
            shaders: Table::new(),
            framebuffers: Table::new(),
            uniforms: Table::new(),
            strings: HashMap::new(),
        })
    });
}

/// Bytes per pixel of an unpacked GL_UNSIGNED_BYTE image.
fn bpp(format: GlEnum) -> usize {
    match format {
        GL_RGBA => 4,
        0x1907 => 3, // GL_RGB
        0x190A => 2, // GL_LUMINANCE_ALPHA
        _ => 1,      // GL_ALPHA, GL_LUMINANCE
    }
}

pub unsafe fn glGetString(name: GlEnum) -> *const u8 {
    with(|s| {
        // The unmasked renderer when the browser shares it.
        let query = match (name, s.gl.get_extension("WEBGL_debug_renderer_info")) {
            (GL_RENDERER, Ok(Some(_))) => 0x9246, // UNMASKED_RENDERER_WEBGL
            (GL_VENDOR, Ok(Some(_))) => 0x9245,   // UNMASKED_VENDOR_WEBGL
            _ => name,
        };
        let value =
            s.gl.get_parameter(query)
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_else(|| "?".into());
        s.strings
            .entry(name)
            .or_insert_with(|| CString::new(value.replace('\0', "")).unwrap_or_default())
            .as_ptr() as *const u8
    })
}

pub unsafe fn glGetError() -> GlEnum {
    with(|s| s.gl.get_error())
}

pub unsafe fn glClearColor(r: f32, g: f32, b: f32, a: f32) {
    with(|s| s.gl.clear_color(r, g, b, a))
}

pub unsafe fn glClear(mask: GlBitfield) {
    with(|s| s.gl.clear(mask))
}

pub unsafe fn glFinish() {
    with(|s| s.gl.finish())
}

pub unsafe fn glFlush() {
    with(|s| s.gl.flush())
}

pub unsafe fn glViewport(x: GlInt, y: GlInt, w: GlSizei, h: GlSizei) {
    with(|s| s.gl.viewport(x, y, w, h))
}

pub unsafe fn glGetIntegerv(pname: GlEnum, params: *mut GlInt) {
    let v = with(|s| {
        s.gl.get_parameter(pname)
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    });
    // SAFETY: the names this shim answers are single-valued, and `params`
    // is valid for one GlInt by GL's contract.
    unsafe { *params = crate::num::sat_i32(v) };
}

// Shaders and programs.

pub unsafe fn glCreateShader(shader_type: GlEnum) -> GlUint {
    with(|s| match s.gl.create_shader(shader_type) {
        Some(sh) => s.shaders.add(sh),
        None => 0,
    })
}

/// One NUL-terminated string (all the core's callers pass that).
///
/// # Panics
/// If `count` isn't 1.
pub unsafe fn glShaderSource(
    shader: GlUint,
    count: GlSizei,
    string: *const *const c_char,
    _length: *const GlInt,
) {
    assert_eq!(count, 1, "glShaderSource: one string only");
    // SAFETY: `string` points at one pointer (count is 1), to a
    // NUL-terminated string valid for the call, by GL's contract.
    let src = unsafe { CStr::from_ptr(*string) }.to_string_lossy();
    with(|s| {
        if let Some(sh) = s.shaders.get(shader) {
            s.gl.shader_source(&sh, &src)
        }
    })
}

pub unsafe fn glCompileShader(shader: GlUint) {
    with(|s| {
        if let Some(sh) = s.shaders.get(shader) {
            s.gl.compile_shader(&sh)
        }
    })
}

pub unsafe fn glGetShaderiv(shader: GlUint, pname: GlEnum, params: *mut GlInt) {
    let v = with(|s| {
        let Some(sh) = s.shaders.get(shader) else {
            return 0;
        };
        match pname {
            GL_INFO_LOG_LENGTH => {
                s.gl.get_shader_info_log(&sh)
                    .map_or(0, |l| gl_sizei(l.len() + 1))
            }
            _ => {
                s.gl.get_shader_parameter(&sh, pname)
                    .as_bool()
                    .map_or(0, GlInt::from)
            }
        }
    });
    // SAFETY: every shader parameter is one GlInt, which `params` is
    // valid for by GL's contract.
    unsafe { *params = v };
}

pub unsafe fn glGetShaderInfoLog(
    shader: GlUint,
    max_len: GlSizei,
    len: *mut GlSizei,
    log: *mut c_char,
) {
    let text = with(|s| {
        s.shaders
            .get(shader)
            .and_then(|sh| s.gl.get_shader_info_log(&sh))
            .unwrap_or_default()
    });
    // SAFETY: `log` holds `max_len` bytes and `len` is null or one
    // GlSizei, by GL's contract, which is write_log's.
    unsafe { write_log(&text, max_len, len, log) };
}

pub unsafe fn glCreateProgram() -> GlUint {
    with(|s| match s.gl.create_program() {
        Some(p) => s.programs.add(p),
        None => 0,
    })
}

pub unsafe fn glAttachShader(program: GlUint, shader: GlUint) {
    with(|s| {
        if let (Some(p), Some(sh)) = (s.programs.get(program), s.shaders.get(shader)) {
            s.gl.attach_shader(&p, &sh)
        }
    })
}

pub unsafe fn glLinkProgram(program: GlUint) {
    with(|s| {
        if let Some(p) = s.programs.get(program) {
            s.gl.link_program(&p)
        }
    })
}

pub unsafe fn glGetProgramiv(program: GlUint, pname: GlEnum, params: *mut GlInt) {
    let v = with(|s| {
        let Some(p) = s.programs.get(program) else {
            return 0;
        };
        match pname {
            GL_INFO_LOG_LENGTH => {
                s.gl.get_program_info_log(&p)
                    .map_or(0, |l| gl_sizei(l.len() + 1))
            }
            _ => {
                s.gl.get_program_parameter(&p, pname)
                    .as_bool()
                    .map_or(0, GlInt::from)
            }
        }
    });
    // SAFETY: every program parameter the core asks for is one GlInt,
    // which `params` is valid for by GL's contract.
    unsafe { *params = v };
}

pub unsafe fn glGetProgramInfoLog(
    program: GlUint,
    max_len: GlSizei,
    len: *mut GlSizei,
    log: *mut c_char,
) {
    let text = with(|s| {
        s.programs
            .get(program)
            .and_then(|p| s.gl.get_program_info_log(&p))
            .unwrap_or_default()
    });
    // SAFETY: `log` holds `max_len` bytes and `len` is null or one
    // GlSizei, by GL's contract, which is write_log's.
    unsafe { write_log(&text, max_len, len, log) };
}

pub unsafe fn glUseProgram(program: GlUint) {
    with(|s| s.gl.use_program(s.programs.get(program).as_ref()))
}

pub unsafe fn glGetAttribLocation(program: GlUint, name: *const c_char) -> GlInt {
    // SAFETY: `name` is NUL-terminated and valid for the call, by GL's
    // contract.
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    with(|s| {
        s.programs
            .get(program)
            .map_or(-1, |p| s.gl.get_attrib_location(&p, &name))
    })
}

/// -1 for a uniform the program doesn't have (or optimised out), as in GL;
/// the glUniform calls ignore -1.
///
/// # Panics
/// Past 2^31 live uniform handles; programs are linked a few at startup.
pub unsafe fn glGetUniformLocation(program: GlUint, name: *const c_char) -> GlInt {
    // SAFETY: `name` is NUL-terminated and valid for the call, by GL's
    // contract.
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    with(|s| {
        match s
            .programs
            .get(program)
            .and_then(|p| s.gl.get_uniform_location(&p, &name))
        {
            Some(loc) => GlInt::try_from(s.uniforms.add(loc))
                .expect("live uniform handles, a few per program, fit a GLint"),
            None => -1,
        }
    })
}

fn uniform(location: GlInt, f: impl FnOnce(&Wgl, &WebGlUniformLocation)) {
    // -1 is "no such uniform", which GL ignores.
    let Ok(location) = GlUint::try_from(location) else {
        return;
    };
    with(|s| {
        if let Some(u) = s.uniforms.get(location) {
            f(&s.gl, &u)
        }
    })
}

pub unsafe fn glUniform1i(location: GlInt, v0: GlInt) {
    uniform(location, |gl, u| gl.uniform1i(Some(u), v0))
}

pub unsafe fn glUniform2i(location: GlInt, v0: GlInt, v1: GlInt) {
    uniform(location, |gl, u| gl.uniform2i(Some(u), v0, v1))
}

pub unsafe fn glUniform1f(location: GlInt, v0: f32) {
    uniform(location, |gl, u| gl.uniform1f(Some(u), v0))
}

pub unsafe fn glUniform2f(location: GlInt, v0: f32, v1: f32) {
    uniform(location, |gl, u| gl.uniform2f(Some(u), v0, v1))
}

pub unsafe fn glUniform4f(location: GlInt, v0: f32, v1: f32, v2: f32, v3: f32) {
    uniform(location, |gl, u| gl.uniform4f(Some(u), v0, v1, v2, v3))
}

pub unsafe fn glUniformMatrix4fv(
    location: GlInt,
    count: GlSizei,
    transpose: u8,
    value: *const f32,
) {
    // SAFETY: `value` holds `count` 4x4 matrices, 16 floats each, by GL's
    // contract, which is mat4s's.
    let m = unsafe { mat4s(value, count) };
    uniform(location, |gl, u| {
        gl.uniform_matrix4fv_with_f32_array(Some(u), transpose != 0, m)
    })
}

// Buffers and drawing.

pub unsafe fn glGenBuffers(n: GlSizei, buffers: *mut GlUint) {
    for i in 0..from_gl_size(n.max(0)) {
        let id = with(|s| s.gl.create_buffer().map_or(0, |b| s.buffers.add(b)));
        // SAFETY: `buffers` holds `n` GlUints by GL's contract, and i < n.
        unsafe { *buffers.add(i) = id };
    }
}

pub unsafe fn glBindBuffer(target: GlEnum, buffer: GlUint) {
    with(|s| s.gl.bind_buffer(target, s.buffers.get(buffer).as_ref()))
}

/// # Panics
/// If `size` is negative, or past `i32` (which an `isize` can't be on
/// wasm32).
pub unsafe fn glBufferData(target: GlEnum, size: isize, data: *const c_void, usage: GlEnum) {
    // SAFETY: non-null `data` holds `size` bytes, by GL's contract.
    match unsafe { bytes(data, from_gl_size(size)) } {
        Some(b) => with(|s| s.gl.buffer_data_with_u8_array(target, b, usage)),
        None => {
            let size = i32::try_from(size).expect("wasm32's isize is i32");
            with(|s| s.gl.buffer_data_with_i32(target, size, usage));
        }
    }
}

pub unsafe fn glVertexAttribPointer(
    index: GlUint,
    size: GlInt,
    type_: GlEnum,
    normalized: u8,
    stride: GlSizei,
    pointer: *const c_void,
) {
    with(|s| {
        s.gl.vertex_attrib_pointer_with_i32(
            index,
            size,
            type_,
            normalized != 0,
            stride,
            pointer as i32,
        )
    })
}

pub unsafe fn glEnableVertexAttribArray(index: GlUint) {
    with(|s| s.gl.enable_vertex_attrib_array(index))
}

pub unsafe fn glDisableVertexAttribArray(index: GlUint) {
    with(|s| s.gl.disable_vertex_attrib_array(index))
}

pub unsafe fn glEnable(cap: GlEnum) {
    with(|s| s.gl.enable(cap))
}

pub unsafe fn glDisable(cap: GlEnum) {
    with(|s| s.gl.disable(cap))
}

pub unsafe fn glBlendFunc(sfactor: GlEnum, dfactor: GlEnum) {
    with(|s| s.gl.blend_func(sfactor, dfactor))
}

pub unsafe fn glBlendFuncSeparate(src_rgb: GlEnum, dst_rgb: GlEnum, src_a: GlEnum, dst_a: GlEnum) {
    with(|s| s.gl.blend_func_separate(src_rgb, dst_rgb, src_a, dst_a))
}

pub unsafe fn glScissor(x: GlInt, y: GlInt, w: GlSizei, h: GlSizei) {
    with(|s| s.gl.scissor(x, y, w, h))
}

pub unsafe fn glDrawArrays(mode: GlEnum, first: GlInt, count: GlSizei) {
    with(|s| s.gl.draw_arrays(mode, first, count))
}

pub unsafe fn glDrawElements(mode: GlEnum, count: GlSizei, type_: GlEnum, indices: *const c_void) {
    with(|s| {
        s.gl.draw_elements_with_i32(mode, count, type_, indices as i32)
    })
}

// Textures and framebuffers.

pub unsafe fn glGenTextures(n: GlSizei, textures: *mut GlUint) {
    for i in 0..from_gl_size(n.max(0)) {
        let id = with(|s| s.gl.create_texture().map_or(0, |t| s.textures.add(t)));
        // SAFETY: `textures` holds `n` GlUints by GL's contract, and i < n.
        unsafe { *textures.add(i) = id };
    }
}

pub unsafe fn glBindTexture(target: GlEnum, texture: GlUint) {
    with(|s| s.gl.bind_texture(target, s.textures.get(texture).as_ref()))
}

pub unsafe fn glActiveTexture(texture: GlEnum) {
    with(|s| s.gl.active_texture(texture))
}

pub unsafe fn glTexParameteri(target: GlEnum, pname: GlEnum, param: GlInt) {
    with(|s| s.gl.tex_parameteri(target, pname, param))
}

/// A failed upload is logged and leaves GL's error set, so `glGetError`
/// reports it to the caller as on the frame (`RenderTarget::alloc`).
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
    // SAFETY: non-null `pixels` holds a width x height image in `format`
    // by GL's contract, at least width * height * bpp bytes (row padding
    // only adds to it).
    let data = unsafe {
        bytes(
            pixels,
            from_gl_size::<_, usize>(width) * from_gl_size::<_, usize>(height) * bpp(format),
        )
    };
    with(|s| {
        let r =
            s.gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
                target,
                level,
                internalformat,
                width,
                height,
                border,
                format,
                type_,
                data,
            );
        if let Err(e) = r {
            log::error!("glTexImage2D: {e:?}");
        }
    })
}

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
    // SAFETY: as glTexImage2D's: at least width * height * bpp bytes.
    let data = unsafe {
        bytes(
            pixels,
            from_gl_size::<_, usize>(width) * from_gl_size::<_, usize>(height) * bpp(format),
        )
    };
    with(|s| {
        let r =
            s.gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_opt_u8_array(
                target, level, xoffset, yoffset, width, height, format, type_, data,
            );
        if let Err(e) = r {
            log::error!("glTexSubImage2D: {e:?}");
        }
    })
}

pub unsafe fn glDeleteTextures(n: GlSizei, textures: *const GlUint) {
    for i in 0..from_gl_size(n.max(0)) {
        // SAFETY: `textures` holds `n` GlUints by GL's contract, and i < n.
        let id = unsafe { *textures.add(i) };
        with(|s| {
            if let Some(t) = s.textures.remove(id) {
                s.gl.delete_texture(Some(&t))
            }
        })
    }
}

pub unsafe fn glGenFramebuffers(n: GlSizei, framebuffers: *mut GlUint) {
    for i in 0..from_gl_size(n.max(0)) {
        let id = with(|s| {
            s.gl.create_framebuffer()
                .map_or(0, |f| s.framebuffers.add(f))
        });
        // SAFETY: `framebuffers` holds `n` GlUints by GL's contract, and
        // i < n.
        unsafe { *framebuffers.add(i) = id };
    }
}

pub unsafe fn glBindFramebuffer(target: GlEnum, framebuffer: GlUint) {
    with(|s| {
        s.gl.bind_framebuffer(target, s.framebuffers.get(framebuffer).as_ref())
    })
}

pub unsafe fn glFramebufferTexture2D(
    target: GlEnum,
    attachment: GlEnum,
    textarget: GlEnum,
    texture: GlUint,
    level: GlInt,
) {
    with(|s| {
        s.gl.framebuffer_texture_2d(
            target,
            attachment,
            textarget,
            s.textures.get(texture).as_ref(),
            level,
        )
    })
}

pub unsafe fn glCheckFramebufferStatus(target: GlEnum) -> GlEnum {
    with(|s| s.gl.check_framebuffer_status(target))
}

pub unsafe fn glDeleteFramebuffers(n: GlSizei, framebuffers: *const GlUint) {
    for i in 0..from_gl_size(n.max(0)) {
        // SAFETY: `framebuffers` holds `n` GlUints by GL's contract, and
        // i < n.
        let id = unsafe { *framebuffers.add(i) };
        with(|s| {
            if let Some(f) = s.framebuffers.remove(id) {
                s.gl.delete_framebuffer(Some(&f))
            }
        })
    }
}
