//! 012's baked-blur shadow atlas: every glyph is packed twice into one
//! `GL_ALPHA` texture, once sharp and once padded and box-blurred, and a
//! line is drawn twice (blurred region offset and tinted dark, then the
//! sharp region on top) with the same shader.
//!
//! 022 changes: the charset also takes extra pre-rasterized glyphs (the
//! weather icons from icons.rs, on private-use codepoints) so they are laid
//! out and shadowed exactly like text, and the blur radius is a parameter so
//! the 80px clock can have a softer shadow than 012's 72px one.
use crate::clock;
use crate::gl::*;
use std::collections::HashMap;
use std::ffi::c_void;

use raam_model::limits::ATLAS_WIDTH;
/// Printable ASCII, plus the degree sign (012's one-codepoint widening).
const CHARSET: [(u32, u32); 2] = [(32, 126), (0xB0, 0xB0)];

/// A coverage bitmap to pack: fontdue's for text, icons.rs's for icons.
pub struct Raster {
    pub ch: char,
    pub width: usize,
    pub height: usize,
    /// Bitmap origin relative to the pen and baseline, fontdue's convention
    /// (`ymin` is the bottom edge's height above the baseline).
    pub xmin: i32,
    pub ymin: i32,
    pub advance: f32,
    pub bitmap: Vec<u8>,
}

#[derive(Clone, Copy)]
pub struct Shadow {
    /// Px of empty border padded around each glyph before blurring.
    pub bleed: usize,
    /// Box-blur radius per pass (horizontal then vertical).
    pub radius: usize,
    /// Multiplies the blurred coverage (clamped): blurring a thin stroke
    /// spreads it so thin that regular-weight text's halo nearly vanishes.
    pub gain: f32,
}

struct Glyph {
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    width: f32,
    height: f32,
    xmin: f32,
    ymin: f32,
    advance: f32,
    su0: f32,
    sv0: f32,
    su1: f32,
    sv1: f32,
    swidth: f32,
    sheight: f32,
    sxmin: f32,
    symin: f32,
}

pub struct FontAtlas {
    pub texture: GlUint,
    pub ascent: f32,
    pub descent: f32,
    glyphs: HashMap<char, Glyph>,
}

fn shelf_pack(
    sizes: &[(usize, usize)],
    start_y: usize,
    pad: usize,
) -> (Vec<(usize, usize)>, usize) {
    let mut cursor_x = pad;
    let mut cursor_y = start_y + pad;
    let mut shelf_h = 0usize;
    let mut region_bottom = cursor_y;
    let mut placements = Vec::with_capacity(sizes.len());
    for &(w0, h0) in sizes {
        let w = w0 + pad;
        let h = h0 + pad;
        if cursor_x + w > ATLAS_WIDTH {
            cursor_x = pad;
            cursor_y += shelf_h + pad;
            shelf_h = 0;
        }
        placements.push((cursor_x, cursor_y));
        cursor_x += w;
        shelf_h = shelf_h.max(h);
        region_bottom = region_bottom.max(cursor_y + shelf_h + pad);
    }
    (placements, region_bottom)
}

fn pad_bitmap(src: &[u8], w: usize, h: usize, bleed: usize) -> (Vec<u8>, usize, usize) {
    let (pw, ph) = (w + 2 * bleed, h + 2 * bleed);
    let mut out = vec![0u8; pw * ph];
    for y in 0..h {
        let dst = (y + bleed) * pw + bleed;
        out[dst..dst + w].copy_from_slice(&src[y * w..(y + 1) * w]);
    }
    (out, pw, ph)
}

