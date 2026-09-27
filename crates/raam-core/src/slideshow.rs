//! 015's Ken Burns + gl-transitions pipeline with 019's chrome controls and
//! 020's Frameo Fill/Fit scaling, now showing Frameo-style collages (see
//! collage.rs for the layout rules) instead of one photo per slide.
//!
//! Each collage tile keeps two GPU textures and no CPU pixels:
//! - `source`: the photo copied down to the tile's cover scale when its
//!   preview lands. Recomposing a tile after a Fill/Fit change reads this,
//!   since `photo_tex` only ever holds the last upload.
//! - `target`: the tile composed exactly as 020 composes a slide (Fill crop
//!   with the face-derived centre, or Fit over a blurred or black
//!   background), at the tile's size.
//!
//! Each frame draws every tile's `target` through its own Ken Burns UV
//! window into the tile's rect (the viewport clips it, so motion never
//! crosses a separator), over a clear in the gap colour. A transition
//! renders both collages, still animating, into two screen-sized scratch
//! targets and feeds those to 015's transitions. A single-photo "collage"
//! skips all of that and runs 020's path unchanged.
//!
//! 027: a slide can be a video clip (always alone). Transitions still blend
//! two still slides: a clip's slide is composed from its real first frame
//! (decoded by a short-lived "probe" player, video.rs) over the blurred
//! first frame, so the transition in lands on exactly what starts playing.
//! When the transition ends, a live player starts, pre-rolls frame 0 (and
//! the audio), then plays, drawn every frame over the same static blurred
//! background. When it ends (or is stopped by Next or the timer) the slide
//! is recomposed from the frame on screen, the player is stopped, and the
//! transition out runs from that still. How long a clip stays is
//! `VideoPlayback`, Frameo's three choices.
use crate::clock;
use crate::collage::{self, Rect};
use crate::gl::*;
use crate::source::{Photo, Plan, TileSource, VideoClip};
use crate::transitions::TransitionProgram;
use crate::video::{ClipFrame, LiveCue, ProbePlayer, ProbeStatus, Tick, Video, VideoPlayer};
use raam_model::limits::{BLUR_WIDTH_PX, GPU_RETRY, HISTORY_LEN, TRANSITION_DURATION};
use raam_model::{FitBackground, GapColour, ScaleMode, VideoPlayback};
use std::cell::OnceCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::time::Duration;

const KB_ZOOM_WIDE: f32 = 1.0;
const KB_ZOOM_TIGHT: f32 = 1.15;
const KB_RANDOM_FOCAL_RADIUS: f32 = 0.15;
/// Half of Frameo's collage separator, 2dp.
const HALF_SEPARATOR_DP: f32 = 2.0;
/// Frameo's selection highlight: an 8dp stroke in its red accent
/// (#ff2a3a) over the tile the menu acts on.
const HIGHLIGHT_DP: f32 = 8.0;
const HIGHLIGHT_RGB: (f32, f32, f32) = (1.0, 42.0 / 255.0, 58.0 / 255.0);

type UvWindow = ((f32, f32), (f32, f32));
const IDENTITY_WINDOW: UvWindow = ((1.0, 1.0), (0.0, 0.0));

/// Slideshow time, which stops while paused.
pub struct SlideClock {
    accumulated: Duration,
    running_since: Option<Duration>,
}

impl SlideClock {
    fn new() -> Self {
        Self {
            accumulated: Duration::ZERO,
            running_since: Some(clock::now()),
        }
    }

    pub fn now(&self) -> Duration {
        self.accumulated + self.running_since.map_or(Duration::ZERO, clock::elapsed)
    }

    pub fn paused(&self) -> bool {
        self.running_since.is_none()
    }

    pub fn set_paused(&mut self, paused: bool) {
        if paused {
            if let Some(t) = self.running_since.take() {
                self.accumulated += clock::elapsed(t);
            }
        } else if self.running_since.is_none() {
            self.running_since = Some(clock::now());
        }
    }
}

pub struct SlideshowSettings {
    pub dwell: Duration,
    /// A `TransitionProgram::name`, or `None` to rotate through all of them.
    pub transition: Option<&'static str>,
    pub ken_burns: bool,
    /// Frameo's "Fill frame by default": the scaling of every photo without
    /// a per-photo choice.
    pub fill_by_default: bool,
    pub fit_background: FitBackground,
    /// What shows through the separators between collage tiles.
    pub gap_colour: GapColour,
    pub video_playback: VideoPlayback,
    /// Sound on, and at what volume (0..1).
    pub video_sound: bool,
    pub video_volume: f32,
}

/// Frameo draws its collage separators as a white background showing
/// through 2dp margins.
fn gap_rgb(colour: GapColour) -> (f32, f32, f32) {
    match colour {
        GapColour::Black => (0.0, 0.0, 0.0),
        GapColour::White => (1.0, 1.0, 1.0),
    }
}

/// What a slide was composed with. The background only matters for Fit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Composition {
    Fill,
    Fit(FitBackground),
}

/// What recomposing or refocusing a slide needs once its pixels are only in
/// `photo_tex` (the RGBA buffer is dropped after upload).
#[derive(Clone)]
struct PhotoMeta {
    asset_id: i64,
    /// The curation key (the photo's SHA-1).
    key: String,
    width: u32,
    height: u32,
    face_focal: Option<(f32, f32)>,
    fill_centre: (f32, f32),
}

impl PhotoMeta {
    fn clone_of(m: &PhotoMeta) -> Self {
        m.clone()
    }

    fn of(photo: &Photo) -> Self {
        Self {
            asset_id: photo.asset_id,
            key: photo.key.clone(),
            width: photo.width,
            height: photo.height,
            face_focal: photo.face_focal,
            fill_centre: photo.fill_centre,
        }
    }
}

struct KenBurns {
    born: Duration,
    duration: Duration,
    zoom_from: f32,
    zoom_to: f32,
    focal: (f32, f32),
}

impl KenBurns {
    fn new(focal: Option<(f32, f32)>, seed: u64, born: Duration, duration: Duration) -> Self {
        let zoom_in = pseudo_random_f32(seed) < 0.5;
        let (zoom_from, zoom_to) = if zoom_in {
            (KB_ZOOM_WIDE, KB_ZOOM_TIGHT)
        } else {
            (KB_ZOOM_TIGHT, KB_ZOOM_WIDE)
        };
        let focal = focal.unwrap_or_else(|| random_focal(seed.wrapping_add(1)));
        Self {
            born,
            duration,
            zoom_from,
            zoom_to,
            focal,
        }
    }

    /// Re-aims at a face after the slide under it was recomposed with a
    /// different scaling. Keeps the zoom and timing so the motion carries on.
    /// 027: no motion (a clip gets no Ken Burns).
    fn still(born: Duration) -> Self {
        Self {
            born,
            duration: Duration::from_secs(1),
            zoom_from: 1.0,
            zoom_to: 1.0,
            focal: (0.5, 0.5),
        }
    }

    fn refocus(&mut self, focal: Option<(f32, f32)>) {
        if let Some(f) = focal {
            self.focal = f;
        }
    }

    fn transform(&self, now: Duration) -> UvWindow {
        let t = (now.saturating_sub(self.born).as_secs_f32() / self.duration.as_secs_f32())
            .clamp(0.0, 1.0);
        let e = smoothstep(t);
        let zoom = lerp(self.zoom_from, self.zoom_to, e);
        let (cx, cy) = if self.zoom_from < self.zoom_to {
            (lerp(0.5, self.focal.0, e), lerp(0.5, self.focal.1, e))
        } else {
            (lerp(self.focal.0, 0.5, e), lerp(self.focal.1, 0.5, e))
        };
        let half = 0.5 / zoom;
        let cx = cx.clamp(half, 1.0 - half);
        let cy = cy.clamp(half, 1.0 - half);
        ((1.0 / zoom, 1.0 / zoom), (cx - half, cy - half))
    }
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn random_focal(seed: u64) -> (f32, f32) {
    let angle = pseudo_random_f32(seed) * std::f32::consts::TAU;
    (
        0.5 + angle.cos() * KB_RANDOM_FOCAL_RADIUS,
        0.5 + angle.sin() * KB_RANDOM_FOCAL_RADIUS,
    )
}

fn face_focal_to_target_uv(
    photo_w: u32,
    photo_h: u32,
    u: f32,
    v: f32,
    target_w: i32,
    target_h: i32,
) -> (f32, f32) {
    let (frac_w, frac_h) = fit_scale(photo_w, photo_h, target_w, target_h);
    (0.5 + (u - 0.5) * frac_w, 0.5 + (v - 0.5) * frac_h)
}

/// The same face point through the fill crop: photo UV -> slide UV via the
/// crop window, clamped because the face may sit in the cropped-off part.
fn face_focal_to_fill_uv(u: f32, v: f32, window: UvWindow) -> (f32, f32) {
    let ((su, sv), (ou, ov)) = window;
    (
        ((u - ou) / su).clamp(0.0, 1.0),
        ((v - ov) / sv).clamp(0.0, 1.0),
    )
}

fn kb_focal(
    meta: &PhotoMeta,
    comp: Composition,
    target_w: i32,
    target_h: i32,
) -> Option<(f32, f32)> {
    let (u, v) = meta.face_focal?;
    Some(match comp {
        Composition::Fit(_) => {
            face_focal_to_target_uv(meta.width, meta.height, u, v, target_w, target_h)
        }
        Composition::Fill => {
            let window = fill_uv(
                meta.width,
                meta.height,
                target_w as u32,
                target_h as u32,
                meta.fill_centre,
            );
            face_focal_to_fill_uv(u, v, window)
        }
    })
}

/// A tiny xorshift64 draw in `[0,1)`, seeded from the clock, a process-wide
/// draw counter (so same-reading draws still differ) and the caller's seed —
/// no wall-clock nanoseconds, per the core's portability rule.
fn pseudo_random_f32(seed: u64) -> f32 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static DRAWS: AtomicU64 = AtomicU64::new(0);
    let n = DRAWS.fetch_add(1, Ordering::Relaxed);
    let nanos = clock::now().as_nanos() as u64 ^ n.wrapping_mul(0x2545_F491_4F6C_DD1D);
    let mut x = (nanos ^ seed.wrapping_mul(0x9E3779B97F4A7C15)) | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x >> 11) as f32 / (1u64 << 53) as f32
}

