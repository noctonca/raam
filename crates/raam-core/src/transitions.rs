//! The slide transitions: five gl-transitions effects
//! (<https://github.com/gl-transitions/gl-transitions>, MIT; fade,
//! directionalwipe, cube and swap by gre, crosswarp by Eke Péter),
//! re-expressed for the frame's GPU. Each draws what its upstream `.glsl`
//! draws, with the same fixed parameters, but not the way upstream does.
//!
//! Upstream runs everything per pixel in the fragment shader, and on the
//! Mali-400 that costs twice. Its fragment shader has no `highp`
//! (`mediump` is fp16, a 10-bit mantissa), so a texture coordinate
//! computed there lands up to 1.2 texels off at 1280 wide, where the
//! desktop and web compute in fp32: the frame drew differently from the
//! other hosts. And per-pixel arithmetic is paid on a million pixels, on a
//! GPU whose budget is mostly gone to memory traffic and the compositor.
//! So anything linear in the screen position (the Ken Burns windows, the
//! wipe's edge distance, crosswarp's offsets) is computed in the vertex
//! shader, which runs in fp32 and is interpolated, and anything that is the
//! same for every pixel moves to the CPU. Cube and swap, whose faces are
//! projective maps of the slides, are drawn as geometry: each face is a
//! quad whose texture v is `base + num / den`, with u, num and den linear
//! across it, so one division per pixel replaces upstream's branches and
//! bounds tests. Measured on the frame against the upstream shaders
//! (offscreen, 1280x800): fade 13.7 -> 10.0 ms, directionalwipe 24.6 ->
//! 13.8, crosswarp 23.5 -> 12.0, cube 49.9 -> 10.9, swap 51.4 -> 13.0;
//! against an fp32 render every one is closer than upstream's on the
//! frame, and on the desktop they match upstream to 1/255 (cube and swap
//! but for a few edge pixels, where rasterisation decides instead of
//! upstream's strict `inBounds`).
//!
//! `from`/`to` are composed tiles or scratch collages, both render
//! targets, so every coordinate below starts from `(aUV.x, 1 - aUV.y)`:
//! a render-to-texture hop flips rows relative to the upload convention
//! (v = 0 is the top of a decoded photo), and a tile goes through one.
//! Without the flip the first screenshot came back upside-down.
use super::gl::*;
use std::ffi::c_void;

/// Every transition's name, in the order [`TransitionProgram::all`] builds
/// them; each `TransitionChoice::shader_name` is one of these.
pub const NAMES: [&str; 5] = ["fade", "directionalwipe", "cube", "crosswarp", "swap"];

/// A Ken Burns window: (scale, offset) applied to a slide's uv.
pub type UvWindow = ((f32, f32), (f32, f32));

// fade: mix(from, to, progress), both windows in the vertex shader.
const VS_FADE: &str = "attribute vec2 aPos; attribute vec2 aUV; \
     uniform vec2 uFromScale; uniform vec2 uFromOffset; uniform vec2 uToScale; uniform vec2 uToOffset; \
     varying vec4 vUVs; \
     void main() { vec2 uv = vec2(aUV.x, 1.0 - aUV.y); \
         vUVs = vec4(uv * uFromScale + uFromOffset, uv * uToScale + uToOffset); \
         gl_Position = vec4(aPos, 0.0, 1.0); }";
const FS_FADE: &str = "precision mediump float; varying vec4 vUVs; \
     uniform sampler2D from; uniform sampler2D to; uniform float progress; \
     void main() { gl_FragColor = mix(texture2D(from, vUVs.xy), texture2D(to, vUVs.zw), progress); }";

