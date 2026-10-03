//! A stand-in for the slideshow under the preset host's menu pages (the
//! live mode draws the real pipeline): the core's gl-transitions shaders
//! between two generated full-screen textures. `--backdrop still` holds
//! the first; a transition name loops the progress without pause, so
//! every frame is a transition frame - the frame's heaviest case under
//! the chrome.
use raam_core::gl::*;
use raam_core::transitions::TransitionProgram;
use std::ffi::c_void;

/// One changeover takes this long, then the next starts at once.
const PERIOD_S: f32 = 2.0;

pub struct Backdrop {
    progs: Vec<TransitionProgram>,
    vbo: GlUint,
    ibo: GlUint,
    from: GlUint,
    to: GlUint,
}

impl Backdrop {
    /// Needs a current GL context. `w`×`h` is the screen, so the textures
    /// sample 1:1 like the pipeline's composed slides.
    pub unsafe fn new(w: i32, h: i32) -> Self {
        unsafe {
            let progs = TransitionProgram::all();
            // Fullscreen quad: aPos (x, y), aUV (u, v) per vertex, as the
            // transition programs read it.
            let quad: [f32; 16] = [
                -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 0.0, 1.0,
            ];
            let idx: [u16; 6] = [0, 1, 2, 0, 2, 3];
            let mut bufs = [0u32; 2];
            glGenBuffers(2, bufs.as_mut_ptr());
            glBindBuffer(GL_ARRAY_BUFFER, bufs[0]);
            glBufferData(
                GL_ARRAY_BUFFER,
                size_of_val(&quad) as isize,
                quad.as_ptr() as *const c_void,
                GL_STATIC_DRAW,
            );
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, bufs[1]);
            glBufferData(
                GL_ELEMENT_ARRAY_BUFFER,
                size_of_val(&idx) as isize,
                idx.as_ptr() as *const c_void,
                GL_STATIC_DRAW,
            );
            glBindBuffer(GL_ARRAY_BUFFER, 0);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
            Self {
                progs,
                vbo: bufs[0],
                ibo: bufs[1],
                from: texture(w, h, 0),
                to: texture(w, h, 1),
            }
        }
    }

    /// Draws transition `name` at the phase `t_s` seconds gives, into the
    /// bound framebuffer, with a slow Ken Burns-like zoom on both slides.
    pub unsafe fn draw(&self, name: &str, t_s: f32, w: i32, h: i32) {
        let Some(p) = self.progs.iter().find(|p| p.name == name) else {
            return;
        };
        let progress = (t_s / PERIOD_S).fract();
        let zoom = 0.9 + 0.05 * (t_s * 0.3).sin();
        let kb = ((zoom, zoom), ((1.0 - zoom) * 0.5, (1.0 - zoom) * 0.5));
        unsafe {
            p.draw(
                self.vbo,
                self.ibo,
                self.from,
                self.to,
                progress,
                w as f32 / h as f32,
                kb,
                kb,
            );
            // The program's attribute slots aren't known here; clear them
            // all so the painter's arrays start clean (as the live host
            // does after the pipeline's draw).
            for i in 0..8 {
                glDisableVertexAttribArray(i);
            }
            glBindBuffer(GL_ARRAY_BUFFER, 0);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, 0);
        }
    }
}

/// A photo-like RGBA texture: broad colour gradients with fine detail, so
/// sampling isn't flattered by a flat image.
unsafe fn texture(w: i32, h: i32, seed: u32) -> GlUint {
    let (wu, hu) = (w as usize, h as usize);
    let mut px = vec![0u8; wu * hu * 4];
    let mut rng = 0x9E37_79B9u32.wrapping_mul(seed + 1);
    for y in 0..hu {
        for x in 0..wu {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let n = (rng & 31) as f32 - 16.0;
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let (r, g, b) = if seed == 0 {
                (
                    40.0 + 180.0 * fx,
                    90.0 + 120.0 * fy,
                    160.0 - 100.0 * fx * fy,
                )
            } else {
                (
                    200.0 - 120.0 * fy,
                    120.0 + 80.0 * (fx * 12.0).sin(),
                    60.0 + 150.0 * fx,
                )
            };
            let i = (y * wu + x) * 4;
            px[i] = (r + n).clamp(0.0, 255.0) as u8;
            px[i + 1] = (g + n).clamp(0.0, 255.0) as u8;
            px[i + 2] = (b + n).clamp(0.0, 255.0) as u8;
            px[i + 3] = 255;
        }
    }
    unsafe {
        let mut tex = 0;
        glGenTextures(1, &mut tex);
        glBindTexture(GL_TEXTURE_2D, tex);
        set_linear_clamp();
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA as GlInt,
            w,
            h,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            px.as_ptr() as *const c_void,
        );
        glBindTexture(GL_TEXTURE_2D, 0);
        tex
    }
}