#[rustfmt::skip]
const QUAD: [f32; 16] = [
    -1.0, -1.0,  0.0, 1.0,
     1.0, -1.0,  1.0, 1.0,
     1.0,  1.0,  1.0, 0.0,
    -1.0,  1.0,  0.0, 0.0,
];
const QUAD_INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];

const VS_SRC: &str = "attribute vec2 aPos; attribute vec2 aUV; \
     uniform vec2 uScale; uniform vec2 uOffset; \
     uniform vec2 uUVScale; uniform vec2 uUVOffset; \
     varying vec2 vUV; \
     void main() { \
         vUV = aUV * uUVScale + uUVOffset; \
         gl_Position = vec4(aPos * uScale + uOffset, 0.0, 1.0); \
     }";

const FS_BLIT_SRC: &str = "precision mediump float; varying vec2 vUV; uniform sampler2D uTex; \
     void main() { gl_FragColor = texture2D(uTex, vUV); }";

const FS_BLUR_SRC: &str = "precision mediump float; varying vec2 vUV; uniform sampler2D uTex; \
     uniform vec2 uTexel; \
     void main() { \
         vec4 c0 = texture2D(uTex, vUV - uTexel); \
         vec4 c1 = texture2D(uTex, vUV); \
         vec4 c2 = texture2D(uTex, vUV + uTexel); \
         gl_FragColor = (c0 + c1 + c2) / 3.0; \
     }";

/// Samples a decoded frame (`GL_TEXTURE_EXTERNAL_OES`) through its
/// transform (the lab's 011 shaders). Linked on the first clip frame drawn:
/// only a host with a real `VideoPlayer` ever needs it, and desktop GL has
/// no external textures to compile it against.
const VS_OES_SRC: &str = "attribute vec2 aPos; attribute vec2 aUV; \
     uniform vec2 uScale; uniform mat4 uTexMatrix; varying vec2 vUV; \
     void main() { \
         vUV = (uTexMatrix * vec4(aUV, 0.0, 1.0)).xy; \
         gl_Position = vec4(aPos * uScale, 0.0, 1.0); \
     }";

const FS_OES_SRC: &str = "#extension GL_OES_EGL_image_external : require\n\
     precision mediump float; varying vec2 vUV; uniform samplerExternalOES uTex; \
     void main() { gl_FragColor = texture2D(uTex, vUV); }";

/// (u, v) -> (u, 1 - v), column-major.
#[rustfmt::skip]
const FLIP_V: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0,
    0.0, -1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 1.0, 0.0, 1.0,
];

/// Column-major `a * b`.
fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

struct OesProgram {
    program: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    u_scale: GlInt,
    u_matrix: GlInt,
    u_tex: GlInt,
}

impl OesProgram {
    unsafe fn new() -> Self {
        unsafe {
            let program = link_program("oes", VS_OES_SRC, FS_OES_SRC);
            Self {
                program,
                a_pos: attrib_loc(program, "aPos"),
                a_uv: attrib_loc(program, "aUV"),
                u_scale: uniform_loc(program, "uScale"),
                u_matrix: uniform_loc(program, "uTexMatrix"),
                u_tex: uniform_loc(program, "uTex"),
            }
        }
    }

    /// The frame's matrix samples with (0,0) at the picture's bottom-left;
    /// the quad's UVs have v=1 at the bottom, so `upright` pre-flips them
    /// (`FLIP_V`) to draw it the right way up in the framebuffer. Without
    /// it the frame lands with row 0 = its top, which is the convention of
    /// an uploaded photo (and of a tile's `source`).
    unsafe fn draw(
        &self,
        quad_vbo: GlUint,
        quad_ibo: GlUint,
        frame: &ClipFrame,
        scale: (f32, f32),
        upright: bool,
    ) {
        let m = if upright {
            mat_mul(&frame.matrix, &FLIP_V)
        } else {
            frame.matrix
        };
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
            glBindTexture(GL_TEXTURE_EXTERNAL_OES, frame.texture);
            glUniform1i(self.u_tex, 0);
            glUniform2f(self.u_scale, scale.0, scale.1);
            glUniformMatrix4fv(self.u_matrix, 1, 0, m.as_ptr());
            glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, std::ptr::null());
        }
    }
}

struct QuadProgram {
    program: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    u_scale: GlInt,
    u_offset: GlInt,
    u_uv_scale: GlInt,
    u_uv_offset: GlInt,
    u_tex: GlInt,
    u_texel: GlInt,
}

impl QuadProgram {
    unsafe fn new(label: &str, fs_src: &str) -> Self {
        unsafe {
            let program = link_program(label, VS_SRC, fs_src);
            Self {
                program,
                a_pos: attrib_loc(program, "aPos"),
                a_uv: attrib_loc(program, "aUV"),
                u_scale: uniform_loc(program, "uScale"),
                u_offset: uniform_loc(program, "uOffset"),
                u_uv_scale: uniform_loc(program, "uUVScale"),
                u_uv_offset: uniform_loc(program, "uUVOffset"),
                u_tex: uniform_loc(program, "uTex"),
                u_texel: uniform_loc(program, "uTexel"),
            }
        }
    }

    unsafe fn bind(&self, quad_vbo: GlUint, quad_ibo: GlUint) {
        unsafe {
            glUseProgram(self.program);
            glBindBuffer(GL_ARRAY_BUFFER, quad_vbo);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, quad_ibo);
            let stride = 4 * 4;
            glVertexAttribPointer(self.a_pos, 2, GL_FLOAT, 0, stride, std::ptr::null());
            glEnableVertexAttribArray(self.a_pos);
            glVertexAttribPointer(self.a_uv, 2, GL_FLOAT, 0, stride, (2 * 4) as *const c_void);
            glEnableVertexAttribArray(self.a_uv);
            glUniform1i(self.u_tex, 0);
        }
    }

    unsafe fn draw(
        &self,
        texture: GlUint,
        scale: (f32, f32),
        uv: UvWindow,
        texel: Option<(f32, f32)>,
    ) {
        unsafe {
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, texture);
            glUniform2f(self.u_scale, scale.0, scale.1);
            glUniform2f(self.u_offset, 0.0, 0.0);
            glUniform2f(self.u_uv_scale, uv.0.0, uv.0.1);
            glUniform2f(self.u_uv_offset, uv.1.0, uv.1.1);
            if let Some((tx, ty)) = texel {
                glUniform2f(self.u_texel, tx, ty);
            }
            glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, std::ptr::null());
        }
    }
}

/// 027: what a clip's tile keeps besides its composed still.
struct VideoTile {
    clip: VideoClip,
    /// The first frame's blur, at the blur chain's small size (same row
    /// convention as `source`), drawn stretched behind the live picture.
    /// `None` = the black Fit background.
    bg: Option<RenderTarget>,
}

/// A clip's first frame being decoded, for a tile of the plan being built.
struct Probe<P: VideoPlayer> {
    slot: usize,
    rect: Rect,
    player: ProbePlayer<P::Clip>,
    meta: PhotoMeta,
    clip: VideoClip,
}

struct Tile {
    rect: Rect,
    source: RenderTarget,
    target: RenderTarget,
    meta: PhotoMeta,
    comp: Composition,
    kb: KenBurns,
    video: Option<VideoTile>,
}

impl Tile {
    unsafe fn destroy(self) {
        unsafe {
            self.source.destroy();
            self.target.destroy();
            if let Some(bg) = self.video.and_then(|v| v.bg) {
                bg.destroy();
            }
        }
    }