// directionalwipe (direction (1, -1), smoothness 0.5): the edge distance
// `dot(v, uv) - edge` is linear in uv; v (the direction, normalised and
// then divided by its L1 norm) and the edge are the same for every pixel.
const VS_WIPE: &str = "attribute vec2 aPos; attribute vec2 aUV; \
     uniform vec2 uFromScale; uniform vec2 uFromOffset; uniform vec2 uToScale; uniform vec2 uToOffset; \
     uniform vec2 uDir; uniform float uEdge; \
     varying vec4 vUVs; varying float vDist; \
     void main() { vec2 uv = vec2(aUV.x, 1.0 - aUV.y); \
         vUVs = vec4(uv * uFromScale + uFromOffset, uv * uToScale + uToOffset); \
         vDist = dot(uDir, uv) - uEdge; \
         gl_Position = vec4(aPos, 0.0, 1.0); }";
const FS_WIPE: &str = "precision mediump float; varying vec4 vUVs; varying float vDist; \
     uniform sampler2D from; uniform sampler2D to; uniform float uGate; uniform float uSmoothness; \
     void main() { float m = uGate * (1.0 - smoothstep(-uSmoothness, 0.0, vDist)); \
         gl_FragColor = mix(texture2D(from, vUVs.xy), texture2D(to, vUVs.zw), m); }";
const WIPE_DIRECTION: (f32, f32) = (1.0, -1.0);
const WIPE_SMOOTHNESS: f32 = 0.5;

// crosswarp: x = smoothstep(0, 1, 2 progress + p.x - 1); upstream samples
// from at ((p - .5)(1 - x) + .5) S + O = (pS + O) - x (p - .5) S and to at
// ((p - .5) x + .5) S' + O' = x (p - .5) S' + (.5 S' + O'). Only x is not
// linear in p.
const VS_CROSS: &str = "attribute vec2 aPos; attribute vec2 aUV; \
     uniform vec2 uFromScale; uniform vec2 uFromOffset; uniform vec2 uToScale; uniform float progress; \
     varying vec4 vFrom; varying vec3 vTo; \
     void main() { vec2 p = vec2(aUV.x, 1.0 - aUV.y); \
         vFrom = vec4(p * uFromScale + uFromOffset, (p - 0.5) * uFromScale); \
         vTo = vec3((p - 0.5) * uToScale, progress * 2.0 + p.x - 1.0); \
         gl_Position = vec4(aPos, 0.0, 1.0); }";
const FS_CROSS: &str = "precision mediump float; varying vec4 vFrom; varying vec3 vTo; \
     uniform sampler2D from; uniform sampler2D to; uniform vec2 uToCentre; \
     void main() { float x = smoothstep(0.0, 1.0, vTo.z); \
         gl_FragColor = mix(texture2D(from, vFrom.xy - x * vFrom.zw), \
                            texture2D(to, x * vTo.xy + uToCentre), x); }";

// cube and swap: faces and their reflections as quads. aQ is (u, num,
// den), each linear across the quad; the texture coordinate is
// (u, base + num / den). A reflection samples at y = -1.2 v + c and is
// weighted by reflection * (1 - y), upstream's `bgColor`.
const VS_GEOM: &str = "attribute vec2 aPos; attribute vec3 aQ; varying vec3 vQ; \
     void main() { vQ = aQ; gl_Position = vec4(aPos, 0.0, 1.0); }";
const FS_FACE: &str = "precision mediump float; varying vec3 vQ; uniform sampler2D uTex; \
     uniform float uBase; uniform vec2 uScale; uniform vec2 uOffset; \
     void main() { vec2 t = vec2(vQ.x, uBase + vQ.y / vQ.z); \
         gl_FragColor = texture2D(uTex, t * uScale + uOffset); }";
const FS_REFLECTION: &str = "precision mediump float; varying vec3 vQ; uniform sampler2D uTex; \
     uniform float uBase; uniform vec2 uScale; uniform vec2 uOffset; \
     uniform float uReflectC; uniform float uReflection; \
     void main() { float y = -1.2 * (uBase + vQ.y / vQ.z) + uReflectC; \
         gl_FragColor = vec4(vec3(uReflection * (1.0 - y)), 0.0) \
             * texture2D(uTex, vec2(vQ.x, y) * uScale + uOffset); }";

