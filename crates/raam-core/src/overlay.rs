//! The always-on clock/date/weather overlay, in two styles (both measured
//! from existing frame apps), each anchorable to any screen corner (style
//! and corner are separate settings):
//! - Simple: Frameo's clock, matched to stock Frameo on the frame within a
//!   few px: an 80sp bold clock, then "date  icon temp" at 40sp bold,
//!   white, soft dark shadow, no panel, 16dp padding.
//! - Detailed: the ImmichFrame web client's widget - small date, big bold
//!   time, then "icon place, temp" and a short description, warm cream.
//!
//! Text is the baked-blur shadow atlas (atlas.rs), not egui, so a closed
//! menu still never runs egui. Geometry is rebuilt into one persistent VBO
//! only when the displayed text changes (a minute tick, a weather update,
//! a setting); every other frame is just the bind plus 2 draw calls per
//! atlas.
use crate::atlas::{FontAtlas, Shadow};
use crate::clock;
use crate::gl::*;
use crate::weather_icons;
use raam_model::{ClockStyle, Corner, LocalTime};
use std::ffi::c_void;

const ROBOTO_REGULAR: &[u8] = include_bytes!("../assets/Roboto-Regular.ttf");
const ROBOTO_BOLD: &[u8] = include_bytes!("../assets/Roboto-Bold.ttf");

/// What the overlay shows; the App controller (app.rs) formats it with
/// `content` from the local time and the weather snapshot.
#[derive(Clone, PartialEq, Debug)]
pub struct Content {
    pub time: String,
    /// "Sep 23".
    pub short_date: String,
    /// "Wed, Sep 23".
    pub long_date: String,
    pub weather: Option<Weather>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Weather {
    pub icon: char,
    /// Rounded: "18°".
    pub temp: String,
    pub description: &'static str,
    pub city: Option<String>,
}

type Rgba = (f32, f32, f32, f32);

const WHITE: Rgba = (1.0, 1.0, 1.0, 1.0);
/// The Simple style's soft shadow, #303030.
const SIMPLE_SHADOW: Rgba = (0.19, 0.19, 0.19, 1.0);
const SIMPLE_SHADOW_OFFSET: (f32, f32) = (0.0, 2.0);
/// Tuned on the frame: warm cream text, black shadow at 0.78, 3px offset.
const CREAM: Rgba = (0.96, 0.87, 0.70, 1.0);
const DETAILED_SHADOW: Rgba = (0.0, 0.0, 0.0, 0.78);
const DETAILED_SHADOW_OFFSET: (f32, f32) = (3.0, 3.0);

const VS_SRC: &str = "attribute vec2 aPos; attribute vec2 aUV; \
     uniform vec2 uScreen; varying vec2 vUV; \
     void main() { \
         vUV = aUV; \
         gl_Position = vec4(aPos.x / uScreen.x * 2.0 - 1.0, 1.0 - aPos.y / uScreen.y * 2.0, 0.0, 1.0); \
     }";
const FS_SRC: &str = "precision mediump float; varying vec2 vUV; \
     uniform sampler2D uTex; uniform vec4 uColor; \
     void main() { gl_FragColor = vec4(uColor.rgb, uColor.a * texture2D(uTex, vUV).a); }";

struct Draw {
    texture: GlUint,
    color: Rgba,
    first: i32,
    count: i32,
}

pub struct ClockOverlay {
    program: GlUint,
    a_pos: GlUint,
    a_uv: GlUint,
    u_screen: GlInt,
    u_color: GlInt,
    u_tex: GlInt,
    vbo: GlUint,
    /// Bold 80: both styles' clock.
    clock: FontAtlas,
    /// Bold 40 + icons: the Simple style's date/weather row.
    date_row: FontAtlas,
    /// Regular 26: the Detailed style's date and description.
    small: FontAtlas,
    /// Regular 32 + icons: the Detailed style's weather line.
    weather: FontAtlas,
    draws: Vec<Draw>,
    key: Option<(ClockStyle, Corner, Content, i32, i32)>,
    pub rebuilds: u32,
}

impl ClockOverlay {
    ///
    /// # Safety
    /// Requires a current GL context.
    pub unsafe fn new() -> Self {
        let t0 = clock::now();
        let regular =
            fontdue::Font::from_bytes(ROBOTO_REGULAR, fontdue::FontSettings::default()).unwrap();
        let bold =
            fontdue::Font::from_bytes(ROBOTO_BOLD, fontdue::FontSettings::default()).unwrap();
        // Roboto's cap height is 0.711 em; icons are centred on it.
        let icon_set = |px: f32, scale: f32, gap: f32| {
            weather_icons::all((px * scale).round() as usize, px * 0.711, gap)
        };
        let clock = FontAtlas::build(
            &bold,
            80.0,
            Shadow {
                bleed: 12,
                radius: 5,
                gain: 1.4,
            },
            Vec::new(),
        );
        let date_row = FontAtlas::build(
            &bold,
            40.0,
            Shadow {
                bleed: 8,
                radius: 3,
                gain: 1.5,
            },
            icon_set(40.0, 1.15, 6.0),
        );
        let small = FontAtlas::build(
            &regular,
            26.0,
            Shadow {
                bleed: 6,
                radius: 3,
                gain: 2.0,
            },
            Vec::new(),
        );
        let weather = FontAtlas::build(
            &regular,
            32.0,
            Shadow {
                bleed: 6,
                radius: 3,
                gain: 2.0,
            },
            icon_set(32.0, 1.15, 8.0),
        );
        unsafe {
            let program = link_program("clock", VS_SRC, FS_SRC);
            let mut vbo = 0;
            glGenBuffers(1, &mut vbo);
            log::info!(
                "overlay ready (4 atlases) in {:.1}ms",
                clock::elapsed(t0).as_secs_f64() * 1000.0
            );
            Self {
                program,
                a_pos: attrib_loc(program, "aPos"),
                a_uv: attrib_loc(program, "aUV"),
                u_screen: uniform_loc(program, "uScreen"),
                u_color: uniform_loc(program, "uColor"),
                u_tex: uniform_loc(program, "uTex"),
                vbo,
                clock,
                date_row,
                small,
                weather,
                draws: Vec::new(),
                key: None,
                rebuilds: 0,
            }
        }
    }

