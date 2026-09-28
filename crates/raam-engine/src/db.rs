//! The frame's SQLite database: `source`, `asset` (what the providers
//! offer, rebuildable from them), `cached_file` (every file we keep on
//! disk, so eviction and deletion are one code path), `curation` (the
//! user's overlay — hidden, Fill/Fit, focus — keyed by the photo's SHA-1,
//! so a renamed local file keeps it and a photo in both sources shares
//! it; sync never touches it and nothing is written back to Immich),
//! `collection`/`collection_asset` (Immich albums and picks), `setting`,
//! `schedule`, `credential` and `playback`.
//!
//! One `Connection` behind a `Mutex`, shared by the library and fetch
//! threads and the writer thread; the render thread never locks it (it
//! reads everything once at startup and sends its writes to the writer).
//! WAL with synchronous=NORMAL: a power cut can lose the last commit but
//! never corrupt the file — the frame survived dozens of dirty
//! power-offs this way. Files are never unlinked under the lock: the
//! functions that drop cached files return their paths, and the caller
//! removes them after letting go.
//!
//! The schema starts at v1, with no upgrade path from the prototype's
//! database. Curation carries over through the JSON export/import
//! (`import_curation`).
use raam_core::clock;
use raam_core::store::transition_str;
use raam_model::limits;
use raam_model::{
    AlbumRow, ClockStyle, Corner, FitBackground, Focus, GapColour, HiddenItem, MediaItem,
    MediaKind, MediaRef, ScaleMode, Settings, SourceKind, TransitionChoice, VideoPlayback,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type Db = Arc<Mutex<Connection>>;

/// Schema versions, applied in order and recorded in `PRAGMA
/// user_version`.
const MIGRATIONS: &[&str] = &["CREATE TABLE source (
       id            INTEGER PRIMARY KEY,
       kind          TEXT NOT NULL CHECK (kind IN ('immich','local')),
       base_url      TEXT,                          -- Immich URL, or the local folder
       enabled       INTEGER NOT NULL DEFAULT 1,
       created_at_ms INTEGER NOT NULL
     );
     -- AUTOINCREMENT so ids are never reused: a plan or queue entry that
     -- outlives its row, or a cache file named by it, must never meet
     -- another photo.
     CREATE TABLE asset (
       id            INTEGER PRIMARY KEY AUTOINCREMENT,
       source_id     INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
       remote_id     TEXT NOT NULL,                 -- Immich asset UUID, or the file's SHA-1
       hash          TEXT,                          -- SHA-1 of the original, hex: the curation key
       location      TEXT,                          -- local: the file's path now
       kind          TEXT NOT NULL DEFAULT 'image' CHECK (kind IN ('image','video')),
       width         INTEGER NOT NULL,              -- post-orientation
       height        INTEGER NOT NULL,
       taken_at_ms   INTEGER,                       -- UTC epoch ms
       added_at_ms   INTEGER NOT NULL,
       file_bytes    INTEGER,                       -- local: change detection
       file_mtime_ms INTEGER,
       focus_x REAL, focus_y REAL,                  -- fill-crop centre from faces, 0..1
       face_x  REAL, face_y  REAL,                  -- largest face (Ken Burns target)
       faces_checked INTEGER NOT NULL DEFAULT 0,
       -- NULL until a clip has been opened, 1 once this frame's decoder
       -- can take it, 0 when it can't (with the reason); 0 keeps it out
       -- of the queue.
       playable      INTEGER,
       unplayable_reason TEXT,
       UNIQUE (source_id, remote_id)
     );
     CREATE INDEX asset_hash ON asset(hash);
     -- No foreign key: curation outlives the asset rows (a photo that
     -- leaves the album and comes back keeps it).
     CREATE TABLE curation (
       key           TEXT PRIMARY KEY,              -- asset.hash, else 'source:remote_id'
       hidden        INTEGER NOT NULL DEFAULT 0,
       scale_mode    TEXT CHECK (scale_mode IN ('fill','fit')),  -- NULL = the default
       focus_x REAL, focus_y REAL,                  -- NULL = the source's focus
       updated_at_ms INTEGER NOT NULL
     );
     CREATE TABLE cached_file (
       asset_id     INTEGER NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
       variant      TEXT NOT NULL CHECK (variant IN ('preview','thumb','original','video')),
       path         TEXT NOT NULL,
       bytes        INTEGER NOT NULL,
       last_used_ms INTEGER NOT NULL,
       PRIMARY KEY (asset_id, variant)
     ) WITHOUT ROWID;
     CREATE INDEX cached_file_lru ON cached_file(last_used_ms);
     -- A collection is an Immich album, listed from the server with its
     -- name and count so the picker works offline too; `selected` is the
     -- user's pick. `collection_asset` records which picked album each
     -- asset came from, so un-picking one drops exactly the assets no
     -- other picked album holds, at once and offline too.
     CREATE TABLE collection (
       id               INTEGER PRIMARY KEY,
       source_id        INTEGER NOT NULL REFERENCES source(id) ON DELETE CASCADE,
       remote_id        TEXT NOT NULL,              -- Immich album UUID
       name             TEXT NOT NULL,
       asset_count      INTEGER NOT NULL DEFAULT 0, -- as the server last said
       selected         INTEGER NOT NULL DEFAULT 0,
       missing_since_ms INTEGER,                    -- picked, but gone from the server's list
       listed_at_ms     INTEGER NOT NULL,
       UNIQUE (source_id, remote_id)
     );
     CREATE TABLE collection_asset (
       collection_id INTEGER NOT NULL REFERENCES collection(id) ON DELETE CASCADE,
       asset_id      INTEGER NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
       PRIMARY KEY (collection_id, asset_id)
     ) WITHOUT ROWID;
     CREATE INDEX collection_asset_asset ON collection_asset(asset_id);
     CREATE TABLE schedule (
       id        INTEGER PRIMARY KEY,
       kind      TEXT NOT NULL CHECK (kind IN ('sleep')),
       start_min INTEGER NOT NULL CHECK (start_min BETWEEN 0 AND 1439),
       end_min   INTEGER NOT NULL CHECK (end_min BETWEEN 0 AND 1439),
       days_mask INTEGER NOT NULL DEFAULT 127,
       enabled   INTEGER NOT NULL DEFAULT 1
     );
     CREATE TABLE setting (
       key   TEXT PRIMARY KEY,
       value TEXT NOT NULL,                         -- JSON
       scope TEXT NOT NULL CHECK (scope IN ('user','device')) DEFAULT 'user'
     );
     CREATE TABLE credential (
       source_id INTEGER PRIMARY KEY REFERENCES source(id) ON DELETE CASCADE,
       api_key   TEXT NOT NULL
     );
     CREATE TABLE playback (
       id            INTEGER PRIMARY KEY CHECK (id = 1),
       current_asset INTEGER REFERENCES asset(id) ON DELETE SET NULL,
       shuffle_seed  INTEGER,
       updated_at_ms INTEGER NOT NULL
     );"];

pub fn now_ms() -> i64 {
    clock::wall().as_millis() as i64
}

/// Opens (creating if needed) and migrates the database, and makes sure
/// both sources exist. Nothing is seeded from the build: the Immich
/// server and key are entered in the settings UI and live only here.
pub fn open(path: &Path, local_dir: &str) -> Result<Db, String> {
    let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .map_err(|e| format!("journal_mode: {e}"))?;
    // Foreign keys stay off until the migrations are done: a table rebuild
    // drops the old table, which with them on would cascade into every row
    // that points at it. The pragma is a no-op inside a transaction, so it
    // can't be switched per migration.
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = OFF; PRAGMA cache_size = -1024;",
    )
    .map_err(|e| format!("pragmas: {e}"))?;
    let version: usize = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(sql)
            .map_err(|e| format!("migration {}: {e}", i + 1))?;
        let broken = tx
            .prepare("PRAGMA foreign_key_check")
            .and_then(|mut s| s.exists([]))
            .map_err(|e| e.to_string())?;
        if broken {
            return Err(format!(
                "migration {}: foreign key check failed, rolled back",
                i + 1
            ));
        }
        tx.pragma_update(None, "user_version", i + 1)
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        log::info!("db: migrated to schema {}", i + 1);
    }
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| format!("pragmas: {e}"))?;
    let sqlite: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    log::info!(
        "db: {} open, SQLite {sqlite}, journal {mode}, schema {}",
        path.display(),
        MIGRATIONS.len()
    );

    if source_id(&conn, SourceKind::Immich).is_none() {
        conn.execute(
            "INSERT INTO source (kind, created_at_ms) VALUES ('immich', ?1)",
            params![now_ms()],
        )
        .map_err(|e| e.to_string())?;
    }
    if source_id(&conn, SourceKind::Local).is_none() {
        conn.execute(
            "INSERT INTO source (kind, base_url, created_at_ms) VALUES ('local', ?1, ?2)",
            params![local_dir, now_ms()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(Arc::new(Mutex::new(conn)))
}

pub fn source_id(conn: &Connection, kind: SourceKind) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM source WHERE kind = ?1",
        [kind.as_str()],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub struct SourceRow {
    pub base_url: String,
    pub enabled: bool,
}

pub fn source(conn: &Connection, kind: SourceKind) -> SourceRow {
    conn.query_row(
        "SELECT base_url, enabled FROM source WHERE kind = ?1",
        [kind.as_str()],
        |r| {
            Ok(SourceRow {
                base_url: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                enabled: r.get(1)?,
            })
        },
    )
    .unwrap_or(SourceRow {
        base_url: String::new(),
        enabled: true,
    })
}

pub fn immich_key(conn: &Connection) -> String {
    conn.query_row(
        "SELECT c.api_key FROM credential c JOIN source s ON s.id = c.source_id WHERE s.kind = 'immich'",
        [],
        |r| r.get(0),
    )
    .unwrap_or_default()
}

pub fn set_source_enabled(
    conn: &Connection,
    kind: SourceKind,
    enabled: bool,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE source SET enabled = ?1 WHERE kind = ?2",
        params![enabled, kind.as_str()],
    )
}

/// The local source's folder (the desktop's `--photos`). The next scan
/// lists it and drops what the old folder held; curation, keyed by the
/// photo, stays.
pub fn set_local_dir(conn: &Connection, dir: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE source SET base_url = ?1 WHERE kind = 'local'",
        [dir],
    )
}