// cube's upstream parameters.
const CUBE_PERSP: f32 = 0.7;
const CUBE_UNZOOM: f32 = 0.3;
const CUBE_REFLECTION: f32 = 0.4;
const CUBE_FLOATING: f32 = 3.0;
// swap's upstream parameters.
const SWAP_REFLECTION: f32 = 0.4;
const SWAP_PERSPECTIVE: f32 = 0.2;
const SWAP_DEPTH: f32 = 3.0;
// Upstream's `project`: a reflection's y is -1.2 v + c.
const REFLECT_SCALE: f32 = -1.2;
const SWAP_REFLECT_C: f32 = -0.02;

/// One corner of a face: its point in the transition's uv space (origin
/// bottom-left, as upstream's `uv`) and its (u, num, den).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Corner {
    pub p: (f32, f32),
    pub q: (f32, f32, f32),
}

/// A face or reflection quad: four corners, counter-clockwise from the
/// u = 0, v = v0 corner.
pub type Quad = [Corner; 4];

/// The v range a reflection covers: 0 < REFLECT_SCALE v + c < 1.
fn reflection_v(c: f32) -> (f32, f32) {
    (c / -REFLECT_SCALE, (c - 1.0) / -REFLECT_SCALE)
}

/// Builds a quad over v in [v0, v1] from a per-(u, v) corner function.
fn quad(v0: f32, v1: f32, corner: impl Fn(f32, f32) -> Corner) -> Quad {
    [
        corner(0.0, v0),
        corner(1.0, v0),
        corner(1.0, v1),
        corner(0.0, v1),
    ]
}

/// The progress cube and swap draw at: their faces divide by progress
/// and 1 - progress, so the ends are held just inside.
fn held(progress: f32) -> f32 {
    progress.clamp(1e-4, 1.0 - 1e-4)
}

/// cube's geometry at `progress`: (from face, to face, from reflection,
/// to reflection), and the unzoom that maps uv space to the screen.
///
/// Upstream samples from at xskew((p - (progress, 0)) / (1 - progress, 1),
/// pf, 0) = (x, (p.y - (1 - pf) x / 2) / (1 + (pf - 1) x)) with
/// x = (p.x - progress) / (1 - progress), and to at xskew(p / (progress,
/// 1), pt, 1), the mirror image; inverting gives the corners below.
#[must_use]
pub fn cube_quads(progress: f32) -> ([Quad; 4], f32) {
    let pr = held(progress);
    let unzoom = CUBE_UNZOOM * 2.0 * (0.5 - (0.5 - progress).abs());
    let pf = 1.0 - pr * (1.0 - CUBE_PERSP);
    let pt = pr * pr + (1.0 - pr * pr) * CUBE_PERSP;
    let from = |u: f32, v: f32| {
        let den = 1.0 + (pf - 1.0) * u;
        Corner {
            p: (pr + u * (1.0 - pr), v * den + 0.5 * (1.0 - pf) * u),
            q: (u, v * den, den),
        }
    };
    let to = |u: f32, v: f32| {
        let s = 1.0 - u;
        let den = 1.0 + (pt - 1.0) * s;
        Corner {
            p: (u * pr, v * den + 0.5 * (1.0 - pt) * s),
            q: (u, v * den, den),
        }
    };
    let (r0, r1) = reflection_v(-CUBE_FLOATING / 100.0);
    (
        [
            quad(0.0, 1.0, from),
            quad(0.0, 1.0, to),
            quad(r1, r0, from),
            quad(r1, r0, to),
        ],
        unzoom,
    )
}

