//! Float conversions that saturate or round on purpose, each with the one
//! `as` it needs. A colour channel, a pixel count, a margin or a column
//! count worked out in floats can come out below zero, past the target
//! type, or NaN at an edge, and the nearest end of the range (0 for NaN)
//! is then the right answer: these say so, where a bare `as` would do it
//! silently. The fraction is dropped, as `as` drops it: round first where
//! rounding is meant.

/// `v` as a colour channel: below 0 or NaN is 0, past 255 is 255.
#[must_use]
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "clamped to 0..=255 first"
)]
pub fn sat_u8(v: f32) -> u8 {
    // NaN comes through `clamp` as NaN, which `as` makes 0.
    v.clamp(0.0, 255.0) as u8
}

/// `v` as a `u32`: below 0 or NaN is 0, past `u32::MAX` is `u32::MAX`.
#[must_use]
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "not below 0 after the max; `as` saturates past the top"
)]
pub fn sat_u32(v: f32) -> u32 {
    // `max` takes the non-NaN side, so NaN is 0 too.
    v.max(0.0) as u32
}

/// `v` as a `usize`: below 0 or NaN is 0, past `usize::MAX` is
/// `usize::MAX`.
#[must_use]
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "not below 0 after the max; `as` saturates past the top"
)]
pub fn sat_usize(v: f32) -> usize {
    // `max` takes the non-NaN side, so NaN is 0 too.
    v.max(0.0) as usize
}

// The signed ones take an `f32` or an `f64`: an `f32` widens to `f64`
// exactly, and `as` from either drops the fraction and saturates alike,
// so the answer is the one `as` gives from the caller's type.

/// `v` as an `i8` (an egui margin): NaN is 0, past either end is that end.
#[must_use]
#[expect(clippy::cast_possible_truncation, reason = "`as` saturates")]
pub fn sat_i8(v: impl Into<f64>) -> i8 {
    v.into() as i8
}

/// `v` as an `i32` (a pixel, a GL value): NaN is 0, past either end is
/// that end.
#[must_use]
#[expect(clippy::cast_possible_truncation, reason = "`as` saturates")]
pub fn sat_i32(v: impl Into<f64>) -> i32 {
    v.into() as i32
}

/// `v` as an `i64`: NaN is 0, past either end is that end.
#[must_use]
#[expect(clippy::cast_possible_truncation, reason = "`as` saturates")]
pub fn sat_i64(v: impl Into<f64>) -> i64 {
    v.into() as i64
}

/// `v` as the nearest `f32`: what a JSON number, a SQLite REAL or a
/// window-system `f64` becomes in the renderer's `f32` world. Past `f32`'s
/// range is infinite, NaN stays NaN.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "rounding to the nearest f32 is the point"
)]
pub fn to_f32(v: f64) -> f32 {
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGES: [f32; 21] = [
        f32::NEG_INFINITY,
        -1e20,
        -1e9,
        -129.0,
        -128.5,
        -1.0,
        -0.5,
        -0.0,
        0.0,
        0.5,
        1.0,
        127.5,
        128.0,
        254.9,
        255.0,
        255.5,
        256.0,
        1e9,
        1e20,
        f32::INFINITY,
        f32::NAN,
    ];

    /// The same answers a bare `as` gives, edges included.
    #[test]
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "comparing against `as`"
    )]
    fn saturate_as_as_does() {
        for v in EDGES {
            assert_eq!(sat_u8(v), v as u8, "{v}");
            assert_eq!(sat_u32(v), v as u32, "{v}");
            assert_eq!(sat_usize(v), v as usize, "{v}");
            assert_eq!(sat_i8(v), v as i8, "{v}");
            assert_eq!(sat_i32(v), v as i32, "{v}");
            assert_eq!(sat_i64(v), v as i64, "{v}");
            let w = f64::from(v) * 1e10;
            assert_eq!(sat_i32(w), w as i32, "{w}");
            assert_eq!(sat_i64(w), w as i64, "{w}");
            assert_eq!(to_f32(w).to_bits(), (w as f32).to_bits(), "{w}");
        }
        assert!(to_f32(f64::NAN).is_nan());
        assert_eq!(to_f32(0.1), 0.1_f32);
    }
}
