//! Raw EGL FFI. The GLES2 bindings live in raam-core (the cfg-selected GL
//! layer); EGL — displays, surfaces, contexts — is window-system glue and
//! belongs to the host.
use std::ffi::c_void;

pub type EglDisplay = *mut c_void;
pub type EglConfig = *mut c_void;
pub type EglContext = *mut c_void;
pub type EglSurface = *mut c_void;
pub type EglInt = i32;
pub type EglBoolean = u32;

pub const EGL_NONE: EglInt = 0x3038;
pub const EGL_SURFACE_TYPE: EglInt = 0x3033;
pub const EGL_WINDOW_BIT: EglInt = 0x0004;
pub const EGL_RENDERABLE_TYPE: EglInt = 0x3040;
pub const EGL_OPENGL_ES2_BIT: EglInt = 0x0004;
pub const EGL_RED_SIZE: EglInt = 0x3024;
pub const EGL_GREEN_SIZE: EglInt = 0x3023;
pub const EGL_BLUE_SIZE: EglInt = 0x3022;
pub const EGL_CONTEXT_CLIENT_VERSION: EglInt = 0x3098;
pub const EGL_PBUFFER_BIT: EglInt = 0x0001;
pub const EGL_WIDTH: EglInt = 0x3057;
pub const EGL_HEIGHT: EglInt = 0x3056;

#[allow(non_snake_case, dead_code)]
unsafe extern "C" {
    pub fn eglGetDisplay(display_id: *mut c_void) -> EglDisplay;
    pub fn eglInitialize(dpy: EglDisplay, major: *mut EglInt, minor: *mut EglInt) -> EglBoolean;
    pub fn eglChooseConfig(
        dpy: EglDisplay,
        attrib_list: *const EglInt,
        configs: *mut EglConfig,
        config_size: EglInt,
        num_config: *mut EglInt,
    ) -> EglBoolean;
    pub fn eglCreateWindowSurface(
        dpy: EglDisplay,
        config: EglConfig,
        win: *mut c_void,
        attrib_list: *const EglInt,
    ) -> EglSurface;
    pub fn eglCreateContext(
        dpy: EglDisplay,
        config: EglConfig,
        share_context: EglContext,
        attrib_list: *const EglInt,
    ) -> EglContext;
    pub fn eglMakeCurrent(
        dpy: EglDisplay,
        draw: EglSurface,
        read: EglSurface,
        ctx: EglContext,
    ) -> EglBoolean;
    pub fn eglSwapBuffers(dpy: EglDisplay, surface: EglSurface) -> EglBoolean;
    pub fn eglGetError() -> EglInt;
    // The window surface comes and goes with the window; a 1x1 pbuffer
    // keeps the context current (and its GL objects alive) in between.
    pub fn eglCreatePbufferSurface(
        dpy: EglDisplay,
        config: EglConfig,
        attrib_list: *const EglInt,
    ) -> EglSurface;
    pub fn eglDestroySurface(dpy: EglDisplay, surface: EglSurface) -> EglBoolean;
}