    fn bytes(&self) -> usize {
        let bg = self
            .video
            .as_ref()
            .and_then(|v| v.bg.as_ref())
            .map_or(0, |b| b.width * b.height);
        4 * (self.source.width * self.source.height + self.target.width * self.target.height + bg)
            as usize
    }
}

struct Collage {
    plan: Plan,
    tiles: Vec<Tile>,
}

impl Collage {
    unsafe fn destroy(self) {
        for t in self.tiles {
            unsafe { t.destroy() };
        }
    }

    fn is_single(&self) -> bool {
        self.plan.layout.is_none()
    }

    fn name(&self) -> &'static str {
        if self.video().is_some() {
            return "video";
        }
        self.plan
            .layout
            .map_or("single", |i| collage::LAYOUTS[i].name)
    }

    /// 027: the clip, if this slide is one (always a single tile).
    fn video(&self) -> Option<&VideoTile> {
        if self.is_single() {
            self.tiles.first().and_then(|t| t.video.as_ref())
        } else {
            None
        }
    }
}

/// A planned collage whose tiles are still arriving from the source.
struct Building<P: VideoPlayer> {
    plan: Plan,
    rects: Vec<Rect>,
    tiles: Vec<Option<Tile>>,
    started: Duration,
    compose_total: Duration,
    probe: Option<Probe<P>>,
}

enum State {
    Idle {
        dwell_start: Duration,
    },
    Transitioning {
        incoming: Collage,
        backwards: bool,
        start: Duration,
        transition_idx: usize,
    },
}

enum Skip {
    Next,
    Prev(Vec<i64>),
}

pub struct Pipeline<P: VideoPlayer> {
    quad_vbo: GlUint,
    quad_ibo: GlUint,
    blit: QuadProgram,
    blur: QuadProgram,
    /// Linked on the first clip frame drawn (`draw_clip_frame`).
    oes: OnceCell<OesProgram>,
    /// The video orchestration over the host's decoders (video.rs). Public
    /// because the pause on hide, the unplayable clips and the player's
    /// audio latency are host business.
    pub video: Video<P>,
    /// When the source may plan again after a GPU drop.
    gpu_retry_at: Option<Duration>,
    photo_tex: GlUint,
    blur_targets: [RenderTarget; 2],
    /// Screen-sized targets a multi-tile collage is rendered into for a
    /// transition. Made when a transition needs them and freed when it
    /// ends, so the 8 MB isn't held through the dwell.
    scratch: Option<[RenderTarget; 2]>,
    transitions: Vec<TransitionProgram>,
    next_transition_idx: usize,
    screen_w: i32,
    screen_h: i32,
    margin_px: i32,
    highlight_px: i32,
    current: Option<Collage>,
    building: Option<Building<P>>,
    ready: Option<Collage>,
    /// Per-photo Fill/Fit choices from the menu, by curation key (SHA-1).
    /// 024: loaded from and saved to `curation` by lib.rs; a photo with no
    /// entry follows "Fill frame by default".
    overrides: HashMap<String, ScaleMode>,
    /// Earlier collages' plans, oldest first. Going back pops from here and
    /// does not push the collage it leaves.
    history: VecDeque<Plan>,
    skip: Option<Skip>,
    /// The tile the menu acts on, in the displayed collage.
    selected: Option<usize>,
    /// The menu is open: draw the selection highlight and hold the timed
    /// advance, as Frameo does.
    menu_open: bool,
    kb_seed: u64,
    /// Hidden this session; plans still holding them are dropped.
    hidden: std::collections::HashSet<String>,
    state: State,
    pub clock: SlideClock,
    pub settings: SlideshowSettings,
    /// Free-memory reader for the log lines (host business: /proc on
    /// Android and Linux, nothing on wasm).
    mem_free_kb: fn() -> Option<u64>,
}

impl<P: VideoPlayer> Pipeline<P> {
    ///
    /// # Safety
    /// Requires a current GL context (it creates buffers, textures and programs).
    pub unsafe fn new(
        screen_w: i32,
        screen_h: i32,
        density_dpi: u32,
        settings: SlideshowSettings,
        player: P,
        mem_free_kb: fn() -> Option<u64>,
    ) -> Self {
        unsafe {
            let mut vbo = 0;
            glGenBuffers(1, &mut vbo);
            glBindBuffer(GL_ARRAY_BUFFER, vbo);
            glBufferData(
                GL_ARRAY_BUFFER,
                (QUAD.len() * 4) as isize,
                QUAD.as_ptr() as *const c_void,
                GL_STATIC_DRAW,
            );
            let mut ibo = 0;
            glGenBuffers(1, &mut ibo);
            glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, ibo);
            glBufferData(
                GL_ELEMENT_ARRAY_BUFFER,
                (QUAD_INDICES.len() * 2) as isize,
                QUAD_INDICES.as_ptr() as *const c_void,
                GL_STATIC_DRAW,
            );

            let mut photo_tex = 0;
            glGenTextures(1, &mut photo_tex);
            glBindTexture(GL_TEXTURE_2D, photo_tex);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);

            let blur_h = ((BLUR_WIDTH_PX as f32) * screen_h as f32 / screen_w as f32)
                .round()
                .max(1.0) as i32;
            let blur_targets = [
                RenderTarget::new(BLUR_WIDTH_PX, blur_h),
                RenderTarget::new(BLUR_WIDTH_PX, blur_h),
            ];

