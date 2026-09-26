//! The tile-source seam: how the slideshow gets its collages. Whatever
//! plans and fetches media implements `TileSource`; the slideshow sees
//! nothing else of it. raam-engine's fetch thread implements it natively;
//! the web host implements it directly over browser fetch. The types that
//! cross the seam live in raam-model (a `Plan` and its `MediaItem`s go
//! core -> source again on Prev, carrying the fetch side's fields through
//! the core untouched).

pub use raam_model::{MediaItem, Photo, Plan, SourceKind, TilePhoto, VideoClip};

/// The slideshow's side of the seam, polled from the render loop. The
/// source parks one plan and then its tiles one at a time (at most one
/// decoded preview in memory at once), and plans nothing new until
/// `consumed`.
pub trait TileSource {
    /// Takes a newly parked plan.
    fn take_plan(&self) -> Option<Plan>;

    /// Takes the parked tile if it belongs to plan `seq`. A tile from an
    /// abandoned plan is dropped.
    fn take_tile(&self, seq: u64) -> Option<TilePhoto>;

    /// Takes the seq of a plan whose fetch failed; that plan won't finish.
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
