//! The Immich REST client, shared by the fetch thread (on-demand
//! previews) and the library thread (album sync, prefetch), against a
//! server URL and key that live in the DB (entered in settings) and can
//! change at runtime. Nothing about the server is compiled in.
//!
//! Blocking HTTP over ureq (rustls). Roots are bundled (webpki): the
//! frame is API 23 and its system trust store is stale. Errors are typed:
//! `ProviderError::Transport` is the offline signal, never a string match.
use raam_core::num;
use raam_model::limits;
use raam_model::{AlbumId, Focus, ProviderError, RemoteId, UserId};

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub url: String,
    pub key: String,
}

pub struct Client {
    http: ureq::Agent,
    pub config: Config,
}

/// An album as the picker lists it.
#[derive(Clone, Debug)]
pub struct RemoteAlbum {
    pub id: AlbumId,
    pub name: String,
    pub asset_count: i64,
}

pub struct RemoteAsset {
    pub id: RemoteId,
    pub width: u32,
    pub height: u32,
    pub taken_at_ms: Option<i64>,
    /// `checksum`: base64 SHA-1 of the original file (checked on live
    /// assets, a video included), as lowercase hex.
    pub sha1_hex: Option<String>,
    pub is_video: bool,
}

/// The agent both clients here share the shape of: bundled webpki roots,
/// manual status handling (a 404 is an answer, not a transport failure).
pub fn agent(timeout: std::time::Duration, user_agent: &str) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(user_agent)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::WebPki)
                .build(),
        )
        .build()
        .into()
}

impl Client {
    pub fn new(config: Config) -> Result<Self, ProviderError> {
        let url = config.url.trim_end_matches('/').to_string();
        let http = agent(
            limits::IMMICH_HTTP_TIMEOUT,
            concat!("raam/", env!("CARGO_PKG_VERSION")),
        );
        Ok(Self {
            http,
            config: Config {
                url,
                key: config.key,
            },
        })
    }

    /// The key's user (`GET /api/users/me`, its UUID `id`): which library
    /// the frame syncs, whatever URL reaches it.
    pub fn user_id(&self) -> Result<UserId, ProviderError> {
        let me = self.get_json("/api/users/me")?;
        me.get("id")
            .and_then(|v| v.as_str())
            .map(UserId::from)
            .ok_or_else(|| ProviderError::Failed("/api/users/me: no id".into()))
    }

    /// Every album the key can see: `GET /api/albums` (owned and shared
    /// with the user). Only what the picker shows is kept.
    pub fn albums(&self) -> Result<Vec<RemoteAlbum>, ProviderError> {
        let albums = self.get_json("/api/albums")?;
        Ok(albums
            .as_array()
            .ok_or(ProviderError::Failed("/api/albums: not a list".into()))?
            .iter()
            .filter_map(|a| {
                Some(RemoteAlbum {
                    id: AlbumId::from(a.get("id")?.as_str()?),
                    name: a.get("albumName")?.as_str()?.to_string(),
                    asset_count: a.get("assetCount").and_then(|v| v.as_i64()).unwrap_or(0),
                })
            })
            .collect())
    }

    /// One album's assets with their oriented size and taken time.
    ///
    /// `GET /api/albums/{id}` has no `assets` array on the tested server
    /// (Immich 3.2.2), so the album is listed through
    /// `POST /api/search/metadata` with `albumIds`, a page at a time. One
    /// album a call: several `albumIds` in one call return only the photos
    /// in ALL of them (probed live), not the union. Each asset's top-level
    /// `width`/`height` is already oriented (probed too).
    pub fn album_assets(&self, album_id: &AlbumId) -> Result<Vec<RemoteAsset>, ProviderError> {
        let mut out = Vec::new();
        let mut page = 1;
        for _ in 0..limits::IMMICH_MAX_PAGES {
            let body = self.post_json(
                "/api/search/metadata",
                &serde_json::json!({ "albumIds": [album_id.as_str()], "size": limits::IMMICH_PAGE_SIZE, "page": page }),
            )?;
            let assets = body.get("assets");
            if let Some(items) = assets
                .and_then(|a| a.get("items"))
                .and_then(|a| a.as_array())
            {
                out.extend(items.iter().filter_map(|a| {
                    Some(RemoteAsset {
                        id: RemoteId::from(a.get("id")?.as_str()?),
                        // A size past u32 is no size: the asset is left out.
                        width: u32::try_from(a.get("width")?.as_u64()?).ok()?,
                        height: u32::try_from(a.get("height")?.as_u64()?).ok()?,
                        taken_at_ms: a
                            .get("fileCreatedAt")
                            .and_then(|v| v.as_str())
                            .and_then(parse_iso_ms),
                        sha1_hex: a
                            .get("checksum")
                            .and_then(|v| v.as_str())
                            .and_then(base64_to_hex),
                        is_video: a.get("type").and_then(|v| v.as_str()) == Some("VIDEO"),
                    })
                }));
            }
            match assets
                .and_then(|a| a.get("nextPage"))
                .and_then(|p| p.as_str())
                .and_then(|p| p.parse().ok())
            {
                Some(next) if next > page => page = next,
                _ => return Ok(out),
            }
        }
        // Never a partial list: the sync would drop every asset past it.
        Err(ProviderError::Failed(format!(
            "album {album_id}: more than {} pages",
            limits::IMMICH_MAX_PAGES
        )))
    }

