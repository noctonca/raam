//! Everything that draws and decides: the egui UI, the slideshow pipeline
//! (collage, compose, Ken Burns, transitions, the clock/weather overlay),
//! and the App controller (app.rs). Synchronous, single-threaded,
//! wasm-clean: no threads, no I/O, no `std::time::Instant`. Time, tiles
//! and video enter through the seams (docs/ARCHITECTURE.md); the GL layer
//! is the cfg-selected module gl.rs.

pub mod app;
pub mod atlas;
pub mod clock;
pub mod collage;
pub mod frame_ui;
pub mod gallery;
pub mod gl;
pub mod icons;
pub mod kit;
pub mod network;
pub mod num;
pub mod overlay;
pub mod painter;
mod palette;
pub mod schedule;
pub mod seams;
pub mod slideshow;
pub mod source;
pub mod store;
pub mod switches;
pub mod theme;
pub mod transitions;
pub mod video;
pub mod weather_icons;

/// Under Miri, every allocation is aligned to 16, as the real allocators
/// do. Miri hands an allocation only the alignment its layout asks for,
/// and egui's text rasteriser, vello_cpu, casts a `Vec<u8>` to `&[u32]`
/// with bytemuck, which panics unless the bytes happen to sit 4-aligned
/// (raam#85). This lets Miri go on through the app tests that draw text.
#[cfg(all(test, miri))]
mod miri_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};

    const ALIGN: usize = 16;

    struct Aligned;

    fn widen(layout: Layout) -> Layout {
        // A layout's alignment and size are already valid, and 16 is a
        // power of two, so only an overflowing size could fail here.
        layout
            .align_to(ALIGN)
            .expect("layout too large to align to 16")
    }

    // SAFETY: every call forwards to System with the same widened layout,
    // so each block is freed or resized with the layout it was made with.
    unsafe impl GlobalAlloc for Aligned {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: widening keeps the caller's non-zero size.
            unsafe { System.alloc(widen(layout)) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: widening keeps the caller's non-zero size.
            unsafe { System.alloc_zeroed(widen(layout)) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: ptr came from alloc with this layout, widened alike.
            unsafe { System.dealloc(ptr, widen(layout)) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: ptr came from alloc with this layout, widened alike,
            // and System keeps the widened alignment when it moves it.
            unsafe { System.realloc(ptr, widen(layout), new_size) }
        }
    }

    #[global_allocator]
    static GLOBAL: Aligned = Aligned;
}
