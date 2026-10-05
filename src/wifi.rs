//! Wi-Fi on Linux through wpa_supplicant's control socket: the network
//! seam (raam-core network.rs) for the desktop host. A worker thread owns
//! two sockets on `/run/wpa_supplicant/<interface>`, one for requests and
//! one `ATTACH`ed for events. It keeps a snapshot of the links and Wi-Fi,
//! runs the UI's commands, and wakes the event loop when the snapshot
//! changes.
//!
//! Nothing here needs root: the socket belongs to a group (`netdev` on
//! Debian), and Raam's user has to be in it. What a device needs is in
//! docs/BUILDING.md, "Wi-Fi".
//!
//! wpa_supplicant's ways that shape this:
//! - `SCAN_RESULTS` is cut at 4 KiB, a few dozen networks, so results are
//!   read one BSS at a time.
//! - Network ids change when wpa_supplicant restarts: networks are found
//!   by name each time.
//! - `SELECT_NETWORK` disables every other network until they're enabled
//!   again, so a join puts back what was on before it.
//! - `SAVE_CONFIG` writes the file with the service's umask.

use raam_core::network::{JoinError, Link, LinkKind, Nearby, Security, Ssid};

/// wpa_supplicant's text, parsed. Pure, so it's tested on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod parse {
    use super::*;
    use std::collections::BTreeMap;

    /// Undoes wpa_supplicant's `printf_encode` (SSIDs in its replies).
    pub fn decode(s: &str) -> Vec<u8> {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 1 < b.len() {
                i += 1;
                match b[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'e' => out.push(0x1b),
                    b'x' if i + 2 < b.len() => {
                        match std::str::from_utf8(&b[i + 1..i + 3])
                            .ok()
                            .and_then(|h| u8::from_str_radix(h, 16).ok())
                        {
                            Some(v) => {
                                out.push(v);
                                i += 2;
                            }
                            None => out.push(b'x'),
                        }
                    }
                    c => out.push(c),
                }
            } else {
                out.push(b[i]);
            }
            i += 1;
        }
        out
    }

    /// An SSID as `SET_NETWORK` takes it unquoted: hex, so no byte needs
    /// escaping.
    pub fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    pub fn kv(s: &str) -> BTreeMap<&str, &str> {
        s.lines().filter_map(|l| l.split_once('=')).collect()
    }

    /// A BSS's security from its flags, e.g. `[WPA2-PSK+SAE-CCMP][ESS]`.
    pub fn security(flags: &str) -> Security {
        let (psk, sae) = (flags.contains("PSK"), flags.contains("SAE"));
        if flags.contains("WEP") {
            Security::Wep
        } else if psk && sae {
            Security::Wpa2Wpa3
        } else if sae {
            Security::Wpa3
        } else if psk {
            Security::Wpa2
        } else if flags.contains("EAP") {
            Security::Enterprise
        } else if flags.contains("OWE") {
            Security::Owe
        } else {
            Security::Open
        }
    }

    /// The joined network's security from `STATUS`'s `key_mgmt`.
    pub fn joined_security(key_mgmt: &str) -> Security {
        match key_mgmt {
            "NONE" => Security::Open,
            "OWE" => Security::Owe,
            k if k.contains("SAE") => Security::Wpa3,
            k if k.contains("EAP") => Security::Enterprise,
            _ => Security::Wpa2,
        }
    }

    /// One access point, from a `BSS` reply.
    #[derive(Debug)]
    pub struct Bss {
        pub id: u32,
        pub ssid: Vec<u8>,
        pub freq: u32,
        pub level: i32,
        pub flags: String,
    }

    /// A `BSS` reply; empty (or `FAIL`) past the last one.
    pub fn bss(reply: &str) -> Option<Bss> {
        let m = kv(reply);
        Some(Bss {
            id: m.get("id")?.parse().ok()?,
            ssid: decode(m.get("ssid").copied().unwrap_or("")),
            freq: m.get("freq").and_then(|f| f.parse().ok()).unwrap_or(0),
            level: m.get("level").and_then(|l| l.parse().ok()).unwrap_or(-100),
            flags: m.get("flags").copied().unwrap_or("").to_string(),
        })
    }

    /// Access points folded into networks by name: the strongest one's
    /// signal and security, every band heard. Hidden ones (no name) are
    /// left out; strongest first.
    pub fn fold(bsses: &[Bss]) -> Vec<Nearby> {
        let mut nets: Vec<Nearby> = Vec::new();
        for b in bsses {
            if b.ssid.is_empty() || b.ssid.iter().all(|&c| c == 0) {
                continue;
            }
            let (two, five) = (b.freq < 3000, b.freq >= 5000);
            let sec = security(&b.flags);
            match nets.iter_mut().find(|n| n.ssid.0 == b.ssid) {
                Some(n) => {
                    n.band_2g |= two;
                    n.band_5g |= five;
                    if b.level > n.rssi {
                        n.rssi = b.level;
                        n.security = sec;
                    }
                }
                None => nets.push(Nearby {
                    ssid: Ssid(b.ssid.clone()),
                    rssi: b.level,
                    security: sec,
                    band_2g: two,
                    band_5g: five,
                }),
            }
        }
        nets.sort_by(|a, b| b.rssi.cmp(&a.rssi).then(a.ssid.cmp(&b.ssid)));
        nets
    }

    /// A network wpa_supplicant knows, from `LIST_NETWORKS`.
    #[derive(Clone, Debug)]
    pub struct Saved {
        pub id: u32,
        pub ssid: Ssid,
        pub disabled: bool,
    }

    pub fn saved(list: &str) -> Vec<Saved> {
        list.lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                Some(Saved {
                    id: f.first()?.parse().ok()?,
                    ssid: Ssid(decode(f.get(1)?)),
                    disabled: f.get(3).is_some_and(|x| x.contains("[DISABLED]")),
                })
            })
            .collect()
    }

    /// The interface of the default route with the lowest metric, from
    /// /proc/net/route.
    pub fn default_route(route: &str) -> Option<String> {
        route
            .lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                (f.len() > 6 && f[1] == "00000000")
                    .then(|| (f[6].parse::<u32>().unwrap_or(u32::MAX), f[0]))
            })
            .min()
            .map(|(_, i)| i.to_string())
    }

    /// The `SET_NETWORK` fields a security needs, besides the key. `sae`:
    /// the adapter and wpa_supplicant can do WPA3's SAE.
    pub fn key_mgmt(sec: Security, sae: bool) -> Option<Vec<(&'static str, &'static str)>> {
        Some(match sec {
            Security::Open => vec![("key_mgmt", "NONE")],
            Security::Owe => vec![("key_mgmt", "OWE"), ("ieee80211w", "2")],
            Security::Wpa2 => vec![("key_mgmt", "WPA-PSK")],
            Security::Wpa2Wpa3 if sae => vec![("key_mgmt", "WPA-PSK SAE"), ("ieee80211w", "1")],
            Security::Wpa2Wpa3 => vec![("key_mgmt", "WPA-PSK")],
            Security::Wpa3 if sae => vec![("key_mgmt", "SAE"), ("ieee80211w", "2")],
            _ => return None,
        })
    }

    #[derive(Debug, PartialEq)]
    pub enum Event<'a> {
        ScanStarted,
        ScanResults,
        ScanFailed,
        Connected(Option<u32>),
        Disconnected,
        /// A network gave up on for a while, and why (`WRONG_KEY`).
        TempDisabled(Option<u32>, &'a str),
        NotFound,
        Other,
    }

    /// An event with its `<level>` already stripped.
    pub fn event(e: &str) -> Event<'_> {
        let mut words = e.split(' ');
        let name = words.next().unwrap_or("");
        let field = |key: &str| e.split([' ', '[', ']']).find_map(|w| w.strip_prefix(key));
        let id = || field("id=").and_then(|v| v.parse().ok());
        match name {
            "CTRL-EVENT-SCAN-STARTED" => Event::ScanStarted,
            "CTRL-EVENT-SCAN-RESULTS" => Event::ScanResults,
            "CTRL-EVENT-SCAN-FAILED" => Event::ScanFailed,
            "CTRL-EVENT-CONNECTED" => Event::Connected(id()),
            "CTRL-EVENT-DISCONNECTED" => Event::Disconnected,
            "CTRL-EVENT-SSID-TEMP-DISABLED" => {
                Event::TempDisabled(id(), field("reason=").unwrap_or(""))
            }
            "CTRL-EVENT-NETWORK-NOT-FOUND" => Event::NotFound,
            _ => Event::Other,
        }
    }

    /// Why a join failed, from a `TempDisabled` reason.
    pub fn failure(reason: &str) -> JoinError {
        if reason == "WRONG_KEY" {
            JoinError::WrongKey
        } else {
            JoinError::Failed
        }
    }

    /// A group's name from /etc/group's text.
    pub fn group_name(groups: &str, gid: u32) -> Option<String> {
        groups.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 2 && f[2].parse() == Ok(gid)).then(|| f[0].to_string())
        })
    }

    /// What kind of link a /sys/class/net entry is, and whether it's
    /// connected. Virtual links (lo, VPNs, bridges) have no `device`.
    pub fn link(name: &str, has_device: bool, wireless: bool, carrier: &str) -> Option<Link> {
        has_device.then(|| Link {
            name: name.to_string(),
            kind: if wireless {
                LinkKind::Wifi
            } else {
                LinkKind::Ethernet
            },
            connected: carrier.trim() == "1",
            default_route: false,
        })
    }
}

