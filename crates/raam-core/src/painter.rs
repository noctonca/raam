//! The GLES2 egui painter. It shares a GL context with the slideshow, so
//! it sets its own viewport and active texture unit instead of assuming
//! them, and disables the vertex attribute arrays it enabled when done - a
//! left-enabled `aColor` array still pointing at egui's dynamic VBO is an
//! out-of-bounds read waiting to happen once the slideshow's 2-attribute
//! programs draw next. Painting is split into `upload` (one VBO/IBO for
//! the whole frame) and `draw`, so an unchanged overlay can be redrawn
//! over a moving slideshow without re-running egui.
//!
//! Two more things:
//! - `pixels_per_point`: egui's meshes and clip rects are in points, so the
//!   screen size uniform is in points and scissors are scaled to pixels.
//! - A text boost in the shader. The font atlas keeps raw coverage in both
//!   themes (so a theme switch never rebuilds it), and meshes drawn with
//!   the font texture get egui's dark-mode curve, `2c - c^2`, applied here
//!   instead, weighted by how light the text colour is. Light text on dark
//!   gets the boost and dark text on light doesn't, per vertex, whatever
//!   the theme. That's the "do the colour compensation in the shader,
//!   based on the active text colour" route epaint's own comment on
//!   `FontColorTransferFunction` suggests.
use crate::gl::*;
use std::collections::HashMap;
use std::ffi::c_void;

// Luma of the un-premultiplied vertex colour (sRGB-encoded, which is what
// the eye judges "light" by). Below 0.3: no boost; above 0.7: full boost.
const VS_SRC: &str = "attribute vec2 aPos; attribute vec2 aUV; attribute vec4 aColor; \
     uniform vec2 uScreenSize; uniform float uBoost; \
     varying vec2 vUV; varying vec4 vColor; varying float vBoost; \
     void main() { \
         vUV = aUV; \
         vColor = aColor; \
         vec3 rgb = aColor.a > 0.0 ? aColor.rgb / aColor.a : vec3(0.0); \
         vBoost = uBoost * smoothstep(0.3, 0.7, dot(rgb, vec3(0.2126, 0.7152, 0.0722))); \
         vec2 ndc = vec2(2.0 * aPos.x / uScreenSize.x - 1.0, 1.0 - 2.0 * aPos.y / uScreenSize.y); \
         gl_Position = vec4(ndc, 0.0, 1.0); \
     }";

// The atlas stores premultiplied white, so the curve applies to all four
// channels alike. Solid shapes sample the atlas's opaque texel (c = 1),
// which the curve leaves at 1.
const FS_SRC: &str = "precision mediump float; varying vec2 vUV; varying vec4 vColor; varying float vBoost; \
     uniform sampler2D uTex; \
     void main() { \
         vec4 t = texture2D(uTex, vUV); \
         gl_FragColor = vColor * (t + vBoost * (t - t * t)); \
     }";

/// The plain shader, used while the boost is off (and for A/B-ing its cost).
const FS_PLAIN: &str = "precision mediump float; varying vec2 vUV; varying vec4 vColor; varying float vBoost; \
     uniform sampler2D uTex; \
     void main() { gl_FragColor = texture2D(uTex, vUV) * vColor; }";

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

struct Program {
    id: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    a_color: GlUint,
    u_screen_size: GlInt,
    u_tex: GlInt,
    u_boost: GlInt,
}

impl Program {
    unsafe fn new(label: &str, fs: &str) -> Self {
        unsafe {
            let id = link_program(label, VS_SRC, fs);
            Self {
                a_pos: attrib_loc(id, "aPos"),
                a_uv: attrib_loc(id, "aUV"),
                a_color: attrib_loc(id, "aColor"),
                u_screen_size: uniform_loc(id, "uScreenSize"),
                u_tex: uniform_loc(id, "uTex"),
                u_boost: uniform_loc(id, "uBoost"),
                id,
            }
        }
    }
}

