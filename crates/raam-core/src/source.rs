//! The tile-source seam: how the slideshow gets its collages. Whatever
//! plans and fetches media implements `TileSource`; the slideshow sees
//! nothing else of it. raam-engine's fetch thread implements it natively;
//! the web host implements it directly over browser fetch. The types that
//! cross the seam live in raam-model (a `Plan` and its `MediaItem`s go
//! core -> source again on Prev, carrying the fetch side's fields through
//! the core untouched).

use crate::{clock, collage, num};
pub use raam_model::{MediaItem, Photo, Plan, SourceKind, TilePhoto, VideoClip};
use std::time::Duration;

/// The slideshow's side of the seam, polled from the render loop. The
/// source parks one plan and then its tiles one at a time (at most one
/// decoded preview in memory at once), and plans nothing new until
/// `consumed`.
pub trait TileSource {
    /// Takes a newly parked plan.
    #[must_use = "a dropped plan leaves the source waiting, and the slideshow stalls"]
    fn take_plan(&self) -> Option<Plan>;

    /// Takes the parked tile if it belongs to plan `seq`. A tile from an
    /// abandoned plan is dropped.
    #[must_use = "a dropped tile leaves its plan unfinished, and the slideshow stalls"]
    fn take_tile(&self, seq: u64) -> Option<TilePhoto>;

    /// Takes the seq of a plan whose fetch failed; that plan won't finish.
    #[must_use = "a failed plan not dropped is waited on for good"]
    fn take_failed(&self) -> Option<u64>;

    /// The slideshow has shown (or discarded) the collage it built; the
    /// source may plan the next one.
    fn consumed(&self);

    /// Asks for an earlier collage again (Prev), abandoning whatever is
    /// being fetched.
    fn request(&self, plan: Plan);

    /// The parked tile for plan `seq` is a clip (left parked).
    fn tile_is_clip(&self, seq: u64) -> bool;

    /// Clips are passed over in planning while the slideshow backs off
    /// after a decoder failure.
    fn set_skip_videos(&self, skip: bool);
}

