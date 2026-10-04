//! The WebGL1 linkage's raw-pointer reads and writes: GL's pointer
//! arguments turned into slices, and info logs copied out. Kept apart, like
//! `table`, so their tests run natively, and under Miri (CI's miri job),
//! which can't run the linkage itself.

use super::{GlSizei, gl_sizei};
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
    let n = log.len().min((max_len.max(1) - 1) as usize);
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
        let mut out = vec![0x55u8; max_len.max(1) as usize];
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