/// swap's geometry at `progress`: (from card, to card, from reflection,
/// to reflection).
///
/// Upstream samples from at ((p - (0, .5)) (size / (1 - persp_c progress),
/// size / (1 - size persp p.x)) + (0, .5)): u is linear in p.x, and
/// v - .5 = (p.y - .5) / den with den = (1 - size persp p.x) / size; to
/// is the same about (1, .5).
#[must_use]
pub fn swap_quads(progress: f32) -> [Quad; 4] {
    let size = 1.0 + (SWAP_DEPTH - 1.0) * progress;
    let persp = SWAP_PERSPECTIVE * progress;
    let from_stretch = size / (1.0 - SWAP_PERSPECTIVE * progress);
    let from = |u: f32, v: f32| {
        let x = u / from_stretch;
        let den = (1.0 - size * persp * x) / size;
        Corner {
            p: (x, 0.5 + (v - 0.5) * den),
            q: (u, (v - 0.5) * den, den),
        }
    };
    let size_to = 1.0 + (SWAP_DEPTH - 1.0) * (1.0 - progress);
    let persp_to = SWAP_PERSPECTIVE * (1.0 - progress);
    let to_stretch = size_to / (1.0 - SWAP_PERSPECTIVE * (1.0 - progress));
    let to = |u: f32, v: f32| {
        let x = 1.0 + (u - 1.0) / to_stretch;
        let den = (1.0 - size_to * persp_to * (0.5 - x)) / size_to;
        Corner {
            p: (x, 0.5 + (v - 0.5) * den),
            q: (u, (v - 0.5) * den, den),
        }
    };
    let (r0, r1) = reflection_v(SWAP_REFLECT_C);
    [
        quad(0.0, 1.0, from),
        quad(0.0, 1.0, to),
        quad(r1, r0, from),
        quad(r1, r0, to),
    ]
}

/// A linked program over the pipeline's quad (aPos, aUV), with its
/// window uniforms.
struct QuadProgram {
    program: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    u_progress: GlInt,
    u_from_scale: GlInt,
    u_from_offset: GlInt,
    u_to_scale: GlInt,
    u_to_offset: GlInt,
}

impl QuadProgram {
    /// # Safety
    /// Requires a current GL context.
    unsafe fn new(name: &str, vs: &str, fs: &str) -> Self {
        // SAFETY: the caller's contract: a current GL context.
        unsafe {
            let program = link_program(name, vs, fs);
            glUseProgram(program);
            // The texture units are fixed for the program's life.
            glUniform1i(uniform_loc(program, "from"), 0);
            glUniform1i(uniform_loc(program, "to"), 1);
            Self {
                program,
                a_pos: attrib_loc(program, "aPos"),
                a_uv: attrib_loc(program, "aUV"),
                u_progress: uniform_loc(program, "progress"),
                u_from_scale: uniform_loc(program, "uFromScale"),
                u_from_offset: uniform_loc(program, "uFromOffset"),
                u_to_scale: uniform_loc(program, "uToScale"),
                u_to_offset: uniform_loc(program, "uToOffset"),
            }
        }
    }
}

/// One of the two programs cube and swap draw with (aPos, aQ).
struct GeomProgram {
    program: GlUint,
    a_pos: GlUint,
    a_q: GlUint,
    u_base: GlInt,
    u_scale: GlInt,
    u_offset: GlInt,
    u_reflect_c: GlInt,
    u_reflection: GlInt,
}

impl GeomProgram {
    /// # Safety
    /// Requires a current GL context.
    unsafe fn new(name: &str, fs: &str) -> Self {
        // SAFETY: the caller's contract: a current GL context.
        unsafe {
            let program = link_program(name, VS_GEOM, fs);
            glUseProgram(program);
            glUniform1i(uniform_loc(program, "uTex"), 0);
            Self {
                program,
                a_pos: attrib_loc(program, "aPos"),
                a_q: attrib_loc(program, "aQ"),
                u_base: uniform_loc(program, "uBase"),
                u_scale: uniform_loc(program, "uScale"),
                u_offset: uniform_loc(program, "uOffset"),
                // The face program has neither (-1, which GL ignores).
                u_reflect_c: uniform_loc(program, "uReflectC"),
                u_reflection: uniform_loc(program, "uReflection"),
            }
        }
    }
}

enum Kind {
    Fade(QuadProgram),
    Wipe {
        program: QuadProgram,
        u_dir: GlInt,
        u_edge: GlInt,
        u_gate: GlInt,
    },
    Crosswarp(QuadProgram),
    Cube(Geom),
    Swap(Geom),
}

