//! Ported gl-transitions (<https://github.com/gl-transitions/gl-transitions>)
//! shaders, adapted to this project's raw GLES2 pipeline. gl-transitions
//! itself only defines the *body* of each effect - `vec4 transition(vec2
//! uv)` plus whatever extra uniforms that effect declares - and assumes a
//! runtime (normally a JS/WebGL harness) supplies `progress`, `ratio`, and
//! `getFromColor`/`getToColor` helpers around it. Each `TRANSITION_*_SRC`
//! constant below is the real upstream `.glsl` file's body, fetched
//! verbatim from the gl-transitions repo (not rewritten), with only the
//! license/author header kept as-is since it's a plain GLSL comment. `wrap()`
//! prepends the fixed preamble supplying that runtime contract and appends a
//! fixed `main()` - the same body works unmodified whether it came from
//! gl-transitions.com's WebGL demo or this GLES2 pipeline.
use super::gl::*;
use std::ffi::c_void;

/// Shared vertex shader for every transition program - a plain fullscreen
/// quad, no per-photo UV transform needed here (each slide's own
/// blur+photo composite already baked the cover-crop/aspect-fit into its
/// FBO texture; the transition pass just blends two already-composited
/// full-frame textures).
// vUV flips the V axis (1.0 - aUV.y) rather than passing aUV straight
// through - `from`/`to` are always RenderTargets (a tile's composed
// `target`, or a scratch target a collage was drawn into), never a
// directly-uploaded texture, and composing a tile is itself one GL
// render-to-texture pass. Every render-to-texture hop toggles which v
// value ends up at the "top" of the content relative to the v=0-is-top
// convention this codebase uses for directly-uploaded JPEG textures
// (chosen so decoded rows go up as they are, with no CPU-side flip); a
// tile goes through exactly one such hop (slideshow.rs `compose_tile`),
// an odd count, so reading it back needs exactly one compensating flip.
// Found on the frame: without it the first screenshot came back
// upside-down, because GL writes NDC y=-1 (the bottom of whatever is
// drawn) to texel row 0 of an FBO's texture, the opposite of the upload
// convention. slideshow.rs's on-screen `draw_collage` needs the identical
// correction (`with_v_flip`).
pub const VS_SRC: &str = "attribute vec2 aPos; attribute vec2 aUV; varying vec2 vUV; \
     void main() { vUV = vec2(aUV.x, 1.0 - aUV.y); gl_Position = vec4(aPos, 0.0, 1.0); }";

// uFromScale/uFromOffset/uToScale/uToOffset carry each slide's own
// independent Ken Burns pan/zoom window - applied at the sampling
// boundary inside getFromColor/getToColor rather than to `vUV` itself, so
// every transition body above keeps working completely unmodified:
// cube/crosswarp/swap already distort the `uv` argument they pass into
// these two helpers (that distortion IS the transition effect), and this
// composes Ken Burns on top of whatever distorted coordinate each shader
// computes, exactly the way it would compose on top of an undistorted
// straight sample for `fade`/`directionalwipe`. `vUV` (and thus every
// `uv` derived from it) is already in the fixed-V-flip space this file's
// own `VS_SRC` applies, so no further flip composition is needed here -
// see slideshow.rs's `draw_collage` for the call site that *does* need to
// compose Ken Burns with that flip by hand (`with_v_flip`: it draws
// through the shared blit program, which has no flip of its own).
const PREAMBLE: &str = "precision mediump float;\nvarying vec2 vUV;\n\
     uniform sampler2D from;\nuniform sampler2D to;\n\
     uniform float progress;\nuniform float ratio;\n\
     uniform vec2 uFromScale;\nuniform vec2 uFromOffset;\n\
     uniform vec2 uToScale;\nuniform vec2 uToOffset;\n\
     vec4 getFromColor(vec2 uv) { return texture2D(from, uv * uFromScale + uFromOffset); }\n\
     vec4 getToColor(vec2 uv) { return texture2D(to, uv * uToScale + uToOffset); }\n";

const TAIL: &str = "\nvoid main() { gl_FragColor = transition(vUV); }\n";

/// Every transition's name, in the order [`TransitionProgram::all`] builds
/// them; each `TransitionChoice::shader_name` is one of these.
pub const NAMES: [&str; 5] = ["fade", "directionalwipe", "cube", "crosswarp", "swap"];

fn wrap(body: &str) -> String {
    format!("{PREAMBLE}{body}{TAIL}")
}