    /// The preview JPEG's bytes, as the server sends them.
    pub fn preview(&self, id: &RemoteId) -> Result<Vec<u8>, ProviderError> {
        let mut resp = self
            .http
            .get(format!(
                "{}/api/assets/{id}/thumbnail?size=preview",
                self.config.url
            ))
            .header("x-api-key", &self.config.key)
            .call()
            .map_err(|e| ProviderError::Transport(format!("preview request: {}", chain(&e))))?;
        let status = resp.status();
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(limits::HTTP_BODY_LIMIT_BYTES)
            .read_to_vec()
            .map_err(|e| ProviderError::Transport(format!("preview body: {e}")))?;
        if !status.is_success() {
            return Err(ProviderError::Failed(format!(
                "preview request failed: {status}"
            )));
        }
        Ok(bytes)
    }

    /// The clip Immich plays in its own web and mobile apps,
    /// `GET /api/assets/{id}/video/playback`: the H.264 transcode when the
    /// server made one, otherwise the original file as uploaded. Streamed
    /// to `dest`, never held in memory (clips run 2-28 MB against a few MB
    /// free). Returns the bytes written. Stops after `max_bytes + 1`: a
    /// count over `max_bytes` means the clip is bigger than that, and the
    /// file cut short.
    pub fn download_video(
        &self,
        id: &RemoteId,
        dest: &std::path::Path,
        max_bytes: u64,
    ) -> Result<u64, ProviderError> {
        let mut resp = self
            .http
            .get(format!(
                "{}/api/assets/{id}/video/playback",
                self.config.url
            ))
            .header("x-api-key", &self.config.key)
            .config()
            .timeout_global(Some(limits::IMMICH_VIDEO_TIMEOUT))
            .build()
            .call()
            .map_err(|e| ProviderError::Transport(format!("video request: {}", chain(&e))))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ProviderError::Failed(format!(
                "video request failed: {status}"
            )));
        }
        let mut file = std::fs::File::create(dest)
            .map_err(|e| ProviderError::Failed(format!("create {}: {e}", dest.display())))?;
        // The reader has no limit of its own (ureq's default is none).
        let mut body = std::io::Read::take(resp.body_mut().as_reader(), max_bytes + 1);
        let n = std::io::copy(&mut body, &mut file)
            .map_err(|e| ProviderError::Transport(format!("video body: {e}")))?;
        file.sync_all()
            .map_err(|e| ProviderError::Failed(format!("sync {}: {e}", dest.display())))?;
        Ok(n)
    }

    /// `GET /api/faces?id=` (probed live): each face's bounding box in
    /// whatever resolution the ML pass used (`imageWidth/Height`, per
    /// asset), so every centre is taken as a fraction of that, and Frameo's
    /// rule turns them into the focus (`Focus::from_faces`). `Err` only when
    /// the server couldn't be asked; a photo with no faces gets the middle
    /// and no target.
    pub fn faces(&self, id: &RemoteId) -> Result<Focus, ProviderError> {
        let faces = self.get_json(&format!("/api/faces?id={id}"))?;
        let mut found: Vec<(f32, (f32, f32))> = Vec::new();
        for face in faces.as_array().into_iter().flatten() {
            let get = |k: &str| face.get(k).and_then(|v| v.as_f64()).map(num::to_f32);
            let (Some(x1), Some(y1), Some(x2), Some(y2), Some(iw), Some(ih)) = (
                get("boundingBoxX1"),
                get("boundingBoxY1"),
                get("boundingBoxX2"),
                get("boundingBoxY2"),
                get("imageWidth"),
                get("imageHeight"),
            ) else {
                continue;
            };
            if iw <= 0.0 || ih <= 0.0 {
                continue;
            }
            found.push((
                (x2 - x1) * (y2 - y1),
                ((x1 + x2) / 2.0 / iw, (y1 + y2) / 2.0 / ih),
            ));
        }
        Ok(Focus::from_faces(&found))
    }

    fn get_json(&self, path: &str) -> Result<serde_json::Value, ProviderError> {
        let resp = self
            .http
            .get(format!("{}{path}", self.config.url))
            .header("x-api-key", &self.config.key)
            .call()
            .map_err(|e| {
                ProviderError::Transport(format!("GET {}: {}", strip_query(path), chain(&e)))
            })?;
        read_json(resp, path)
    }

    fn post_json(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        let resp = self
            .http
            .post(format!("{}{path}", self.config.url))
            .header("x-api-key", &self.config.key)
            .send_json(body)
            .map_err(|e| ProviderError::Transport(format!("POST {path}: {}", chain(&e))))?;
        read_json(resp, path)
    }
}