/// cube's and swap's programs and the buffer their corners stream through.
struct Geom {
    face: GeomProgram,
    reflection: GeomProgram,
    vbo: GlUint,
}

impl Geom {
    /// # Safety
    /// Requires a current GL context.
    unsafe fn new(name: &str) -> Self {
        // SAFETY: the caller's contract: a current GL context; `vbo` is a
        // local the call fills with one name.
        unsafe {
            let mut vbo = 0;
            glGenBuffers(1, &mut vbo);
            Self {
                face: GeomProgram::new(&format!("{name} face"), FS_FACE),
                reflection: GeomProgram::new(&format!("{name} reflection"), FS_REFLECTION),
                vbo,
            }
        }
    }

    /// Draws `quads` with `program`, each with its texture and window,
    /// mapping uv space to NDC through `unzoom` (cube's pull-back).
    ///
    /// # Safety
    /// Requires a current GL context and every texture live.
    unsafe fn draw(
        &self,
        program: &GeomProgram,
        quads: &[(&Quad, GlUint, UvWindow)],
        base: f32,
        reflect: (f32, f32),
        unzoom: f32,
    ) {
        // SAFETY: the caller's contract: a current GL context and live
        // textures. `vertices` is 20 floats (80 bytes), all of it handed to
        // glBufferData, and the attribute offsets (0 and 8 of a 20-byte
        // stride) and the six u16 indices the draw reads stay inside its
        // four vertices.
        unsafe {
            glUseProgram(program.program);
            glBindBuffer(GL_ARRAY_BUFFER, self.vbo);
            glUniform1f(program.u_base, base);
            glUniform1f(program.u_reflect_c, reflect.0);
            glUniform1f(program.u_reflection, reflect.1);
            for (corners, texture, window) in quads {
                let mut vertices = [0f32; 20];
                for (v, c) in vertices
                    .as_chunks_mut::<5>()
                    .0
                    .iter_mut()
                    .zip(corners.iter())
                {
                    let ndc = |x: f32| 2.0 * (x + unzoom * 0.5) / (1.0 + unzoom) - 1.0;
                    v.copy_from_slice(&[ndc(c.p.0), ndc(c.p.1), c.q.0, c.q.1, c.q.2]);
                }
                glBufferData(
                    GL_ARRAY_BUFFER,
                    gl_byte_len(&vertices),
                    vertices.as_ptr() as *const c_void,
                    GL_DYNAMIC_DRAW,
                );
                let stride = 5 * 4;
                glVertexAttribPointer(program.a_pos, 2, GL_FLOAT, 0, stride, std::ptr::null());
                glEnableVertexAttribArray(program.a_pos);
                glVertexAttribPointer(
                    program.a_q,
                    3,
                    GL_FLOAT,
                    0,
                    stride,
                    (2 * 4) as *const c_void,
                );
                glEnableVertexAttribArray(program.a_q);
                glUniform2f(program.u_scale, window.0.0, window.0.1);
                glUniform2f(program.u_offset, window.1.0, window.1.1);
                glBindTexture(GL_TEXTURE_2D, *texture);
                glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, std::ptr::null());
            }
        }
    }
}

pub struct TransitionProgram {
    pub name: &'static str,
    kind: Kind,
}

