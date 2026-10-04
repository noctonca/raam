//! The tile-source seam: how the slideshow gets its collages. Whatever
//! plans and fetches media implements `TileSource`; the slideshow sees
//! nothing else of it. raam-engine's fetch thread implements it natively;
//! the web host implements it directly over browser fetch. The types that
//! cross the seam live in raam-model (a `Plan` and its `MediaItem`s go
//! core -> source again on Prev, carrying the fetch side's fields through
//! the core untouched).

use crate::{clock, collage};
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
    let (tw, th) = (rect.w as u32, rect.h as u32);
    let s = (tw as f32 / photo.width as f32).max(th as f32 / photo.height as f32);
    let (cover_w, cover_h) = (
        (photo.width as f32 * s).ceil() as u32,
        (photo.height as f32 * s).ceil() as u32,
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
                    let sum = src[r0 + c + k] as u16
                        + src[r0 + c + 4 + k] as u16
                        + src[r1 + c + k] as u16
                        + src[r1 + c + 4 + k] as u16;
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
        let at = |x: usize, y: usize, k: usize| src[4 * (y * 4 + x) + k] as u16;
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
}
