//! 016/018's GLES2 egui painter. Changes for sharing a GL context with the
//! slideshow: it sets its own viewport and active texture unit instead of
//! assuming them, and disables the vertex attribute arrays it enabled when
//! done - a left-enabled `aColor` array still pointing at egui's dynamic VBO
//! is an out-of-bounds read waiting to happen once the slideshow's
//! 2-attribute programs draw next. Painting is split into `upload` (one
//! VBO/IBO for the whole frame) and `draw`, so an unchanged overlay can be
//! redrawn over a moving slideshow without re-running egui.
use crate::gl::*;
use std::collections::HashMap;
use std::ffi::c_void;

const VS_SRC: &str = "attribute vec2 aPos; attribute vec2 aUV; attribute vec4 aColor; \
     uniform vec2 uScreenSize; varying vec2 vUV; varying vec4 vColor; \
     void main() { \
         vUV = aUV; \
         vColor = aColor; \
         vec2 ndc = vec2(2.0 * aPos.x / uScreenSize.x - 1.0, 1.0 - 2.0 * aPos.y / uScreenSize.y); \
         gl_Position = vec4(ndc, 0.0, 1.0); \
     }";

const FS_SRC: &str = "precision mediump float; varying vec2 vUV; varying vec4 vColor; \
     uniform sampler2D uTex; \
     void main() { gl_FragColor = texture2D(uTex, vUV) * vColor; }";

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

pub struct Painter {
    program: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    a_color: GlUint,
    u_screen_size: GlInt,
    u_tex: GlInt,
    vbo: GlUint,
    ibo: GlUint,
    textures: HashMap<egui::TextureId, GlUint>,
    cmds: Vec<DrawCmd>,
}

struct DrawCmd {
    texture: egui::TextureId,
    scissor: (i32, i32, i32, i32),
    vert_byte_offset: usize,
    idx_byte_offset: usize,
    idx_count: i32,
}

impl Painter {
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn new() -> Self {
        unsafe {
            let program = link_program("egui", VS_SRC, FS_SRC);
            let mut vbo = 0;
            glGenBuffers(1, &mut vbo);
            let mut ibo = 0;
            glGenBuffers(1, &mut ibo);
            Self {
                a_pos: attrib_loc(program, "aPos"),
                a_uv: attrib_loc(program, "aUV"),
                a_color: attrib_loc(program, "aColor"),
                u_screen_size: uniform_loc(program, "uScreenSize"),
                u_tex: uniform_loc(program, "uTex"),
                program,
                vbo,
                ibo,
                textures: HashMap::new(),
                cmds: Vec::new(),
            }
        }
    }

    pub fn set_texture(&mut self, id: egui::TextureId, delta: &egui::epaint::ImageDelta) {
        let [w, h] = delta.image.size();
        let rgba = image_to_rgba_bytes(&delta.image);
        unsafe {
            let tex = *self.textures.entry(id).or_insert_with(|| {
                let mut t = 0;
                glGenTextures(1, &mut t);
                glBindTexture(GL_TEXTURE_2D, t);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
                t
            });
            glBindTexture(GL_TEXTURE_2D, tex);
            match delta.pos {
                None => glTexImage2D(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA as i32,
                    w as i32,
                    h as i32,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    rgba.as_ptr() as *const c_void,
                ),
                Some([x, y]) => glTexSubImage2D(
                    GL_TEXTURE_2D,
                    0,
                    x as i32,
                    y as i32,
                    w as i32,
                    h as i32,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    rgba.as_ptr() as *const c_void,
                ),
            }
        }
    }

    pub fn free_texture(&mut self, id: egui::TextureId) {
        if let Some(tex) = self.textures.remove(&id) {
            unsafe { glDeleteTextures(1, &tex) };
        }
    }