/// The Immich server and key from settings. Either one empty is stored
/// as none, as on a fresh install.
pub fn set_immich_server(conn: &Connection, url: &str, key: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE source SET base_url = NULLIF(?1, '') WHERE kind = 'immich'",
        [url],
    )?;
    if key.is_empty() {
        conn.execute(
            "DELETE FROM credential WHERE source_id = (SELECT id FROM source WHERE kind = 'immich')",
            [],
        )?;
    } else {
        conn.execute(
            "INSERT INTO credential (source_id, api_key) SELECT id, ?1 FROM source WHERE kind = 'immich'
             ON CONFLICT (source_id) DO UPDATE SET api_key = excluded.api_key",
            [key],
        )?;
    }
    Ok(())
}

// ---- settings ----------------------------------------------------------

/// Loads saved settings over the defaults in `s`. Returns the keys found,
/// so callers can tell a saved value from a default.
pub fn load_settings(conn: &Connection, s: &mut Settings) -> Vec<String> {
    let mut rows: HashMap<String, serde_json::Value> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM setting")
        && let Ok(iter) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (k, v) in iter.flatten() {
            if let Ok(v) = serde_json::from_str(&v) {
                rows.insert(k, v);
            }
        }
    }
    let str_of = |k: &str| rows.get(k).and_then(|v| v.as_str()).map(str::to_string);
    if let Some(v) = rows.get("slideshow.interval_s").and_then(|v| v.as_f64()) {
        s.interval_secs = v as f32;
    }
    if let Some(v) = str_of("slideshow.transition") {
        s.transition = TransitionChoice::ALL
            .into_iter()
            .find(|t| transition_str(*t) == v)
            .unwrap_or(s.transition);
    }
    if let Some(v) = rows.get("slideshow.ken_burns").and_then(|v| v.as_bool()) {
        s.ken_burns_enabled = v;
    }
    if let Some(v) = rows
        .get("display.fill_by_default")
        .and_then(|v| v.as_bool())
    {
        s.fill_by_default = v;
    }
    if let Some(v) = str_of("display.fit_background") {
        s.fit_background = if v == "black" {
            FitBackground::Black
        } else {
            FitBackground::Blurred
        };
    }
    if let Some(v) = rows.get("collage.max").and_then(|v| v.as_u64()) {
        s.collage_max = (v as usize).clamp(1, limits::LARGEST_LAYOUT);
    }
    if let Some(v) = str_of("collage.gap_colour") {
        s.gap_colour = if v == "white" {
            GapColour::White
        } else {
            GapColour::Black
        };
    }
    if let Some(v) = str_of("overlay.clock_style") {
        s.clock_style = match v.as_str() {
            "off" => ClockStyle::Off,
            "detailed" => ClockStyle::Detailed,
            _ => ClockStyle::Simple,
        };
    } else if let Some(v) = str_of("overlay.clock") {
        // Pre-split rows (one key conflating style and corner): read-alias.
        (s.clock_style, s.clock_corner) = match v.as_str() {
            "off" => (ClockStyle::Off, s.clock_corner),
            "bottomleft" => (ClockStyle::Detailed, Corner::BottomLeft),
            _ => (ClockStyle::Simple, Corner::TopRight),
        };
    }
    if let Some(v) = str_of("overlay.clock_corner") {
        s.clock_corner = match v.as_str() {
            "topleft" => Corner::TopLeft,
            "bottomleft" => Corner::BottomLeft,
            "bottomright" => Corner::BottomRight,
            _ => Corner::TopRight,
        };
    }
    if let Some(v) = rows.get("overlay.weather").and_then(|v| v.as_bool()) {
        s.weather_enabled = v;
    }
    if let Some(v) = rows.get("locale.clock_24h").and_then(|v| v.as_bool()) {
        s.clock_24h = v;
    }
    if let Some(v) = str_of("ui.theme") {
        s.dark_theme = v != "light";
    }
    if let Some(v) = rows.get("cache.cap_mb").and_then(|v| v.as_u64()) {
        s.cache_cap_mb = v as u32;
    }
    if let Some(v) = str_of("video.playback") {
        s.video_playback = VideoPlayback::parse(&v);
    }
    if let Some(v) = rows.get("video.sound").and_then(|v| v.as_bool()) {
        s.video_sound = v;
    }
    if let Some(v) = rows.get("video.audio_delay_ms").and_then(|v| v.as_i64()) {
        s.audio_delay_ms =
            (v as i32).clamp(limits::AUDIO_DELAY_RANGE.0, limits::AUDIO_DELAY_RANGE.1);
    }
    if let Some(v) = rows.get("video.volume").and_then(|v| v.as_f64()) {
        s.video_volume = (v as f32).clamp(0.0, 1.0);
    }
    if let Ok((start, end, enabled)) = conn.query_row(
        "SELECT start_min, end_min, enabled FROM schedule WHERE kind = 'sleep' ORDER BY id LIMIT 1",
        [],
        |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, bool>(2)?,
            ))
        },
    ) {
        s.sleep_min = start;
        s.wake_min = end;
        s.sleep_enabled = enabled;
    }
    s.server_url = source(conn, SourceKind::Immich).base_url;
    s.api_key = immich_key(conn);
    rows.into_keys().collect()
}