#[cfg(target_os = "linux")]
pub use worker::{WpaNetwork as Net, spawn};

/// No wpa_supplicant off Linux: Connectivity says Wi-Fi isn't available.
#[cfg(not(target_os = "linux"))]
pub fn spawn(_waker: std::sync::Arc<dyn raam_core::seams::Waker>) -> Option<Net> {
    None
}

#[cfg(not(target_os = "linux"))]
pub struct Net;

#[cfg(not(target_os = "linux"))]
impl raam_core::network::Network for Net {
    fn version(&self) -> u64 {
        0
    }
    fn snapshot(&self) -> raam_core::network::NetSnapshot {
        unreachable!("never spawned")
    }
    fn send(&self, _cmd: raam_core::network::NetCommand) {}
}

/// Built on Linux, and in tests on any Unix, where a socket pair stands in
/// for wpa_supplicant.
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only the tests use it off Linux")
)]
mod worker {
    use super::parse::{self, Event};
    use super::*;
    use raam_core::clock;
    use raam_core::network::{
        self, Current, Join, JoinStage, NetCommand, NetSnapshot, Network, Wifi,
    };
    use raam_core::seams::Waker;
    use raam_model::limits::WIFI_EVENT_WAIT;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixDatagram;
    use std::path::Path;
    use std::sync::mpsc;
    // What only the Linux build uses: opening the real sockets, and the loop.
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    #[cfg(target_os = "linux")]
    use {
        raam_model::limits::WIFI_REPLY_WAIT,
        std::os::linux::net::SocketAddrExt,
        std::os::unix::net::SocketAddr,
        std::path::PathBuf,
        std::sync::atomic::{AtomicU32, Ordering},
        std::sync::mpsc::{RecvTimeoutError, TryRecvError},
    };

    const DIR: &str = "/run/wpa_supplicant";
    /// How often the links and Wi-Fi status are read with nothing going
    /// on, and how often a lost socket is retried.
    const REFRESH: Duration = Duration::from_secs(5);
    /// Joined, waiting for an address: status is read this often.
    const ADDRESS_POLL: Duration = Duration::from_millis(500);
    /// A join that hasn't connected by now failed. A wrong key shows at
    /// about 10 s, a missing network at about 23 s (docs/BUILDING.md,
    /// "Wi-Fi").
    const JOIN_TIMEOUT: Duration = Duration::from_secs(40);
    /// Joined but no address by now: DHCP isn't answering.
    const ADDRESS_TIMEOUT: Duration = Duration::from_secs(30);
    /// wpa_supplicant scans this many times for a network before a join
    /// gives up on it.
    const NOT_FOUND_LIMIT: u32 = 3;
    /// `BSS` fields: id, freq, level, flags, ssid.
    const BSS_MASK: &str = "MASK=0x1885";

    /// What went wrong driving Wi-Fi, by where it came from.
    #[derive(Debug)]
    pub enum WifiError {
        /// The control socket failed, or a reply didn't come within
        /// `WIFI_REPLY_WAIT`: the environment. The sockets are opened
        /// again, since a reply that comes late would be read as the next
        /// request's.
        Socket {
            doing: String,
            source: std::io::Error,
        },
        /// wpa_supplicant answered, but not as the request needs (`FAIL`,
        /// an id that isn't one). The request fails; the socket is good.
        Refused { doing: String, reply: String },
        /// Wi-Fi can't be used here (no adapter, no wpa_supplicant, no
        /// permission, a blocked radio), in the person's words for
        /// Connectivity. Looked for again every `REFRESH`.
        Unavailable(String),
        /// A command that can't run as asked: a join already under way, a
        /// network that isn't saved, a security this adapter can't join, a
        /// password wpa_supplicant wouldn't take. The input.
        Rejected(String),
    }

    impl WifiError {
        /// The sockets have to be opened again: they failed, or Wi-Fi
        /// went away under them.
        pub fn reconnects(&self) -> bool {
            match self {
                WifiError::Socket { .. } | WifiError::Unavailable(_) => true,
                WifiError::Refused { .. } | WifiError::Rejected(_) => false,
            }
        }

        fn socket(doing: &str) -> impl FnOnce(std::io::Error) -> Self {
            move |source| WifiError::Socket {
                doing: doing.to_string(),
                source,
            }
        }
    }