pub struct Painter {
    boosted: Program,
    plain: Program,
    /// Apply the text boost (`theme::TextMode::Shader`).
    pub text_boost: bool,
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
    /// Makes the vertex and index buffers and links egui's two programs.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn new() -> Self {
        unsafe {
            let mut vbo = 0;
            glGenBuffers(1, &mut vbo);
            let mut ibo = 0;
            glGenBuffers(1, &mut ibo);
            Self {
                boosted: Program::new("egui-boost", FS_SRC),
                plain: Program::new("egui", FS_PLAIN),
                text_boost: false,
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
                set_linear_clamp();
                t
            });
            glBindTexture(GL_TEXTURE_2D, tex);
            // SAFETY: `rgba` is w * h * 4 bytes (image_to_rgba_bytes asserts
            // it), what GL reads for a w x h RGBA/UNSIGNED_BYTE upload, and
            // it outlives the call; GL copies it before returning.
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
    pub fn upload(
        &mut self,
        primitives: &[egui::ClippedPrimitive],
        ppp: f32,
        screen_w: i32,
        screen_h: i32,
    ) {
        let mut verts: Vec<GpuVertex> = Vec::new();
        let mut indices: Vec<u16> = Vec::new();
        self.cmds.clear();
        tessellated(
            primitives,
            ppp,
            (screen_w, screen_h),
            &mut verts,
            &mut indices,
            &mut self.cmds,
        );
        // SAFETY: each size is its Vec's length in bytes, and both Vecs
        // outlive the calls; GL copies the data before returning.
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

    pub fn draw(&self, ppp: f32, screen_w: i32, screen_h: i32) {
        let p = if self.text_boost {
            &self.boosted
        } else {
            &self.plain
        };
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
            glUseProgram(p.id);
            glUniform2f(
                p.u_screen_size,
                screen_w as f32 / ppp,
                screen_h as f32 / ppp,
            );
            glUniform1i(p.u_tex, 0);
            glBindBuffer(GL_ARRAY_BUFFER, self.vbo);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, self.ibo);
            glEnableVertexAttribArray(p.a_pos);
            glEnableVertexAttribArray(p.a_uv);
            glEnableVertexAttribArray(p.a_color);
            let stride = std::mem::size_of::<GpuVertex>() as i32;
            for cmd in &self.cmds {
                let Some(&tex) = self.textures.get(&cmd.texture) else {
                    log::warn!("no GL texture for {:?}, skipping mesh", cmd.texture);
                    continue;
                };
                let base = cmd.vert_byte_offset;
                glVertexAttribPointer(p.a_pos, 2, GL_FLOAT, 0, stride, base as *const c_void);
                glVertexAttribPointer(
                    p.a_uv,
                    2,
                    GL_FLOAT,
                    0,
                    stride,
                    (base + 2 * 4) as *const c_void,
                );
                glVertexAttribPointer(
                    p.a_color,
                    4,
                    GL_UNSIGNED_BYTE,
                    1,
                    stride,
                    (base + 4 * 4) as *const c_void,
                );
                // Only the font atlas (egui's managed texture 0) holds glyph
                // coverage; user images must not be curved.
                let boost = self.text_boost && cmd.texture == egui::TextureId::default();
                glUniform1f(p.u_boost, if boost { 1.0 } else { 0.0 });
                let (x, y, w, h) = cmd.scissor;
                glScissor(x, y, w, h);
                glBindTexture(GL_TEXTURE_2D, tex);
                // SAFETY: the offsets are into the buffers `upload` filled
                // from the same `cmds`, and each part's indices point only
                // at its own vertices (`tessellated`), so GL reads in bounds.
                glDrawElements(
                    GL_TRIANGLES,
                    cmd.idx_count,
                    GL_UNSIGNED_SHORT,
                    cmd.idx_byte_offset as *const c_void,
                );
            }
            glDisable(GL_SCISSOR_TEST);
            glDisable(GL_BLEND);
            glDisableVertexAttribArray(p.a_pos);
            glDisableVertexAttribArray(p.a_uv);
            glDisableVertexAttribArray(p.a_color);
        }
    }
}

/// The CPU half of `upload`: a frame's meshes as one vertex and one index
/// list plus the draw list over them. GLES2 has only 16-bit indices, so a
/// mesh past `u16::MAX` vertices (a long scrolled list, say) is split into
/// parts that fit, the way epaint suggests, instead of being dropped.
fn tessellated(
    primitives: &[egui::ClippedPrimitive],
    ppp: f32,
    (screen_w, screen_h): (i32, i32),
    verts: &mut Vec<GpuVertex>,
    indices: &mut Vec<u16>,
    cmds: &mut Vec<DrawCmd>,
) {
    for prim in primitives {
        let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive else {
            continue;
        };
        if mesh.indices.is_empty() {
            continue;
        }
        let clip = egui::Rect::from_min_max(
            (prim.clip_rect.min.to_vec2() * ppp).to_pos2(),
            (prim.clip_rect.max.to_vec2() * ppp).to_pos2(),
        );
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
        let mut push = |vertices: &[egui::epaint::Vertex], part: &mut dyn Iterator<Item = u16>| {
            let idx_start = indices.len();
            let vert_byte_offset = verts.len() * std::mem::size_of::<GpuVertex>();
            verts.extend(vertices.iter().map(|v| GpuVertex {
                pos: [v.pos.x, v.pos.y],
                uv: [v.uv.x, v.uv.y],
                color: v.color.to_array(),
            }));
            indices.extend(part);
            cmds.push(DrawCmd {
                texture: mesh.texture_id,
                scissor: (x, y_gl, w, h),
                vert_byte_offset,
                idx_byte_offset: idx_start * 2,
                idx_count: i32::try_from(indices.len() - idx_start)
                    .expect("a 16-bit part's indices fit i32"),
            });
        };
        // epaint's own bound for one 16-bit part.
        if mesh.vertices.len() <= usize::from(u16::MAX) {
            push(&mesh.vertices, &mut mesh.indices.iter().map(|&i| i as u16));
        } else {
            for part in mesh.clone().split_to_u16() {
                push(&part.vertices, &mut part.indices.into_iter());
            }
        }
    }
}