            let transitions = TransitionProgram::all();
            log::info!(
                "{} transitions linked: {}",
                transitions.len(),
                transitions
                    .iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            // Android's dp: px = dp * densityDpi / 160.
            let dp = |v: f32| ((v * density_dpi as f32 / 160.0) + 0.5) as i32;
            let margin_px = dp(HALF_SEPARATOR_DP).max(1);
            let highlight_px = dp(HIGHLIGHT_DP).max(1);
            log::info!(
                "density {density_dpi}dpi -> half separator {margin_px}px, highlight {highlight_px}px"
            );

            Self {
                quad_vbo: vbo,
                quad_ibo: ibo,
                blit: QuadProgram::new("blit", FS_BLIT_SRC),
                blur: QuadProgram::new("blur", FS_BLUR_SRC),
                oes: OnceCell::new(),
                video: Video::new(player),
                gpu_retry_at: None,
                photo_tex,
                blur_targets,
                scratch: None,
                transitions,
                next_transition_idx: 0,
                screen_w,
                screen_h,
                margin_px,
                highlight_px,
                current: None,
                building: None,
                ready: None,
                overrides: HashMap::new(),
                history: VecDeque::new(),
                skip: None,
                selected: None,
                menu_open: false,
                kb_seed: 0,
                hidden: Default::default(),
                state: State::Idle {
                    dwell_start: Duration::ZERO,
                },
                clock: SlideClock::new(),
                settings,
                mem_free_kb,
            }
        }
    }

    pub fn has_slide(&self) -> bool {
        self.current.is_some()
    }

    pub fn is_transitioning(&self) -> bool {
        matches!(self.state, State::Transitioning { .. })
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    fn default_scale(&self) -> ScaleMode {
        if self.settings.fill_by_default {
            ScaleMode::Fill
        } else {
            ScaleMode::Fit
        }
    }

    fn scale_mode(&self, key: &str) -> ScaleMode {
        self.overrides
            .get(key)
            .copied()
            .unwrap_or(self.default_scale())
    }

    pub fn set_overrides(&mut self, overrides: HashMap<String, ScaleMode>) {
        self.overrides = overrides;
    }

    fn wanted_comp(&self, key: &str) -> Composition {
        match self.scale_mode(key) {
            ScaleMode::Fill => Composition::Fill,
            ScaleMode::Fit => Composition::Fit(self.settings.fit_background),
        }
    }

    /// The collage on screen for the menu: the incoming one while a
    /// transition runs, since that is the one about to stay.
    fn displayed(&self) -> Option<&Collage> {
        match &self.state {
            State::Transitioning { incoming, .. } => Some(incoming),
            State::Idle { .. } => self.current.as_ref(),
        }
    }

    /// Frameo opens the menu on the photo that was tapped and highlights
    /// it inside a collage. Returns the tile index, if any.
    pub fn select_at(&mut self, x: f32, y: f32) -> Option<usize> {
        let hit = self
            .displayed()
            .and_then(|c| c.tiles.iter().position(|t| t.rect.contains(x, y)));
        // A tap on a separator selects the first tile, like the menu key's
        // "first media of the page".
        self.selected = Some(hit.unwrap_or(0));
        self.selected
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Frameo stops the timed advance while its menu shows and, when the
    /// menu goes, schedules the next advance a full interval later. Manual
    /// Next/Prev still work while it is open.
    pub fn set_menu_open(&mut self, open: bool) {
        if self.menu_open
            && !open
            && let State::Idle { dwell_start } = &mut self.state
        {
            *dwell_start = self.clock.now();
        }
        self.menu_open = open;
    }

    fn selected_tile(&self) -> Option<&Tile> {
        let c = self.displayed()?;
        c.tiles.get(
            self.selected
                .unwrap_or(0)
                .min(c.tiles.len().saturating_sub(1)),
        )
    }

    pub fn shown_scale_mode(&self) -> Option<ScaleMode> {
        self.selected_tile()
            .filter(|t| t.video.is_none())
            .map(|t| self.scale_mode(&t.meta.key))
    }

    /// 027: the selected tile is a clip (no Fill/Fit for it, as in Frameo:
    /// "'Fit to frame' / 'Fill frame' is not available for videos").
    pub fn shown_is_video(&self) -> bool {
        self.selected_tile().is_some_and(|t| t.video.is_some())
    }

    /// e.g. "3_2 · tile 2/3", for the status line.
    pub fn shown_layout(&self) -> String {
        match self.displayed() {
            Some(c) if c.video().is_some() => {
                let v = c.video().unwrap();
                let total = v.clip.info.duration_us as f64 / 1e6;
                match self.video.live_progress() {
                    Some((pos, audio)) if matches!(self.state, State::Idle { .. }) => format!(
                        "video {:.1} / {total:.1} s{}",
                        pos as f64 / 1e6 % total.max(0.001),
                        if audio { ", sound" } else { ", muted" }
                    ),
                    _ => format!(
                        "video {total:.1} s{}",
                        if self.video.done() { ", ended" } else { "" }
                    ),
                }
            }
            Some(c) if c.is_single() => "single photo".to_string(),
            Some(c) => format!(
                "collage {} · tile {}/{}",
                c.name(),
                self.selected.unwrap_or(0).min(c.tiles.len() - 1) + 1,
                c.tiles.len()
            ),
            None => "-".to_string(),
        }
    }

    /// Flips the selected tile's photo between Fill and Fit, remembered for
    /// that photo. Recomposed by the next `update`. Flipping back to the
    /// default drops the override (NULL in `curation`), so the photo
    /// follows the default again if it changes. Returns what to persist.
    pub fn toggle_shown_scale(&mut self) -> Option<(String, Option<ScaleMode>)> {
        let (key, id) = self
            .selected_tile()
            .filter(|t| t.video.is_none())
            .map(|t| (t.meta.key.clone(), t.meta.asset_id))?;
        let mode = match self.scale_mode(&key) {
            ScaleMode::Fill => ScaleMode::Fit,
            ScaleMode::Fit => ScaleMode::Fill,
        };
        let stored = (mode != self.default_scale()).then_some(mode);
        log::info!("per-photo scaling for asset {id} -> {mode:?} (override {stored:?})");
        match stored {
            Some(m) => self.overrides.insert(key.clone(), m),
            None => self.overrides.remove(&key),
        };
        Some((key, stored))
    }

    /// The selected tile's photo: (curation key, asset id).
    pub fn shown_photo(&self) -> Option<(String, i64)> {
        self.selected_tile()
            .map(|t| (t.meta.key.clone(), t.meta.asset_id))
    }

    /// A photo was hidden: drop it from the back history and from a built
    /// but unshown collage, and move on from the one on screen. The
    /// source's queue reload keeps it out of every plan after this; until
    /// that lands, a plan holding it is dropped as soon as it's built.
    pub fn forget(&mut self, key: &str, source: &dyn TileSource) {
        let before = self.history.len();
        self.history
            .retain(|p| p.assets.iter().all(|a| a.key != key));
        if self
            .ready
            .as_ref()
            .is_some_and(|r| r.plan.assets.iter().any(|a| a.key == key))
        {
            let r = self.ready.take().unwrap();
            log::info!(
                "dropping built plan {} (it holds the hidden photo)",
                r.plan.seq
            );
            unsafe { r.destroy() };
            source.consumed();
        }
        // A plan still being built is dropped once it's ready (see `pull`):
        // the source is waiting to hand over its tiles.
        self.hidden.insert(key.to_string());
        log::info!(
            "forgot hidden photo {key}: history {before} -> {}",
            self.history.len()
        );
        if self
            .displayed()
            .is_some_and(|c| c.plan.assets.iter().any(|a| a.key == key))
        {
            self.request_next();
        }
    }

    /// Whether an idle tile on screen no longer matches its settings.
    pub fn recompose_pending(&self) -> bool {
        match (&self.state, &self.current) {
            (State::Idle { .. }, Some(c)) => c
                .tiles
                .iter()
                .any(|t| t.video.is_none() && t.comp != self.wanted_comp(&t.meta.key)),
            _ => false,
        }
    }

    pub fn is_animating(&self) -> bool {
        self.is_transitioning()
            || (self.has_slide() && self.settings.ken_burns && !self.clock.paused())
            // 027: a first frame on its way, or a clip playing (not paused).
            || self.building.as_ref().is_some_and(|b| b.probe.is_some())
            || self.video.animating(self.video_paused())
    }

    fn video_paused(&self) -> bool {
        self.menu_open || self.clock.paused()
    }

    /// When the loop must run `update` again with nothing drawn meanwhile:
    /// the slide's end, or a clip's bookkeeping (`video_deadline`).
    pub fn next_deadline(&self) -> Option<Duration> {
        match (self.slide_deadline(), self.video_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 027: the video machinery's polling (video.rs), and the retry after
    /// a GPU drop.
    fn video_deadline(&self) -> Option<Duration> {
        let gpu = self
            .gpu_retry_at
            .map(|t| t.saturating_sub(clock::now()).max(Duration::from_millis(1)));
        match (self.video.deadline(), gpu) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn slide_deadline(&self) -> Option<Duration> {
        let State::Idle { dwell_start } = &self.state else {
            return None;
        };
        if !self.has_slide() || self.clock.paused() || self.skip.is_some() || self.menu_open {
            return None;
        }
        // 027: a clip's slide ends with the clip; only "then wait" has a
        // timer, started when it ended.
        if self.current.as_ref().is_some_and(|c| c.video().is_some())
            && !(self.video.done() && self.settings.video_playback == VideoPlayback::Wait)
        {
            return None;
        }
        let left = self
            .settings
            .dwell
            .saturating_sub(self.clock.now().saturating_sub(*dwell_start));
        (!left.is_zero()).then_some(left)
    }

    pub fn request_next(&mut self) {
        log::info!("next requested");
        self.skip = Some(Skip::Next);
    }

    pub fn request_prev(&mut self, source: &dyn TileSource) {
        let Some(plan) = self.history.pop_back() else {
            log::info!("prev requested but history is empty");
            return;
        };
        log::info!(
            "prev requested -> {} {:?} ({} left in history)",
            plan.layout.map_or("single", |i| collage::LAYOUTS[i].name),
            plan.ids(),
            self.history.len()
        );
        self.skip = Some(Skip::Prev(plan.ids()));
        source.request(plan);
    }

    /// Takes a newly parked plan and any tile for the one being built,
    /// composing each tile as it lands. 027: a clip's tile waits for its
    /// first frame (a probe player), polled here each frame.
    fn pull(&mut self, source: &dyn TileSource) {
        if let Some(plan) = source.take_plan() {
            // A new plan (a Prev request) during a GPU retry wait: that
            // wait's `consumed` would abandon it.
            self.gpu_retry_at = None;
            if let Some(b) = self.building.take() {
                log::info!("dropping unfinished plan {}", b.plan.seq);
                Self::destroy_building(b);
            }
            if let Some(r) = self.ready.take() {
                log::info!("dropping built but unshown plan {}", r.plan.seq);
                unsafe { r.destroy() };
            }
            let rects = match plan.layout {
                Some(i) => collage::LAYOUTS[i].rects(self.screen_w, self.screen_h, self.margin_px),
                None => vec![Rect {
                    x: 0,
                    y: 0,
                    w: self.screen_w,
                    h: self.screen_h,
                }],
            };
            log::info!(
                "building plan {} ({}): {:?}",
                plan.seq,
                plan.layout.map_or("single", |i| collage::LAYOUTS[i].name),
                rects
            );
            let n = rects.len();
            self.building = Some(Building {
                plan,
                rects,
                tiles: (0..n).map(|_| None).collect(),
                started: clock::now(),
                compose_total: Duration::ZERO,
                probe: None,
            });
        }
        if let Some(failed) = source.take_failed()
            && self.building.as_ref().is_some_and(|b| b.plan.seq == failed)
        {
            log::warn!("plan {failed} failed, dropping it");
            self.drop_building();
        }
        if self.building.as_ref().is_some_and(|b| b.probe.is_some()) {
            self.poll_probe(source);
            return;
        }
        let Some(seq) = self.building.as_ref().map(|b| b.plan.seq) else {
            return;
        };
        if source.tile_is_clip(seq) {
            if self.video.backing_off() {
                source.take_tile(seq);
                log::warn!(
                    "plan {seq} dropped: its clip comes while backing off after a decoder failure"
                );
                self.drop_building();
                source.consumed();
                return;
            }
            // One video decoder at a time: the probe waits for the playing
            // clip to end and for the last decoder's release (the still
            // after a clip holds a full interval, time enough to probe).
            if self.video.decoder_busy() {
                return;
            }
        }
        let Some(tile_photo) = source.take_tile(seq) else {
            return;
        };
        let rect = self.building.as_ref().unwrap().rects[tile_photo.slot];
        if let Some(clip) = tile_photo.photo.video.clone() {
            let meta = PhotoMeta::of(&tile_photo.photo);
            match self.video.open_probe(&clip, meta.asset_id) {
                Ok(player) => {
                    self.building.as_mut().unwrap().probe = Some(Probe {
                        slot: tile_photo.slot,
                        rect,
                        player,
                        meta,
                        clip,
                    });
                }
                Err(e) => {
                    // The source waits for `consumed` before it plans
                    // again, so a dropped plan must say so; the backoff
                    // keeps a host that can't open players at all (or a
                    // frame under pressure) from retrying clip after clip.
                    log::error!("clip {}: can't open a player: {e}", meta.asset_id);
                    self.video
                        .record_failure(&format!("clip {} open: {e}", meta.asset_id));
                    self.drop_building();
                    log::warn!("plan {seq} dropped");
                    source.consumed();
                }
            }
            return;
        }
        let start = clock::now();
        let tile = match self.make_tile(tile_photo.photo, rect) {
            Ok(tile) => tile,
            Err(e) => {
                self.drop_building_gpu(&e);
                return;
            }
        };
        let b = self.building.as_mut().unwrap();
        b.compose_total += clock::elapsed(start);
        b.tiles[tile_photo.slot] = Some(tile);
        self.finish_building(source);
    }

    unsafe fn destroy_building_tiles(b: &mut Building<P>) {
        for t in b.tiles.iter_mut().filter_map(Option::take) {
            unsafe { t.destroy() };
        }
    }

    fn destroy_building(mut b: Building<P>) {
        unsafe { Self::destroy_building_tiles(&mut b) };
        if let Some(p) = b.probe.take() {
            p.player.stop();
        }
    }

    /// The plan being built can't be (a fetch failed, or a clip wouldn't
    /// open): drop it, and a Prev request for it with it.
    fn drop_building(&mut self) {
        let Some(b) = self.building.take() else {
            return;
        };
        if matches!(&self.skip, Some(Skip::Prev(ids)) if *ids == b.plan.ids()) {
            log::warn!("that was the prev request, dropping it too");
            self.skip = None;
        }
        Self::destroy_building(b);
    }

    /// A tile couldn't get its render targets (GPU out of memory, seen at
    /// boot): the plan is dropped, rather than taking the render thread
    /// down, and the next one asked for after GPU_RETRY (`update`).
    fn drop_building_gpu(&mut self, why: &str) {
        let seq = self.building.as_ref().map(|b| b.plan.seq);
        log::error!(
            "plan {seq:?} dropped: {why}, MemFree {:?}KB; next plan in {GPU_RETRY:?}",
            (self.mem_free_kb)()
        );
        self.drop_building();
        self.gpu_retry_at = Some(clock::now() + GPU_RETRY);
    }

    /// A clip's first frame: once it's on the texture (video.rs polls the
    /// probe), the tile is composed from it and the probe player stopped.
    fn poll_probe(&mut self, source: &dyn TileSource) {
        let b = self.building.as_mut().unwrap();
        let probe = b.probe.as_mut().unwrap();
        let asset = probe.meta.asset_id;
        match self.video.poll_probe(&mut probe.player, asset) {
            ProbeStatus::Waiting => return,
            ProbeStatus::Failed => {
                let seq = b.plan.seq;
                self.drop_building();
                log::warn!("plan {seq} dropped");
                source.consumed();
                return;
            }
            ProbeStatus::Ready => {}
        }
        let probe = b.probe.take().unwrap();
        let t0 = clock::now();
        let tile = match self.make_video_tile(&probe) {
            Ok(tile) => tile,
            Err(e) => {
                probe.player.stop();
                self.drop_building_gpu(&e);
                // The decoder's buffers are the likely pressure: back off
                // from clips as after a decoder failure.
                self.video
                    .record_failure(&format!("clip {} tile: {e}", probe.meta.asset_id));
                return;
            }
        };
        let (w, h) = probe.clip.info.display();
        log::info!(
            "clip {} first frame composed ({}x{} shown, {} {}x{} rotation {}) {:.0} ms after opening, compose {:?}",
            probe.meta.asset_id,
            w,
            h,
            probe.clip.info.mime,
            probe.clip.info.coded_w,
            probe.clip.info.coded_h,
            probe.clip.info.rotation,
            probe.player.age().as_secs_f64() * 1000.0,
            clock::elapsed(t0)
        );
        probe.player.stop();
        let b = self.building.as_mut().unwrap();
        b.compose_total += clock::elapsed(t0);
        b.tiles[probe.slot] = Some(tile);
        self.finish_building(source);
    }

    fn finish_building(&mut self, source: &dyn TileSource) {
        if !self
            .building
            .as_ref()
            .is_some_and(|b| b.tiles.iter().all(Option::is_some))
        {
            return;
        }
        let b = self.building.take().unwrap();
        let tiles: Vec<Tile> = b.tiles.into_iter().map(Option::unwrap).collect();
        let bytes: usize = tiles.iter().map(Tile::bytes).sum();
        log::info!(
            "plan {} ready: {} tiles in {:?} (render-thread compose {:?} total), {:.1} MB of tile textures, MemFree {:?}KB",
            b.plan.seq,
            tiles.len(),
            clock::elapsed(b.started),
            b.compose_total,
            bytes as f64 / 1_048_576.0,
            (self.mem_free_kb)(),
        );
        if b.plan.assets.iter().any(|a| self.hidden.contains(&a.key)) {
            log::info!("dropping plan {}: it holds a hidden photo", b.plan.seq);
            for t in tiles {
                unsafe { t.destroy() };
            }
            source.consumed();
            return;
        }
        self.ready = Some(Collage {
            plan: b.plan,
            tiles,
        });
    }

    /// Undo of a hide.
    pub fn unforget(&mut self, key: &str) {
        self.hidden.remove(key);
    }

    pub fn update(&mut self, source: &dyn TileSource) {
        if self.gpu_retry_at.is_some_and(|t| clock::now() >= t) {
            self.gpu_retry_at = None;
            source.consumed();
        }
        self.video
            .watch_decoders(self.building.as_ref().is_some_and(|b| b.probe.is_some()));
        source.set_skip_videos(self.video.backing_off());
        self.pull(source);
        if self.current.is_none() {
            if let Some(c) = self.ready.take() {
                log::info!("bootstrapped first collage {} ({})", c.plan.seq, c.name());
                self.current = Some(c);
                self.state = State::Idle {
                    dwell_start: self.clock.now(),
                };
                source.consumed();
                self.start_live();
            }
            return;
        }
        if self.recompose_pending() {
            self.recompose_current();
        }
        self.tick_live();
        let State::Idle { dwell_start } = &self.state else {
            return;
        };
        let timed =
            !self.menu_open && self.clock.now().saturating_sub(*dwell_start) >= self.settings.dwell;
        let is_video = self.current.as_ref().is_some_and(|c| c.video().is_some());
        let due = self.skip.is_some()
            || if is_video {
                // 027: a clip's slide ends with the clip (Continue, and Loop
                // once its last pass is over); "then wait" holds its last
                // frame an interval more.
                !self.menu_open
                    && self.video.done()
                    && (self.settings.video_playback != VideoPlayback::Wait || timed)
            } else {
                timed
            };
        if !due {
            return;
        }
        let want = match &self.skip {
            Some(Skip::Prev(ids)) => Some(ids.clone()),
            _ => None,
        };
        let matches = self
            .ready
            .as_ref()
            .is_some_and(|r| want.as_ref().is_none_or(|w| *w == r.plan.ids()));
        if matches {
            let incoming = self.ready.take().unwrap();
            self.skip = None;
            source.consumed();
            // A clip cut short (Next, Prev, a hide): its still becomes the
            // frame it was on.
            self.finish_live();
            self.start_transition(incoming, want.is_some());
        }
    }

    fn new_kb(&mut self, meta: &PhotoMeta, comp: Composition, w: i32, h: i32) -> KenBurns {
        self.kb_seed = self.kb_seed.wrapping_add(1);
        KenBurns::new(
            kb_focal(meta, comp, w, h),
            self.kb_seed,
            self.clock.now(),
            self.settings.dwell + TRANSITION_DURATION,
        )
    }

    /// Uploads a tile's photo, keeps a cover-scale copy of it as the tile's
    /// `source`, composes the tile, and drops the CPU pixels. Fails when the
    /// GPU can't give a render target (out of memory), with nothing leaked.
    fn make_tile(&mut self, photo: Photo, rect: Rect) -> Result<Tile, String> {
        let mem_before = (self.mem_free_kb)();
        let t0 = clock::now();
        let meta = PhotoMeta::of(&photo);
        self.upload_photo(&photo);
        drop(photo);
        let t_upload = clock::elapsed(t0);
        // Cover scale for this tile, never above the preview's own size:
        // enough for Fill's crop and for Fit (which is smaller).
        let s = (rect.w as f32 / meta.width as f32)
            .max(rect.h as f32 / meta.height as f32)
            .min(1.0);
        let sw = ((meta.width as f32 * s).round() as i32).max(1);
        let sh = ((meta.height as f32 * s).round() as i32).max(1);
        // The source already box-halves previews to within 2x of this
        // size (fetch.rs `shrink_to_cover`), so this is normally one draw. If a
        // photo ever arrives larger, one bilinear draw at 3-4x would skip
        // texels and alias, so halve first (each halving draw averages 2x2
        // texels) until the last step is under 2x. Every draw
        // is flipped, which keeps the upload's orientation (row 0 = the
        // photo's top), so the compose code treats `source` like photo_tex.
        let mut halvings: Vec<RenderTarget> = Vec::new();
        let (mut cur_tex, mut cw, mut ch) = (self.photo_tex, meta.width as i32, meta.height as i32);
        unsafe {
            self.blit.bind(self.quad_vbo, self.quad_ibo);
            while cw / 2 >= sw && ch / 2 >= sh {
                let half = match RenderTarget::try_new((cw + 1) / 2, (ch + 1) / 2) {
                    Ok(half) => half,
                    Err(e) => {
                        for h in halvings.drain(..) {
                            h.destroy();
                        }
                        self.release_photo();
                        return Err(e);
                    }
                };
                half.bind_and_viewport();
                self.blit
                    .draw(cur_tex, (1.0, 1.0), with_v_flip(IDENTITY_WINDOW), None);
                (cur_tex, cw, ch) = (half.texture, half.width, half.height);
                halvings.push(half);
            }
        }
        let steps = halvings.len();
        let source = unsafe { RenderTarget::try_new(sw, sh) };
        unsafe {
            if let Ok(source) = &source {
                source.bind_and_viewport();
                self.blit
                    .draw(cur_tex, (1.0, 1.0), with_v_flip(IDENTITY_WINDOW), None);
            }
            for h in halvings.drain(..) {
                h.destroy();
            }
            self.release_photo();
        }
        let source = source?;
        let target = match unsafe { RenderTarget::try_new(rect.w, rect.h) } {
            Ok(target) => target,
            Err(e) => {
                unsafe { source.destroy() };
                return Err(e);
            }
        };
        let comp = self.wanted_comp(&meta.key);
        self.compose_tile(source.texture, &target, &meta, comp);
        let kb = self.new_kb(&meta, comp, rect.w, rect.h);
        log::info!(
            "tile {} {}x{} -> source {sw}x{sh} ({steps} halvings), tile {}x{} as {comp:?} in {:?} (upload {:?}) - MemFree {mem_before:?}KB -> {:?}KB",
            meta.asset_id,
            meta.width,
            meta.height,
            rect.w,
            rect.h,
            clock::elapsed(t0),
            t_upload,
            (self.mem_free_kb)(),
        );
        Ok(Tile {
            rect,
            source,
            target,
            meta,
            comp,
            kb,
            video: None,
        })
    }

    /// 027: a clip's tile, from the first frame on the probe's texture:
    /// `source` gets the frame (cover-scaled, row 0 = top, like an uploaded
    /// photo), the blur chain runs on it once for the background, and the
    /// tile is composed as Fit over it. Fails like `make_tile`.
    fn make_video_tile(&mut self, probe: &Probe<P>) -> Result<Tile, String> {
        let rect = probe.rect;
        let (w, h) = (probe.meta.width.max(1), probe.meta.height.max(1));
        let s = (rect.w as f32 / w as f32)
            .max(rect.h as f32 / h as f32)
            .min(1.0);
        let sw = ((w as f32 * s).round() as i32).max(1);
        let sh = ((h as f32 * s).round() as i32).max(1);
        let source = unsafe { RenderTarget::try_new(sw, sh) }?;
        unsafe {
            source.bind_and_viewport();
            self.draw_clip_frame(&self.video.probe_frame(&probe.player), (1.0, 1.0), false);
        }
        let comp = Composition::Fit(self.settings.fit_background);
        let bg = if self.settings.fit_background == FitBackground::Blurred {
            self.run_blur_chain(source.texture, w, h, rect.w, rect.h);
            let blurred = &self.blur_targets[1];
            match unsafe { RenderTarget::try_new(blurred.width, blurred.height) } {
                Ok(bg) => unsafe {
                    bg.bind_and_viewport();
                    self.blit.bind(self.quad_vbo, self.quad_ibo);
                    self.blit.draw(
                        blurred.texture,
                        (1.0, 1.0),
                        with_v_flip(IDENTITY_WINDOW),
                        None,
                    );
                    Some(bg)
                },
                Err(e) => {
                    unsafe { source.destroy() };
                    return Err(e);
                }
            }
        } else {
            None
        };
        let target = match unsafe { RenderTarget::try_new(rect.w, rect.h) } {
            Ok(target) => target,
            Err(e) => unsafe {
                source.destroy();
                if let Some(bg) = bg {
                    bg.destroy();
                }
                return Err(e);
            },
        };
        let meta = PhotoMeta {
            width: w,
            height: h,
            ..PhotoMeta::clone_of(&probe.meta)
        };
        self.compose_video_target(source.texture, &target, &meta, bg.as_ref());
        let kb = KenBurns::still(self.clock.now());
        Ok(Tile {
            rect,
            source,
            target,
            meta,
            comp,
            kb,
            video: Some(VideoTile {
                clip: probe.clip.clone(),
                bg,
            }),
        })
    }

    /// Draws a decoded frame into whatever framebuffer is bound. `upright`
    /// pre-flips for drawing to the screen; a tile's `source` copy draws
    /// with `false` (row 0 = the picture's top).
    ///
    /// # Safety
    /// Requires a current GL context.
    unsafe fn draw_clip_frame(&self, frame: &ClipFrame, scale: (f32, f32), upright: bool) {
        let oes = self.oes.get_or_init(|| unsafe { OesProgram::new() });
        unsafe { oes.draw(self.quad_vbo, self.quad_ibo, frame, scale, upright) };
    }

    /// A clip's still: the stored background (or black), then `source` fit.
    fn compose_video_target(
        &self,
        source: GlUint,
        target: &RenderTarget,
        meta: &PhotoMeta,
        bg: Option<&RenderTarget>,
    ) {
        unsafe {
            target.bind_and_viewport();
            self.blit.bind(self.quad_vbo, self.quad_ibo);
            match bg {
                Some(bg) => self
                    .blit
                    .draw(bg.texture, (1.0, 1.0), IDENTITY_WINDOW, None),
                None => {
                    glClearColor(0.0, 0.0, 0.0, 1.0);
                    glClear(GL_COLOR_BUFFER_BIT);
                }
            }
            self.blit.draw(
                source,
                fit_scale(meta.width, meta.height, target.width, target.height),
                IDENTITY_WINDOW,
                None,
            );
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glFinish();
        }
    }

    /// The current slide's clip and the video settings, as video.rs wants
    /// them. An associated fn so its borrow stays off `self.video`.
    fn live_cue<'a>(current: &'a Option<Collage>, settings: &SlideshowSettings) -> LiveCue<'a> {
        LiveCue {
            clip: current
                .as_ref()
                .and_then(|c| c.video().map(|v| (&v.clip, c.tiles[0].meta.asset_id))),
            sound: settings.video_sound.then_some(1.0),
            playback: settings.video_playback,
        }
    }

    /// 027: the clip on screen starts playing (its slide just became the
    /// current one).
    fn start_live(&mut self) {
        let cue = Self::live_cue(&self.current, &self.settings);
        self.video.start(&cue);
    }

    /// Keeps the playing clip in step (video.rs); when it ends or fails,
    /// restills its slide and restarts the dwell.
    fn tick_live(&mut self) {
        let paused = self.video_paused();
        let looping = self.settings.video_playback == VideoPlayback::Loop
            && match &self.state {
                State::Idle { dwell_start } => {
                    self.clock.now().saturating_sub(*dwell_start) < self.settings.dwell
                }
                _ => false,
            };
        let cue = Self::live_cue(&self.current, &self.settings);
        if self.video.tick(&cue, paused, looping) == Tick::Finished {
            self.finish_live();
            if let State::Idle { dwell_start } = &mut self.state {
                // "Then wait": a full interval from the end, as Frameo
                // restarts its timer when the clip completes.
                *dwell_start = self.clock.now();
            }
        }
    }

    /// Recomposes the clip's still from the frame on screen, then stops
    /// the player.
    fn finish_live(&mut self) {
        let Some(finished) = self.video.finish() else {
            return;
        };
        if let Some(frame) = finished.frame
            && let Some(mut current) = self.current.take()
        {
            let t0 = clock::now();
            let tile = &mut current.tiles[0];
            unsafe {
                tile.source.bind_and_viewport();
                self.draw_clip_frame(&frame, (1.0, 1.0), false);
            }
            let bg = tile.video.as_ref().and_then(|v| v.bg.as_ref());
            self.compose_video_target(tile.source.texture, &tile.target, &tile.meta, bg);
            log::info!(
                "still recomposed from the frame on screen in {:?}",
                clock::elapsed(t0)
            );
            self.current = Some(current);
        }
        finished.stop();
    }

    fn recompose_current(&mut self) {
        let Some(mut current) = self.current.take() else {
            return;
        };
        for tile in current.tiles.iter_mut() {
            let comp = self.wanted_comp(&tile.meta.key);
            if comp == tile.comp {
                continue;
            }
            let t0 = clock::now();
            self.compose_tile(tile.source.texture, &tile.target, &tile.meta, comp);
            tile.kb
                .refocus(kb_focal(&tile.meta, comp, tile.rect.w, tile.rect.h));
            log::info!(
                "recomposed {} as {comp:?} (was {:?}) in {:?}",
                tile.meta.asset_id,
                tile.comp,
                clock::elapsed(t0)
            );
            tile.comp = comp;
        }
        self.current = Some(current);
    }

    fn start_transition(&mut self, incoming: Collage, backwards: bool) {
        // Every tile's Ken Burns starts with the transition, as 020's slide did.
        let now = self.clock.now();
        let mut incoming = incoming;
        for t in incoming.tiles.iter_mut() {
            t.kb.born = now;
        }
        let transition_idx = match self.settings.transition {
            Some(name) => self
                .transitions
                .iter()
                .position(|t| t.name == name)
                .unwrap_or(0),
            None => {
                let i = self.next_transition_idx;
                self.next_transition_idx = (i + 1) % self.transitions.len();
                i
            }
        };
        log::info!(
            "starting transition '{}' to plan {} ({}, {} tiles: {:?}) backwards={backwards}",
            self.transitions[transition_idx].name,
            incoming.plan.seq,
            incoming.name(),
            incoming.tiles.len(),
            incoming
                .tiles
                .iter()
                .map(|t| format!("{}:{:?}", t.meta.asset_id, t.comp))
                .collect::<Vec<_>>(),
        );
        self.selected = None;
        self.state = State::Transitioning {
            incoming,
            backwards,
            start: clock::now(),
            transition_idx,
        };
    }

    fn upload_photo(&self, photo: &Photo) {
        unsafe {
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, self.photo_tex);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                photo.width as i32,
                photo.height as i32,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                photo.rgba.as_ptr() as *const c_void,
            );
        }
    }

    /// Shrinks `photo_tex` to 1x1 once `source` has been drawn from it, so
    /// the last preview (about 6 MB) isn't held until the next upload.
    fn release_photo(&self) {
        unsafe {
            glBindTexture(GL_TEXTURE_2D, self.photo_tex);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                1,
                1,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                [0u8; 4].as_ptr() as *const c_void,
            );
        }
    }

    /// 020's slide composition, into a tile-sized `target` from `source`
    /// (same orientation convention as `photo_tex`).
    fn compose_tile(
        &self,
        source: GlUint,
        target: &RenderTarget,
        meta: &PhotoMeta,
        comp: Composition,
    ) {
        match comp {
            Composition::Fill => unsafe {
                let window = fill_uv(
                    meta.width,
                    meta.height,
                    target.width as u32,
                    target.height as u32,
                    meta.fill_centre,
                );
                target.bind_and_viewport();
                self.blit.bind(self.quad_vbo, self.quad_ibo);
                self.blit.draw(source, (1.0, 1.0), window, None);
            },
            Composition::Fit(background) => {
                if background == FitBackground::Blurred {
                    self.run_blur_chain(
                        source,
                        meta.width,
                        meta.height,
                        target.width,
                        target.height,
                    );
                }
                unsafe {
                    target.bind_and_viewport();
                    self.blit.bind(self.quad_vbo, self.quad_ibo);
                    if background == FitBackground::Blurred {
                        self.blit.draw(
                            self.blur_targets[1].texture,
                            (1.0, 1.0),
                            IDENTITY_WINDOW,
                            None,
                        );
                    } else {
                        glClearColor(0.0, 0.0, 0.0, 1.0);
                        glClear(GL_COLOR_BUFFER_BIT);
                    }
                    let scale = fit_scale(meta.width, meta.height, target.width, target.height);
                    self.blit.draw(source, scale, IDENTITY_WINDOW, None);
                }
            }
        }
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glFinish();
        }
    }

    /// 020's blur chain, cover-cropped to the tile's aspect. The blur
    /// targets keep the screen's aspect; the tile draw stretches them back.
    fn run_blur_chain(&self, source: GlUint, photo_w: u32, photo_h: u32, tile_w: i32, tile_h: i32) {
        let [a, b] = &self.blur_targets;
        let cover = cover_uv(photo_w, photo_h, tile_w as u32, tile_h as u32);
        let texel_h = (1.0 / a.width as f32, 0.0);
        let texel_v = (0.0, 1.0 / a.height as f32);
        unsafe {
            self.blur.bind(self.quad_vbo, self.quad_ibo);
            a.bind_and_viewport();
            self.blur.draw(source, (1.0, 1.0), cover, Some(texel_h));
            b.bind_and_viewport();
            self.blur
                .draw(a.texture, (1.0, 1.0), IDENTITY_WINDOW, Some(texel_v));
            for _ in 0..2 {
                a.bind_and_viewport();
                self.blur
                    .draw(b.texture, (1.0, 1.0), IDENTITY_WINDOW, Some(texel_h));
                b.bind_and_viewport();
                self.blur
                    .draw(a.texture, (1.0, 1.0), IDENTITY_WINDOW, Some(texel_v));
            }
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
        }
    }

    fn kb_window(&self, kb: &KenBurns) -> UvWindow {
        if self.settings.ken_burns {
            kb.transform(self.clock.now())
        } else {
            IDENTITY_WINDOW
        }
    }

    /// Draws a collage into the bound framebuffer (the screen or a scratch
    /// target, both screen-sized): a clear in the gap colour, then each tile
    /// through its Ken Burns window with the viewport set to its rect.
    unsafe fn draw_collage(&self, c: &Collage, highlight: Option<usize>) {
        unsafe {
            glViewport(0, 0, self.screen_w, self.screen_h);
            if !c.is_single() {
                let (r, g, b) = gap_rgb(self.settings.gap_colour);
                glClearColor(r, g, b, 1.0);
                glClear(GL_COLOR_BUFFER_BIT);
            }
            self.blit.bind(self.quad_vbo, self.quad_ibo);
            for t in &c.tiles {
                let r = t.rect;
                glViewport(r.x, self.screen_h - r.y - r.h, r.w, r.h);
                self.blit.draw(
                    t.target.texture,
                    (1.0, 1.0),
                    with_v_flip(self.kb_window(&t.kb)),
                    None,
                );
            }
            glViewport(0, 0, self.screen_w, self.screen_h);
            if let Some(i) = highlight
                && let Some(t) = c.tiles.get(i)
            {
                let r = t.rect;
                let s = self.highlight_px.min(r.w / 2).min(r.h / 2);
                let y = self.screen_h - r.y - r.h;
                let (cr, cg, cb) = HIGHLIGHT_RGB;
                glClearColor(cr, cg, cb, 1.0);
                glEnable(GL_SCISSOR_TEST);
                for (x0, y0, w0, h0) in [
                    (r.x, y, r.w, s),
                    (r.x, y + r.h - s, r.w, s),
                    (r.x, y, s, r.h),
                    (r.x + r.w - s, y, s, r.h),
                ] {
                    glScissor(x0, y0, w0, h0);
                    glClear(GL_COLOR_BUFFER_BIT);
                }
                glDisable(GL_SCISSOR_TEST);
            }
        }
    }

    pub fn draw_frame(&mut self) {
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glViewport(0, 0, self.screen_w, self.screen_h);
            glClearColor(0.05, 0.06, 0.09, 1.0);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        let needs_scratch = match (&self.state, &self.current) {
            (State::Transitioning { incoming, .. }, Some(c)) => {
                !(c.is_single() && incoming.is_single())
            }
            _ => false,
        };
        // Without its scratch pair (GPU out of memory) a collage transition
        // becomes a cut to the incoming collage.
        let mut cut = false;
        if needs_scratch && self.scratch.is_none() {
            let pair = unsafe { RenderTarget::try_new(self.screen_w, self.screen_h) }.and_then(
                |a| match unsafe { RenderTarget::try_new(self.screen_w, self.screen_h) } {
                    Ok(b) => Ok([a, b]),
                    Err(e) => {
                        unsafe { a.destroy() };
                        Err(e)
                    }
                },
            );
            match pair {
                Ok(pair) => {
                    self.scratch = Some(pair);
                    log::info!(
                        "allocated two {}x{} transition scratch targets",
                        self.screen_w,
                        self.screen_h
                    );
                }
                Err(e) => {
                    log::error!(
                        "no transition scratch ({e}), MemFree {:?}KB: cutting instead",
                        (self.mem_free_kb)()
                    );
                    cut = true;
                }
            }
        }
        let Some(current) = self.current.as_ref() else {
            return;
        };

        let mut finished = false;
        match &self.state {
            State::Idle { .. } => unsafe {
                // 027: the playing clip, over its static background
                // (`live_frame` is None while `debug.video.show_still=1`
                // asks for the composed still instead).
                match (self.video.live_frame(), current.video()) {
                    (Some(frame), Some(v)) => {
                        let meta = &current.tiles[0].meta;
                        glViewport(0, 0, self.screen_w, self.screen_h);
                        match &v.bg {
                            Some(bg) => {
                                self.blit.bind(self.quad_vbo, self.quad_ibo);
                                self.blit
                                    .draw(bg.texture, (1.0, 1.0), IDENTITY_WINDOW, None);
                            }
                            None => {
                                glClearColor(0.0, 0.0, 0.0, 1.0);
                                glClear(GL_COLOR_BUFFER_BIT);
                            }
                        }
                        let scale =
                            fit_scale(meta.width, meta.height, self.screen_w, self.screen_h);
                        self.draw_clip_frame(&frame, scale, true);
                    }
                    _ => {
                        // Frameo only highlights inside a collage.
                        let highlight = if self.menu_open && !current.is_single() {
                            Some(self.selected.unwrap_or(0))
                        } else {
                            None
                        };
                        self.draw_collage(current, highlight);
                    }
                }
            },
            State::Transitioning { incoming, .. } if cut => {
                unsafe { self.draw_collage(incoming, None) };
                finished = true;
            }
            State::Transitioning {
                incoming,
                start,
                transition_idx,
                ..
            } => {
                // Transitions run on wall time even while paused, so a manual
                // next/prev while paused still completes and then holds.
                let elapsed = clock::elapsed(*start);
                let progress = (elapsed.as_secs_f32() / TRANSITION_DURATION.as_secs_f32()).min(1.0);
                let ratio = self.screen_w as f32 / self.screen_h as f32;
                let (from_tex, to_tex, from_kb, to_kb) =
                    if current.is_single() && incoming.is_single() {
                        // 020's path: the slides themselves, each with its Ken Burns.
                        let (a, b) = (&current.tiles[0], &incoming.tiles[0]);
                        (
                            a.target.texture,
                            b.target.texture,
                            in_shader_uv(self.kb_window(&a.kb)),
                            in_shader_uv(self.kb_window(&b.kb)),
                        )
                    } else {
                        let [sa, sb] = self.scratch.as_ref().unwrap();
                        unsafe {
                            glBindFramebuffer(GL_FRAMEBUFFER, sa.fbo);
                            self.draw_collage(current, None);
                            glBindFramebuffer(GL_FRAMEBUFFER, sb.fbo);
                            self.draw_collage(incoming, None);
                            glBindFramebuffer(GL_FRAMEBUFFER, 0);
                            glViewport(0, 0, self.screen_w, self.screen_h);
                        }
                        (
                            sa.texture,
                            sb.texture,
                            in_shader_uv(IDENTITY_WINDOW),
                            in_shader_uv(IDENTITY_WINDOW),
                        )
                    };
                unsafe {
                    self.transitions[*transition_idx].draw(
                        self.quad_vbo,
                        self.quad_ibo,
                        from_tex,
                        to_tex,
                        progress,
                        ratio,
                        from_kb,
                        to_kb,
                    );
                }
                finished = elapsed >= TRANSITION_DURATION;
            }
        }

        if finished {
            let dwell_start = self.clock.now();
            let old_state = std::mem::replace(&mut self.state, State::Idle { dwell_start });
            if let State::Transitioning {
                incoming,
                backwards,
                transition_idx,
                ..
            } = old_state
            {
                let old = self.current.replace(incoming).unwrap();
                if !backwards {
                    self.history.push_back(old.plan.clone());
                    if self.history.len() > HISTORY_LEN {
                        self.history.pop_front();
                    }
                }
                unsafe { old.destroy() };
                if let Some([a, b]) = self.scratch.take() {
                    unsafe {
                        a.destroy();
                        b.destroy();
                    }
                }
                log::info!(
                    "transition '{}' complete, history={}",
                    self.transitions[transition_idx].name,
                    self.history.len()
                );
            }
            self.start_live();
        }
    }
}

