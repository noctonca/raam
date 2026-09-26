//! Everything that draws and decides: the egui UI, the slideshow pipeline
//! (collage, compose, Ken Burns, transitions, the clock/weather overlay),
//! and — after migration step 5b — the App controller. Synchronous,
//! single-threaded, wasm-clean: no threads, no I/O, no
//! `std::time::Instant`. Time, tiles and video enter through the seams
//! (docs/ARCHITECTURE.md); the GL layer is the cfg-selected module gl.rs.

pub mod atlas;
pub mod clock;
pub mod collage;
pub mod gl;
pub mod overlay;
pub mod painter;
pub mod schedule;
pub mod seams;
pub mod slideshow;
pub mod source;
pub mod switches;
pub mod transitions;
pub mod ui;
pub mod video;
pub mod weather_icons;
