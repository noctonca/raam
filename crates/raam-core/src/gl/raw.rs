//! The WebGL1 linkage's raw-pointer reads and writes: GL's pointer
//! arguments turned into slices, and info logs copied out. Kept apart, like
//! `table`, so their tests run natively, and under Miri (CI's miri job),
//! which can't run the linkage itself.

use super::{GL_ALPHA, GL_RGBA, GL_UNSIGNED_BYTE, GlEnum, GlSizei, from_gl_size, gl_sizei};
use std::ffi::{c_char, c_void};

/// `len` bytes at `ptr`, or none for a null pointer.
///
/// # Safety
/// A non-null `ptr` must be valid for reading `len` bytes for as long as
/// the slice is used (the GL call's duration).
pub(super) unsafe fn bytes<'a>(ptr: *const c_void, len: usize) -> Option<&'a [u8]> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: non-null, and valid for `len` bytes by the caller's
        // contract; u8 has no alignment or validity requirement.
        Some(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) })
    }
}

/// The bytes GL reads for a `width` x `height` upload in `format` and
/// `type_`: the length a glTexImage2D/glTexSubImage2D pixel pointer is
/// read for. Only the uploads the core makes are known: RGBA and ALPHA,
/// one unsigned byte a channel, in rows needing no padding at GL's
/// default unpack alignment of 4 (raam never changes it). Anything else
/// would need its own size here, so it panics rather than reading a
/// guess's worth of the caller's buffer.
///
/// # Panics
/// On a type other than `GL_UNSIGNED_BYTE`, a format other than RGBA or
/// ALPHA, a negative side, or rows GL would pad.
pub(super) fn image_len(width: GlSizei, height: GlSizei, format: GlEnum, type_: GlEnum) -> usize {
    assert_eq!(
        type_, GL_UNSIGNED_BYTE,
        "texture upload type 0x{type_:x}: only GL_UNSIGNED_BYTE is sized"
    );
    let bytes_per_texel = match format {
        GL_RGBA => 4,
        GL_ALPHA => 1,
        _ => panic!("texture upload format 0x{format:x}: only RGBA and ALPHA are sized"),
    };
    let row: usize = from_gl_size::<_, usize>(width) * bytes_per_texel;
    let rows: usize = from_gl_size(height);
    assert!(
        row.is_multiple_of(4) || rows <= 1,
        "a {width}x{height} upload in 0x{format:x} has rows GL pads to 4 bytes"
    );
    row * rows
}

/// `count` 4x4 matrices at `value`, 16 floats each; none for a count of 0
/// or less, whatever `value` is (GL lets it be null then).
///
/// # Safety
/// For a positive `count`, `value` must be non-null, aligned, and valid for
/// reading `16 * count` floats for as long as the slice is used.
pub(super) unsafe fn mat4s<'a>(value: *const f32, count: GlSizei) -> &'a [f32] {
    match usize::try_from(count) {
        Ok(n) if n > 0 => {
            // SAFETY: valid for `16 * count` floats by the caller's contract.
            unsafe { std::slice::from_raw_parts(value, 16 * n) }
        }
        _ => &[],
    }
}

