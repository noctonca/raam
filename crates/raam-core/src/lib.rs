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