fn with_v_flip(kb: UvWindow) -> UvWindow {
    let (scale, offset) = kb;
    ((scale.0, -scale.1), (offset.0, 1.0 - offset.1))
}

fn in_shader_uv(kb: UvWindow) -> UvWindow {
    let (scale, offset) = kb;
    (scale, (offset.0, 1.0 - scale.1 - offset.1))
}

fn cover_uv(pw: u32, ph: u32, tw: u32, th: u32) -> UvWindow {
    let photo_aspect = pw as f32 / ph as f32;
    let target_aspect = tw as f32 / th as f32;
    if photo_aspect > target_aspect {
        let scale_u = target_aspect / photo_aspect;
        ((scale_u, 1.0), ((1.0 - scale_u) / 2.0, 0.0))
    } else {
        let scale_v = photo_aspect / target_aspect;
        ((1.0, scale_v), (0.0, (1.0 - scale_v) / 2.0))
    }
}

/// Frameo's fill crop: scale to cover, then offset only the overflowing
/// axis by `(screen - scaled) * centre`. As a UV window that is a start of
/// `(1 - visible) * centre`, so centre 0 keeps the left/top edge, 1 the
/// right/bottom, 0.5 is centred - the face is biased toward, not put in,
/// the middle.
fn fill_uv(pw: u32, ph: u32, tw: u32, th: u32, centre: (f32, f32)) -> UvWindow {
    let photo_aspect = pw as f32 / ph as f32;
    let target_aspect = tw as f32 / th as f32;
    if photo_aspect > target_aspect {
        let scale_u = target_aspect / photo_aspect;
        ((scale_u, 1.0), ((1.0 - scale_u) * centre.0, 0.0))
    } else {
        let scale_v = photo_aspect / target_aspect;
        ((1.0, scale_v), (0.0, (1.0 - scale_v) * centre.1))
    }
}