    /// Rebuilds the VBO if `style`/`corner`/`content`/screen differ from
    /// the last build. Returns true if it rebuilt.
    pub fn update(
        &mut self,
        style: ClockStyle,
        corner: Corner,
        content: &Content,
        sw: i32,
        sh: i32,
    ) -> bool {
        if self.key.as_ref().is_some_and(|(s, k, c, w, h)| {
            *s == style && *k == corner && c == content && *w == sw && *h == sh
        }) {
            return false;
        }
        let mut verts = Vec::new();
        let mut draws = Vec::new();
        let lines = match style {
            ClockStyle::Off => Vec::new(),
            ClockStyle::Simple => self.simple_lines(content, corner, sw as f32, sh as f32),
            ClockStyle::Detailed => self.detailed_lines(content, corner, sw as f32, sh as f32),
        };
        let (shadow_color, (dx, dy), color) = match style {
            ClockStyle::Detailed => (DETAILED_SHADOW, DETAILED_SHADOW_OFFSET, CREAM),
            ClockStyle::Simple | ClockStyle::Off => (SIMPLE_SHADOW, SIMPLE_SHADOW_OFFSET, WHITE),
        };
        // All shadows first, then all text, so no line's shadow darkens the
        // line above it. One draw per (atlas, pass).
        for shadow in [true, false] {
            for atlas in [&self.clock, &self.date_row, &self.small, &self.weather] {
                let first = (verts.len() / 4) as i32;
                for (a, text, x, baseline) in &lines {
                    if std::ptr::eq(*a, atlas) {
                        let (x, y) = if shadow {
                            (x + dx, baseline + dy)
                        } else {
                            (*x, *baseline)
                        };
                        atlas.append_line(&mut verts, text, x, y, shadow);
                    }
                }
                let count = (verts.len() / 4) as i32 - first;
                if count > 0 {
                    let color = if shadow { shadow_color } else { color };
                    draws.push(Draw {
                        texture: atlas.texture,
                        color,
                        first,
                        count,
                    });
                }
            }
        }
        drop(lines);
        self.draws = draws;
        if !verts.is_empty() {
            unsafe {
                glBindBuffer(GL_ARRAY_BUFFER, self.vbo);
                glBufferData(
                    GL_ARRAY_BUFFER,
                    (verts.len() * 4) as isize,
                    verts.as_ptr() as *const c_void,
                    GL_STATIC_DRAW,
                );
            }
        }
        self.key = Some((style, corner, content.clone(), sw, sh));
        self.rebuilds += 1;
        true
    }

    /// The Simple style, top-anchored, then shifted and aligned for the
    /// corner: the clock, then "date  icon temp" pulled 16dp up into its
    /// box, as Frameo's row sits.
    fn simple_lines(
        &self,
        c: &Content,
        corner: Corner,
        sw: f32,
        sh: f32,
    ) -> Vec<(&FontAtlas, String, f32, f32)> {
        let pad = 16.0;
        let mut out = Vec::new();
        let clock_baseline = pad + self.clock.ascent;
        out.push((&self.clock, c.time.clone(), 0.0, clock_baseline));
        let row_top = pad + self.clock.ascent + self.clock.descent - 16.0;
        let row_baseline = row_top + self.date_row.ascent;
        let mut row = c.short_date.clone();
        if let Some(w) = &c.weather {
            row.push_str(&format!(" {}{}", w.icon, w.temp));
        }
        out.push((&self.date_row, row, 0.0, row_baseline));
        let block_bottom = row_baseline + self.date_row.descent;
        align_block(&mut out, corner, pad, 2.0, sw, sh, block_bottom);
        out
    }

