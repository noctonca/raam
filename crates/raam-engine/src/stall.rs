//! Test-only: `debug.video.stall=<site>[@<writer>]:<seconds>` holds a write
//! window open, so a power cut can be aimed into it (raam#17,
//! docs/plan/power-cut-tests.md). Unset, or anything that doesn't parse,
//! never stalls.
//!
//! Sites, in a write's order:
//! - `export`: the curation change is committed, the export not yet written;
//! - `rename`: a temp file is written and synced, not yet renamed;
//! - `row`: a preview is renamed and its directory synced, its row not yet
//!   committed.
//!
//! Writers: `curation` (the export), `local` (local previews), `prefetch`
//! and `fetch` (Immich previews, from the library and fetch threads).

use crate::Host;
use raam_model::limits::DEBUG_STALL_MAX;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Site {
    Export,
    Rename,
    Row,
}

impl Site {
    fn name(self) -> &'static str {
        match self {
            Site::Export => "export",
            Site::Rename => "rename",
            Site::Row => "row",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Writer {
    Curation,
    Local,
    Prefetch,
    Fetch,
}

impl Writer {
    fn name(self) -> &'static str {
        match self {
            Writer::Curation => "curation",
            Writer::Local => "local",
            Writer::Prefetch => "prefetch",
            Writer::Fetch => "fetch",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stall {
    site: Site,
    /// None: every writer.
    writer: Option<Writer>,
    time: Duration,
}

/// Sleeps here if the prop names this site (and this writer, if it names
/// one). `what` goes in the log line, which is the marker a test waits for.
pub fn at(host: &Host, site: Site, writer: Writer, what: &str) {
    let prop = host.switches.get("debug.video.stall");
    if prop.trim().is_empty() {
        return;
    }
    let Some(stall) = parse(&prop) else {
        log::warn!(
            "debug.video.stall={prop}: not <site>[@<writer>]:<seconds> up to {}, ignored",
            DEBUG_STALL_MAX.as_secs()
        );
        return;
    };
    if stall.site != site || stall.writer.is_some_and(|w| w != writer) {
        return;
    }
    log::warn!(
        "debug.video.stall: {} of {what} by {} for {}s",
        site.name(),
        writer.name(),
        stall.time.as_secs()
    );
    std::thread::sleep(stall.time);
}

fn parse(prop: &str) -> Option<Stall> {
    let (target, secs) = prop.trim().split_once(':')?;
    let (site, writer) = match target.split_once('@') {
        Some((s, w)) => (s, Some(w)),
        None => (target, None),
    };
    let site = match site {
        "export" => Site::Export,
        "rename" => Site::Rename,
        "row" => Site::Row,
        _ => return None,
    };
    let writer = match writer {
        None => None,
        Some("curation") => Some(Writer::Curation),
        Some("local") => Some(Writer::Local),
        Some("prefetch") => Some(Writer::Prefetch),
        Some("fetch") => Some(Writer::Fetch),
        Some(_) => return None,
    };
    let time = Duration::from_secs(secs.parse::<u64>().ok()?);
    if time.is_zero() || time > DEBUG_STALL_MAX {
        return None;
    }
    Some(Stall { site, writer, time })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_site_a_writer_and_seconds() {
        assert_eq!(
            parse("export:30"),
            Some(Stall {
                site: Site::Export,
                writer: None,
                time: Duration::from_secs(30)
            })
        );
        assert_eq!(
            parse(" rename@prefetch:5 "),
            Some(Stall {
                site: Site::Rename,
                writer: Some(Writer::Prefetch),
                time: Duration::from_secs(5)
            })
        );
        assert_eq!(
            parse("row@fetch:1").map(|s| (s.site, s.writer)),
            Some((Site::Row, Some(Writer::Fetch)))
        );
    }

    /// A typo must never stall a write: the frame may be in daily use.
    #[test]
    fn anything_malformed_never_stalls() {
        let max = DEBUG_STALL_MAX.as_secs();
        for prop in [
            "",
            "export",
            "export:",
            "export:0",
            "export:-1",
            "export:1.5",
            "export:x",
            &format!("export:{}", max + 1),
            "Export:5",
            "rename@:5",
            "rename@thread:5",
            "write:5",
            ":5",
        ] {
            assert_eq!(parse(prop), None, "{prop:?}");
        }
        assert!(parse(&format!("export:{max}")).is_some());
    }
}