fn fit_scale(pw: u32, ph: u32, sw: i32, sh: i32) -> (f32, f32) {
    let scale = (sw as f32 / pw as f32).min(sh as f32 / ph as f32);
    (pw as f32 * scale / sw as f32, ph as f32 * scale / sh as f32)
}

// ---- the controller's view (app.rs) ------------------------------------

/// The App controller drives the pipeline through this narrow surface, so
/// controller tests run against a fake with no GL behind it.
impl<P: VideoPlayer> crate::app::Slideshow for Pipeline<P> {
    fn settings_mut(&mut self) -> &mut SlideshowSettings {
        &mut self.settings
    }
    fn set_clock_paused(&mut self, paused: bool) {
        self.clock.set_paused(paused);
    }
    fn pause_video(&mut self) {
        self.video.pause_now();
    }
    fn set_menu_open(&mut self, open: bool) {
        Pipeline::set_menu_open(self, open);
    }
    fn clear_selection(&mut self) {
        Pipeline::clear_selection(self);
    }
    fn select_at(&mut self, x: f32, y: f32) -> Option<usize> {
        Pipeline::select_at(self, x, y)
    }
    fn update(&mut self, source: &dyn TileSource) {
        Pipeline::update(self, source);
    }
    fn request_next(&mut self) {
        Pipeline::request_next(self);
    }
    fn request_prev(&mut self, source: &dyn TileSource) {
        Pipeline::request_prev(self, source);
    }
    fn toggle_shown_scale(&mut self) -> Option<(String, Option<ScaleMode>)> {
        Pipeline::toggle_shown_scale(self)
    }
    fn shown_photo(&self) -> Option<(String, i64)> {
        Pipeline::shown_photo(self)
    }
    fn forget(&mut self, key: &str, source: &dyn TileSource) {
        Pipeline::forget(self, key, source);
    }
    fn unforget(&mut self, key: &str) {
        Pipeline::unforget(self, key);
    }
    fn shown_scale_mode(&self) -> Option<ScaleMode> {
        Pipeline::shown_scale_mode(self)
    }
    fn shown_is_video(&self) -> bool {
        Pipeline::shown_is_video(self)
    }
    fn shown_layout(&self) -> String {
        Pipeline::shown_layout(self)
    }
    fn is_animating(&self) -> bool {
        Pipeline::is_animating(self)
    }
    fn recompose_pending(&self) -> bool {
        Pipeline::recompose_pending(self)
    }
    fn next_deadline(&self) -> Option<Duration> {
        Pipeline::next_deadline(self)
    }
}
