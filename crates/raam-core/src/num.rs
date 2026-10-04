//! Float to unsigned conversions that saturate on purpose. A colour
//! channel, a pixel count or a column count worked out in `f32` can come
//! out below zero (or NaN) at an edge, and 0 is then the right answer:
//! these say so, where a bare `as` would do it silently. The fraction is
//! dropped, as `as` drops it: round first where rounding is meant.

/// `v` as a colour channel: below 0 or NaN is 0, past 255 is 255.
#[must_use]
#[expect(clippy::cast_sign_loss, reason = "clamped to 0..=255 first")]
pub fn sat_u8(v: f32) -> u8 {
    // NaN comes through `clamp` as NaN, which `as` makes 0.
    v.clamp(0.0, 255.0) as u8
}

/// `v` as a `u32`: below 0 or NaN is 0, past `u32::MAX` is `u32::MAX`.
#[must_use]
#[expect(clippy::cast_sign_loss, reason = "not below 0 after the max")]
pub fn sat_u32(v: f32) -> u32 {
    // `max` takes the non-NaN side, so NaN is 0 too.
    v.max(0.0) as u32
}

/// `v` as a `usize`: below 0 or NaN is 0, past `usize::MAX` is
/// `usize::MAX`.
#[must_use]
#[expect(clippy::cast_sign_loss, reason = "not below 0 after the max")]
pub fn sat_usize(v: f32) -> usize {
    // `max` takes the non-NaN side, so NaN is 0 too.
    v.max(0.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same answers a bare `as` gives, edges included.
    #[test]
    fn saturate_as_as_does() {
        let edges = [
            f32::NEG_INFINITY,
            -1e9,
            -1.0,
            -0.5,
            -0.0,
            0.0,
            0.5,
            1.0,
            127.5,
            254.9,
            255.0,
            255.5,
            256.0,
            1e9,
            1e20,
            f32::INFINITY,
            f32::NAN,
        ];
        for v in edges {
            #[expect(clippy::cast_sign_loss, reason = "comparing against `as`")]
            let (u8_as, u32_as, usize_as) = (v as u8, v as u32, v as usize);
            assert_eq!(sat_u8(v), u8_as, "{v}");
            assert_eq!(sat_u32(v), u32_as, "{v}");
            assert_eq!(sat_usize(v), usize_as, "{v}");
        }
    }
}