pub fn save_settings(
    conn: &Connection,
    rows: &[(&'static str, serde_json::Value)],
    sleep: (bool, u32, u32),
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO setting (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )?;
        for (k, v) in rows {
            stmt.execute(params![k, v.to_string()])?;
        }
    }
    tx.execute("DELETE FROM schedule WHERE kind = 'sleep'", [])?;
    tx.execute(
        "INSERT INTO schedule (kind, start_min, end_min, enabled) VALUES ('sleep', ?1, ?2, ?3)",
        params![sleep.1, sleep.2, sleep.0],
    )?;
    tx.commit()
}

// ---- albums --------------------------------------------------------------

/// The first album list picks this one, so a new install has something to
/// play before anyone opens the picker.
pub const DEFAULT_ALBUM: &str = "Favorites";
const ALBUMS_SEEDED: &str = "immich.albums_seeded";

/// Brings `collection` in line with the server's album list: names and
/// counts updated, albums that are gone deleted unless picked (a picked one
/// is kept and marked missing, so the picker can say so). The first list
/// ever picks `DEFAULT_ALBUM`. Returns the picked albums that are there.
pub fn update_albums(
    conn: &Connection,
    albums: &[crate::immich::RemoteAlbum],
) -> rusqlite::Result<Vec<String>> {
    let source = source_id(conn, SourceKind::Immich).ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let now = now_ms();
    let seeded = conn
        .query_row(
            "SELECT 1 FROM setting WHERE key = ?1",
            [ALBUMS_SEEDED],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let tx = conn.unchecked_transaction()?;
    for a in albums {
        tx.execute(
            "INSERT INTO collection (source_id, remote_id, name, asset_count, listed_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (source_id, remote_id) DO UPDATE SET name = excluded.name,
               asset_count = excluded.asset_count, listed_at_ms = excluded.listed_at_ms, missing_since_ms = NULL",
            params![source, a.id, a.name, a.asset_count, now],
        )?;
    }
    // Everything not listed just now is gone from the server.
    tx.execute(
        "DELETE FROM collection WHERE source_id = ?1 AND listed_at_ms != ?2 AND selected = 0",
        params![source, now],
    )?;
    let missing = tx.execute(
        "UPDATE collection SET missing_since_ms = ?2 WHERE source_id = ?1 AND listed_at_ms != ?2 AND missing_since_ms IS NULL",
        params![source, now],
    )?;
    if missing > 0 {
        log::warn!("db: {missing} picked album(s) no longer on the server");
    }
    if !seeded {
        let picked = tx.execute(
            "UPDATE collection SET selected = 1 WHERE source_id = ?1 AND name = ?2 COLLATE NOCASE",
            params![source, DEFAULT_ALBUM],
        )?;
        tx.execute(
            "INSERT INTO setting (key, value, scope) VALUES (?1, 'true', 'device') ON CONFLICT (key) DO NOTHING",
            [ALBUMS_SEEDED],
        )?;
        log::info!("db: first album list, {picked} album named {DEFAULT_ALBUM:?} picked");
    }
    tx.commit()?;
    Ok(selected_albums(conn))
}

/// The picked albums that are on the server (as last listed).
pub fn selected_albums(conn: &Connection) -> Vec<String> {
    conn.prepare(
        "SELECT c.remote_id FROM collection c JOIN source s ON s.id = c.source_id
         WHERE s.kind = 'immich' AND c.selected = 1 AND c.missing_since_ms IS NULL ORDER BY c.name",
    )
    .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
    .unwrap_or_default()
}

pub fn albums(conn: &Connection) -> Vec<AlbumRow> {
    conn.prepare(
        "SELECT c.remote_id, c.name, c.asset_count, c.selected, c.missing_since_ms IS NOT NULL,
                (SELECT COUNT(*) FROM collection_asset m WHERE m.collection_id = c.id)
         FROM collection c JOIN source s ON s.id = c.source_id WHERE s.kind = 'immich'
         ORDER BY c.name COLLATE NOCASE",
    )
    .and_then(|mut s| {
        s.query_map([], |r| {
            Ok(AlbumRow {
                remote_id: r.get(0)?,
                name: r.get(1)?,
                asset_count: r.get(2)?,
                selected: r.get(3)?,
                missing: r.get(4)?,
                synced: r.get(5)?,
            })
        })?
        .collect()
    })
    .unwrap_or_default()
}

/// Picks or un-picks an album. Un-picking drops, at once, every Immich asset
/// no other picked album holds (through `delete_assets`; curation stays).
/// Returns how many were dropped and their files, to remove once the lock
/// is let go.
pub fn select_album(
    conn: &Connection,
    remote_id: &str,
    on: bool,
) -> rusqlite::Result<(usize, Vec<PathBuf>)> {
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE collection SET selected = ?2 WHERE remote_id = ?1
           AND source_id = (SELECT id FROM source WHERE kind = 'immich')",
        params![remote_id, on],
    )?;
    let mut dropped = (0, Vec::new());
    if changed > 0 && !on {
        tx.execute(
            "DELETE FROM collection_asset WHERE collection_id =
               (SELECT c.id FROM collection c JOIN source s ON s.id = c.source_id
                WHERE s.kind = 'immich' AND c.remote_id = ?1)",
            [remote_id],
        )?;
        let orphans: Vec<i64> = tx
            .prepare(
                "SELECT a.id FROM asset a JOIN source s ON s.id = a.source_id WHERE s.kind = 'immich'
                   AND NOT EXISTS (SELECT 1 FROM collection_asset m WHERE m.asset_id = a.id)",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        dropped = (orphans.len(), delete_assets(&tx, &orphans)?);
    }
    tx.commit()?;
    Ok(dropped)
}

/// The Immich user the synced library belongs to (the key's `users/me`).
const LIBRARY_USER: &str = "immich.user_id";

/// Checks the synced Immich library is still the key's. A user id that
/// changed means another server, or another user on it: its albums, picks
/// and assets belong to a library the frame no longer shows, so all of
/// them go, and the next album list picks `DEFAULT_ALBUM` again as on a
/// new install. Curation stays (keyed by hash, so it applies again if the
/// new library holds the same photos). The same server under a new URL
/// keeps everything. The first check only records the id. Returns `None`
/// if nothing changed, else the dropped assets' files to remove.
pub fn check_library(conn: &Connection, user_id: &str) -> rusqlite::Result<Option<Vec<PathBuf>>> {
    let value = serde_json::Value::String(user_id.to_string()).to_string();
    let known: Option<String> = conn
        .query_row(
            "SELECT value FROM setting WHERE key = ?1",
            [LIBRARY_USER],
            |r| r.get(0),
        )
        .optional()?;
    if known.as_deref() == Some(value.as_str()) {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction()?;
    let mut files = None;
    if known.is_some() {
        let ids: Vec<i64> = tx
            .prepare("SELECT a.id FROM asset a JOIN source s ON s.id = a.source_id WHERE s.kind = 'immich'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        files = Some(delete_assets(&tx, &ids)?);
        tx.execute("DELETE FROM collection WHERE source_id = (SELECT id FROM source WHERE kind = 'immich')", [])?;
        tx.execute("DELETE FROM setting WHERE key = ?1", [ALBUMS_SEEDED])?;
        log::warn!(
            "db: the Immich key is another user now, a different library: {} assets and every album dropped",
            ids.len()
        );
    }
    tx.execute(
        "INSERT INTO setting (key, value, scope) VALUES (?1, ?2, 'device') ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![LIBRARY_USER, value],
    )?;
    tx.commit()?;
    Ok(files)
}

/// Rewrites which collections each of a source's assets came from, after a
/// sync (`items` are what the provider listed, already upserted).
pub fn set_memberships(conn: &Connection, source: i64, items: &[MediaRef]) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM collection_asset WHERE asset_id IN (SELECT id FROM asset WHERE source_id = ?1)",
        [source],
    )?;
    let collections: HashMap<String, i64> = conn
        .prepare("SELECT remote_id, id FROM collection WHERE source_id = ?1")?
        .query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO collection_asset (collection_id, asset_id)
         SELECT ?1, id FROM asset WHERE source_id = ?2 AND remote_id = ?3",
    )?;
    for m in items {
        for c in &m.collections {
            if let Some(cid) = collections.get(c) {
                stmt.execute(params![cid, source, m.id])?;
            }
        }
    }
    Ok(())
}

// ---- curation ------------------------------------------------------------

/// The curation key of an asset row: its hash, else source and id.
pub const KEY_SQL: &str = "COALESCE(a.hash, s.kind || ':' || a.remote_id)";

pub fn load_overrides(conn: &Connection) -> HashMap<String, ScaleMode> {
    let mut out = HashMap::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT key, scale_mode FROM curation WHERE scale_mode IS NOT NULL")
        && let Ok(iter) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
    {
        for (key, mode) in iter.flatten() {
            out.insert(
                key,
                if mode == "fit" {
                    ScaleMode::Fit
                } else {
                    ScaleMode::Fill
                },
            );
        }
    }
    out
}

pub fn set_scale(conn: &Connection, key: &str, mode: Option<ScaleMode>) -> rusqlite::Result<usize> {
    let mode = mode.map(|m| match m {
        ScaleMode::Fill => "fill",
        ScaleMode::Fit => "fit",
    });
    conn.execute(
        "INSERT INTO curation (key, scale_mode, updated_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET scale_mode = excluded.scale_mode, updated_at_ms = excluded.updated_at_ms",
        params![key, mode, now_ms()],
    )
}

pub fn set_hidden(conn: &Connection, key: &str, hidden: bool) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO curation (key, hidden, updated_at_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET hidden = excluded.hidden, updated_at_ms = excluded.updated_at_ms",
        params![key, hidden, now_ms()],
    )
}

