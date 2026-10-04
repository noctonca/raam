//! The baked-blur shadow atlas: every glyph is packed twice into one
//! `GL_ALPHA` texture, once sharp and once padded and box-blurred, and a
//! line is drawn twice (blurred region offset and tinted dark, then the
//! sharp region on top) with the same shader. The soft shadow costs no
//! extra shader, render target or per-frame blur, only a blur at startup.
//!
//! The charset also takes extra pre-rasterized glyphs (the weather icons
//! from weather_icons.rs, on private-use codepoints) so they are laid out
//! and shadowed exactly like text, and the blur radius is a parameter so
//! the 80px clock can have a softer shadow than the smaller lines.
use crate::gl::*;
use crate::{clock, num};
use std::collections::HashMap;
use std::ffi::c_void;

use raam_model::limits::ATLAS_WIDTH;

// GL's default unpack alignment is 4: a row width that isn't a multiple of
// it would make the upload read past `pixels`.
const _: () = assert!(ATLAS_WIDTH.is_multiple_of(4));

/// Printable ASCII and the Latin-1 Supplement (NBSP to ÿ, the degree sign
/// among them), so a weather city such as "Malmö" or "São Paulo" reads as
/// given; anything else draws as '?'.
const CHARSET: [(u32, u32); 2] = [(32, 126), (0xA0, 0xFF)];

/// A coverage bitmap to pack: fontdue's for text, weather_icons.rs's for
/// icons.
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
        // A wider glyph would run off its shelf into the next row's.
        assert!(
            w0 + 2 * pad <= ATLAS_WIDTH,
            "a {w0} px glyph doesn't fit the {ATLAS_WIDTH} px atlas"
        );
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

/// Two box-blur passes, running sums (plain nested loops scale with the
/// radius, and the clock's radius is the largest).
fn box_blur(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    // The window at `i` is `i - r ..= i + r`, clipped to the line: it
    // starts as `0 ..= r`, takes in `i + r + 1` and lets go of `i - r`
    // once that is on the line.
    let pass =
        |src: &[u8], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| -> Vec<u8> {
            let mut out = vec![0u8; src.len()];
            for line in 0..count {
                let mut sum = 0u32;
                let mut n = 0u32;
                for i in 0..len.min(r + 1) {
                    sum += u32::from(src[at(line, i)]);
                    n += 1;
                }
                for i in 0..len {
                    out[at(line, i)] = (sum / n.max(1)) as u8;
                    let add = i + r + 1;
                    if add < len {
                        sum += u32::from(src[at(line, add)]);
                        n += 1;
                    }
                    if i >= r {
                        sum -= u32::from(src[at(line, i - r)]);
                        n -= 1;
                    }
                }
            }
            out
        };
    let tmp = pass(src, w, h, &|y, x| y * w + x);
    pass(&tmp, h, w, &|x, y| y * w + x)
}

/// The CPU half of an atlas: its coverage pixels, `ATLAS_WIDTH` wide, and
/// where each glyph landed.
struct Packed {
    pixels: Vec<u8>,
    height: usize,
    glyphs: HashMap<char, Glyph>,
}

/// Rasterizes the charset and `extra`, blurs each glyph's shadow copy and
/// packs both into one bitmap.
fn pack(font: &fontdue::Font, px: f32, shadow: Shadow, extra: Vec<Raster>) -> Packed {
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
                    .for_each(|v| *v = num::sat_u8(f32::from(*v) * shadow.gain));
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

    log::info!(
        "built {px}px atlas {ATLAS_WIDTH}x{atlas_h} ({} KB) in {:.1}ms (blur r={} {:.1}ms)",
        ATLAS_WIDTH * atlas_h / 1024,
        clock::elapsed(t0).as_secs_f64() * 1000.0,
        shadow.radius,
        t_blur.as_secs_f64() * 1000.0,
    );
    Packed {
        pixels,
        height: atlas_h,
        glyphs,
    }
}

impl FontAtlas {
    /// Rasterises the charset (plus `extra`, the weather icons) at `px`,
    /// with each glyph's blurred shadow, packs both into one alpha atlas
    /// and uploads it.
    ///
    /// # Panics
    /// If the packed pixels fall short of the atlas size (a packing bug).
    ///
    /// # Safety
    /// Requires a current GL context (it makes and fills a texture).
    pub unsafe fn build(font: &fontdue::Font, px: f32, shadow: Shadow, extra: Vec<Raster>) -> Self {
        let Packed {
            pixels,
            height: atlas_h,
            glyphs,
        } = pack(font, px, shadow, extra);
        assert_eq!(pixels.len(), ATLAS_WIDTH * atlas_h, "atlas pixels short");
        let mut texture = 0;
        // SAFETY: `pixels` is ATLAS_WIDTH * atlas_h bytes (asserted above),
        // what GL reads for that ALPHA/UNSIGNED_BYTE upload at an unpack
        // alignment of 4 (ATLAS_WIDTH is a multiple of 4); GL copies it.
        unsafe {
            glActiveTexture(GL_TEXTURE0);
            glGenTextures(1, &mut texture);
            glBindTexture(GL_TEXTURE_2D, texture);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                gl_enum_param(GL_ALPHA),
                gl_sizei(ATLAS_WIDTH),
                gl_sizei(atlas_h),
                0,
                GL_ALPHA,
                GL_UNSIGNED_BYTE,
                pixels.as_ptr() as *const c_void,
            );
            set_linear_clamp();
        }
        let lm = font
            .horizontal_line_metrics(px)
            .expect("font has no horizontal line metrics");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glyph_as_wide_as_the_atlas_fits_its_own_shelf() {
        let (at, _) = shelf_pack(&[(10, 10), (ATLAS_WIDTH - 4, 10)], 0, 2);
        assert_eq!(at, [(2, 2), (2, 16)]);
    }

    #[test]
    #[should_panic(expected = "doesn't fit")]
    fn a_glyph_wider_than_the_atlas_is_refused() {
        shelf_pack(&[(ATLAS_WIDTH - 3, 10)], 0, 2);
    }

    const SHADOW: Shadow = Shadow {
        bleed: 6,
        radius: 3,
        gain: 2.0,
    };

    fn roboto() -> fontdue::Font {
        fontdue::Font::from_bytes(
            &include_bytes!("../assets/Roboto-Regular.ttf")[..],
            fontdue::FontSettings::default(),
        )
        .unwrap()
    }

    #[test]
    fn a_weather_city_with_accents_has_its_own_glyphs() {
        crate::clock::fake::install();
        let packed = pack(&roboto(), 26.0, SHADOW, Vec::new());
        for ch in ['ö', 'ã', 'é', 'ñ', 'ß', '°'] {
            assert!(packed.glyphs.contains_key(&ch), "{ch:?} would draw as '?'");
        }
    }

    #[test]
    fn both_overlay_fonts_draw_every_charset_glyph() {
        // A codepoint a font lacks would rasterize its empty .notdef.
        let bold = fontdue::Font::from_bytes(
            &include_bytes!("../assets/Roboto-Bold.ttf")[..],
            fontdue::FontSettings::default(),
        )
        .unwrap();
        for font in [roboto(), bold] {
            for ch in CHARSET
                .iter()
                .flat_map(|&(a, b)| a..=b)
                .filter_map(char::from_u32)
            {
                assert_ne!(font.lookup_glyph_index(ch), 0, "{ch:?} missing");
            }
        }
    }
}
