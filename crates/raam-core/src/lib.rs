//! Everything that draws and decides: the egui UI (theme, kit, frame_ui,
//! gallery), the slideshow pipeline (collage, compose, Ken Burns,
//! transitions, overlay), and the App controller. Synchronous,
//! single-threaded, wasm-clean: no threads, no I/O, no `std::time::Instant`.
//! Time, tiles and video enter through the seams in docs/ARCHITECTURE.md.