// --- fade.glsl --- Author: gre --- License: MIT ---
// The trivial baseline: a straight linear cross-dissolve between the two
// textures. No extra uniforms.
const TRANSITION_FADE_SRC: &str = "
// Author: gre
// License: MIT
vec4 transition (vec2 uv) {
  return mix(
    getFromColor(uv),
    getToColor(uv),
    progress
  );
}
";

// --- directionalwipe.glsl --- Author: gre --- License: MIT ---
// A hard-edged directional wipe with a soft (smoothstep) edge band.
const TRANSITION_DIRECTIONALWIPE_SRC: &str = "
// Author: gre
// License: MIT
uniform vec2 direction; // = vec2(1.0, -1.0)
uniform float smoothness; // = 0.5

const vec2 center = vec2(0.5, 0.5);

vec4 transition (vec2 uv) {
  vec2 v = normalize(direction);
  v /= abs(v.x)+abs(v.y);
  float d = v.x * center.x + v.y * center.y;
  float m =
    (1.0-step(progress, 0.0)) *
    (1.0 - smoothstep(-smoothness, 0.0, v.x * uv.x + v.y * uv.y - (d-0.5+progress*(1.+smoothness))));
  return mix(getFromColor(uv), getToColor(uv), m);
}
";

// --- cube.glsl --- Author: gre --- License: MIT ---
// A pseudo-3D perspective "page turn"/skew effect with a reflection.
const TRANSITION_CUBE_SRC: &str = "
// Author: gre
// License: MIT
uniform float persp; // = 0.7
uniform float unzoom; // = 0.3
uniform float reflection; // = 0.4
uniform float floating; // = 3.0

vec2 project (vec2 p) {
  return p * vec2(1.0, -1.2) + vec2(0.0, -floating/100.);
}

bool inBounds (vec2 p) {
  return all(lessThan(vec2(0.0), p)) && all(lessThan(p, vec2(1.0)));
}

vec4 bgColor (vec2 p, vec2 pfr, vec2 pto) {
  vec4 c = vec4(0.0, 0.0, 0.0, 1.0);
  pfr = project(pfr);
  if (inBounds(pfr)) {
    c += mix(vec4(0.0), getFromColor(pfr), reflection * mix(1.0, 0.0, pfr.y));
  }
  pto = project(pto);
  if (inBounds(pto)) {
    c += mix(vec4(0.0), getToColor(pto), reflection * mix(1.0, 0.0, pto.y));
  }
  return c;
}

vec2 xskew (vec2 p, float persp, float center) {
  float x = mix(p.x, 1.0-p.x, center);
  return (
    (
      vec2( x, (p.y - 0.5*(1.0-persp) * x) / (1.0+(persp-1.0)*x) )
      - vec2(0.5-distance(center, 0.5), 0.0)
    )
    * vec2(0.5 / distance(center, 0.5) * (center<0.5 ? 1.0 : -1.0), 1.0)
    + vec2(center<0.5 ? 0.0 : 1.0, 0.0)
  );
}

vec4 transition(vec2 op) {
  float uz = unzoom * 2.0*(0.5-distance(0.5, progress));
  vec2 p = -uz*0.5+(1.0+uz) * op;
  vec2 fromP = xskew(
    (p - vec2(progress, 0.0)) / vec2(1.0-progress, 1.0),
    1.0-mix(progress, 0.0, persp),
    0.0
  );
  vec2 toP = xskew(
    p / vec2(progress, 1.0),
    mix(pow(progress, 2.0), 1.0, persp),
    1.0
  );
  if (inBounds(fromP)) {
    return getFromColor(fromP);
  }
  else if (inBounds(toP)) {
    return getToColor(toP);
  }
  return bgColor(op, fromP, toP);
}
";

// --- pixelize.glsl does NOT compile on this device: its ARM Mali-400
// GLSL ES 1.00 compiler rejects two of its globals (`dist`, `squareSize`)
// for reading the `progress`/`steps` uniforms in their *global-scope*
// initializers - "S0012: Global variable initializer must be a constant
// expression", the driver's log as `link_program`'s panic prints it to
// logcat, labelled with the transition's name. That restriction is real
// GLES2/WebGL divergence, not a bug in this port: gl-transitions targets
// browsers' WebGL1 contexts (ANGLE on most desktops), which are more
// permissive here than this embedded driver. crosswarp.glsl takes its
// place, rather than forcing an incompatible shader to work.

