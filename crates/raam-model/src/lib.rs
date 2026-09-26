//! The leaf crate: settings, value enums, media types, named limits,
//! typed errors. No dependencies, no platform types, no I/O.
//! Everything else depends on this; this depends on nothing.
//!
//! The experiments' three type-level dependency cycles all came from value
//! types living in GPU and UI modules; this crate is what breaks them, so
//! it exists from day one and stays a leaf forever (docs/ARCHITECTURE.md).

pub mod limits;
mod media;
mod settings;
mod stats;
mod time;

pub use media::*;
pub use settings::*;
pub use stats::*;
pub use time::*;