/// Hidden photos, most recently hidden first, labelled by where they are.
pub fn hidden_list(conn: &Connection) -> Vec<HiddenItem> {
    // The key is computed from both tables, so they join before the LEFT JOIN.
    let sql = format!(
        "SELECT c.key, MAX(s.kind), MAX(a.location), MAX(a.taken_at_ms) FROM curation c
         LEFT JOIN (asset a JOIN source s ON s.id = a.source_id) ON {KEY_SQL} = c.key
         WHERE c.hidden = 1 GROUP BY c.key ORDER BY MAX(c.updated_at_ms) DESC"
    );
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&sql)
        && let Ok(iter) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })
    {
        for (key, kind, location, taken) in iter.flatten() {
            let short = &key[..key.len().min(8)];
            let label = match (kind.as_deref(), location) {
                (Some("local"), Some(loc)) => {
                    format!("Folder: {}", loc.rsplit('/').next().unwrap_or(&loc))
                }
                (Some(_), _) => format!(
                    "Immich, taken {} ({short})",
                    taken.map_or("?".into(), fmt_date)
                ),
                (None, _) => format!("not in any source now ({short})"),
            };
            out.push(HiddenItem { key, label });
        }
    }
    out
}

/// The whole overlay as JSON, with where each photo is, for `adb pull`:
/// the frame has no backup path for app data.
pub fn curation_json(conn: &Connection) -> String {
    let sql = format!(
        "SELECT c.key, c.hidden, c.scale_mode, c.focus_x, c.focus_y, c.updated_at_ms,
                (SELECT json_group_array(json_object('source', s.kind, 'id', a.remote_id, 'location', a.location))
                 FROM asset a JOIN source s ON s.id = a.source_id WHERE {KEY_SQL} = c.key)
         FROM curation c ORDER BY c.key"
    );
    let mut items = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&sql)
        && let Ok(iter) = stmt.query_map([], |r| {
            Ok(serde_json::json!({
                "sha1": r.get::<_, String>(0)?,
                "hidden": r.get::<_, bool>(1)?,
                "scale_mode": r.get::<_, Option<String>>(2)?,
                "focus": r.get::<_, Option<f64>>(3)?.zip(r.get::<_, Option<f64>>(4)?).map(|(x, y)| [x, y]),
                "updated_at_ms": r.get::<_, i64>(5)?,
                "copies": serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(6)?).unwrap_or_default(),
            }))
        })
    {
        items.extend(iter.flatten());
    }
    serde_json::to_string_pretty(&serde_json::json!({
        "format": "raam curation v1",
        "exported_at_ms": now_ms(),
        "items": items,
    }))
    .unwrap_or_default()
}

