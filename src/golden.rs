//! The golden suite's tools (scripts/goldens.sh): a shot's pixels as a
//! hash, PNG files in and out, and `--diff`, which compares two shots
//! without a window and draws where they differ.
use std::path::Path;

/// An 8-bit RGB image, rows top-down.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
}

/// FNV-1a, 64-bit, over the size and the pixels: a regression tripwire,
/// not a defence against anyone, so ten lines beat a dependency. The size
/// goes in so a resized shot can't collide with the one it replaced.
pub fn hash(img: &Image) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let size = [img.w.to_le_bytes(), img.h.to_le_bytes()];
    for b in size.iter().flatten().chain(&img.rgb) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn write_png(path: &Path, img: &Image) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.w, img.h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    // Half the default's size on a UI shot, for a few ms more a file.
    enc.set_compression(png::Compression::Best);
    enc.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&img.rgb))
        .map_err(|e| e.to_string())
}

/// Any 8-bit PNG as RGB, alpha dropped: a browser's canvas export is RGBA.
pub fn read_png(path: &Path) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let px = &buf[..info.buffer_size()];
    let rgb = match info.color_type {
        png::ColorType::Rgb => px.to_vec(),
        png::ColorType::Rgba => px
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&v| [v, v, v]).collect(),
        png::ColorType::GrayscaleAlpha => px
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0]; 3])
            .collect(),
        png::ColorType::Indexed => return Err(format!("{}: palette not expanded", path.display())),
    };
    Ok(Image {
        w: info.width,
        h: info.height,
        rgb,
    })
}

/// How two same-sized shots differ.
struct Report {
    /// Pixels whose largest channel difference is over the tolerance.
    over: usize,
    /// Pixels that differ at all.
    differ: usize,
    /// The largest channel difference anywhere.
    max: u8,
    /// The box around the pixels over the tolerance: x0, y0, x1, y1.
    bbox: Option<[u32; 4]>,
}

fn compare(a: &Image, b: &Image, tolerance: u8, mut mark: impl FnMut(usize, bool)) -> Report {
    let mut r = Report {
        over: 0,
        differ: 0,
        max: 0,
        bbox: None,
    };
    let (pa, pb) = (a.rgb.as_chunks::<3>().0, b.rgb.as_chunks::<3>().0);
    for (i, (p, q)) in pa.iter().zip(pb).enumerate() {
        let d = (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0);
        r.max = r.max.max(d);
        r.differ += usize::from(d > 0);
        mark(i, d > tolerance);
        if d > tolerance {
            r.over += 1;
            let i = u32::try_from(i).expect("a shot's pixel index fits u32");
            let (x, y) = (i % a.w, i / a.w);
            r.bbox = Some(match r.bbox {
                None => [x, y, x, y],
                Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
            });
        }
    }
    r
}

/// `--diff A B`: prints how B differs from A and exits 0 when no pixel
/// differs by more than `tolerance` levels in any channel, 1 when some do
/// (or the sizes differ), 2 when a file can't be read. `out` gets B dimmed
/// to grey with every pixel over the tolerance in magenta.
pub fn run_diff(a: &Path, b: &Path, tolerance: u8, out: Option<&Path>) -> i32 {
    let (ia, ib) = match (read_png(a), read_png(b)) {
        (Ok(ia), Ok(ib)) => (ia, ib),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if (ia.w, ia.h) != (ib.w, ib.h) {
        println!("sizes differ: {}x{} and {}x{}", ia.w, ia.h, ib.w, ib.h);
        return 1;
    }
    let mut marked = out.map(|_| ib.rgb.clone());
    let r = compare(&ia, &ib, tolerance, |i, over| {
        if let Some(px) = marked.as_mut() {
            let p = &mut px[i * 3..i * 3 + 3];
            if over {
                p.copy_from_slice(&[255, 0, 255]);
            } else {
                let luma = (u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2])) / 10;
                p.fill(64 + u8::try_from(luma / 4).expect("a luma byte over 4"));
            }
        }
    });
    let total = ia.w as usize * ia.h as usize;
    match r.bbox {
        None if r.max == 0 => println!("identical"),
        None => println!(
            "same within {tolerance} levels: {} of {total} pixels differ, by {} at most",
            r.differ, r.max
        ),
        Some([x0, y0, x1, y1]) => println!(
            "{} of {total} pixels differ by more than {tolerance} levels (by {} at most), \
             within {x0},{y0} to {x1},{y1}",
            r.over, r.max
        ),
    }
    if let (Some(path), Some(rgb)) = (out, marked) {
        let img = Image { rgb, ..ib };
        if let Err(e) = write_png(path, &img) {
            eprintln!("{}: {e}", path.display());
            return 2;
        }
    }
    i32::from(r.over > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, v: u8) -> Image {
        Image {
            w,
            h,
            rgb: vec![v; (w * h * 3) as usize],
        }
    }

    #[test]
    fn the_hash_sees_one_level_and_the_size() {
        let a = image(4, 2, 10);
        let mut b = image(4, 2, 10);
        assert_eq!(hash(&a), hash(&b));
        b.rgb[13] = 11;
        assert_ne!(hash(&a), hash(&b));
        // The same bytes in another shape.
        assert_ne!(hash(&a), hash(&image(2, 4, 10)));
    }

    #[test]
    fn compare_boxes_only_what_is_over_the_tolerance() {
        let a = image(4, 3, 100);
        let mut b = image(4, 3, 100);
        // (1, 0) off by 2, (3, 2) by 5, (0, 2) by 9 in blue.
        b.rgb[3] = 102;
        b.rgb[(2 * 4 + 3) * 3 + 1] = 95;
        b.rgb[(2 * 4) * 3 + 2] = 109;
        let r = compare(&a, &b, 2, |_, _| {});
        assert_eq!((r.over, r.differ, r.max), (2, 3, 9));
        assert_eq!(r.bbox, Some([0, 2, 3, 2]));
        let r = compare(&a, &b, 9, |_, _| {});
        assert_eq!((r.over, r.bbox), (0, None));
    }
}