    /// The Detailed style, top-anchored (long date, time, weather line,
    /// description), then shifted and aligned for the corner.
    fn detailed_lines(
        &self,
        c: &Content,
        corner: Corner,
        sw: f32,
        sh: f32,
    ) -> Vec<(&FontAtlas, String, f32, f32)> {
        let pad = 32.0;
        let mut y = pad;
        let mut out = Vec::new();
        fn push_down<'a>(
            out: &mut Vec<(&'a FontAtlas, String, f32, f32)>,
            atlas: &'a FontAtlas,
            text: String,
            y: &mut f32,
        ) {
            let baseline = *y + atlas.ascent;
            *y = baseline + atlas.descent;
            out.push((atlas, text, 0.0, baseline));
        }
        push_down(&mut out, &self.small, c.long_date.clone(), &mut y);
        push_down(&mut out, &self.clock, c.time.clone(), &mut y);
        if let Some(w) = &c.weather {
            let line = match &w.city {
                Some(city) => format!("{}{}, {}", w.icon, city, w.temp),
                None => format!("{}{}", w.icon, w.temp),
            };
            push_down(&mut out, &self.weather, line, &mut y);
            push_down(&mut out, &self.small, w.description.to_string(), &mut y);
        }
        align_block(&mut out, corner, pad, 0.0, sw, sh, y);
        out
    }

    pub fn draw(&self, sw: i32, sh: i32) {
        if self.draws.is_empty() {
            return;
        }
        unsafe {
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            glViewport(0, 0, sw, sh);
            glEnable(GL_BLEND);
            glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            glUseProgram(self.program);
            glBindBuffer(GL_ARRAY_BUFFER, self.vbo);
            glVertexAttribPointer(self.a_pos, 2, GL_FLOAT, 0, 16, std::ptr::null());
            glEnableVertexAttribArray(self.a_pos);
            glVertexAttribPointer(self.a_uv, 2, GL_FLOAT, 0, 16, 8 as *const c_void);
            glEnableVertexAttribArray(self.a_uv);
            glActiveTexture(GL_TEXTURE0);
            glUniform1i(self.u_tex, 0);
            glUniform2f(self.u_screen, sw as f32, sh as f32);
            for d in &self.draws {
                glBindTexture(GL_TEXTURE_2D, d.texture);
                glUniform4f(self.u_color, d.color.0, d.color.1, d.color.2, d.color.3);
                glDrawArrays(GL_TRIANGLES, d.first, d.count);
            }
            glDisableVertexAttribArray(self.a_pos);
            glDisableVertexAttribArray(self.a_uv);
            glDisable(GL_BLEND);
        }
    }
}

/// Anchors a top-left-built block of lines to `corner`: right corners
/// right-align every line at `sw - pad - inset`, left corners start each
/// at `pad + inset`; bottom corners shift every baseline down so the
/// block's bottom sits at `sh - pad`.
fn align_block(
    lines: &mut [(&FontAtlas, String, f32, f32)],
    corner: Corner,
    pad: f32,
    inset: f32,
    sw: f32,
    sh: f32,
    block_bottom: f32,
) {
    let dy = if corner.is_top() {
        0.0
    } else {
        sh - pad - block_bottom
    };
    for (atlas, text, x, baseline) in lines.iter_mut() {
        *x = if corner.is_left() {
            pad + inset
        } else {
            sw - pad - inset - atlas.text_width(text)
        };
        *baseline += dy;
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub fn content(style: ClockStyle, h24: bool, t: &LocalTime, weather: Option<Weather>) -> Content {
    let time = if h24 {
        format!("{:02}:{:02}", t.hour, t.min)
    } else {
        let h = if t.hour % 12 == 0 { 12 } else { t.hour % 12 };
        // The Simple style drops am/pm from its 12h clock; Detailed keeps it.
        match style {
            ClockStyle::Detailed => {
                format!("{h}:{:02} {}", t.min, if t.hour < 12 { "AM" } else { "PM" })
            }
            ClockStyle::Simple | ClockStyle::Off => format!("{h}:{:02}", t.min),
        }
    };
    // A month or weekday out of range is a clock bug, not a time to show.
    assert!(
        (0..12).contains(&t.mon),
        "LocalTime month {} out of range",
        t.mon
    );
    assert!(
        (0..7).contains(&t.wday),
        "LocalTime weekday {} out of range",
        t.wday
    );
    let mon = MONTHS[t.mon as usize];
    Content {
        time,
        short_date: format!("{mon} {}", t.mday),
        long_date: format!("{}, {mon} {}", DAYS[t.wday as usize], t.mday),
        weather,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(mon: i32, wday: i32) -> LocalTime {
        LocalTime {
            hour: 9,
            min: 5,
            sec: 0,
            mday: 3,
            mon,
            wday,
        }
    }

    #[test]
    fn content_names_the_month_and_day() {
        let c = content(ClockStyle::Detailed, true, &at(9, 6), None);
        assert_eq!(c.time, "09:05");
        assert_eq!(c.long_date, "Sat, Oct 3");
    }

    #[test]
    #[should_panic(expected = "month 12 out of range")]
    fn a_month_out_of_range_is_a_bug() {
        content(ClockStyle::Detailed, true, &at(12, 0), None);
    }

    #[test]
    #[should_panic(expected = "weekday -1 out of range")]
    fn a_weekday_out_of_range_is_a_bug() {
        content(ClockStyle::Detailed, true, &at(0, -1), None);
    }
}
