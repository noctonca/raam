//! The media types: what a provider lists (`MediaRef`), what planning
//! works from (`MediaItem`, `Plan`) and what crosses the `TileSource`
//! seam back to the pipeline (`Photo`, `TilePhoto`, `VideoClip`).

/// Where a photo or clip comes from. Stored in the DB by name (`as_str`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    Immich,
    Local,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Immich => "immich",
            SourceKind::Local => "local",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "local" {
            SourceKind::Local
        } else {
            SourceKind::Immich
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaKind {
    Photo,
    /// Played by the host's video stack, never put in a collage.
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Photo => "image",
            MediaKind::Video => "video",
        }
    }
}

/// Where to look in a photo: Frameo's fill-crop centre and the largest face
/// (the Ken Burns target), both 0..1 with `v=0` at the top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    pub centre: (f32, f32),
    pub face: Option<(f32, f32)>,
}

impl Focus {
    /// Frameo's rule over a detector's faces, each `(area, centre)` with
    /// the centre in 0..1 fractions, `v=0` at the top: the fill-crop centre
    /// is the mean of every face centre with `v` moved 0.05 lower (headroom
    /// above the faces) and capped at 1.0, and the Ken Burns target is the
    /// largest face's centre. No faces: the middle, and no target.
    pub fn from_faces(faces: &[(f32, (f32, f32))]) -> Self {
        let face = faces.iter().max_by(|a, b| a.0.total_cmp(&b.0)).map(|f| f.1);
        if faces.is_empty() {
            return Self {
                centre: (0.5, 0.5),
                face,
            };
        }
        let n = faces.len() as f32;
        let u = faces.iter().map(|f| f.1.0).sum::<f32>() / n;
        let v = faces.iter().map(|f| f.1.1).sum::<f32>() / n;
        Self {
            centre: (u, (v + 0.05).min(1.0)),
            face,
        }
    }
}

/// One item as a provider lists it. It carries only what the renderer
/// needs — a stable id, the size after rotation, the capture time (UTC ms,
/// normalised at the provider boundary), the kind and an optional focus —
/// so collage planning, Fill/Fit and Ken Burns never know where a photo
/// came from.
#[derive(Clone, Debug)]
pub struct MediaRef {
    /// The provider's stable id: an Immich asset UUID, or a local file's
    /// content hash.
    pub id: String,
    /// SHA-1 of the original file, lowercase hex: the curation key, so a
    /// renamed local file keeps its curation and a photo in both sources
    /// shares one.
    pub sha1: Option<String>,
    /// Local: where the file is now (not its identity).
    pub location: Option<String>,
    pub width: u32,
    pub height: u32,
    pub taken_at_ms: Option<i64>,
    pub kind: MediaKind,
    /// `None` = centre it. Immich fills it lazily (`fetch_focus`), since
    /// faces are one request per asset.
    pub focus: Option<Focus>,
    /// Local: (bytes, mtime ms), to skip rehashing unchanged files.
    pub stamp: Option<(i64, i64)>,
    /// The provider's collections (Immich album ids) this item was listed
    /// from; empty for the folder.
    pub collections: Vec<String>,
}

impl MediaRef {
    /// What `fetch_preview`/`fetch_focus` need, rebuilt from a DB row.
    pub fn stored(id: String, location: Option<String>) -> Self {
        Self {
            id,
            sha1: None,
            location,
            width: 0,
            height: 0,
            taken_at_ms: None,
            kind: MediaKind::Photo,
            focus: None,
            stamp: None,
            collections: Vec::new(),
        }
    }
}

/// What a probe of a clip's container reads, without decoding anything.
/// Whether a given frame's decoder can play it is the host's judgment
/// (device capability data lives with the host, `MediaProbe::unplayable`),
/// not this type's.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipInfo {
    pub mime: String,
    /// As coded (before rotation).
    pub coded_w: u32,
    pub coded_h: u32,
    /// The container's rotation, 0/90/180/270 (`rotation-degrees`).
    pub rotation: i32,
    pub duration_us: i64,
    pub has_audio: bool,
    pub audio: Option<String>,
}

impl ClipInfo {
    /// The size as shown, after rotation.
    pub fn display(&self) -> (u32, u32) {
        if self.rotation % 180 != 0 {
            (self.coded_h, self.coded_w)
        } else {
            (self.coded_w, self.coded_h)
        }
    }
}

/// A photo or clip the slideshow may show, with what planning a collage
/// needs: the oriented metadata size, and enough for the source to fetch
/// its pixels later (also again, for a Prev-requested plan).
#[derive(Clone, Debug)]
pub struct MediaItem {
    pub asset: i64,
    /// The curation key (SHA-1 when known).
    pub key: String,
    pub source: SourceKind,
    pub remote_id: String,
    pub width: u32,
    pub height: u32,
    /// A video clip (always shown alone, never in a collage).
    pub video: bool,
    /// Local: the file (a local video plays from where it is).
    pub location: Option<String>,
}

/// One collage to build: `layout` indexes the core's collage layouts
/// (`None` = a single full-screen photo) and `assets[slot]` fills that slot.
#[derive(Clone, Debug)]
pub struct Plan {
    pub seq: u64,
    pub layout: Option<usize>,
    pub assets: Vec<MediaItem>,
}

impl Plan {
    pub fn ids(&self) -> Vec<i64> {
        self.assets.iter().map(|a| a.asset).collect()
    }
}

#[derive(Clone, Debug)]
pub struct VideoClip {
    pub path: String,
    pub info: ClipInfo,
}

pub struct Photo {
    pub asset_id: i64,
    /// The curation key (SHA-1), for Fill/Fit and Hide.
    pub key: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Ken Burns target: the largest face's centre (0..1, `v=0` at the top).
    pub face_focal: Option<(f32, f32)>,
    /// Fill-frame crop centre, Frameo's rule: the mean of every face centre,
    /// `v` nudged down by 0.05 (capped at 1.0), or (0.5, 0.5) with no faces.
    pub fill_centre: (f32, f32),
    /// A clip rather than pixels (`rgba` is empty; `width`/`height` are its
    /// size as shown). The render thread decodes its first frame.
    pub video: Option<VideoClip>,
}

pub struct TilePhoto {
    pub seq: u64,
    pub slot: usize,
    pub photo: Photo,
}

#[cfg(test)]
mod tests {
    use super::Focus;

    #[test]
    fn no_faces_is_the_middle_with_no_target() {
        assert_eq!(
            Focus::from_faces(&[]),
            Focus {
                centre: (0.5, 0.5),
                face: None
            }
        );
    }

    #[test]
    fn faces_give_frameos_fill_centre_and_the_largest_as_target() {
        // Two faces: the mean centre, 0.05 lower; the bigger one is aimed at.
        let f = Focus::from_faces(&[(0.01, (0.2, 0.3)), (0.04, (0.6, 0.5))]);
        assert!((f.centre.0 - 0.4).abs() < 1e-6 && (f.centre.1 - 0.45).abs() < 1e-6);
        assert_eq!(f.face, Some((0.6, 0.5)));
        // The headroom nudge never leaves the photo.
        let low = Focus::from_faces(&[(0.02, (0.5, 0.98))]);
        assert_eq!(low.centre, (0.5, 1.0));
    }
}
