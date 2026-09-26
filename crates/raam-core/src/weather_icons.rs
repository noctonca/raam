//! Weather icons drawn procedurally into coverage bitmaps and packed into
//! the text atlas as extra glyphs on private-use codepoints, so they get the
//! same baked shadow and the same draw call as the text around them.
//!
//! Roboto has no weather glyphs, and a bundled icon font would add a second
//! licence to track. Each icon is a union/difference of simple signed
//! distance shapes (circles, capsules, a polygon) in a unit box, rendered
//! with a 1px anti-aliased edge. Monochrome, like the text.
use crate::atlas::Raster;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Sun,
    Moon,
    PartlyCloudyDay,
    PartlyCloudyNight,
    Cloud,
    Fog,
    Rain,
    Snow,
    Thunder,
}

impl Icon {
    pub const ALL: [Icon; 9] = [
        Icon::Sun,
        Icon::Moon,
        Icon::PartlyCloudyDay,
        Icon::PartlyCloudyNight,
        Icon::Cloud,
        Icon::Fog,
        Icon::Rain,
        Icon::Snow,
        Icon::Thunder,
    ];

    pub fn ch(self) -> char {
        char::from_u32(0xE000 + Icon::ALL.iter().position(|&i| i == self).unwrap() as u32).unwrap()
    }
}

/// WMO weather interpretation code (Open-Meteo's `weather_code`) to a short
/// description and an icon.
pub fn describe(code: i64, is_day: bool) -> (&'static str, Icon) {
    let partly = if is_day {
        Icon::PartlyCloudyDay
    } else {
        Icon::PartlyCloudyNight
    };
    match code {
        0 => ("Clear sky", if is_day { Icon::Sun } else { Icon::Moon }),
        1 => ("Mainly clear", partly),
        2 => ("Partly cloudy", partly),
        3 => ("Overcast", Icon::Cloud),
        45 => ("Fog", Icon::Fog),
        48 => ("Rime fog", Icon::Fog),
        51 => ("Light drizzle", Icon::Rain),
        53 => ("Drizzle", Icon::Rain),
        55 => ("Dense drizzle", Icon::Rain),
        56 | 57 => ("Freezing drizzle", Icon::Rain),
        61 => ("Light rain", Icon::Rain),
        63 => ("Rain", Icon::Rain),
        65 => ("Heavy rain", Icon::Rain),
        66 | 67 => ("Freezing rain", Icon::Rain),
        71 => ("Light snow", Icon::Snow),
        73 => ("Snow", Icon::Snow),
        75 => ("Heavy snow", Icon::Snow),
        77 => ("Snow grains", Icon::Snow),
        80 => ("Light showers", Icon::Rain),
        81 => ("Showers", Icon::Rain),
        82 => ("Heavy showers", Icon::Rain),
        85 | 86 => ("Snow showers", Icon::Snow),
        95 => ("Thunderstorm", Icon::Thunder),
        96 | 99 => ("Thunderstorm, hail", Icon::Thunder),
        _ => ("Unknown", Icon::Cloud),
    }
}

// Signed distances in unit-box coordinates (x right, y down), negative inside.
fn circle(p: (f32, f32), c: (f32, f32), r: f32) -> f32 {
    ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt() - r
}

fn capsule(p: (f32, f32), a: (f32, f32), b: (f32, f32), r: f32) -> f32 {
    let (pa, ba) = ((p.0 - a.0, p.1 - a.1), (b.0 - a.0, b.1 - a.1));
    let h = ((pa.0 * ba.0 + pa.1 * ba.1) / (ba.0 * ba.0 + ba.1 * ba.1)).clamp(0.0, 1.0);
    ((pa.0 - ba.0 * h).powi(2) + (pa.1 - ba.1 * h).powi(2)).sqrt() - r
}

/// Convex polygon, vertices clockwise in screen space (y down).
fn polygon(p: (f32, f32), v: &[(f32, f32)]) -> f32 {
    let mut d = f32::MIN;
    for i in 0..v.len() {
        let (a, b) = (v[i], v[(i + 1) % v.len()]);
        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
        let len = (ex * ex + ey * ey).sqrt();
        // Outward normal of a clockwise (y-down) edge.
        let (nx, ny) = (ey / len, -ex / len);
        d = d.max((p.0 - a.0) * nx + (p.1 - a.1) * ny);
    }
    d
}