// --- crosswarp.glsl --- Author: Eke Péter <peterekepeter@gmail.com> --- License: MIT ---
// A warped cross-dissolve: the wipe front is offset by each pixel's own x
// position (smoothstep-shaped), so the dissolve boundary sweeps left-to-
// right instead of fading uniformly like `fade`. No extra uniforms, and
// no global-scope uniform reads - the same shape of shader as `fade` but
// visually distinct, a safer 4th pick than another perspective-heavy one.
const TRANSITION_CROSSWARP_SRC: &str = "
// Author: Eke Péter <peterekepeter@gmail.com>
// License: MIT
vec4 transition(vec2 p) {
  float x = progress;
  x=smoothstep(.0,1.0,(x*2.0+p.x-1.0));
  return mix(getFromColor((p-.5)*(1.-x)+.5), getToColor((p-.5)*x+.5), x);
}
";

// --- swap.glsl --- Author: gre --- License: MIT ---
// A pseudo-3D "card swap": the outgoing photo slides/scales back and down
// while the incoming one slides in from the opposite side, each with its
// own reflection - same `project`/`inBounds`/`bgColor` shape as `cube`
// (same author, same style), so it was a safe bet to compile cleanly here
// too, and did on the first try.
const TRANSITION_SWAP_SRC: &str = "
// Author: gre
// License: MIT
uniform float reflection; // = 0.4
uniform float perspective; // = 0.2
uniform float depth; // = 3.0

const vec4 black = vec4(0.0, 0.0, 0.0, 1.0);
const vec2 boundMin = vec2(0.0, 0.0);
const vec2 boundMax = vec2(1.0, 1.0);

bool inBounds (vec2 p) {
  return all(lessThan(boundMin, p)) && all(lessThan(p, boundMax));
}

vec2 project (vec2 p) {
  return p * vec2(1.0, -1.2) + vec2(0.0, -0.02);
}

vec4 bgColor (vec2 p, vec2 pfr, vec2 pto) {
  vec4 c = black;
  pfr = project(pfr);
  if (inBounds(pfr)) {
    c += mix(black, getFromColor(pfr), reflection * mix(1.0, 0.0, pfr.y));
  }
  pto = project(pto);
  if (inBounds(pto)) {
    c += mix(black, getToColor(pto), reflection * mix(1.0, 0.0, pto.y));
  }
  return c;
}

vec4 transition (vec2 p) {
  vec2 pfr, pto = vec2(-1.);

  float size = mix(1.0, depth, progress);
  float persp = perspective * progress;
  pfr = (p + vec2(-0.0, -0.5)) * vec2(size/(1.0-perspective*progress), size/(1.0-size*persp*p.x)) + vec2(0.0, 0.5);

  size = mix(1.0, depth, 1.-progress);
  persp = perspective * (1.-progress);
  pto = (p + vec2(-1.0, -0.5)) * vec2(size/(1.0-perspective*(1.0-progress)), size/(1.0-size*persp*(0.5-p.x))) + vec2(1.0, 0.5);

  if (progress < 0.5) {
    if (inBounds(pfr)) {
      return getFromColor(pfr);
    }
    if (inBounds(pto)) {
      return getToColor(pto);
    }
  }
  if (inBounds(pto)) {
    return getToColor(pto);
  }
  if (inBounds(pfr)) {
    return getFromColor(pfr);
  }
  return bgColor(p, pfr, pto);
}
";

/// A linked transition program plus the uniform locations this pipeline
/// needs to drive every frame (`progress`) and the ones it only needs to
/// set once at creation (texture units, the effect's own tunables, at their
/// gl-transitions-documented defaults, kept fixed rather than exposed as
/// settings - each effect runs as upstream designed it, untuned).
pub struct TransitionProgram {
    pub name: &'static str,
    program: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    u_progress: GlInt,
    u_ratio: GlInt,
    u_from_scale: GlInt,
    u_from_offset: GlInt,
    u_to_scale: GlInt,
    u_to_offset: GlInt,
}

impl TransitionProgram {
    /// Links one transition shader and binds its two texture units;
    /// `extra` sets the shader's own fixed uniforms.
    ///
    /// # Safety
    /// Requires a current GL context.
    unsafe fn new(name: &'static str, body: &str, extra: impl FnOnce(GlUint)) -> Self {
        // SAFETY: the caller's contract: a current GL context.
        unsafe {
            let src = wrap(body);
            let program = link_program(name, VS_SRC, &src);
            glUseProgram(program);
            // Texture units are static for the life of the program - bind
            // once here, not per-draw.
            let u_from = uniform_loc(program, "from");
            let u_to = uniform_loc(program, "to");
            glUniform1i(u_from, 0);
            glUniform1i(u_to, 1);
            extra(program);
            Self {
                name,
                program,
                a_pos: attrib_loc(program, "aPos"),
                a_uv: attrib_loc(program, "aUV"),
                u_progress: uniform_loc(program, "progress"),
                u_ratio: uniform_loc(program, "ratio"),
                u_from_scale: uniform_loc(program, "uFromScale"),
                u_from_offset: uniform_loc(program, "uFromOffset"),
                u_to_scale: uniform_loc(program, "uToScale"),
                u_to_offset: uniform_loc(program, "uToOffset"),
            }
        }
    }

