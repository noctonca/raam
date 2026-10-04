//! `?page=<name>`: a gallery page or a frame_ui preset, drawn as the
//! desktop preset host draws it (the root `raam` binary's `--page`), so
//! the two can be pixel-diffed, and so the page's design-system tab can
//! show the component kit. `theme=light|dark` picks the theme and
//! `backdrop=black|white` what the menu bar floats over (the desktop's
//! flat backdrops; its generated `still` photo stays desktop-only).
//!
//! Input goes to egui as a finger, as the Android host feeds it (the
//! controller's `push_touch`): no hover states the frame can't show.
//!
//! `shot=1` is a screenshot run, as the desktop's `--screenshot` is:
//! egui's clock is the pass count, a pass every animation frame, and the
//! passes stop once egui has settled, so the canvas holds still for the
//! test and its pixels can't depend on when it's read. The golden suite's
//! web diff (scripts/web-diff.py) shoots this way.

use raam_core::clock;
use raam_core::frame_ui::{self, SHOT_PASS, SHOT_SETTLED};
use raam_core::gallery::{self, Gallery};
use raam_core::gl::*;
use raam_core::painter::Painter;
use raam_core::theme::{self, Options, TextMode};
use std::time::Duration;

pub struct Preset {
    ctx: egui::Context,
    schemes: theme::Schemes,
    opts: Options,
    painter: Painter,
    page: String,
    /// The frame UI, when `page` named one of its presets.
    frame: Option<frame_ui::AppState>,
    gallery: Gallery,
    info: gallery::ProbeInfo,
    clear: f32,
    screen: (i32, i32),
    events: Vec<egui::Event>,
    due: Duration,
    passes: u32,
    /// `shot=1`: the virtual clock, and no pass once settled.
    shot: bool,
    settled: bool,
}

impl Preset {
    pub fn new(
        page: &str,
        query: &web_sys::UrlSearchParams,
        w: i32,
        h: i32,
    ) -> Result<Self, String> {
        let dark = query.get("theme").as_deref() != Some("light");
        let clear = match query.get("backdrop").as_deref() {
            Some("white") => 1.0,
            _ => 0.0,
        };
        let ctx = egui::Context::default();
        let opts = Options::default();
        let schemes = theme::Schemes::baked();
        theme::install_fonts(&ctx);
        theme::install(&ctx, schemes, opts);
        ctx.set_theme(if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        let mut gallery = Gallery::default();
        let frame = match gallery::Page::from_name(page) {
            Some(p) => {
                gallery.page = p;
                None
            }
            None => {
                let mut st = frame_ui::preset(page).ok_or(format!("no page named {page}"))?;
                // The Display page's segmented control agrees with the theme.
                st.settings.dark_theme = dark;
                Some(st)
            }
        };
        // SAFETY: start makes the canvas's context current before making a
        // Preset, on the page's one thread (without it the WebGL shim
        // panics rather than misbehaving).
        let mut painter = unsafe { Painter::new() };
        painter.text_boost = opts.text_mode == TextMode::Shader;
        let mut max_tex = 0;
        // SAFETY: the context is current, as for the painter;
        // GL_MAX_TEXTURE_SIZE is single-valued, so one GlInt is written.
        unsafe { glGetIntegerv(GL_MAX_TEXTURE_SIZE, &mut max_tex) };
        let info = gallery::ProbeInfo {
            text_mode: Some(opts.text_mode),
            subpixel: opts.subpixel_binning,
            ppp: 1.0,
            gl_max_texture: max_tex,
            ..Default::default()
        };
        log::info!(
            "page {page} ({}, {})",
            if dark { "dark" } else { "light" },
            if clear > 0.5 { "white" } else { "black" }
        );
        Ok(Self {
            ctx,
            schemes,
            opts,
            painter,
            page: page.to_string(),
            frame,
            gallery,
            info,
            clear,
            screen: (w, h),
            events: Vec::new(),
            due: Duration::ZERO,
            passes: 0,
            shot: query.get("shot").as_deref() == Some("1"),
            settled: false,
        })
    }

    /// A finger as egui on the frame sees it.
    pub fn touch(&mut self, phase: egui::TouchPhase, pos: egui::Pos2, id: i32) {
        let touch = egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(u64::from(id as u32)),
            phase,
            pos,
            force: Some(0.5),
        };
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        match phase {
            egui::TouchPhase::Start => {
                self.events
                    .extend([egui::Event::PointerMoved(pos), button(true), touch])
            }
            egui::TouchPhase::Move => self.events.extend([egui::Event::PointerMoved(pos), touch]),
            egui::TouchPhase::End => self.events.extend([
                egui::Event::PointerMoved(pos),
                button(false),
                touch,
                egui::Event::PointerGone,
            ]),
            egui::TouchPhase::Cancel => self.events.extend([touch, egui::Event::PointerGone]),
        }
    }

