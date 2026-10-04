//! The identifiers that travel side by side through the library, the
//! fetch thread and the slideshow, one type each, so passing an album's
//! id where an asset's is wanted is a compile error, not a 404 or a
//! wrong row. Each is made where its value enters (a DB row, the Immich
//! JSON, a file's hash) and carried as the type from there on.
//!
//! SQL binding stays in raam-engine: the orphan rule keeps rusqlite's
//! `ToSql`/`FromSql` from being implemented there for these types, and
//! this crate takes no dependencies, so the DB code binds `.get()` /
//! `.as_str()` and wraps what it reads.

use std::fmt;

/// The frame's own row for a photo or clip (`asset.id`). Stable while the
/// row lives; a resync that drops and re-adds an item gives it a new one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct AssetId(i64);

impl AssetId {
    pub const fn new(row: i64) -> Self {
        Self(row)
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub struct $name(String);

        impl $name {
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self(id.to_string())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id! {
    /// A provider's stable id for an item (`asset.remote_id`): an Immich
    /// asset UUID, or a local file's content hash. Unique within one
    /// source, not across them.
    RemoteId
}

string_id! {
    /// An Immich album's UUID (`collection.remote_id`).
    AlbumId
}

string_id! {
    /// An Immich user's UUID, from `GET /api/users/me`: whose library the
    /// frame has synced.
    UserId
}

string_id! {
    /// What curation (hidden, Fill/Fit, focus) is keyed by: the original
    /// file's SHA-1 in lowercase hex when known, else `source:remote_id`.
    /// A photo in both sources shares one, and a renamed local file keeps
    /// its own.
    CurationKey
}

impl CurationKey {
    /// The first eight characters, for a label or a log line.
    pub fn short(&self) -> &str {
        let end = self
            .0
            .char_indices()
            .nth(8)
            .map_or(self.0.len(), |(i, _)| i);
        &self.0[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_cuts_at_eight_characters_and_never_inside_one() {
        assert_eq!(CurationKey::new("0123456789abcdef").short(), "01234567");
        assert_eq!(CurationKey::new("abc").short(), "abc");
        assert_eq!(CurationKey::new("ééééééééé").short(), "éééééééé");
    }

    #[test]
    fn ids_print_as_their_value() {
        assert_eq!(AssetId::new(42).to_string(), "42");
        assert_eq!(AlbumId::new("a-b").to_string(), "a-b");
    }
}
