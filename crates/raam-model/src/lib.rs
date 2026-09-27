//! The leaf crate: settings, value enums, media types, named limits,
//! typed errors. No dependencies, no platform types, no I/O.
//! Everything else depends on this; this depends on nothing.
//!
//! Value types living in GPU and UI modules are what create type-level
//! dependency cycles; keeping them here breaks those cycles, so this crate
//! stays a leaf forever (docs/ARCHITECTURE.md).

mod error;
pub mod limits;
mod media;
mod settings;
mod stats;
mod time;

pub use error::*;
pub use media::*;
pub use settings::*;
pub use stats::*;
pub use time::*;