    impl std::fmt::Display for WifiError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                WifiError::Socket { doing, source } => write!(f, "{doing}: {source}"),
                WifiError::Refused { doing, reply } => write!(f, "{doing}: {reply}"),
                WifiError::Unavailable(why) | WifiError::Rejected(why) => write!(f, "{why}"),
            }
        }
    }

    impl std::error::Error for WifiError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                WifiError::Socket { source, .. } => Some(source),
                WifiError::Refused { .. } | WifiError::Unavailable(_) | WifiError::Rejected(_) => {
                    None
                }
            }
        }
    }

    /// A request as errors and logs name it: its first words, so a key
    /// never reaches either.
    fn doing(cmd: &str) -> String {
        cmd.split(' ').take(3).collect::<Vec<_>>().join(" ")
    }

    #[cfg(target_os = "linux")]
    static SOCKETS: AtomicU32 = AtomicU32::new(0);

    /// One datagram socket to wpa_supplicant.
    struct Ctrl(UnixDatagram);

    impl Ctrl {
        /// Bound in the abstract namespace (no file to clean up).
        #[cfg(target_os = "linux")]
        fn open(path: &Path) -> std::io::Result<Ctrl> {
            let name = format!(
                "raam-wifi-{}-{}",
                std::process::id(),
                SOCKETS.fetch_add(1, Ordering::Relaxed)
            );
            let sock = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name)?)?;
            sock.connect(path)?;
            Ctrl::new(sock, WIFI_REPLY_WAIT)
        }

        /// `wait` bounds each send and each reply.
        fn new(sock: UnixDatagram, wait: Duration) -> std::io::Result<Ctrl> {
            assert!(!wait.is_zero(), "a zero wait means none to the socket");
            sock.set_read_timeout(Some(wait))?;
            sock.set_write_timeout(Some(wait))?;
            Ok(Ctrl(sock))
        }

        fn req(&self, cmd: &str) -> Result<String, WifiError> {
            self.0
                .send(cmd.as_bytes())
                .map_err(WifiError::socket(&doing(cmd)))?;
            let mut buf = vec![0u8; 16 * 1024];
            let n = self
                .0
                .recv(&mut buf)
                .map_err(WifiError::socket(&doing(cmd)))?;
            Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
        }

        fn ok(&self, cmd: &str) -> Result<(), WifiError> {
            let r = self.req(cmd)?;
            if r.trim() == "OK" {
                Ok(())
            } else {
                Err(WifiError::Refused {
                    doing: doing(cmd),
                    reply: r.trim().to_string(),
                })
            }
        }

        /// The next event, level stripped; `None` if none came in `wait`.
        fn event(&self, wait: Duration) -> Result<Option<String>, WifiError> {
            self.0
                .set_read_timeout(Some(wait))
                .map_err(WifiError::socket("events"))?;
            let mut buf = vec![0u8; 4096];
            match self.0.recv(&mut buf) {
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]);
                    Ok(Some(match s.find('>') {
                        Some(i) if s.starts_with('<') => s[i + 1..].to_string(),
                        _ => s.into_owned(),
                    }))
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(None)
                }
                Err(e) => Err(WifiError::socket("events")(e)),
            }
        }
    }

    struct Shared {
        snap: Mutex<(u64, NetSnapshot)>,
        waker: Arc<dyn Waker>,
    }

    /// The seam's handle: the event loop reads snapshots and sends
    /// commands; dropping it stops the worker.
    pub struct WpaNetwork {
        shared: Arc<Shared>,
        tx: mpsc::Sender<NetCommand>,
    }

    impl Network for WpaNetwork {
        fn version(&self) -> u64 {
            self.shared.snap.lock().unwrap().0
        }
        fn snapshot(&self) -> NetSnapshot {
            self.shared.snap.lock().unwrap().1.clone()
        }
        fn send(&self, cmd: NetCommand) {
            let _ = self.tx.send(cmd);
        }
    }

    #[cfg(target_os = "linux")]
    pub fn spawn(waker: Arc<dyn Waker>) -> Option<WpaNetwork> {
        let (links, address) = links();
        let shared = Arc::new(Shared {
            snap: Mutex::new((
                1,
                NetSnapshot {
                    links,
                    address,
                    wifi: Err("Looking for Wi-Fi…".into()),
                },
            )),
            waker,
        });
        let (tx, rx) = mpsc::channel();
        let worker = Worker::new(shared.clone(), rx);
        std::thread::Builder::new()
            .name("wifi".into())
            .spawn(move || worker.run())
            .map_err(|e| log::warn!("wifi: no worker thread: {e}"))
            .ok()?;
        Some(WpaNetwork { shared, tx })
    }

    /// What a join changed that has to be put back: the networks
    /// `SELECT_NETWORK` disabled, and the one it added. Each is known by id
    /// and name, so after a wpa_supplicant restart (new ids) it never
    /// touches another network.
    #[derive(Debug)]
    struct Restore {
        enable: Vec<parse::Saved>,
        remove: Option<(u32, Ssid)>,
    }

    impl Restore {
        /// `remove_added`: the join failed, so a network it added goes too.
        fn after(j: &Joining, remove_added: bool) -> Restore {
            Restore {
                enable: j.before.iter().filter(|s| !s.disabled).cloned().collect(),
                remove: (remove_added && j.added).then(|| (j.id, j.ssid.clone())),
            }
        }
    }

    /// A join under way.
    struct Joining {
        id: u32,
        ssid: Ssid,
        /// Added for this join: removed again if it fails.
        added: bool,
        /// The networks as they were, to put back.
        before: Vec<parse::Saved>,
        started: Instant,
        connected: Option<Instant>,
        not_found: u32,
    }

    struct Worker {
        shared: Arc<Shared>,
        rx: mpsc::Receiver<NetCommand>,
        /// Requests, then events.
        ctrl: Option<(Ctrl, Ctrl)>,
        /// The interface they're on.
        iface: String,
        sae: bool,
        wifi: Wifi,
        saved: Vec<parse::Saved>,
        /// Turned off with nothing saved, which wpa_supplicant can't hold.
        off: bool,
        join: Option<Joining>,
        /// Put back after the next refresh, so it works from the networks
        /// as they are; kept across a reconnect.
        restore: Option<Restore>,
        refreshed: Option<Instant>,
    }

    impl Worker {
        fn new(shared: Arc<Shared>, rx: mpsc::Receiver<NetCommand>) -> Worker {
            Worker {
                shared,
                rx,
                ctrl: None,
                iface: String::new(),
                sae: false,
                wifi: Wifi::default(),
                saved: Vec::new(),
                off: false,
                join: None,
                restore: None,
                refreshed: None,
            }
        }

        /// Bounded: each turn waits at most `WIFI_EVENT_WAIT` for an event,
        /// or `REFRESH` while wpa_supplicant can't be reached.
        #[cfg(target_os = "linux")]
        fn run(mut self) {
            loop {
                if self.ctrl.is_none() {
                    match self.connect() {
                        Ok(()) => {
                            log::info!("wifi: connected to wpa_supplicant");
                            self.refreshed = None;
                        }
                        Err(why) => {
                            self.publish(Err(why.to_string()));
                            // Commands wait for the socket; none can run.
                            match self.rx.recv_timeout(REFRESH) {
                                Err(RecvTimeoutError::Disconnected) => return,
                                Ok(cmd) => log::info!("wifi: {cmd:?} dropped, no wpa_supplicant"),
                                Err(RecvTimeoutError::Timeout) => {}
                            }
                            continue;
                        }
                    }
                }
                if let Err(e) = self.pass() {
                    self.recover(e);
                    if self.ctrl.is_none() {
                        continue;
                    }
                }
                match self.rx.try_recv() {
                    Err(TryRecvError::Disconnected) => return,
                    Ok(cmd) => {
                        if let Err(e) = self.command(cmd) {
                            self.recover(e);
                        }
                    }
                    Err(TryRecvError::Empty) => {}
                }
                if self.ctrl.is_some() {
                    self.publish(Ok(self.wifi.clone()));
                }
            }
        }

        /// After an error: one that reconnects drops the sockets and fails
        /// a join under way, whose events they carried. What the join
        /// changed is put back once they're open again.
        fn recover(&mut self, e: WifiError) {
            if !e.reconnects() {
                log::warn!("wifi: {e}");
                return;
            }
            log::warn!("wifi: {e}; reconnecting");
            self.end_join(JoinStage::Failed(JoinError::Failed));
            self.ctrl = None;
        }

        /// The sockets, which every request needs.
        ///
        /// # Panics
        ///
        /// If they're closed: `run` reconnects before anything else.
        fn ctrl(&self) -> &(Ctrl, Ctrl) {
            self.ctrl
                .as_ref()
                .expect("a request with no socket: run reconnects first")
        }

        /// One turn: an event if one comes within `WIFI_EVENT_WAIT`, the
        /// join's clock, and the status when it's due.
        fn pass(&mut self) -> Result<(), WifiError> {
            let ev = self.ctrl().1.event(WIFI_EVENT_WAIT)?;
            if let Some(e) = ev {
                self.on_event(&e)?;
            }
            self.step_join()?;
            let every = if self.join.as_ref().is_some_and(|j| j.connected.is_some()) {
                ADDRESS_POLL
            } else {
                REFRESH
            };
            if self.refreshed.is_none_or(|t| t.elapsed() >= every) {
                self.refresh()?;
                self.put_back()?;
            }
            Ok(())
        }

        /// Puts back what a join changed, against the networks the last
        /// refresh read. A refused step won't succeed later, so it's
        /// logged and the rest goes on; a socket error keeps it all for
        /// after the reconnect.
        fn put_back(&mut self) -> Result<(), WifiError> {
            let Some(r) = &self.restore else {
                return Ok(());
            };
            let known =
                |id: u32, ssid: &Ssid| self.saved.iter().any(|s| s.id == id && s.ssid == *ssid);
            let mut steps: Vec<String> = Vec::new();
            if let Some((id, ssid)) = &r.remove
                && known(*id, ssid)
            {
                steps.push(format!("REMOVE_NETWORK {id}"));
            }
            for s in r.enable.iter().filter(|s| known(s.id, &s.ssid)) {
                steps.push(format!("ENABLE_NETWORK {}", s.id));
            }
            for step in &steps {
                match self.ok(step) {
                    Ok(()) => {}
                    Err(e) if e.reconnects() => return Err(e),
                    Err(e) => log::warn!("wifi: not put back: {e}"),
                }
            }
            log::info!(
                "wifi: put back what the join changed ({} steps)",
                steps.len()
            );
            self.restore = None;
            self.refresh()
        }

        /// Ends a join under way before a command that changes the
        /// networks, and puts back what it changed, so the command acts on
        /// the networks as they were and the undo can't override it.
        fn settle_join(&mut self) -> Result<(), WifiError> {
            self.end_join(JoinStage::Failed(JoinError::Failed));
            if self.restore.is_some() {
                self.refresh()?;
                self.put_back()?;
            }
            Ok(())
        }

        fn req(&self, cmd: &str) -> Result<String, WifiError> {
            self.ctrl().0.req(cmd)
        }

        fn ok(&self, cmd: &str) -> Result<(), WifiError> {
            self.ctrl().0.ok(cmd)
        }

        /// Finds the Wi-Fi interface and opens its sockets, or says in the
        /// person's words why Wi-Fi can't be set up.
        #[cfg(target_os = "linux")]
        fn connect(&mut self) -> Result<(), WifiError> {
            let unavailable = |why: String| Err(WifiError::Unavailable(why));
            let (links, _) = links();
            let wireless: Vec<&str> = links
                .iter()
                .filter(|l| l.kind == LinkKind::Wifi)
                .map(|l| l.name.as_str())
                .collect();
            let Some(first) = wireless.first() else {
                return unavailable("This device has no Wi-Fi adapter.".into());
            };
            let Some(iface) = wireless.iter().find(|i| Path::new(DIR).join(i).exists()) else {
                if Path::new("/run/NetworkManager").exists() {
                    return unavailable("Wi-Fi on this device is managed by NetworkManager, which Raam can't set up yet. Use nmcli or the desktop's network settings.".into());
                }
                return unavailable(format!(
                    "wpa_supplicant isn't running for {first}, so Raam can't set up Wi-Fi."
                ));
            };
            if let Some(why) = blocked(iface) {
                return unavailable(why);
            }
            let path = Path::new(DIR).join(iface);
            let open = |p: &PathBuf| {
                Ctrl::open(p).map_err(|e| WifiError::Unavailable(unreachable_why(e, p)))
            };
            let req = open(&path)?;
            let ev = open(&path)?;
            ev.ok("ATTACH")?;
            self.iface = iface.to_string();
            self.sae = req
                .req("GET_CAPABILITY key_mgmt")
                .is_ok_and(|r| r.split_whitespace().any(|k| k == "SAE"));
            self.ctrl = Some((req, ev));
            Ok(())
        }

        fn refresh(&mut self) -> Result<(), WifiError> {
            self.refreshed = Some(Instant::now());
            self.saved = parse::saved(&self.req("LIST_NETWORKS")?);
            let status = self.req("STATUS")?;
            let st = parse::kv(&status);
            self.wifi.current = if st.get("wpa_state") == Some(&"COMPLETED") {
                let poll = self.req("SIGNAL_POLL").unwrap_or_default();
                let sig = parse::kv(&poll);
                Some(Current {
                    ssid: Ssid(parse::decode(st.get("ssid").copied().unwrap_or(""))),
                    rssi: sig.get("RSSI").and_then(|r| r.parse().ok()),
                    freq_mhz: st.get("freq").and_then(|f| f.parse().ok()).unwrap_or(0),
                    security: parse::joined_security(st.get("key_mgmt").copied().unwrap_or("")),
                    address: st.get("ip_address").map(|a| a.to_string()),
                    link_mbps: sig.get("LINKSPEED").and_then(|l| l.parse().ok()),
                })
            } else {
                None
            };
            let mut names: Vec<Ssid> = Vec::new();
            for s in &self.saved {
                if !names.contains(&s.ssid) {
                    names.push(s.ssid.clone());
                }
            }
            self.wifi.saved = names;
            self.wifi.enabled = if self.saved.is_empty() {
                !self.off
            } else {
                self.saved.iter().any(|s| !s.disabled)
            };
            // Blocked since: the same "can't" as at the start.
            match blocked(&self.iface) {
                Some(why) => Err(WifiError::Unavailable(why)),
                None => Ok(()),
            }
        }

        fn on_event(&mut self, e: &str) -> Result<(), WifiError> {
            match parse::event(e) {
                Event::ScanStarted => self.wifi.scanning = true,
                Event::ScanFailed => self.wifi.scanning = false,
                Event::ScanResults => {
                    self.wifi.nearby = self.read_scan()?;
                    self.wifi.scanning = false;
                    let t = clock::local(clock::wall_secs());
                    self.wifi.scanned_at = format!("{:02}:{:02}", t.hour, t.min);
                }
                Event::Connected(id) => {
                    if let Some(j) = &mut self.join
                        && id.is_none_or(|i| i == j.id)
                    {
                        j.connected = Some(Instant::now());
                        self.set_stage(JoinStage::Addressing);
                    }
                    self.refreshed = None;
                }
                Event::Disconnected => self.refreshed = None,
                Event::TempDisabled(id, reason) => {
                    if self
                        .join
                        .as_ref()
                        .is_some_and(|j| id.is_none_or(|i| i == j.id))
                    {
                        log::info!("wifi: join refused ({reason})");
                        self.end_join(JoinStage::Failed(parse::failure(reason)));
                    }
                }
                Event::NotFound => {
                    if let Some(j) = &mut self.join {
                        j.not_found += 1;
                        if j.not_found >= NOT_FOUND_LIMIT {
                            self.end_join(JoinStage::Failed(JoinError::NotFound));
                        }
                    }
                }
                Event::Other => {}
            }
            Ok(())
        }

        /// Every BSS, one request each: `SCAN_RESULTS` stops at 4 KiB.
        fn read_scan(&self) -> Result<Vec<Nearby>, WifiError> {
            let mut all = Vec::new();
            let mut reply = self.req(&format!("BSS FIRST {BSS_MASK}"))?;
            while let Some(b) = parse::bss(&reply) {
                let id = b.id;
                all.push(b);
                if all.len() > 1024 {
                    break;
                }
                reply = self.req(&format!("BSS NEXT-{id} {BSS_MASK}"))?;
            }
            Ok(parse::fold(&all))
        }

        fn command(&mut self, cmd: NetCommand) -> Result<(), WifiError> {
            log::info!("wifi: {cmd:?}");
            match cmd {
                NetCommand::Scan => match self.req("SCAN")?.trim() {
                    // Busy: wpa_supplicant's own scan is on, and its
                    // results come the same way.
                    "OK" | "FAIL-BUSY" => self.wifi.scanning = true,
                    other => {
                        return Err(WifiError::Refused {
                            doing: "SCAN".into(),
                            reply: other.to_string(),
                        });
                    }
                },
                NetCommand::SetEnabled(on) => {
                    self.settle_join()?;
                    self.off = !on;
                    if !self.saved.is_empty() {
                        self.ok(if on {
                            "ENABLE_NETWORK all"
                        } else {
                            "DISABLE_NETWORK all"
                        })?;
                        self.save();
                    }
                    self.refresh()?;
                }
                NetCommand::Forget(ssid) => {
                    self.settle_join()?;
                    for s in self.saved.iter().filter(|s| s.ssid == ssid) {
                        self.ok(&format!("REMOVE_NETWORK {}", s.id))?;
                    }
                    self.save();
                    self.refresh()?;
                }
                NetCommand::Connect(ssid) => {
                    if self.join.is_some() {
                        return Err(WifiError::Rejected("a join is already under way".into()));
                    }
                    self.settle_join()?;
                    let id = self
                        .saved
                        .iter()
                        .find(|s| s.ssid == ssid)
                        .map(|s| s.id)
                        .ok_or_else(|| WifiError::Rejected("not a saved network".into()))?;
                    self.start_join(id, ssid, false);
                    if let Err(e) = self.ok(&format!("SELECT_NETWORK {id}")) {
                        self.end_join(JoinStage::Failed(JoinError::Failed));
                        return Err(e);
                    }
                }
                NetCommand::Join {
                    ssid,
                    security,
                    key,
                    hidden,
                } => {
                    if self.join.is_some() {
                        return Err(WifiError::Rejected("a join is already under way".into()));
                    }
                    self.settle_join()?;
                    let reply = self.req("ADD_NETWORK")?;
                    let id: u32 = reply.trim().parse().map_err(|_| WifiError::Refused {
                        doing: "ADD_NETWORK".into(),
                        reply: reply.trim().to_string(),
                    })?;
                    self.start_join(id, ssid.clone(), true);
                    if let Err(e) = self.set_up(id, &ssid, security, &key, hidden) {
                        self.end_join(JoinStage::Failed(JoinError::Failed));
                        return Err(e);
                    }
                }
            }
            Ok(())
        }

        fn start_join(&mut self, id: u32, ssid: Ssid, added: bool) {
            assert!(
                self.join.is_none(),
                "a join over {:?}",
                self.join.as_ref().map(|j| &j.ssid)
            );
            assert!(
                self.restore.is_none(),
                "a join must start from the networks as they were, not {:?}",
                self.restore
            );
            self.wifi.join = Some(Join {
                ssid: ssid.clone(),
                stage: JoinStage::Connecting,
            });
            self.join = Some(Joining {
                id,
                ssid,
                added,
                before: self.saved.clone(),
                started: Instant::now(),
                connected: None,
                not_found: 0,
            });
        }

        /// The new network's fields, then `SELECT_NETWORK`.
        fn set_up(
            &self,
            id: u32,
            ssid: &Ssid,
            sec: Security,
            key: &str,
            hidden: bool,
        ) -> Result<(), WifiError> {
            self.ok(&format!("SET_NETWORK {id} ssid {}", parse::hex(&ssid.0)))?;
            let fields = parse::key_mgmt(sec, self.sae).ok_or_else(|| {
                WifiError::Rejected(format!("{} can't be joined here", sec.label()))
            })?;
            for (k, v) in fields {
                self.ok(&format!("SET_NETWORK {id} {k} {v}"))?;
            }
            if sec.needs_key() {
                if let Some(p) = network::key_problem(key) {
                    return Err(WifiError::Rejected(format!("the password: {p}")));
                }
                // Quoted: a passphrase, which SAE needs as it is.
                // wpa_supplicant reads to the last quote, so quotes inside
                // are fine.
                self.ok(&format!("SET_NETWORK {id} psk \"{key}\""))?;
            }
            if hidden {
                self.ok(&format!("SET_NETWORK {id} scan_ssid 1"))?;
            }
            self.ok(&format!("SELECT_NETWORK {id}"))
        }

        fn set_stage(&mut self, stage: JoinStage) {
            if let Some(j) = &mut self.wifi.join {
                j.stage = stage;
            }
        }

        fn step_join(&mut self) -> Result<(), WifiError> {
            let Some(j) = &self.join else {
                return Ok(());
            };
            match j.connected {
                None if j.started.elapsed() > JOIN_TIMEOUT => {
                    self.end_join(JoinStage::Failed(JoinError::Failed));
                }
                Some(t) => {
                    let joined = self
                        .wifi
                        .current
                        .as_ref()
                        .is_some_and(|c| c.ssid == j.ssid && c.address.is_some());
                    if joined {
                        return self.finish_join();
                    } else if t.elapsed() > ADDRESS_TIMEOUT {
                        self.end_join(JoinStage::Failed(JoinError::NoAddress));
                    }
                }
                None => {}
            }
            Ok(())
        }

        /// Joined: older entries of the same name go, the others come back
        /// as they were, and it's all saved. Whatever goes wrong here, the
        /// join stays joined: a step wpa_supplicant refuses is logged, and
        /// a socket error leaves the others to be put back after the
        /// reconnect, unsaved.
        fn finish_join(&mut self) -> Result<(), WifiError> {
            let j = self
                .join
                .as_ref()
                .expect("finishing a join that isn't under way");
            assert!(
                j.connected.is_some(),
                "finishing {:?} before it connected",
                j.ssid
            );
            let mut steps: Vec<String> = Vec::new();
            for s in j.before.iter().filter(|s| s.id != j.id) {
                if j.added && s.ssid == j.ssid {
                    steps.push(format!("REMOVE_NETWORK {}", s.id));
                } else if !s.disabled {
                    steps.push(format!("ENABLE_NETWORK {}", s.id));
                }
            }
            for step in &steps {
                match self.ok(step) {
                    Ok(()) => {}
                    Err(e) if e.reconnects() => {
                        self.restore = self.join.as_ref().map(|j| Restore::after(j, false));
                        self.end_join(JoinStage::Joined { remembered: false });
                        return Err(e);
                    }
                    Err(e) => log::warn!("wifi: joined, but {e}"),
                }
            }
            let remembered = self.save();
            self.end_join(JoinStage::Joined { remembered });
            Ok(())
        }

        /// Ends the join with `stage`. A failed one is put back after the
        /// next refresh (`put_back`), never on the spot: it may be ending
        /// because the sockets just failed.
        fn end_join(&mut self, stage: JoinStage) {
            let Some(j) = self.join.take() else {
                return;
            };
            log::info!("wifi: join of {:?} ended: {stage:?}", j.ssid);
            match stage {
                JoinStage::Failed(_) => {
                    assert!(self.restore.is_none(), "two undos: {:?}", self.restore);
                    self.restore = Some(Restore::after(&j, true));
                }
                JoinStage::Joined { .. } => {}
                JoinStage::Connecting | JoinStage::Addressing => {
                    unreachable!("a join ends joined or failed, not {stage:?}")
                }
            }
            self.set_stage(stage);
            self.refreshed = None;
        }

        /// Writes wpa_supplicant's config; false if it can't
        /// (`update_config=0`).
        fn save(&self) -> bool {
            match self.ok("SAVE_CONFIG") {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("wifi: not saved ({e}): set update_config=1");
                    false
                }
            }
        }

        fn publish(&self, wifi: Result<Wifi, String>) {
            let (links, address) = links();
            let snap = NetSnapshot {
                links,
                address,
                wifi,
            };
            let mut s = self.shared.snap.lock().unwrap();
            if s.1 != snap {
                s.0 += 1;
                s.1 = snap;
                drop(s);
                self.shared.waker.wake();
            }
        }
    }

    /// Why a socket couldn't be opened, in the person's words.
    fn unreachable_why(e: std::io::Error, path: &Path) -> String {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            let group = std::fs::metadata(path)
                .ok()
                .and_then(|m| {
                    let groups = std::fs::read_to_string("/etc/group").ok()?;
                    parse::group_name(&groups, m.gid())
                })
                .unwrap_or_else(|| "netdev".into());
            format!(
                "Raam can't reach wpa_supplicant. Add its user to the {group} group (sudo usermod -aG {group} $USER), then start Raam again."
            )
        } else {
            format!("Can't reach wpa_supplicant: {e}.")
        }
    }

    /// Whether the interface's radio is blocked (rfkill), in the person's
    /// words. An unprivileged program can read the block but not lift it.
    fn blocked(iface: &str) -> Option<String> {
        let phy = format!("/sys/class/net/{iface}/phy80211");
        let dirs = std::fs::read_dir(&phy).ok()?;
        for d in dirs.filter_map(|d| d.ok()) {
            if !d.file_name().to_string_lossy().starts_with("rfkill") {
                continue;
            }
            let read = |f: &str| std::fs::read_to_string(d.path().join(f)).unwrap_or_default();
            if read("hard").trim() == "1" {
                return Some("Wi-Fi is switched off by a switch on this device.".into());
            }
            if read("soft").trim() == "1" {
                return Some("Wi-Fi is blocked on this device. Unblock it once with: sudo rfkill unblock wifi".into());
            }
        }
        None
    }

    /// The wired and Wi-Fi ports, and the address traffic leaves from.
    fn links() -> (Vec<Link>, Option<String>) {
        let route = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
        let default = parse::default_route(&route);
        let mut out: Vec<Link> = std::fs::read_dir("/sys/class/net")
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let p = e.path();
                        let carrier =
                            std::fs::read_to_string(p.join("carrier")).unwrap_or_default();
                        let mut l = parse::link(
                            &name,
                            p.join("device").exists(),
                            p.join("wireless").exists() || p.join("phy80211").exists(),
                            &carrier,
                        )?;
                        l.default_route = default.as_deref() == Some(name.as_str());
                        Some(l)
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| {
            (a.kind == LinkKind::Wifi, &a.name).cmp(&(b.kind == LinkKind::Wifi, &b.name))
        });
        // connect() on UDP sends nothing: it picks the route's source.
        let address = default.as_ref().and_then(|_| {
            let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
            s.connect("192.0.2.1:9").ok()?;
            Some(s.local_addr().ok()?.ip().to_string())
        });
        (out, address)
    }

    /// The worker against a socket pair whose other end plays
    /// wpa_supplicant, on any Unix.
    #[cfg(test)]
    mod tests {
        use super::*;

        struct NoWake;
        impl Waker for NoWake {
            fn wake(&self) {}
        }

        /// Short, so a request nobody answers fails fast.
        const WAIT: Duration = Duration::from_millis(50);

        /// A worker with its sockets open; the other ends of the requests
        /// and the events.
        fn rig() -> (Worker, UnixDatagram, UnixDatagram) {
            let (req, wpa) = UnixDatagram::pair().unwrap();
            let (ev, wpa_events) = UnixDatagram::pair().unwrap();
            let shared = Arc::new(Shared {
                snap: Mutex::new((
                    1,
                    NetSnapshot {
                        links: Vec::new(),
                        address: None,
                        wifi: Err(String::new()),
                    },
                )),
                waker: Arc::new(NoWake),
            });
            let (_tx, rx) = mpsc::channel();
            let mut w = Worker::new(shared, rx);
            w.ctrl = Some((Ctrl::new(req, WAIT).unwrap(), Ctrl::new(ev, WAIT).unwrap()));
            (w, wpa, wpa_events)
        }

        /// The requests wpa_supplicant has been sent, in order.
        fn sent(wpa: &UnixDatagram) -> Vec<String> {
            wpa.set_nonblocking(true).unwrap();
            let mut buf = [0u8; 4096];
            let mut out = Vec::new();
            while let Ok(n) = wpa.recv(&mut buf) {
                out.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                assert!(out.len() < 100, "a request flood: {out:?}");
            }
            out
        }

        #[test]
        fn a_reply_that_comes_late_drops_the_sockets() {
            let (mut w, wpa, _events) = rig();
            let e = w.command(NetCommand::Scan).unwrap_err();
            assert!(e.reconnects(), "no reply in time: {e}");
            w.recover(e);
            assert!(
                w.ctrl.is_none(),
                "kept, the late reply would be read as the next request's"
            );
            assert_eq!(sent(&wpa), ["SCAN"]);
        }

        #[test]
        fn a_refused_request_keeps_the_sockets() {
            let (mut w, wpa, _events) = rig();
            wpa.send(b"FAIL\n").unwrap();
            let e = w.command(NetCommand::Scan).unwrap_err();
            assert!(!e.reconnects(), "wpa_supplicant answered: {e}");
            w.recover(e);
            assert!(w.ctrl.is_some());
        }

        #[test]
        fn a_key_never_reaches_an_error() {
            let (w, _wpa, _events) = rig();
            let e = w.ok("SET_NETWORK 3 psk \"hunter22\"").unwrap_err();
            assert!(!e.to_string().contains("hunter22"), "{e}");
        }

        fn ssid(name: &str) -> Ssid {
            Ssid(name.as_bytes().to_vec())
        }

        fn saved(id: u32, name: &str, disabled: bool) -> parse::Saved {
            parse::Saved {
                id,
                ssid: ssid(name),
                disabled,
            }
        }

        /// Queues wpa_supplicant's replies, in the order they'll be read.
        fn answer(wpa: &UnixDatagram, replies: &[&str]) {
            for r in replies {
                wpa.send(r.as_bytes()).unwrap();
            }
        }

        /// New sockets, as after a reconnect.
        fn reopen(w: &mut Worker) -> (UnixDatagram, UnixDatagram) {
            assert!(w.ctrl.is_none(), "reopening open sockets");
            let (req, wpa) = UnixDatagram::pair().unwrap();
            let (ev, wpa_events) = UnixDatagram::pair().unwrap();
            w.ctrl = Some((Ctrl::new(req, WAIT).unwrap(), Ctrl::new(ev, WAIT).unwrap()));
            (wpa, wpa_events)
        }

        const LIST: &str = "network id / ssid / bssid / flags\n";
        const NOT_JOINED: &str = "wpa_state=DISCONNECTED\n";

        /// "New", added for the join as id 5, over "Home" (id 0).
        fn joining_new(w: &mut Worker) {
            w.saved = vec![saved(0, "Home", false)];
            w.start_join(5, ssid("New"), true);
        }

        fn stage(w: &Worker) -> JoinStage {
            w.wifi.join.as_ref().expect("a join was started").stage
        }

        #[test]
        fn a_failed_cleanup_keeps_the_network_just_joined() {
            let (mut w, wpa, _events) = rig();
            joining_new(&mut w);
            w.join.as_mut().unwrap().connected = Some(Instant::now());
            w.wifi.current = Some(Current {
                ssid: ssid("New"),
                rssi: None,
                freq_mhz: 2412,
                security: Security::Wpa2,
                address: Some("192.168.0.9".into()),
                link_mbps: None,
            });
            // ENABLE_NETWORK 0 refused, SAVE_CONFIG fine.
            answer(&wpa, &["FAIL\n", "OK\n"]);
            if let Err(e) = w.step_join() {
                w.recover(e);
            }
            assert_eq!(stage(&w), JoinStage::Joined { remembered: true });
            assert_eq!(sent(&wpa), ["ENABLE_NETWORK 0", "SAVE_CONFIG"]);
        }

        #[test]
        fn a_join_cut_off_by_a_dead_socket_is_put_back_after_reconnecting() {
            let (mut w, wpa, _events) = rig();
            joining_new(&mut w);
            let timed_out = std::io::Error::from(std::io::ErrorKind::TimedOut);
            w.recover(WifiError::socket("STATUS")(timed_out));
            assert_eq!(stage(&w), JoinStage::Failed(JoinError::Failed));
            assert!(
                sent(&wpa).is_empty(),
                "nothing goes to sockets that just failed"
            );

            let (wpa, _events) = reopen(&mut w);
            let list = format!("{LIST}0\tHome\tany\t[DISABLED]\n5\tNew\tany\t[DISABLED]\n");
            let after = format!("{LIST}0\tHome\tany\t\n");
            answer(
                &wpa,
                &[&list, NOT_JOINED, "OK\n", "OK\n", &after, NOT_JOINED],
            );
            w.refresh().unwrap();
            w.put_back().unwrap();
            assert_eq!(
                sent(&wpa),
                [
                    "LIST_NETWORKS",
                    "STATUS",
                    "REMOVE_NETWORK 5",
                    "ENABLE_NETWORK 0",
                    "LIST_NETWORKS",
                    "STATUS"
                ]
            );
            assert!(w.restore.is_none());
            assert!(w.wifi.enabled, "Home is back on");
        }

        #[test]
        fn a_restart_that_renumbered_the_networks_is_left_alone() {
            let (mut w, wpa, _events) = rig();
            joining_new(&mut w);
            w.end_join(JoinStage::Failed(JoinError::Failed));
            // wpa_supplicant restarted: ids 0 and 5 are other networks now.
            let list = format!("{LIST}0\tOffice\tany\t[DISABLED]\n5\tCafe\tany\t\n");
            answer(&wpa, &[&list, NOT_JOINED, &list, NOT_JOINED]);
            w.refresh().unwrap();
            w.put_back().unwrap();
            assert_eq!(
                sent(&wpa),
                ["LIST_NETWORKS", "STATUS", "LIST_NETWORKS", "STATUS"]
            );
        }

        #[test]
        fn turning_wifi_off_during_a_join_stays_off() {
            let (mut w, wpa, _events) = rig();
            joining_new(&mut w);
            let during = format!("{LIST}0\tHome\tany\t[DISABLED]\n5\tNew\tany\t\n");
            let back = format!("{LIST}0\tHome\tany\t\n");
            let off = format!("{LIST}0\tHome\tany\t[DISABLED]\n");
            #[rustfmt::skip]
            answer(&wpa, &[
                &during, NOT_JOINED, "OK\n", "OK\n", &back, NOT_JOINED,
                "OK\n", "OK\n", &off, NOT_JOINED,
            ]);
            w.command(NetCommand::SetEnabled(false)).unwrap();
            assert!(w.join.is_none(), "the join ended first");
            assert_eq!(
                sent(&wpa),
                [
                    "LIST_NETWORKS",
                    "STATUS",
                    "REMOVE_NETWORK 5",
                    "ENABLE_NETWORK 0",
                    "LIST_NETWORKS",
                    "STATUS",
                    "DISABLE_NETWORK all",
                    "SAVE_CONFIG",
                    "LIST_NETWORKS",
                    "STATUS"
                ]
            );
            assert!(!w.wifi.enabled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse::*;
    use super::*;

    #[test]
    fn ssids_decode_from_wpa_supplicants_escapes() {
        assert_eq!(decode("__[iFi]__"), b"__[iFi]__");
        assert_eq!(decode(r"caf\xc3\xa9"), "café".as_bytes());
        assert_eq!(decode(r#"a\"b\\c"#), br#"a"b\c"#);
        assert_eq!(hex(b"Home"), "486f6d65");
    }

    #[test]
    fn flags_give_the_security() {
        assert_eq!(security("[WPA2-PSK-CCMP][WPS][ESS]"), Security::Wpa2);
        assert_eq!(
            security("[WPA2-PSK+SAE-CCMP][SAE-H2E][WPS][ESS][FILS]"),
            Security::Wpa2Wpa3
        );
        assert_eq!(security("[WPA2-SAE-CCMP][ESS]"), Security::Wpa3);
        assert_eq!(security("[WPA-PSK-CCMP+TKIP][ESS]"), Security::Wpa2);
        assert_eq!(security("[WPA2-EAP-CCMP][ESS]"), Security::Enterprise);
        assert_eq!(security("[WEP][ESS]"), Security::Wep);
        assert_eq!(security("[WPA2-OWE-CCMP][ESS]"), Security::Owe);
        assert_eq!(security("[ESS][FILS]"), Security::Open);
    }

    #[test]
    fn access_points_fold_into_networks_strongest_first() {
        let b = |id, ssid: &str, freq, level, flags: &str| Bss {
            id,
            ssid: ssid.as_bytes().to_vec(),
            freq,
            level,
            flags: flags.into(),
        };
        let nets = fold(&[
            b(0, "Home", 5240, -72, "[WPA2-PSK-CCMP][ESS]"),
            b(1, "Home", 2432, -62, "[WPA2-PSK-CCMP][ESS]"),
            b(2, "", 2432, -40, "[WPA2-PSK-CCMP][ESS]"),
            b(3, "Cafe", 2412, -50, "[ESS]"),
        ]);
        let names: Vec<String> = nets.iter().map(|n| n.ssid.show()).collect();
        assert_eq!(names, ["Cafe", "Home"], "the hidden one is left out");
        let home = &nets[1];
        assert_eq!(home.rssi, -62);
        assert!(home.band_2g && home.band_5g);
    }

    #[test]
    fn a_bss_reply_parses_and_the_end_is_none() {
        let r = "id=7\nfreq=5240\nlevel=-70\nflags=[WPA2-PSK-CCMP][ESS]\nssid=__[iFi]__\n";
        let b = bss(r).unwrap();
        assert_eq!((b.id, b.freq, b.level), (7, 5240, -70));
        assert_eq!(b.ssid, b"__[iFi]__");
        assert!(bss("").is_none());
        assert!(bss("FAIL\n").is_none());
    }

    #[test]
    fn saved_networks_list_with_their_disabled_flag() {
        let list = "network id / ssid / bssid / flags\n0\tHome\tany\t[CURRENT]\n3\tFlat\tany\t[DISABLED]\n";
        let s = saved(list);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].id, s[0].disabled), (0, false));
        assert_eq!(
            (s[1].id, s[1].ssid.show(), s[1].disabled),
            (3, "Flat".into(), true)
        );
    }

    #[test]
    fn the_default_route_is_the_lowest_metric() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
            wlan0\t00000000\t0100A8C0\t0003\t0\t0\t3004\t00000000\n\
            eth0\t00000000\t0100A8C0\t0003\t0\t0\t1002\t00000000\n\
            eth0\t0000A8C0\t00000000\t0001\t0\t0\t1002\t00FFFFFF\n";
        assert_eq!(default_route(route).as_deref(), Some("eth0"));
        assert_eq!(default_route("Iface\n"), None);
    }

    #[test]
    fn events_parse_with_their_ids_and_reasons() {
        assert_eq!(
            event(
                "CTRL-EVENT-SSID-TEMP-DISABLED id=1 ssid=\"__[iFi]__\" auth_failures=1 duration=10 reason=WRONG_KEY"
            ),
            Event::TempDisabled(Some(1), "WRONG_KEY")
        );
        assert_eq!(
            event(
                "CTRL-EVENT-CONNECTED - Connection to 78:8c:b5:00:00:00 completed [id=2 id_str=]"
            ),
            Event::Connected(Some(2))
        );
        assert_eq!(event("CTRL-EVENT-NETWORK-NOT-FOUND "), Event::NotFound);
        assert_eq!(event("CTRL-EVENT-SCAN-RESULTS "), Event::ScanResults);
        assert_eq!(event("WPS-AP-AVAILABLE "), Event::Other);
        assert_eq!(failure("WRONG_KEY"), JoinError::WrongKey);
        assert_eq!(failure("CONN_FAILED"), JoinError::Failed);
    }

    #[test]
    fn a_joins_fields_follow_the_security_and_the_adapter() {
        assert_eq!(
            key_mgmt(Security::Open, true),
            Some(vec![("key_mgmt", "NONE")])
        );
        assert_eq!(
            key_mgmt(Security::Wpa2Wpa3, true),
            Some(vec![("key_mgmt", "WPA-PSK SAE"), ("ieee80211w", "1")])
        );
        // No SAE here: WPA2 alone still joins a transition network.
        assert_eq!(
            key_mgmt(Security::Wpa2Wpa3, false),
            Some(vec![("key_mgmt", "WPA-PSK")])
        );
        assert_eq!(key_mgmt(Security::Wpa3, false), None);
        assert_eq!(key_mgmt(Security::Enterprise, true), None);
    }

    #[test]
    fn a_sockets_group_is_named_from_etc_group() {
        let g = "root:x:0:\nnetdev:x:102:noctonca\n";
        assert_eq!(group_name(g, 102).as_deref(), Some("netdev"));
        assert_eq!(group_name(g, 5), None);
    }

    #[test]
    fn only_real_ports_are_links() {
        assert!(link("tailscale0", false, false, "1").is_none());
        let l = link("wlan0", true, true, "1\n").unwrap();
        assert_eq!((l.kind, l.connected), (LinkKind::Wifi, true));
        let e = link("eth0", true, false, "0").unwrap();
        assert_eq!((e.kind, e.connected), (LinkKind::Ethernet, false));
    }
}