fn cloud(p: (f32, f32), dx: f32, dy: f32, s: f32) -> f32 {
    let q = ((p.0 - dx) / s, (p.1 - dy) / s);
    let d = circle(q, (0.33, 0.62), 0.17)
        .min(circle(q, (0.55, 0.50), 0.24))
        .min(circle(q, (0.76, 0.64), 0.15))
        .min(capsule(q, (0.33, 0.66), (0.76, 0.66), 0.13));
    d * s
}

fn sun(p: (f32, f32), c: (f32, f32), s: f32) -> f32 {
    let mut d = circle(p, c, 0.19 * s);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        let (ca, sa) = (a.cos(), a.sin());
        d = d.min(capsule(
            p,
            (c.0 + ca * 0.30 * s, c.1 + sa * 0.30 * s),
            (c.0 + ca * 0.42 * s, c.1 + sa * 0.42 * s),
            0.035 * s,
        ));
    }
    d
}

fn moon(p: (f32, f32), c: (f32, f32), s: f32) -> f32 {
    circle(p, c, 0.34 * s).max(-circle(p, (c.0 + 0.2 * s, c.1 - 0.12 * s), 0.3 * s))
}

/// Keeps a gap between a shape behind (`back`) and the cloud in front.
fn behind(back: f32, front: f32, gap: f32) -> f32 {
    back.max(-(front - gap)).min(front)
}

fn sdf(icon: Icon, p: (f32, f32)) -> f32 {
    match icon {
        Icon::Sun => sun(p, (0.5, 0.5), 1.0),
        Icon::Moon => moon(p, (0.5, 0.5), 1.0),
        Icon::PartlyCloudyDay => behind(sun(p, (0.36, 0.36), 0.75), cloud(p, 0.1, 0.12, 0.9), 0.05),
        Icon::PartlyCloudyNight => {
            behind(moon(p, (0.38, 0.36), 0.75), cloud(p, 0.1, 0.12, 0.9), 0.05)
        }
        Icon::Cloud => cloud(p, 0.0, 0.0, 1.0),
        Icon::Fog => capsule(p, (0.18, 0.34), (0.82, 0.34), 0.05)
            .min(capsule(p, (0.12, 0.52), (0.88, 0.52), 0.05))
            .min(capsule(p, (0.22, 0.70), (0.78, 0.70), 0.05)),
        Icon::Rain => {
            let c = cloud(p, 0.0, -0.14, 1.0);
            let drops = capsule(p, (0.34, 0.72), (0.29, 0.90), 0.035)
                .min(capsule(p, (0.52, 0.72), (0.47, 0.90), 0.035))
                .min(capsule(p, (0.70, 0.72), (0.65, 0.90), 0.035));
            c.min(drops)
        }
        Icon::Snow => {
            let c = cloud(p, 0.0, -0.14, 1.0);
            let flakes = circle(p, (0.32, 0.80), 0.05)
                .min(circle(p, (0.52, 0.88), 0.05))
                .min(circle(p, (0.72, 0.80), 0.05));
            c.min(flakes)
        }
        Icon::Thunder => {
            let c = cloud(p, 0.0, -0.16, 1.0);
            let bolt = polygon(p, &[(0.52, 0.56), (0.62, 0.56), (0.55, 0.72), (0.42, 0.72)]).min(
                polygon(p, &[(0.50, 0.70), (0.62, 0.70), (0.45, 0.97), (0.44, 0.97)]),
            );
            behind(c, bolt, 0.04)
        }
    }
}

/// One icon as an atlas glyph: `size` px square, vertically centred on the
/// text's cap height, followed by `gap` px before the next glyph.
pub fn raster(icon: Icon, size: usize, cap_height: f32, gap: f32) -> Raster {
    let mut bitmap = vec![0u8; size * size];
    let px = 1.0 / size as f32;
    for y in 0..size {
        for x in 0..size {
            let p = ((x as f32 + 0.5) * px, (y as f32 + 0.5) * px);
            let d = sdf(icon, p) / px; // in pixels
            bitmap[y * size + x] = ((0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    Raster {
        ch: icon.ch(),
        width: size,
        height: size,
        xmin: 0,
        ymin: (cap_height / 2.0 - size as f32 / 2.0).round() as i32,
        advance: size as f32 + gap,
        bitmap,
    }
}

pub fn all(size: usize, cap_height: f32, gap: f32) -> Vec<Raster> {
    Icon::ALL
        .iter()
        .map(|&i| raster(i, size, cap_height, gap))
        .collect()
}