fn read_json(
    mut resp: ureq::http::Response<ureq::Body>,
    path: &str,
) -> Result<serde_json::Value, ProviderError> {
    let status = resp.status();
    let body = resp
        .body_mut()
        .with_config()
        .limit(limits::HTTP_BODY_LIMIT_BYTES)
        .read_to_string()
        .map_err(|e| ProviderError::Transport(format!("{}: body: {e}", strip_query(path))))?;
    if !status.is_success() {
        return Err(ProviderError::Failed(format!(
            "{}: {status}",
            strip_query(path)
        )));
    }
    serde_json::from_str(&body)
        .map_err(|e| ProviderError::Failed(format!("{}: json: {e}", strip_query(path))))
}

fn strip_query(path: &str) -> &str {
    path.split('?').next().unwrap_or(path)
}

/// Lowercase hex, two digits a byte.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").expect("writing to a String never fails");
    }
    out
}

/// Standard base64 (with padding) to lowercase hex.
fn base64_to_hex(s: &str) -> Option<String> {
    let mut bits: u32 = 0;
    let mut n = 0;
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.bytes().filter(|&c| c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            // The low byte: the bits above it went out already.
            out.push((bits >> n).to_le_bytes()[0]);
        }
    }
    (!out.is_empty()).then(|| to_hex(&out))
}

/// "2024-05-01T10:20:30.000Z" (Immich's UTC timestamps) to epoch ms.
fn parse_iso_ms(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+']).next()?;
    let mut t = time.split(':');
    let (hh, mm) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    let ss: f64 = t.next().unwrap_or("0").parse().ok()?;
    Some(
        (days_from_civil(y, m, day) * 86_400 + hh * 3600 + mm * 60) * 1000
            + num::sat_i64(ss * 1000.0),
    )
}

/// Howard Hinnant's days-from-civil, for UTC dates without a date crate.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        out.push_str(" <- ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Serves one response with a `len`-byte body on a local port.
    fn serve_once(len: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&vec![0xaa; len]);
        });
        url
    }

    /// Immich's `checksum` of an empty file is the SHA-1 the local folder
    /// computes for it, so the two sources match.
    #[test]
    fn a_base64_checksum_reads_as_the_hex_sha1() {
        assert_eq!(
            base64_to_hex("2jmj7l5rSw0yVb/vlWAYkK/YBwk=").as_deref(),
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        assert_eq!(base64_to_hex("not base64!"), None);
        assert_eq!(base64_to_hex(""), None);
    }

    /// A clip bigger than the room left stops one byte past it, so the
    /// caller can tell, and never fills the disk.
    #[test]
    fn a_clip_download_stops_past_its_limit() {
        let dir = std::env::temp_dir().join(format!("raam-download-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("clip.mp4.part");
        for (len, max, written) in [(1000, 4096, 1000), (10_000, 4096, 4097)] {
            let client = Client::new(Config {
                url: serve_once(len),
                key: "key".into(),
            })
            .unwrap();
            assert_eq!(
                client
                    .download_video(&RemoteId::new("a"), &dest, max)
                    .unwrap(),
                written
            );
            assert_eq!(std::fs::metadata(&dest).unwrap().len(), written);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