/// The import of a curation export: it seeds `curation` once, when the
/// table is empty and the export file exists. Accepts Raam's own exports
/// and the prototype's ("immich-frame-rs curation v1"), which is how the
/// frame's curation crosses installs.
pub fn import_curation(conn: &Connection, path: &Path) -> Result<usize, String> {
    let have: i64 = conn
        .query_row("SELECT COUNT(*) FROM curation", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if have > 0 || !path.is_file() {
        return Ok(0);
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let format = json.get("format").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(format, "raam curation v1" | "immich-frame-rs curation v1") {
        return Err(format!("{}: unknown format {format:?}", path.display()));
    }
    let items = json
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or("no items array")?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut imported = 0;
    {
        let mut stmt = tx
            .prepare(
                "INSERT OR REPLACE INTO curation (key, hidden, scale_mode, focus_x, focus_y, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .map_err(|e| e.to_string())?;
        for item in items {
            let Some(key) = item.get("sha1").and_then(|v| v.as_str()) else {
                continue;
            };
            let focus = item.get("focus").and_then(|v| v.as_array());
            let (fx, fy) = match focus {
                Some(f) if f.len() == 2 => (f[0].as_f64(), f[1].as_f64()),
                _ => (None, None),
            };
            stmt.execute(params![
                key,
                item.get("hidden")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                item.get("scale_mode").and_then(|v| v.as_str()),
                fx,
                fy,
                item.get("updated_at_ms")
                    .and_then(|v| v.as_i64())
                    .unwrap_or_else(now_ms),
            ])
            .map_err(|e| e.to_string())?;
            imported += 1;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(imported)
}

fn fmt_date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // civil_from_days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

// ---- playback ------------------------------------------------------------

pub fn load_playback(conn: &Connection) -> Option<(Option<i64>, u64)> {
    conn.query_row(
        "SELECT current_asset, shuffle_seed FROM playback WHERE id = 1",
        [],
        |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
            ))
        },
    )
    .optional()
    .ok()
    .flatten()
}

pub fn save_playback(conn: &Connection, current: i64, seed: u64) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO playback (id, current_asset, shuffle_seed, updated_at_ms) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT (id) DO UPDATE SET current_asset = excluded.current_asset,
           shuffle_seed = excluded.shuffle_seed, updated_at_ms = excluded.updated_at_ms",
        params![current, seed as i64, now_ms()],
    )
}

// ---- the slideshow's queue -------------------------------------------------

/// The `cached_file` variant an asset of this kind is played from.
pub const VARIANT_SQL: &str = "CASE a.kind WHEN 'video' THEN 'video' ELSE 'preview' END";

