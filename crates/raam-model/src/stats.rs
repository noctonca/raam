//! What the settings panel shows about the library, refreshed after each
//! library step and published behind a version counter.

/// An album as the picker shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct AlbumRow {
    pub remote_id: String,
    pub name: String,
    /// The server's count (videos included).
    pub asset_count: i64,
    pub selected: bool,
    /// Picked, but no longer on the server (deleted, or unshared).
    pub missing: bool,
    /// Assets synced from it.
    pub synced: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HiddenItem {
    pub key: String,
    pub label: String,
}

/// What the settings panel shows, refreshed after each library step.
#[derive(Clone, Default, PartialEq)]
pub struct Stats {
    pub immich_assets: i64,
    pub immich_cached: i64,
    pub cache_bytes: i64,
    pub cap_bytes: i64,
    pub local_assets: i64,
    pub local_ready: i64,
    pub shared: i64,
    pub local_dir: String,
    pub local_note: String,
    pub immich_note: String,
    pub prefetch_note: String,
    pub free_bytes: u64,
    pub hidden: Vec<HiddenItem>,
    pub export_note: String,
    /// The album list as last fetched (saved, so it shows offline).
    pub albums: Vec<AlbumRow>,
    pub albums_note: String,
    /// Videos in the enabled sources, how many are ready to play, how
    /// many this frame can't play, and why.
    pub videos: i64,
    pub videos_ready: i64,
    pub videos_unplayable: i64,
    pub unplayable_reasons: Vec<String>,
}