impl TransitionProgram {
    /// Every transition, in [`NAMES`]'s order.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn all() -> Vec<TransitionProgram> {
        // SAFETY: the caller's contract: a current GL context.
        unsafe {
            let wipe = QuadProgram::new(NAMES[1], VS_WIPE, FS_WIPE);
            glUniform1f(uniform_loc(wipe.program, "uSmoothness"), WIPE_SMOOTHNESS);
            let wipe = Kind::Wipe {
                u_dir: uniform_loc(wipe.program, "uDir"),
                u_edge: uniform_loc(wipe.program, "uEdge"),
                u_gate: uniform_loc(wipe.program, "uGate"),
                program: wipe,
            };
            let kinds = [
                Kind::Fade(QuadProgram::new(NAMES[0], VS_FADE, FS_FADE)),
                wipe,
                Kind::Cube(Geom::new(NAMES[2])),
                Kind::Crosswarp(QuadProgram::new(NAMES[3], VS_CROSS, FS_CROSS)),
                Kind::Swap(Geom::new(NAMES[4])),
            ];
            NAMES
                .iter()
                .zip(kinds)
                .map(|(&name, kind)| TransitionProgram { name, kind })
                .collect()
        }
    }

    /// Fade blends; the pipeline can draw a collage fade as one collage
    /// blended over the other, without composing either into a target.
    #[must_use]
    pub fn is_fade(&self) -> bool {
        matches!(self.kind, Kind::Fade(_))
    }

    /// Draws the transition into the bound framebuffer and viewport (both
    /// screen-sized), from `from_tex` to `to_tex` at `progress` (0.0-1.0).
    /// `from_kb`/`to_kb` are each slide's Ken Burns window in this file's
    /// uv space: the outgoing slide keeps moving until the draw finishes
    /// it, it doesn't freeze when the transition starts.
    ///
    /// # Panics
    /// If `progress` is outside 0..=1.
    ///
    /// # Safety
    /// Requires a current GL context, `quad_vbo` holding the four pos+uv
    /// vertices and `quad_ibo` the six u16 indices every draw reads (the
    /// pipeline's quad), and both textures live.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn draw(
        &self,
        quad_vbo: GlUint,
        quad_ibo: GlUint,
        from_tex: GlUint,
        to_tex: GlUint,
        progress: f32,
        from_kb: UvWindow,
        to_kb: UvWindow,
    ) {
        assert!(
            (0.0..=1.0).contains(&progress),
            "progress {progress} out of 0..=1"
        );
        // SAFETY: the caller's contract: a current GL context, the
        // pipeline's quad (the attribute offsets within its 16-byte
        // vertices and the six indices GL reads are in bounds) and live
        // textures.
        unsafe {
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, quad_ibo);
            match &self.kind {
                Kind::Fade(p) | Kind::Crosswarp(p) => {
                    bind_quad(p, quad_vbo, from_tex, to_tex, progress, from_kb, to_kb);
                    if matches!(self.kind, Kind::Crosswarp(_)) {
                        glUniform2f(
                            uniform_loc(p.program, "uToCentre"),
                            0.5 * to_kb.0.0 + to_kb.1.0,
                            0.5 * to_kb.0.1 + to_kb.1.1,
                        );
                    }
                    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, std::ptr::null());
                }
                Kind::Wipe {
                    program,
                    u_dir,
                    u_edge,
                    u_gate,
                } => {
                    bind_quad(
                        program, quad_vbo, from_tex, to_tex, progress, from_kb, to_kb,
                    );
                    let (dx, dy) = WIPE_DIRECTION;
                    let norm = (dx * dx + dy * dy).sqrt();
                    let (nx, ny) = (dx / norm, dy / norm);
                    let l1 = nx.abs() + ny.abs();
                    let dir = (nx / l1, ny / l1);
                    let centre = 0.5 * dir.0 + 0.5 * dir.1;
                    glUniform2f(*u_dir, dir.0, dir.1);
                    glUniform1f(*u_edge, centre - 0.5 + progress * (1.0 + WIPE_SMOOTHNESS));
                    glUniform1f(*u_gate, if progress > 0.0 { 1.0 } else { 0.0 });
                    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, std::ptr::null());
                }
                Kind::Cube(g) => {
                    let ([from, to, from_r, to_r], unzoom) = cube_quads(progress);
                    let c = -CUBE_FLOATING / 100.0;
                    glClearColor(0.0, 0.0, 0.0, 1.0);
                    glClear(GL_COLOR_BUFFER_BIT);
                    glActiveTexture(GL_TEXTURE0);
                    // The reflections and faces never overlap one another.
                    g.draw(
                        &g.reflection,
                        &[(&from_r, from_tex, from_kb), (&to_r, to_tex, to_kb)],
                        0.0,
                        (c, CUBE_REFLECTION),
                        unzoom,
                    );
                    g.draw(
                        &g.face,
                        &[(&from, from_tex, from_kb), (&to, to_tex, to_kb)],
                        0.0,
                        (0.0, 0.0),
                        unzoom,
                    );
                }
                Kind::Swap(g) => {
                    let [from, to, from_r, to_r] = swap_quads(progress);
                    glClearColor(0.0, 0.0, 0.0, 1.0);
                    glClear(GL_COLOR_BUFFER_BIT);
                    glActiveTexture(GL_TEXTURE0);
                    // The cards' reflections can overlap: upstream adds them.
                    glEnable(GL_BLEND);
                    glBlendFunc(GL_ONE, GL_ONE);
                    g.draw(
                        &g.reflection,
                        &[(&from_r, from_tex, from_kb), (&to_r, to_tex, to_kb)],
                        0.5,
                        (SWAP_REFLECT_C, SWAP_REFLECTION),
                        0.0,
                    );
                    glDisable(GL_BLEND);
                    // Upstream shows from over to until halfway, then to
                    // over from: the one on top is drawn last.
                    let from = (&from, from_tex, from_kb);
                    let to = (&to, to_tex, to_kb);
                    let order = if progress < 0.5 {
                        [to, from]
                    } else {
                        [from, to]
                    };
                    g.draw(&g.face, &order, 0.5, (0.0, 0.0), 0.0);
                }
            }
        }
    }
}