/// Every photo the slideshow may show from the enabled sources, minus the
/// hidden ones (so they never reach collage grouping), one per curation
/// key: a photo in both sources plays once, from the local preview if it
/// has one, else a cached Immich preview, else the server. A local photo
/// needs its preview made; an Immich one needs a cached preview when
/// `immich_cached_only` (offline). With no server (never set, or
/// removed) the Immich photos wait, cache and all, as the menu's "add a
/// server" says; the same server entered again brings them straight back.
pub fn eligible(conn: &Connection, immich_cached_only: bool) -> Vec<MediaItem> {
    let sql = format!(
        "SELECT a.id, {KEY_SQL}, s.kind, a.remote_id, a.location, a.width, a.height, f.asset_id IS NOT NULL, a.kind
         FROM asset a JOIN source s ON s.id = a.source_id
         LEFT JOIN curation c ON c.key = {KEY_SQL}
         LEFT JOIN cached_file f ON f.asset_id = a.id AND f.variant = {VARIANT_SQL}
         WHERE s.enabled = 1 AND (s.kind = 'local' OR s.base_url <> '')
           AND COALESCE(c.hidden, 0) = 0 AND a.width > 0 AND a.height > 0
           AND COALESCE(a.playable, 1) = 1
           AND (f.asset_id IS NOT NULL OR (s.kind = 'immich' AND ?1 = 0) OR (s.kind = 'local' AND a.kind = 'video'))"
    );
    let mut rows: Vec<(u8, MediaItem)> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(&sql)
        && let Ok(iter) = stmt.query_map([immich_cached_only], |r| {
            let source = SourceKind::parse(&r.get::<_, String>(2)?);
            let ready: bool = r.get(7)?;
            let pref = match (source, ready) {
                (SourceKind::Local, _) => 0,
                (SourceKind::Immich, true) => 1,
                (SourceKind::Immich, false) => 2,
            };
            Ok((
                pref,
                MediaItem {
                    asset: r.get(0)?,
                    key: r.get(1)?,
                    source,
                    remote_id: r.get(3)?,
                    width: r.get(5)?,
                    height: r.get(6)?,
                    video: r.get::<_, String>(8)? == "video",
                    location: r.get(4)?,
                },
            ))
        })
    {
        rows.extend(iter.flatten());
    }
    rows.sort_by(|a, b| a.1.key.cmp(&b.1.key).then(a.0.cmp(&b.0)));
    rows.dedup_by(|a, b| a.1.key == b.1.key);
    rows.into_iter().map(|r| r.1).collect()
}

/// How many photos are in more than one source (the de-duplicated ones).
pub fn shared_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM (SELECT hash FROM asset WHERE hash IS NOT NULL GROUP BY hash HAVING COUNT(DISTINCT source_id) > 1)",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// The asset's focus: (fill centre, largest face, faces looked up). A
/// curated focus replaces the source's fill centre.
pub fn focus(conn: &Connection, asset: i64) -> ((f32, f32), Option<(f32, f32)>, bool) {
    let sql = format!(
        "SELECT COALESCE(c.focus_x, a.focus_x), COALESCE(c.focus_y, a.focus_y), a.face_x, a.face_y, a.faces_checked
         FROM asset a JOIN source s ON s.id = a.source_id LEFT JOIN curation c ON c.key = {KEY_SQL} WHERE a.id = ?1"
    );
    conn.query_row(&sql, [asset], |r| {
        let fx: Option<f64> = r.get(0)?;
        let fy: Option<f64> = r.get(1)?;
        let hx: Option<f64> = r.get(2)?;
        let hy: Option<f64> = r.get(3)?;
        Ok((
            fx.zip(fy).map_or((0.5, 0.5), |(x, y)| (x as f32, y as f32)),
            hx.zip(hy).map(|(x, y)| (x as f32, y as f32)),
            r.get::<_, bool>(4)?,
        ))
    })
    .unwrap_or(((0.5, 0.5), None, false))
}

/// The provider's focus (`None` = centre it); either way it's been asked.
pub fn set_focus(conn: &Connection, asset: i64, focus: Option<Focus>) -> rusqlite::Result<usize> {
    let centre = focus.map(|f| f.centre);
    let face = focus.and_then(|f| f.face);
    conn.execute(
        "UPDATE asset SET focus_x = ?2, focus_y = ?3, face_x = ?4, face_y = ?5, faces_checked = 1 WHERE id = ?1",
        params![asset, centre.map(|c| c.0), centre.map(|c| c.1), face.map(|f| f.0), face.map(|f| f.1)],
    )
}

/// The local folder's items as last scanned, to seed its memo so files
/// that haven't changed aren't hashed again after a restart.
pub fn local_known(conn: &Connection) -> Vec<MediaRef> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT a.remote_id, a.hash, a.location, a.width, a.height, a.taken_at_ms, a.file_bytes, a.file_mtime_ms, a.kind
         FROM asset a JOIN source s ON s.id = a.source_id WHERE s.kind = 'local'",
    ) && let Ok(iter) = stmt.query_map([], |r| {
        let stamp = r.get::<_, Option<i64>>(6)?.zip(r.get::<_, Option<i64>>(7)?);
        Ok(MediaRef {
            id: r.get(0)?,
            sha1: r.get(1)?,
            location: r.get(2)?,
            width: r.get(3)?,
            height: r.get(4)?,
            taken_at_ms: r.get(5)?,
            kind: if r.get::<_, String>(8)? == "video" { MediaKind::Video } else { MediaKind::Photo },
            focus: None,
            stamp,
            collections: Vec::new(),
        })
    }) {
        out.extend(iter.flatten());
    }
    out
}

// ---- files on disk -------------------------------------------------------