/// Two box-blur passes, running sums (012's nested loops scale with the
/// radius, and 022's clock radius is larger).
fn box_blur(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let r = r as i32;
    let pass =
        |src: &[u8], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| -> Vec<u8> {
            let mut out = vec![0u8; src.len()];
            for line in 0..count {
                let mut sum = 0u32;
                let mut n = 0u32;
                for i in 0..=r.min(len as i32 - 1) {
                    sum += src[at(line, i as usize)] as u32;
                    n += 1;
                }
                for i in 0..len as i32 {
                    out[at(line, i as usize)] = (sum / n.max(1)) as u8;
                    let add = i + r + 1;
                    if add < len as i32 {
                        sum += src[at(line, add as usize)] as u32;
                        n += 1;
                    }
                    let sub = i - r;
                    if sub >= 0 {
                        sum -= src[at(line, sub as usize)] as u32;
                        n -= 1;
                    }
                }
            }
            out
        };
    let tmp = pass(src, w, h, &|y, x| y * w + x);
    pass(&tmp, h, w, &|x, y| y * w + x)
}

impl FontAtlas {
    pub fn build(font: &fontdue::Font, px: f32, shadow: Shadow, extra: Vec<Raster>) -> Self {
        let t0 = clock::now();
        let mut rasters: Vec<Raster> = CHARSET
            .iter()
            .flat_map(|&(a, b)| a..=b)
            .map(|code| {
                let ch = char::from_u32(code).unwrap();
                let (m, bitmap) = font.rasterize(ch, px);
                Raster {
                    ch,
                    width: m.width,
                    height: m.height,
                    xmin: m.xmin,
                    ymin: m.ymin,
                    advance: m.advance_width,
                    bitmap,
                }
            })
            .collect();
        rasters.extend(extra);
        let pad = 1usize;

        let mut sharp_order: Vec<usize> = (0..rasters.len()).collect();
        sharp_order.sort_by(|&a, &b| rasters[b].height.cmp(&rasters[a].height));
        let sharp_sizes: Vec<(usize, usize)> = sharp_order
            .iter()
            .map(|&i| (rasters[i].width, rasters[i].height))
            .collect();
        let (sharp_placements, sharp_end_y) = shelf_pack(&sharp_sizes, 0, pad);

        let t_blur0 = clock::elapsed(t0);
        let blurred: Vec<(Vec<u8>, usize, usize)> = rasters
            .iter()
            .map(|r| {
                let (padded, pw, ph) = pad_bitmap(&r.bitmap, r.width, r.height, shadow.bleed);
                let mut b = box_blur(&padded, pw, ph, shadow.radius);
                if shadow.gain != 1.0 {
                    b.iter_mut()
                        .for_each(|v| *v = (*v as f32 * shadow.gain).min(255.0) as u8);
                }
                (b, pw, ph)
            })
            .collect();
        let t_blur = clock::elapsed(t0) - t_blur0;

        let mut shadow_order: Vec<usize> = (0..blurred.len()).collect();
        shadow_order.sort_by(|&a, &b| blurred[b].2.cmp(&blurred[a].2));
        let shadow_sizes: Vec<(usize, usize)> = shadow_order
            .iter()
            .map(|&i| (blurred[i].1, blurred[i].2))
            .collect();
        let (shadow_placements, atlas_h) = shelf_pack(&shadow_sizes, sharp_end_y, pad);

        let mut pixels = vec![0u8; ATLAS_WIDTH * atlas_h];
        let mut sharp_at = vec![(0usize, 0usize); rasters.len()];
        for (idx, &i) in sharp_order.iter().enumerate() {
            let r = &rasters[i];
            let (x, y) = sharp_placements[idx];
            sharp_at[i] = (x, y);
            for row in 0..r.height {
                let dst = (y + row) * ATLAS_WIDTH + x;
                pixels[dst..dst + r.width]
                    .copy_from_slice(&r.bitmap[row * r.width..(row + 1) * r.width]);
            }
        }
        let mut shadow_at = vec![(0usize, 0usize); rasters.len()];
        for (idx, &i) in shadow_order.iter().enumerate() {
            let (bitmap, bw, bh) = &blurred[i];
            let (x, y) = shadow_placements[idx];
            shadow_at[i] = (x, y);
            for row in 0..*bh {
                let dst = (y + row) * ATLAS_WIDTH + x;
                pixels[dst..dst + bw].copy_from_slice(&bitmap[row * bw..(row + 1) * bw]);
            }
        }

        let (aw, ah) = (ATLAS_WIDTH as f32, atlas_h as f32);
        let mut glyphs = HashMap::with_capacity(rasters.len());
        for (i, r) in rasters.iter().enumerate() {
            let (sx, sy) = sharp_at[i];
            let (hx, hy) = shadow_at[i];
            let (_, bw, bh) = &blurred[i];
            glyphs.insert(
                r.ch,
                Glyph {
                    u0: sx as f32 / aw,
                    v0: sy as f32 / ah,
                    u1: (sx + r.width) as f32 / aw,
                    v1: (sy + r.height) as f32 / ah,
                    width: r.width as f32,
                    height: r.height as f32,
                    xmin: r.xmin as f32,
                    ymin: r.ymin as f32,
                    advance: r.advance,
                    su0: hx as f32 / aw,
                    sv0: hy as f32 / ah,
                    su1: (hx + bw) as f32 / aw,
                    sv1: (hy + bh) as f32 / ah,
                    swidth: *bw as f32,
                    sheight: *bh as f32,
                    sxmin: r.xmin as f32 - shadow.bleed as f32,
                    symin: r.ymin as f32 - shadow.bleed as f32,
                },
            );
        }

        let mut texture = 0;
        unsafe {
            glActiveTexture(GL_TEXTURE0);
            glGenTextures(1, &mut texture);
            glBindTexture(GL_TEXTURE_2D, texture);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_ALPHA as i32,
                ATLAS_WIDTH as i32,
                atlas_h as i32,
                0,
                GL_ALPHA,
                GL_UNSIGNED_BYTE,
                pixels.as_ptr() as *const c_void,
            );
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
        }
        let lm = font
            .horizontal_line_metrics(px)
            .expect("font has no horizontal line metrics");
        log::info!(
            "built {px}px atlas {ATLAS_WIDTH}x{atlas_h} ({} KB) in {:.1}ms (blur r={} {:.1}ms)",
            ATLAS_WIDTH * atlas_h / 1024,
            clock::elapsed(t0).as_secs_f64() * 1000.0,
            shadow.radius,
            t_blur.as_secs_f64() * 1000.0,
        );
        Self {
            texture,
            ascent: lm.ascent,
            descent: -lm.descent,
            glyphs,
        }
    }

    fn glyph(&self, ch: char) -> Option<&Glyph> {
        self.glyphs.get(&ch).or_else(|| self.glyphs.get(&'?'))
    }

    pub fn text_width(&self, text: &str) -> f32 {
        text.chars()
            .filter_map(|c| self.glyph(c))
            .map(|g| g.advance)
            .sum()
    }

    /// Sharp quads (`shadow == false`) or blurred shadow quads, 4 floats per
    /// vertex (x, y, u, v), two triangles per glyph.
    pub fn append_line(
        &self,
        out: &mut Vec<f32>,
        text: &str,
        left_x: f32,
        baseline_y: f32,
        shadow: bool,
    ) {
        let mut pen_x = left_x;
        for ch in text.chars() {
            let Some(g) = self.glyph(ch) else { continue };
            let (w, h, xmin, ymin, u0, v0, u1, v1) = if shadow {
                (
                    g.swidth, g.sheight, g.sxmin, g.symin, g.su0, g.sv0, g.su1, g.sv1,
                )
            } else {
                (g.width, g.height, g.xmin, g.ymin, g.u0, g.v0, g.u1, g.v1)
            };
            if w > 0.0 && h > 0.0 {
                let x0 = (pen_x + xmin).round();
                let x1 = x0 + w;
                let y1 = (baseline_y - ymin).round();
                let y0 = y1 - h;
                #[rustfmt::skip]
                out.extend_from_slice(&[
                    x0, y0, u0, v0,  x1, y0, u1, v0,  x1, y1, u1, v1,
                    x0, y0, u0, v0,  x1, y1, u1, v1,  x0, y1, u0, v1,
                ]);
            }
            pen_x += g.advance;
        }
    }
}