/// Copies `log` into a caller's GL info-log buffer, NUL-terminated and
/// truncated to `max_len`, as GL does.
///
/// # Safety
/// `out` must be valid for writing `max_len` bytes (at least one), and
/// `len` null or valid for one write, as glGet*InfoLog require.
pub(super) unsafe fn write_log(log: &str, max_len: GlSizei, len: *mut GlSizei, out: *mut c_char) {
    let n = log.len().min(from_gl_size(max_len.max(1) - 1));
    // SAFETY: `n` + 1 <= max(max_len, 1) bytes go to `out`, which holds
    // `max_len`; `log` can't overlap the caller's buffer, and `len` is
    // written only when non-null.
    unsafe {
        std::ptr::copy_nonoverlapping(log.as_ptr(), out.cast::<u8>(), n);
        *out.add(n) = 0;
        if !len.is_null() {
            *len = gl_sizei(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_reads_the_given_length_and_null_is_none() {
        let data = [1u8, 2, 3, 4];
        // SAFETY: `data` holds 4 bytes and outlives the slice.
        let got = unsafe { bytes(data.as_ptr().cast(), 3) };
        assert_eq!(got, Some(&data[..3]));
        // SAFETY: null is allowed.
        assert_eq!(unsafe { bytes(std::ptr::null(), 16) }, None);
    }

    #[test]
    fn image_len_sizes_rgba_and_alpha() {
        assert_eq!(image_len(3, 2, GL_RGBA, GL_UNSIGNED_BYTE), 24);
        assert_eq!(image_len(8, 2, GL_ALPHA, GL_UNSIGNED_BYTE), 16);
        // One row is never padded.
        assert_eq!(image_len(3, 1, GL_ALPHA, GL_UNSIGNED_BYTE), 3);
        assert_eq!(image_len(0, 0, GL_RGBA, GL_UNSIGNED_BYTE), 0);
    }

    #[test]
    #[should_panic(expected = "only GL_UNSIGNED_BYTE")]
    fn image_len_refuses_a_packed_type() {
        // GL_UNSIGNED_SHORT_5_6_5: 2 bytes a texel, which a size from the
        // format alone would read past.
        let _ = image_len(4, 4, GL_RGBA, 0x8363);
    }

    #[test]
    #[should_panic(expected = "only RGBA and ALPHA")]
    fn image_len_refuses_an_unknown_format() {
        let _ = image_len(4, 4, 0x1907, GL_UNSIGNED_BYTE); // GL_RGB
    }

    #[test]
    #[should_panic(expected = "rows GL pads")]
    fn image_len_refuses_padded_rows() {
        let _ = image_len(3, 2, GL_ALPHA, GL_UNSIGNED_BYTE);
    }

    #[test]
    fn mat4s_reads_whole_matrices_and_none_for_no_count() {
        let m: Vec<f32> = (0..32).map(|i| i as f32).collect();
        // SAFETY: `m` holds two matrices.
        assert_eq!(unsafe { mat4s(m.as_ptr(), 2) }, &m[..]);
        // SAFETY: one of the two.
        assert_eq!(unsafe { mat4s(m.as_ptr(), 1) }, &m[..16]);
        // A null pointer with no matrices to read, as GL allows.
        // SAFETY: count is not positive, so nothing is read.
        assert!(unsafe { mat4s(std::ptr::null(), 0) }.is_empty());
        // SAFETY: as above.
        assert!(unsafe { mat4s(std::ptr::null(), -1) }.is_empty());
    }

    /// Runs `write_log` into a buffer of `max_len` bytes (one for 0, as
    /// GL's contract asks), returning what landed and the reported length.
    fn log_into(log: &str, max_len: GlSizei) -> (Vec<u8>, GlSizei) {
        let mut out = vec![0x55u8; from_gl_size(max_len.max(1))];
        let mut len: GlSizei = -1;
        // SAFETY: `out` holds max(max_len, 1) bytes and `len` is one write.
        unsafe { write_log(log, max_len, &mut len, out.as_mut_ptr().cast()) };
        (out, len)
    }

    #[test]
    fn write_log_fits_truncates_and_always_terminates() {
        assert_eq!(log_into("ok", 8), (b"ok\0\x55\x55\x55\x55\x55".to_vec(), 2));
        // Exactly room for the text and its NUL.
        assert_eq!(log_into("error", 6), (b"error\0".to_vec(), 5));
        // One short: the last byte goes to the NUL.
        assert_eq!(log_into("error", 5), (b"erro\0".to_vec(), 4));
        // No room at all but the NUL.
        assert_eq!(log_into("error", 0), (b"\0".to_vec(), 0));
        assert_eq!(log_into("error", -3), (b"\0".to_vec(), 0));
    }

    #[test]
    fn write_log_skips_a_null_length() {
        let mut out = [0x55u8; 4];
        // SAFETY: `out` holds 4 bytes; a null `len` is allowed.
        unsafe { write_log("abc", 4, std::ptr::null_mut(), out.as_mut_ptr().cast()) };
        assert_eq!(&out, b"abc\0");
    }
}