pub fn cached_preview(conn: &Connection, asset: i64) -> Option<PathBuf> {
    conn.query_row(
        "SELECT path FROM cached_file WHERE asset_id = ?1 AND variant = 'preview'",
        [asset],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .map(PathBuf::from)
}

/// An Immich clip's cached playback transcode.
pub fn cached_video(conn: &Connection, asset: i64) -> Option<PathBuf> {
    conn.query_row(
        "SELECT path FROM cached_file WHERE asset_id = ?1 AND variant = 'video'",
        [asset],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .map(PathBuf::from)
}

/// The frame's decoder can't take this clip: out of the queue for
/// good (until the cache is cleared), its cached file dropped. Returns the
/// files to remove.
pub fn mark_unplayable(conn: &Connection, asset: i64, reason: &str) -> Vec<PathBuf> {
    let _ = conn.execute(
        "UPDATE asset SET playable = 0, unplayable_reason = ?2 WHERE id = ?1",
        params![asset, reason],
    );
    drop_cached(conn, asset)
}

/// The host has no player (`MediaProbe::no_player`): every clip not yet
/// marked is marked unplayable for `reason`, without its file. Returns
/// how many were marked and the files to remove.
pub fn mark_clips_unplayable(conn: &Connection, reason: &str) -> (usize, Vec<PathBuf>) {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM asset WHERE kind = 'video' AND COALESCE(playable, 1) = 1")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .unwrap_or_default();
    let files = ids
        .iter()
        .flat_map(|&id| mark_unplayable(conn, id, reason))
        .collect();
    (ids.len(), files)
}

pub fn set_playable(conn: &Connection, asset: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE asset SET playable = 1, unplayable_reason = NULL WHERE id = ?1",
        [asset],
    )
}

/// (videos, playable ones ready to play, unplayable, the unplayable ones'
/// reasons) over the enabled sources.
pub fn video_counts(conn: &Connection) -> (i64, i64, i64, Vec<String>) {
    let (total, ready, bad) = conn
        .query_row(
            "SELECT COUNT(*),
                    COUNT(CASE WHEN COALESCE(a.playable, 1) = 1 AND (f.asset_id IS NOT NULL OR s.kind = 'local') THEN 1 END),
                    COUNT(CASE WHEN a.playable = 0 THEN 1 END)
             FROM asset a JOIN source s ON s.id = a.source_id
             LEFT JOIN cached_file f ON f.asset_id = a.id AND f.variant = 'video'
             WHERE a.kind = 'video' AND s.enabled = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((0, 0, 0));
    let reasons = conn
        .prepare(
            "SELECT DISTINCT unplayable_reason FROM asset a JOIN source s ON s.id = a.source_id
             WHERE a.playable = 0 AND s.enabled = 1 AND unplayable_reason IS NOT NULL",
        )
        .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0))?.collect())
        .unwrap_or_default();
    (total, ready, bad, reasons)
}

pub fn touch_cached(conn: &Connection, asset: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE cached_file SET last_used_ms = ?2 WHERE asset_id = ?1",
        params![asset, now_ms()],
    )
}

/// Total bytes of cached files for one source.
pub fn cached_bytes(conn: &Connection, kind: SourceKind) -> i64 {
    conn.query_row(
        "SELECT COALESCE(SUM(c.bytes), 0) FROM cached_file c JOIN asset a ON a.id = c.asset_id
         JOIN source s ON s.id = a.source_id WHERE s.kind = ?1",
        [kind.as_str()],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// The single way cached files leave: their rows go, and their paths come
/// back for `remove_files` once the caller has let go of the lock. A crash
/// in between leaves files no row points at, which the startup sweep
/// deletes; asset ids are never reused (AUTOINCREMENT), so no new asset can
/// take such a file for its own first.
pub fn drop_cached(conn: &Connection, asset: i64) -> Vec<PathBuf> {
    let paths = cached_paths(conn, asset);
    let _ = conn.execute("DELETE FROM cached_file WHERE asset_id = ?1", [asset]);
    paths
}

fn cached_paths(conn: &Connection, asset: i64) -> Vec<PathBuf> {
    conn.prepare_cached("SELECT path FROM cached_file WHERE asset_id = ?1")
        .and_then(|mut s| {
            s.query_map([asset], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// The single way assets leave: the rows go (the cascade takes
/// `cached_file` and memberships with them; curation stays, keyed by hash)
/// and their files' paths come back, as for `drop_cached`.
pub fn delete_assets(conn: &Connection, assets: &[i64]) -> rusqlite::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut delete = conn.prepare_cached("DELETE FROM asset WHERE id = ?1")?;
    for &id in assets {
        paths.extend(cached_paths(conn, id));
        delete.execute([id])?;
    }
    Ok(paths)
}

/// Removes files `drop_cached` and `delete_assets` gave back. Call it
/// without the DB lock held.
pub fn remove_files(paths: &[PathBuf]) {
    for p in paths {
        if let Err(e) = std::fs::remove_file(p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("db: removing {}: {e}", p.display());
        }
    }
}

/// Least recently used Immich previews, oldest first, never `except`.
pub fn lru_immich(conn: &Connection, except: i64, limit: usize) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT c.asset_id, c.bytes FROM cached_file c JOIN asset a ON a.id = c.asset_id
         JOIN source s ON s.id = a.source_id
         WHERE s.kind = 'immich' AND c.asset_id != ?1 ORDER BY c.last_used_ms LIMIT ?2",
    ) && let Ok(iter) = stmt.query_map(params![except, limit as i64], |r| {
        Ok((r.get(0)?, r.get(1)?))
    }) {
        out.extend(iter.flatten());
    }
    out
}

pub fn insert_cached(
    conn: &Connection,
    asset: i64,
    path: &Path,
    bytes: i64,
    used_ms: i64,
) -> rusqlite::Result<usize> {
    insert_cached_variant(conn, asset, "preview", path, bytes, used_ms)
}

pub fn insert_cached_variant(
    conn: &Connection,
    asset: i64,
    variant: &str,
    path: &Path,
    bytes: i64,
    used_ms: i64,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO cached_file (asset_id, variant, path, bytes, last_used_ms) VALUES (?1, ?5, ?2, ?3, ?4)
         ON CONFLICT (asset_id, variant) DO UPDATE SET path = excluded.path, bytes = excluded.bytes,
           last_used_ms = excluded.last_used_ms",
        params![asset, path.to_string_lossy(), bytes, used_ms, variant],
    )
}

/// Startup sweep over `dirs`: rows whose file is gone lose the row, files
/// no row points at are deleted. Returns (rows dropped, files deleted).
pub fn sweep(conn: &Connection, dirs: &[&Path]) -> (usize, usize) {
    let mut known = std::collections::HashSet::new();
    let mut missing = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT asset_id, path FROM cached_file")
        && let Ok(iter) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
    {
        for (id, path) in iter.flatten() {
            if Path::new(&path).is_file() {
                known.insert(PathBuf::from(path));
            } else {
                missing.push(id);
            }
        }
    }
    for id in &missing {
        let _ = conn.execute("DELETE FROM cached_file WHERE asset_id = ?1", [id]);
    }
    let mut orphans = 0;
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !known.contains(&p) && std::fs::remove_file(&p).is_ok() {
                orphans += 1;
            }
        }
    }
    (missing.len(), orphans)
}