    /// Every transition, in [`NAMES`]'s order.
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn all() -> Vec<TransitionProgram> {
        // SAFETY: the caller's contract: a current GL context, under which
        // the closures run too (`new` calls them before returning).
        unsafe {
            vec![
                TransitionProgram::new(NAMES[0], TRANSITION_FADE_SRC, |_p| {}),
                TransitionProgram::new(NAMES[1], TRANSITION_DIRECTIONALWIPE_SRC, |p| {
                    glUniform2f(uniform_loc(p, "direction"), 1.0, -1.0);
                    glUniform1f(uniform_loc(p, "smoothness"), 0.5);
                }),
                TransitionProgram::new(NAMES[2], TRANSITION_CUBE_SRC, |p| {
                    glUniform1f(uniform_loc(p, "persp"), 0.7);
                    glUniform1f(uniform_loc(p, "unzoom"), 0.3);
                    glUniform1f(uniform_loc(p, "reflection"), 0.4);
                    glUniform1f(uniform_loc(p, "floating"), 3.0);
                }),
                TransitionProgram::new(NAMES[3], TRANSITION_CROSSWARP_SRC, |_p| {}),
                TransitionProgram::new(NAMES[4], TRANSITION_SWAP_SRC, |p| {
                    glUniform1f(uniform_loc(p, "reflection"), 0.4);
                    glUniform1f(uniform_loc(p, "perspective"), 0.2);
                    glUniform1f(uniform_loc(p, "depth"), 3.0);
                }),
            ]
        }
    }

    /// Draws the transition into whatever framebuffer/viewport is currently
    /// bound, sampling `from_tex`/`to_tex` (each a full-screen composited
    /// slide) at the given `progress` (0.0-1.0) and screen `ratio`.
    /// `from_kb`/`to_kb` are each slide's own independent Ken Burns
    /// (scale, offset) UV-window transform - the outgoing slide keeps
    /// animating on its own clock right up until this draw finishes it
    /// off, it does not freeze the moment the transition began.
    ///
    /// # Safety
    /// Requires a current GL context, `quad_vbo` holding the four
    /// pos+uv vertices and `quad_ibo` the six u16 indices the draw reads
    /// (the pipeline's quad), and both textures live.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn draw(
        &self,
        quad_vbo: GlUint,
        quad_ibo: GlUint,
        from_tex: GlUint,
        to_tex: GlUint,
        progress: f32,
        ratio: f32,
        from_kb: ((f32, f32), (f32, f32)),
        to_kb: ((f32, f32), (f32, f32)),
    ) {
        // SAFETY: the caller's contract: a current GL context and the
        // pipeline's quad bound, so the attribute offsets (within its
        // 16-byte vertices) and the six indices GL reads are in bounds.
        unsafe {
            glUseProgram(self.program);
            glBindBuffer(GL_ARRAY_BUFFER, quad_vbo);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, quad_ibo);
            let stride = 4 * 4;
            glVertexAttribPointer(self.a_pos, 2, GL_FLOAT, 0, stride, std::ptr::null());
            glEnableVertexAttribArray(self.a_pos);
            glVertexAttribPointer(self.a_uv, 2, GL_FLOAT, 0, stride, (2 * 4) as *const c_void);
            glEnableVertexAttribArray(self.a_uv);

            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, from_tex);
            glActiveTexture(GL_TEXTURE1);
            glBindTexture(GL_TEXTURE_2D, to_tex);
            glActiveTexture(GL_TEXTURE0);

            glUniform1f(self.u_progress, progress);
            glUniform1f(self.u_ratio, ratio);
            glUniform2f(self.u_from_scale, from_kb.0.0, from_kb.0.1);
            glUniform2f(self.u_from_offset, from_kb.1.0, from_kb.1.1);
            glUniform2f(self.u_to_scale, to_kb.0.0, to_kb.0.1);
            glUniform2f(self.u_to_offset, to_kb.1.0, to_kb.1.1);

            glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, std::ptr::null());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NAMES;
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
}