/// The image as GL's RGBA bytes, exactly `w * h * 4` of them: the upload
/// hands GL a bare pointer and the size, and the driver reads that many.
fn image_to_rgba_bytes(image: &egui::ImageData) -> Vec<u8> {
    let [w, h] = image.size();
    let rgba: Vec<u8> = match image {
        egui::ImageData::Color(color_image) => color_image
            .pixels
            .iter()
            .flat_map(|c| c.to_array())
            .collect(),
    };
    assert_eq!(
        rgba.len(),
        w * h * 4,
        "egui image {w}x{h}: wrong byte count"
    );
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{Mesh, Primitive, Vertex};

    /// A strip of separate triangles, `tris` of them, so every part split
    /// off is a whole number of triangles.
    fn mesh(tris: u32) -> egui::ClippedPrimitive {
        let mut m = Mesh::default();
        for i in 0..tris * 3 {
            m.vertices.push(Vertex {
                pos: egui::pos2(i as f32, 0.0),
                uv: egui::Pos2::ZERO,
                color: egui::Color32::WHITE,
            });
            m.indices.push(i);
        }
        egui::ClippedPrimitive {
            clip_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 480.0)),
            primitive: Primitive::Mesh(m),
        }
    }

    fn tessellate(prims: &[egui::ClippedPrimitive]) -> (Vec<GpuVertex>, Vec<u16>, Vec<DrawCmd>) {
        let (mut v, mut i, mut c) = (Vec::new(), Vec::new(), Vec::new());
        tessellated(prims, 1.0, (800, 480), &mut v, &mut i, &mut c);
        (v, i, c)
    }

    #[test]
    fn a_mesh_past_u16_vertices_is_split_not_dropped() {
        let tris = 30_000; // 90,000 vertices
        let (verts, indices, cmds) = tessellate(&[mesh(tris)]);
        assert!(
            cmds.len() > 1,
            "the mesh should be split, got {} parts",
            cmds.len()
        );
        let drawn: i32 = cmds.iter().map(|c| c.idx_count).sum();
        assert_eq!(drawn as u32, tris * 3, "every triangle is drawn");
        let vsize = std::mem::size_of::<GpuVertex>();
        let mut ends: Vec<usize> = cmds.iter().map(|c| c.vert_byte_offset / vsize).collect();
        ends.push(verts.len());
        for (k, cmd) in cmds.iter().enumerate() {
            let start = cmd.idx_byte_offset / 2;
            let part = &indices[start..start + cmd.idx_count as usize];
            let len = ends[k + 1] - ends[k];
            assert!(
                part.iter().all(|&i| usize::from(i) < len),
                "part {k} indexes past its vertices"
            );
        }
    }

    #[test]
    fn a_small_mesh_stays_one_part() {
        let (verts, indices, cmds) = tessellate(&[mesh(2), mesh(1)]);
        assert_eq!(cmds.len(), 2);
        assert_eq!((verts.len(), indices.len()), (9, 9));
        assert_eq!(
            cmds[1].vert_byte_offset,
            6 * std::mem::size_of::<GpuVertex>()
        );
        assert_eq!((cmds[1].idx_byte_offset, cmds[1].idx_count), (12, 3));
    }

    #[test]
    fn an_image_uploads_four_bytes_a_texel() {
        let image = egui::ColorImage {
            size: [3, 2],
            source_size: egui::vec2(3.0, 2.0),
            pixels: vec![egui::Color32::RED; 6],
        };
        let rgba = image_to_rgba_bytes(&egui::ImageData::Color(image.into()));
        assert_eq!(rgba.len(), 24);
    }

    #[test]
    #[should_panic(expected = "wrong byte count")]
    fn an_image_short_of_its_size_is_refused() {
        // GL would read 24 bytes from a 20-byte buffer.
        let image = egui::ColorImage {
            size: [3, 2],
            source_size: egui::vec2(3.0, 2.0),
            pixels: vec![egui::Color32::RED; 5],
        };
        image_to_rgba_bytes(&egui::ImageData::Color(image.into()));
    }
}