// ---- counts for the settings panel ---------------------------------------

/// (assets, assets with a cached/made preview) for one source.
pub fn counts(conn: &Connection, kind: SourceKind) -> (i64, i64) {
    conn.query_row(
        &format!(
            "SELECT COUNT(*), COUNT(CASE WHEN c.asset_id IS NOT NULL OR (s.kind = 'local' AND a.kind = 'video') THEN 1 END)
             FROM asset a JOIN source s ON s.id = a.source_id
             LEFT JOIN cached_file c ON c.asset_id = a.id AND c.variant = {VARIANT_SQL} WHERE s.kind = ?1"
        ),
        [kind.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_test_clock as install_clock;

    fn asset(conn: &Connection, id: &str, kind: &str, playable: Option<(bool, &str)>) {
        conn.execute(
            "INSERT INTO asset (source_id, remote_id, kind, width, height, added_at_ms, playable, unplayable_reason)
             VALUES ((SELECT id FROM source WHERE kind = 'immich'), ?1, ?2, 640, 480, 0, ?3, ?4)",
            params![id, kind, playable.map(|p| p.0), playable.map(|p| p.1)],
        )
        .unwrap();
    }

    #[test]
    fn a_host_with_no_player_keeps_every_clip_out_of_the_queue() {
        install_clock();
        let db = open(Path::new(":memory:"), "").unwrap();
        let conn = db.lock().unwrap();
        set_immich_server(&conn, "http://immich.local:2283", "key").unwrap();
        asset(&conn, "photo", "image", None);
        asset(&conn, "clip", "video", None);
        asset(&conn, "big", "video", Some((false, "1920x1088 at most")));
        let queued = |conn: &Connection| -> Vec<String> {
            let mut ids: Vec<String> = eligible(conn, false)
                .into_iter()
                .map(|m| m.remote_id)
                .collect();
            ids.sort();
            ids
        };
        // A clip nobody has tried yet is queued, as on the frame.
        assert_eq!(queued(&conn), ["clip", "photo"]);

        let (marked, files) = mark_clips_unplayable(&conn, "no video player on this host");
        assert_eq!((marked, files.len()), (1, 0));
        assert_eq!(queued(&conn), ["photo"]);
        let reason = |id: &str| -> String {
            conn.query_row(
                "SELECT unplayable_reason FROM asset WHERE remote_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(reason("clip"), "no video player on this host");
        // A clip already out keeps the reason it had.
        assert_eq!(reason("big"), "1920x1088 at most");
        // Nothing left to mark the second time.
        assert_eq!(mark_clips_unplayable(&conn, "again").0, 0);
    }

    #[test]
    fn a_removed_server_keeps_its_photos_out_until_one_is_set_again() {
        install_clock();
        let db = open(Path::new(":memory:"), "").unwrap();
        let conn = db.lock().unwrap();
        let queued = |conn: &Connection| -> Vec<String> {
            eligible(conn, false)
                .into_iter()
                .map(|m| m.remote_id)
                .collect()
        };
        let loaded = |conn: &Connection| -> (String, String) {
            let mut s = Settings::defaults("", "");
            load_settings(conn, &mut s);
            (s.server_url, s.api_key)
        };
        asset(&conn, "photo", "image", None);
        // A fresh install: no server, so nothing from Immich plays.
        assert_eq!(queued(&conn), [] as [&str; 0]);

        set_immich_server(&conn, "http://immich.local:2283", "key").unwrap();
        assert_eq!(queued(&conn), ["photo"]);

        // Emptied in settings: stored as none, as on a fresh install. The
        // photo and its row stay for the server's return.
        set_immich_server(&conn, "", "key").unwrap();
        let url: Option<String> = conn
            .query_row(
                "SELECT base_url FROM source WHERE kind = 'immich'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(url, None);
        assert_eq!(loaded(&conn), (String::new(), "key".to_string()));
        assert_eq!(queued(&conn), [] as [&str; 0]);
        assert_eq!(counts(&conn, SourceKind::Immich).0, 1);

        // An emptied key goes too.
        set_immich_server(&conn, "http://immich.local:2283", "").unwrap();
        assert_eq!(
            loaded(&conn),
            ("http://immich.local:2283".to_string(), String::new())
        );
        let keys: i64 = conn
            .query_row("SELECT COUNT(*) FROM credential", [], |r| r.get(0))
            .unwrap();
        assert_eq!(keys, 0);
        assert_eq!(queued(&conn), ["photo"]);
    }

    #[test]
    fn every_setting_round_trips_through_the_db() {
        install_clock();
        let db = open(Path::new(":memory:"), "").unwrap();
        let conn = db.lock().unwrap();
        let mut s = Settings::defaults("", "");
        s.interval_secs = 25.0;
        s.transition = TransitionChoice::Cube;
        s.ken_burns_enabled = false;
        s.fill_by_default = false;
        s.fit_background = FitBackground::Black;
        s.collage_max = 2;
        s.gap_colour = GapColour::White;
        s.clock_style = ClockStyle::Detailed;
        s.clock_corner = Corner::BottomLeft;
        s.weather_enabled = true;
        s.clock_24h = false;
        s.dark_theme = false;
        s.cache_cap_mb = 2048;
        s.video_playback = VideoPlayback::Loop;
        s.video_sound = true;
        s.video_volume = 0.25;
        s.audio_delay_ms = 90;
        // Every row off its default, so one that doesn't load shows (and
        // a new setting fails here until it is added above).
        let rows = raam_core::store::settings_rows(&s);
        let defaults = raam_core::store::settings_rows(&Settings::defaults("", ""));
        for (row, default) in rows.iter().zip(&defaults) {
            assert_ne!(row, default, "{} is at its default", row.0);
        }
        save_settings(&conn, &rows, (false, 60, 420)).unwrap();

        let mut loaded = Settings::defaults("", "");
        load_settings(&conn, &mut loaded);
        assert_eq!(raam_core::store::settings_rows(&loaded), rows);
        assert_eq!(
            (loaded.sleep_enabled, loaded.sleep_min, loaded.wake_min),
            (false, 60, 420)
        );
    }
}