    /// Keys and text, already egui's.
    pub fn input(&mut self, events: impl IntoIterator<Item = egui::Event>) {
        self.events.extend(events);
    }

    pub fn frame(&mut self) {
        let now = if self.shot {
            SHOT_PASS * self.passes
        } else {
            clock::now()
        };
        if self.shot {
            // Held still for the test, until it taps.
            if self.settled && self.events.is_empty() {
                return;
            }
        } else if self.events.is_empty() && now < self.due {
            return;
        }
        let (w, h) = self.screen;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(w as f32, h as f32),
            )),
            time: Some(now.as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            focused: true,
            events: std::mem::take(&mut self.events),
            ..Default::default()
        };
        let mut reqs = Vec::new();
        let (g, info, frame) = (&mut self.gallery, &self.info, &mut self.frame);
        let mut out = self.ctx.run_ui(raw, |ui| match frame.as_mut() {
            Some(st) => frame_ui::draw(ui, st),
            None => reqs = gallery::draw(ui, g, info),
        });
        if let Some(st) = self.frame.as_mut() {
            frame_ui::stand_in(st);
            // The Display page's theme choice, applied as the controller
            // applies it on the frame.
            let want = if st.settings.dark_theme {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            };
            if self.ctx.theme() != want {
                self.ctx.set_theme(want);
                self.due = Duration::ZERO;
            }
        }
        self.passes += 1;
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                if *id == egui::TextureId::default() && delta.pos.is_none() {
                    let [aw, ah] = delta.image.size();
                    self.info.atlas = [aw, ah];
                    self.info.atlas_uploads += 1;
                }
                self.painter.set_texture(*id, delta);
            }
        }
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        self.painter.upload(&prims, out.pixels_per_point, w, h);
        for id in &out.textures_delta.free {
            self.painter.free_texture(*id);
        }
        // Applied above; epaint asserts on dropping an uncleared delta.
        out.textures_delta.clear();
        self.info.atlas_fill = self.ctx.fonts(|f| f.font_atlas_fill_ratio());
        // SAFETY: the canvas's context, current since start made it so, on
        // the page's one thread.
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glViewport(0, 0, w, h);
            glClearColor(self.clear, self.clear, self.clear, 1.0);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        self.painter.draw(out.pixels_per_point, w, h);
        let delay = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        self.settled = self.passes > 2 && delay >= SHOT_SETTLED;
        // The first passes lay out and then settle, as the desktop's do.
        self.due = if self.passes < 3 {
            now
        } else {
            now.checked_add(delay).unwrap_or(Duration::MAX)
        };
        self.apply(&reqs);
    }

    /// The gallery's host-only requests. The canvas is the frame's screen
    /// at ppp 1, so its ppp switch (a device probe) does nothing here.
    fn apply(&mut self, reqs: &[gallery::Request]) {
        for r in reqs {
            match *r {
                gallery::Request::Theme(t) => self.ctx.set_theme(t),
                gallery::Request::TextMode(m) => {
                    self.opts.text_mode = m;
                    theme::install(&self.ctx, self.schemes, self.opts);
                    self.painter.text_boost = m == TextMode::Shader;
                    self.info.text_mode = Some(m);
                }
                gallery::Request::Subpixel(b) => {
                    self.opts.subpixel_binning = b;
                    theme::install(&self.ctx, self.schemes, self.opts);
                    self.info.subpixel = b;
                }
                gallery::Request::Ppp(_) => {}
            }
        }
        if !reqs.is_empty() {
            self.due = Duration::ZERO;
        }
    }

    /// Settled once egui has nothing due soon: a test screenshots then.
    pub fn status(&self) -> String {
        let idle = if self.shot {
            self.settled
        } else {
            self.passes > 2
                && self
                    .due
                    .checked_sub(clock::now())
                    .is_some_and(|d| d >= SHOT_SETTLED)
        };
        serde_json::json!({ "mode": "page", "page": self.page, "idle": idle, "passes": self.passes })
            .to_string()
    }
}