/// Binds a quad program over the pipeline's quad, both textures and the
/// per-frame uniforms.
///
/// # Safety
/// As [`TransitionProgram::draw`].
unsafe fn bind_quad(
    p: &QuadProgram,
    quad_vbo: GlUint,
    from_tex: GlUint,
    to_tex: GlUint,
    progress: f32,
    from_kb: UvWindow,
    to_kb: UvWindow,
) {
    // SAFETY: the caller's contract (as `draw`).
    unsafe {
        glUseProgram(p.program);
        glBindBuffer(GL_ARRAY_BUFFER, quad_vbo);
        let stride = 4 * 4;
        glVertexAttribPointer(p.a_pos, 2, GL_FLOAT, 0, stride, std::ptr::null());
        glEnableVertexAttribArray(p.a_pos);
        glVertexAttribPointer(p.a_uv, 2, GL_FLOAT, 0, stride, (2 * 4) as *const c_void);
        glEnableVertexAttribArray(p.a_uv);
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, from_tex);
        glActiveTexture(GL_TEXTURE1);
        glBindTexture(GL_TEXTURE_2D, to_tex);
        glActiveTexture(GL_TEXTURE0);
        glUniform1f(p.u_progress, progress);
        glUniform2f(p.u_from_scale, from_kb.0.0, from_kb.0.1);
        glUniform2f(p.u_from_offset, from_kb.1.0, from_kb.1.1);
        glUniform2f(p.u_to_scale, to_kb.0.0, to_kb.0.1);
        glUniform2f(p.u_to_offset, to_kb.1.0, to_kb.1.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raam_model::TransitionChoice;

    #[test]
    fn every_chosen_transition_has_a_program() {
        for choice in TransitionChoice::ALL {
            if let Some(name) = choice.shader_name() {
                assert!(
                    NAMES.contains(&name),
                    "{choice:?} names '{name}', which has no program"
                );
            }
        }
    }

    // Upstream's per-pixel maps, in f64, to check the corners against.

    fn xskew(p: (f64, f64), persp: f64, centre: f64) -> (f64, f64) {
        let x = p.0 + (1.0 - 2.0 * p.0) * centre;
        let y = (p.1 - 0.5 * (1.0 - persp) * x) / (1.0 + (persp - 1.0) * x);
        let d = (centre - 0.5).abs();
        let sign = if centre < 0.5 { 1.0 } else { -1.0 };
        let shift = if centre < 0.5 { 0.0 } else { 1.0 };
        ((x - (0.5 - d)) * (0.5 / d * sign) + shift, y)
    }

    fn cube_upstream(p: (f64, f64), progress: f64) -> ((f64, f64), (f64, f64)) {
        let persp = f64::from(CUBE_PERSP);
        let from = xskew(
            ((p.0 - progress) / (1.0 - progress), p.1),
            1.0 - progress * (1.0 - persp),
            0.0,
        );
        let to = xskew(
            (p.0 / progress, p.1),
            progress * progress + (1.0 - progress * progress) * persp,
            1.0,
        );
        (from, to)
    }

    fn swap_upstream(p: (f64, f64), progress: f64) -> ((f64, f64), (f64, f64)) {
        let (persp_c, depth) = (f64::from(SWAP_PERSPECTIVE), f64::from(SWAP_DEPTH));
        let size = 1.0 + (depth - 1.0) * progress;
        let persp = persp_c * progress;
        let from = (
            p.0 * size / (1.0 - persp_c * progress),
            (p.1 - 0.5) * size / (1.0 - size * persp * p.0) + 0.5,
        );
        let size = 1.0 + (depth - 1.0) * (1.0 - progress);
        let persp = persp_c * (1.0 - progress);
        let to = (
            (p.0 - 1.0) * size / (1.0 - persp_c * (1.0 - progress)) + 1.0,
            (p.1 - 0.5) * size / (1.0 - size * persp * (0.5 - p.0)) + 0.5,
        );
        (from, to)
    }

    /// The texture coordinate a corner gives (u, base + num / den).
    fn sampled(c: &Corner, base: f32) -> (f64, f64) {
        (f64::from(c.q.0), f64::from(base + c.q.1 / c.q.2))
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    fn p64(c: &Corner) -> (f64, f64) {
        (f64::from(c.p.0), f64::from(c.p.1))
    }

    #[test]
    fn cube_corners_sample_where_upstream_does() {
        for i in 1..20 {
            let progress = i as f32 / 20.0;
            let ([from, to, from_r, to_r], _) = cube_quads(progress);
            for (quads, side) in [([from, from_r], 0), ([to, to_r], 1)] {
                for c in quads.iter().flatten() {
                    let up = cube_upstream(p64(c), f64::from(progress));
                    let want = if side == 0 { up.0 } else { up.1 };
                    assert!(
                        close(sampled(c, 0.0), want),
                        "cube at {progress}: corner {c:?} samples {:?}, upstream {want:?}",
                        sampled(c, 0.0)
                    );
                }
            }
        }
    }

    #[test]
    fn swap_corners_sample_where_upstream_does() {
        for i in 1..20 {
            let progress = i as f32 / 20.0;
            let [from, to, from_r, to_r] = swap_quads(progress);
            for (quads, side) in [([from, from_r], 0), ([to, to_r], 1)] {
                for c in quads.iter().flatten() {
                    let up = swap_upstream(p64(c), f64::from(progress));
                    let want = if side == 0 { up.0 } else { up.1 };
                    assert!(
                        close(sampled(c, 0.5), want),
                        "swap at {progress}: corner {c:?} samples {:?}, upstream {want:?}",
                        sampled(c, 0.5)
                    );
                }
            }
        }
    }

    #[test]
    fn a_face_spans_its_whole_slide_and_a_reflection_upstreams_band() {
        let ([from, _, from_r, _], _) = cube_quads(0.4);
        let vs: Vec<f32> = from.iter().map(|c| c.q.1 / c.q.2).collect();
        assert!(
            vs.iter().all(|v| v.abs() < 1e-6 || (v - 1.0).abs() < 1e-6),
            "{vs:?}"
        );
        // A reflection covers exactly 0 < -1.2 v + c < 1.
        let c = -CUBE_FLOATING / 100.0;
        for corner in &from_r {
            let y = REFLECT_SCALE * (corner.q.1 / corner.q.2) + c;
            assert!(y.abs() < 1e-5 || (y - 1.0).abs() < 1e-5, "reflection y {y}");
        }
    }
}