/// Halves a decoded preview on the source's side of the seam (2x2 box
/// average) while it stays at least the tile's cover size, so the render
/// thread uploads a near-tile-sized texture (a 420x398 tile needs about a
/// quarter of a 1440x1920 preview's pixels) instead of stalling ~100 ms
/// per tile on `glTexImage2D`. The GPU's final copy then shrinks by under
/// 2x, where one bilinear tap doesn't alias. (The image crate's
/// triangle-filter resize did the same job at 0.35-2.4 s per tile on the
/// frame's CPU.) Fill needs the cover scale; Fit is smaller, so it is
/// enough for both. Every source runs it, so a tile reaches the GPU the
/// same size whichever host fetched it.
///
/// # Panics
///
/// If `rect` is empty. A 0x0 tile's cover size is 0x0, which every
/// halving still covers, so the loop would never end; a tile empty in one
/// direction only means the screen or layout is broken.
pub fn shrink_to_cover(photo: &mut Photo, rect: collage::Rect) -> Duration {
    assert!(
        rect.w > 0 && rect.h > 0,
        "shrink_to_cover into an empty tile {}x{}",
        rect.w,
        rect.h
    );
    let start = clock::now();
    // An empty preview has nothing to halve. Its cover size is 0 * inf =
    // NaN, which casts to 0, so a 0x0 one would "still cover" at every
    // halving and the loop would never end.
    if photo.width == 0 || photo.height == 0 {
        return clock::elapsed(start);
    }
    let side = |n: i32| u32::try_from(n).expect("a tile's side, asserted positive above");
    let (tw, th) = (side(rect.w), side(rect.h));
    let s = (tw as f32 / photo.width as f32).max(th as f32 / photo.height as f32);
    let (cover_w, cover_h) = (
        num::sat_u32((photo.width as f32 * s).ceil()),
        num::sat_u32((photo.height as f32 * s).ceil()),
    );
    while photo.width / 2 >= cover_w && photo.height / 2 >= cover_h {
        let (w, h) = (photo.width / 2, photo.height / 2);
        let src = &photo.rgba;
        let stride = photo.width as usize * 4;
        let mut out = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            let r0 = 2 * y * stride;
            let r1 = r0 + stride;
            for x in 0..w as usize {
                let (c, o) = (8 * x, 4 * (y * w as usize + x));
                for k in 0..4 {
                    let sum = u16::from(src[r0 + c + k])
                        + u16::from(src[r0 + c + 4 + k])
                        + u16::from(src[r1 + c + k])
                        + u16::from(src[r1 + c + 4 + k]);
                    out[o + k] = ((sum + 2) / 4) as u8;
                }
            }
        }
        photo.rgba = out;
        photo.width = w;
        photo.height = h;
    }
    clock::elapsed(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(w: u32, h: u32) -> Photo {
        Photo {
            asset_id: raam_model::AssetId::new(1),
            key: "k".into(),
            width: w,
            height: h,
            rgba: (0..w * h * 4).map(|i| (i % 251) as u8).collect(),
            face_focal: None,
            fill_centre: (0.5, 0.5),
            video: None,
        }
    }

    #[test]
    fn a_preview_halves_while_it_still_covers_the_tile() {
        clock::fake::install();
        // 1440x960 into a 420x398 tile: the cover scale is 597x398, so one
        // halving (720x480) still covers and a second (360x240) wouldn't.
        let mut p = photo(1440, 960);
        let rect = collage::Rect {
            x: 0,
            y: 0,
            w: 420,
            h: 398,
        };
        shrink_to_cover(&mut p, rect);
        assert_eq!((p.width, p.height), (720, 480));
        assert_eq!(p.rgba.len(), 720 * 480 * 4);
        // Already near the tile: untouched.
        let mut q = photo(800, 500);
        shrink_to_cover(&mut q, rect);
        assert_eq!((q.width, q.height), (800, 500));
    }

    #[test]
    fn a_halving_averages_each_two_by_two_block() {
        clock::fake::install();
        let mut p = photo(4, 2);
        let full = collage::Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        let src = p.rgba.clone();
        shrink_to_cover(&mut p, full);
        // 4x2 -> 2x1 (then 1x0 would not cover a 1x1 tile).
        assert_eq!((p.width, p.height), (2, 1));
        let at = |x: usize, y: usize, k: usize| u16::from(src[4 * (y * 4 + x) + k]);
        let want = ((at(0, 0, 0) + at(1, 0, 0) + at(0, 1, 0) + at(1, 1, 0) + 2) / 4) as u8;
        assert_eq!(p.rgba[0], want);
    }

    #[test]
    #[should_panic(expected = "empty tile")]
    fn an_empty_tile_is_refused() {
        clock::fake::install();
        // A 0x0 tile covers at 0x0, which halving never drops below.
        let mut p = photo(4, 2);
        let empty = collage::Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        shrink_to_cover(&mut p, empty);
    }

    /// A 0x0 preview is left as it is; it once looped for ever (found by
    /// the properties below).
    #[test]
    fn an_empty_preview_is_left_alone() {
        clock::fake::install();
        let tile = collage::Rect {
            x: 0,
            y: 0,
            w: 420,
            h: 398,
        };
        for (w, h) in [(0, 0), (0, 7), (7, 0)] {
            let mut p = photo(w, h);
            shrink_to_cover(&mut p, tile);
            assert_eq!((p.width, p.height, p.rgba.len()), (w, h, 0));
        }
    }

    // Not under miri: proptest reads the working directory (its
    // regressions file) and the OS's randomness, which miri's isolation
    // refuses, and miri is here for unsafe code, which these don't touch.
    #[cfg(not(miri))]
    mod properties {
        use super::*;
        use proptest::prelude::*;
        use std::panic::{AssertUnwindSafe, catch_unwind};

        fn tile(w: i32, h: i32) -> collage::Rect {
            collage::Rect { x: 0, y: 0, w, h }
        }

        /// A tile side: empty, small, screen-sized, or any i32 at all
        /// (negative and past any screen included).
        fn side() -> impl Strategy<Value = i32> {
            prop_oneof![0..=2i32, 1..=2000i32, any::<i32>()]
        }

        /// A preview side: up to 600, so a case stays a few ms in a debug
        /// build; 0 included (a decoder handing over nothing).
        fn preview_side() -> impl Strategy<Value = u32> {
            prop_oneof![0..=4u32, 0..=600u32]
        }

        /// The source's size scaled to just cover the tile, in f64
        /// (independent of the f32 arithmetic under test).
        fn cover_size(w: u32, h: u32, rect: collage::Rect) -> (f64, f64) {
            let s = (f64::from(rect.w) / f64::from(w)).max(f64::from(rect.h) / f64::from(h));
            (f64::from(w) * s, f64::from(h) * s)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            /// Any preview into any tile returns (the 0x0 tile once looped
            /// for ever), or panics if and only if the tile is empty.
            #[test]
            fn returns_or_refuses_only_an_empty_tile(
                w in preview_side(),
                h in preview_side(),
                tw in side(),
                th in side(),
            ) {
                clock::fake::install();
                let mut p = photo(w, h);
                let done = catch_unwind(AssertUnwindSafe(|| shrink_to_cover(&mut p, tile(tw, th))));
                prop_assert_eq!(done.is_err(), tw <= 0 || th <= 0);
            }

            /// The result is the preview halved k times in both directions
            /// together (k = 0 when it is already near the tile), so it
            /// stays inside the source, keeps its shape to within the
            /// halving's rounding, and has exactly its pixels' bytes.
            #[test]
            fn halves_whole_preview_in_step(
                w in preview_side(),
                h in preview_side(),
                tw in 1..=i32::MAX,
                th in 1..=i32::MAX,
            ) {
                clock::fake::install();
                let mut p = photo(w, h);
                shrink_to_cover(&mut p, tile(tw, th));
                let k = (0..32).find(|k| (w >> k, h >> k) == (p.width, p.height));
                prop_assert!(k.is_some(), "{}x{} -> {}x{}", w, h, p.width, p.height);
                prop_assert_eq!(p.rgba.len(), (p.width * p.height * 4) as usize);
                if k != Some(0) && p.width > 0 && p.height > 0 {
                    // Each floor loses under a pixel per side.
                    let source = f64::from(w) / f64::from(h);
                    let result = f64::from(p.width) / f64::from(p.height);
                    let slack = 1.0 / f64::from(p.width) + 1.0 / f64::from(p.height);
                    prop_assert!((result / source - 1.0).abs() <= slack * 1.01,
                        "{}x{} -> {}x{}", w, h, p.width, p.height);
                }
            }

            /// A halved preview still covers the tile (the GPU never has
            /// to enlarge it), and is the smallest halving that does.
            #[test]
            fn halves_to_the_smallest_cover(
                w in 1..=600u32,
                h in 1..=600u32,
                tw in 1..=2000i32,
                th in 1..=2000i32,
            ) {
                clock::fake::install();
                let rect = tile(tw, th);
                let mut p = photo(w, h);
                shrink_to_cover(&mut p, rect);
                let (cover_w, cover_h) = cover_size(w, h, rect);
                // One pixel of slack for the f32 arithmetic under test.
                if (p.width, p.height) != (w, h) {
                    prop_assert!(f64::from(p.width) >= cover_w - 1.0
                        && f64::from(p.height) >= cover_h - 1.0,
                        "{}x{} -> {}x{} under the {}x{} cover", w, h, p.width, p.height,
                        cover_w, cover_h);
                }
                prop_assert!(f64::from(p.width / 2) < cover_w + 1.0
                    || f64::from(p.height / 2) < cover_h + 1.0,
                    "{}x{} -> {}x{} could halve again over the {}x{} cover", w, h,
                    p.width, p.height, cover_w, cover_h);
            }

            /// Averaging keeps a flat colour flat: no rounding drift, no
            /// channel bleeding into its neighbour.
            #[test]
            fn a_flat_colour_stays_that_colour(
                w in 1..=256u32,
                h in 1..=256u32,
                tw in 1..=64i32,
                th in 1..=64i32,
                rgba: [u8; 4],
            ) {
                clock::fake::install();
                let mut p = photo(w, h);
                p.rgba = rgba.repeat((w * h) as usize);
                shrink_to_cover(&mut p, tile(tw, th));
                let (pixels, rest) = p.rgba.as_chunks::<4>();
                prop_assert!(rest.is_empty() && pixels.iter().all(|px| *px == rgba));
            }
        }
    }
}