    /// Uploads a frame's tessellated meshes into one persistent VBO/IBO pair
    /// and records a draw list, so frames where egui has nothing new to say
    /// can redraw the overlay with `draw` alone (no run_ui, no tessellate,
    /// no buffer upload).
    pub fn upload(&mut self, primitives: &[egui::ClippedPrimitive], screen_w: i32, screen_h: i32) {
        let mut verts: Vec<GpuVertex> = Vec::new();
        let mut indices: Vec<u16> = Vec::new();
        self.cmds.clear();
        for prim in primitives {
            let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive else {
                continue;
            };
            if mesh.indices.is_empty() {
                continue;
            }
            if mesh.vertices.len() > u16::MAX as usize {
                log::warn!(
                    "mesh has {} vertices (> u16::MAX), skipping",
                    mesh.vertices.len()
                );
                continue;
            }
            let clip = prim.clip_rect;
            let x = clip.min.x.max(0.0).floor() as i32;
            let w = (clip.max.x.min(screen_w as f32) - clip.min.x.max(0.0))
                .max(0.0)
                .ceil() as i32;
            let h = (clip.max.y.min(screen_h as f32) - clip.min.y.max(0.0))
                .max(0.0)
                .ceil() as i32;
            let y_gl = (screen_h as f32 - clip.max.y.min(screen_h as f32))
                .max(0.0)
                .floor() as i32;
            self.cmds.push(DrawCmd {
                texture: mesh.texture_id,
                scissor: (x, y_gl, w, h),
                vert_byte_offset: verts.len() * std::mem::size_of::<GpuVertex>(),
                idx_byte_offset: indices.len() * 2,
                idx_count: mesh.indices.len() as i32,
            });
            verts.extend(mesh.vertices.iter().map(|v| GpuVertex {
                pos: [v.pos.x, v.pos.y],
                uv: [v.uv.x, v.uv.y],
                color: v.color.to_array(),
            }));
            indices.extend(mesh.indices.iter().map(|&i| i as u16));
        }
        unsafe {
            glBindBuffer(GL_ARRAY_BUFFER, self.vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                (verts.len() * std::mem::size_of::<GpuVertex>()) as isize,
                verts.as_ptr() as *const c_void,
                GL_DYNAMIC_DRAW,
            );
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, self.ibo);
            glBufferData(
                GL_ELEMENT_ARRAY_BUFFER,
                (indices.len() * 2) as isize,
                indices.as_ptr() as *const c_void,
                GL_DYNAMIC_DRAW,
            );
        }
    }

    pub fn draw(&self, screen_w: i32, screen_h: i32) {
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glViewport(0, 0, screen_w, screen_h);
            glActiveTexture(GL_TEXTURE0);
            glEnable(GL_BLEND);
            glBlendFuncSeparate(
                GL_ONE,
                GL_ONE_MINUS_SRC_ALPHA,
                GL_ONE,
                GL_ONE_MINUS_SRC_ALPHA,
            );
            glEnable(GL_SCISSOR_TEST);
            glUseProgram(self.program);
            glUniform2f(self.u_screen_size, screen_w as f32, screen_h as f32);
            glUniform1i(self.u_tex, 0);
            glBindBuffer(GL_ARRAY_BUFFER, self.vbo);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, self.ibo);
            glEnableVertexAttribArray(self.a_pos);
            glEnableVertexAttribArray(self.a_uv);
            glEnableVertexAttribArray(self.a_color);
            let stride = std::mem::size_of::<GpuVertex>() as i32;
            for cmd in &self.cmds {
                let Some(&tex) = self.textures.get(&cmd.texture) else {
                    log::warn!("no GL texture for {:?}, skipping mesh", cmd.texture);
                    continue;
                };
                let base = cmd.vert_byte_offset;
                glVertexAttribPointer(self.a_pos, 2, GL_FLOAT, 0, stride, base as *const c_void);
                glVertexAttribPointer(
                    self.a_uv,
                    2,
                    GL_FLOAT,
                    0,
                    stride,
                    (base + 2 * 4) as *const c_void,
                );
                glVertexAttribPointer(
                    self.a_color,
                    4,
                    GL_UNSIGNED_BYTE,
                    1,
                    stride,
                    (base + 4 * 4) as *const c_void,
                );
                let (x, y, w, h) = cmd.scissor;
                glScissor(x, y, w, h);
                glBindTexture(GL_TEXTURE_2D, tex);
                glDrawElements(
                    GL_TRIANGLES,
                    cmd.idx_count,
                    GL_UNSIGNED_SHORT,
                    cmd.idx_byte_offset as *const c_void,
                );
            }
            glDisable(GL_SCISSOR_TEST);
            glDisable(GL_BLEND);
            glDisableVertexAttribArray(self.a_pos);
            glDisableVertexAttribArray(self.a_uv);
            glDisableVertexAttribArray(self.a_color);
        }
    }
}

fn image_to_rgba_bytes(image: &egui::ImageData) -> Vec<u8> {
    match image {
        egui::ImageData::Color(color_image) => color_image
            .pixels
            .iter()
            .flat_map(|c| c.to_array())
            .collect(),
    }
}
