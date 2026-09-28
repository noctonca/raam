//! The network seam: the device's links and its Wi-Fi, for Settings →
//! Connectivity. The Linux desktop host drives wpa_supplicant on a worker
//! thread (src/wifi.rs); a host without it passes no network, and the
//! section says Wi-Fi can't be set up here.
//!
//! The controller copies a snapshot into the UI state whenever its version
//! moves, and sends the UI's commands on. A command's outcome shows in a
//! later snapshot: scans take seconds and joins longer, so nothing here
//! waits.

use crate::icons;
use std::fmt;

/// A network name: up to 32 bytes, not always UTF-8.
#[derive(Clone, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Ssid(pub Vec<u8>);

impl Ssid {
    pub fn new(name: &str) -> Self {
        Ssid(name.as_bytes().to_vec())
    }
    /// For display: invalid UTF-8 shows as U+FFFD.
    pub fn show(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

impl fmt::Debug for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.show())
    }
}

/// How a network is secured, as far as joining it goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    /// No password, and nothing encrypted.
    Open,
    /// No password, but encrypted (Wi-Fi Enhanced Open).
    Owe,
    /// WPA or WPA2 with a password.
    Wpa2,
    /// WPA2 and WPA3 both offered, one password.
    Wpa2Wpa3,
    Wpa3,
    /// Too old to join: WEP.
    Wep,
    /// A user name and password or a certificate (802.1X).
    Enterprise,
}

impl Security {
    pub fn needs_key(self) -> bool {
        matches!(self, Security::Wpa2 | Security::Wpa2Wpa3 | Security::Wpa3)
    }

    pub fn supported(self) -> bool {
        !matches!(self, Security::Wep | Security::Enterprise)
    }

    pub fn label(self) -> &'static str {
        match self {
            Security::Open => "Open",
            Security::Owe => "Open, encrypted",
            Security::Wpa2 => "WPA2",
            Security::Wpa2Wpa3 => "WPA2/WPA3",
            Security::Wpa3 => "WPA3",
            Security::Wep => "WEP",
            Security::Enterprise => "Enterprise",
        }
    }
}

/// Why a Wi-Fi password can't be used, if it can't. A passphrase is 8 to
/// 63 bytes (IEEE 802.11i); control characters can't be sent. A
/// 64-character key would be the hex form, which people don't type.
pub fn key_problem(key: &str) -> Option<&'static str> {
    if key.len() < 8 {
        Some("At least 8 characters")
    } else if key.len() > 63 {
        Some("At most 63 characters")
    } else if key.chars().any(char::is_control) {
        Some("Letters, digits, spaces and symbols only")
    } else {
        None
    }
}

/// Signal as 0 to 4 bars: even steps from -88 to -55 dBm.
pub fn bars(rssi: i32) -> u8 {
    match rssi {
        r if r >= -55 => 4,
        r if r >= -66 => 3,
        r if r >= -77 => 2,
        r if r >= -88 => 1,
        _ => 0,
    }
}

/// Signal in words, for the details.
pub fn signal_words(rssi: i32) -> &'static str {
    match bars(rssi) {
        4 => "Excellent",
        3 => "Good",
        2 => "Fair",
        1 => "Weak",
        _ => "Very weak",
    }
}

/// The bars icon, with a lock when the network needs a password.
pub fn signal_icon(bars: u8, locked: bool) -> char {
    match (bars, locked) {
        (0, _) => icons::SIGNAL_WIFI_0_BAR,
        (1, false) => icons::NETWORK_WIFI_1_BAR,
        (1, true) => icons::NETWORK_WIFI_1_BAR_LOCKED,
        (2, false) => icons::NETWORK_WIFI_2_BAR,
        (2, true) => icons::NETWORK_WIFI_2_BAR_LOCKED,
        (3, false) => icons::NETWORK_WIFI_3_BAR,
        (3, true) => icons::NETWORK_WIFI_3_BAR_LOCKED,
        (_, false) => icons::SIGNAL_WIFI_4_BAR,
        (_, true) => icons::SIGNAL_WIFI_4_BAR_LOCK,
    }
}

/// "2.4 GHz", "5 GHz" or both, from which bands a name was heard on.
pub fn bands(two: bool, five: bool) -> &'static str {
    match (two, five) {
        (true, true) => "2.4 and 5 GHz",
        (false, true) => "5 GHz",
        _ => "2.4 GHz",
    }
}

/// One network in range, by name: its access points folded into one.
#[derive(Clone, Debug, PartialEq)]
pub struct Nearby {
    pub ssid: Ssid,
    /// The strongest access point's, in dBm.
    pub rssi: i32,
    pub security: Security,
    pub band_2g: bool,
    pub band_5g: bool,
}

/// The Wi-Fi network in use.
#[derive(Clone, Debug, PartialEq)]
pub struct Current {
    pub ssid: Ssid,
    pub rssi: Option<i32>,
    pub freq_mhz: u32,
    pub security: Security,
    /// Its IPv4 address; `None` while it's being given one.
    pub address: Option<String>,
    pub link_mbps: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkKind {
    Ethernet,
    Wifi,
}

/// A network port the device has: a wired one or a Wi-Fi adapter. Virtual
/// links (VPNs, bridges) aren't listed.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub name: String,
    pub kind: LinkKind,
    /// A cable in, or Wi-Fi associated.
    pub connected: bool,
    /// Traffic leaves this way (the default route with the lowest metric).
    pub default_route: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinError {
    WrongKey,
    NotFound,
    /// Joined, but no address came (DHCP).
    NoAddress,
    Failed,
}

impl JoinError {
    pub fn message(self) -> &'static str {
        match self {
            JoinError::WrongKey => "Wrong password",
            JoinError::NotFound => "Not found nearby",
            JoinError::NoAddress => "Joined, but the network gave the frame no address",
            JoinError::Failed => "Couldn't join",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinStage {
    Connecting,
    /// Associated: waiting for an address.
    Addressing,
    /// `remembered` is false when the network couldn't be saved, so it's
    /// gone after a restart.
    Joined {
        remembered: bool,
    },
    Failed(JoinError),
}

impl JoinStage {
    pub fn busy(self) -> bool {
        matches!(self, JoinStage::Connecting | JoinStage::Addressing)
    }
}

/// The last join asked for, and how far it got.
#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    pub ssid: Ssid,
    pub stage: JoinStage,
}

/// Wi-Fi as Raam can set it up.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Wifi {
    /// Off: no network is used, and nothing is scanned.
    pub enabled: bool,
    pub current: Option<Current>,
    /// In range, strongest first. The current network is among them.
    pub nearby: Vec<Nearby>,
    /// Remembered networks, joined again when in range.
    pub saved: Vec<Ssid>,
    pub scanning: bool,
    /// When the last scan finished, as "14:32"; empty before the first.
    pub scanned_at: String,
    pub join: Option<Join>,
}

impl Wifi {
    pub fn is_saved(&self, ssid: &Ssid) -> bool {
        self.saved.contains(ssid)
    }
    pub fn joining(&self) -> bool {
        self.join.as_ref().is_some_and(|j| j.stage.busy())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetSnapshot {
    pub links: Vec<Link>,
    /// The address traffic leaves from, if there's a way out.
    pub address: Option<String>,
    /// Wi-Fi, or why Raam can't set it up here, in the person's words.
    pub wifi: Result<Wifi, String>,
}

impl NetSnapshot {
    /// The link traffic leaves by.
    pub fn route(&self) -> Option<&Link> {
        self.links.iter().find(|l| l.default_route)
    }
    pub fn ethernet_connected(&self) -> bool {
        self.links
            .iter()
            .any(|l| l.kind == LinkKind::Ethernet && l.connected)
    }
}

pub enum NetCommand {
    Scan,
    /// Join a network, and remember it once joined. `key` is empty for a
    /// network with no password.
    Join {
        ssid: Ssid,
        security: Security,
        key: String,
        hidden: bool,
    },
    /// Join a remembered network with what's saved for it.
    Connect(Ssid),
    Forget(Ssid),
    SetEnabled(bool),
}

// A password never reaches a log.
impl fmt::Debug for NetCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            NetCommand::Scan => write!(f, "Scan"),
            NetCommand::Join {
                ssid,
                security,
                hidden,
                ..
            } => write!(f, "Join({ssid:?}, {security:?}, hidden={hidden})"),
            NetCommand::Connect(s) => write!(f, "Connect({s:?})"),
            NetCommand::Forget(s) => write!(f, "Forget({s:?})"),
            NetCommand::SetEnabled(on) => write!(f, "SetEnabled({on})"),
        }
    }
}

/// The host's network worker.
pub trait Network {
    /// Bumped whenever the snapshot changes.
    fn version(&self) -> u64;
    fn snapshot(&self) -> NetSnapshot;
    /// Hands a command to the worker; its outcome shows in a later
    /// snapshot.
    fn send(&self, cmd: NetCommand);
}

// ---------------------------------------------------------------------------
// Fixtures, for the presets and the goldens.
// ---------------------------------------------------------------------------

fn nearby(name: &str, rssi: i32, security: Security, two: bool, five: bool) -> Nearby {
    Nearby {
        ssid: Ssid::new(name),
        rssi,
        security,
        band_2g: two,
        band_5g: five,
    }
}

/// Ethernet and Wi-Fi both up, Ethernet carrying the traffic; networks of
/// every kind around, strongest first.
pub fn sample() -> NetSnapshot {
    NetSnapshot {
        links: vec![
            Link {
                name: "eth0".into(),
                kind: LinkKind::Ethernet,
                connected: true,
                default_route: true,
            },
            Link {
                name: "wlan0".into(),
                kind: LinkKind::Wifi,
                connected: true,
                default_route: false,
            },
        ],
        address: Some("192.168.1.20".into()),
        wifi: Ok(Wifi {
            enabled: true,
            current: Some(Current {
                ssid: Ssid::new("Home"),
                rssi: Some(-58),
                freq_mhz: 5240,
                security: Security::Wpa2,
                address: Some("192.168.1.31".into()),
                link_mbps: Some(390),
            }),
            nearby: vec![
                nearby("Home", -58, Security::Wpa2, true, true),
                nearby("Home IoT", -61, Security::Wpa2, true, false),
                nearby("Café Lumière", -67, Security::Open, true, false),
                nearby("Neighbour 5G", -72, Security::Wpa2Wpa3, false, true),
                nearby("Office", -79, Security::Enterprise, true, true),
                nearby("Printer-2F", -84, Security::Wpa3, true, false),
            ],
            saved: vec![Ssid::new("Home"), Ssid::new("Holiday flat")],
            scanning: false,
            scanned_at: "14:32".into(),
            join: None,
        }),
    }
}

/// No cable and no Wi-Fi joined yet: a first run on a Wi-Fi-only device.
pub fn sample_unjoined() -> NetSnapshot {
    let mut s = sample();
    s.links = vec![Link {
        name: "wlan0".into(),
        kind: LinkKind::Wifi,
        connected: false,
        default_route: false,
    }];
    s.address = None;
    if let Ok(w) = &mut s.wifi {
        w.current = None;
        w.saved.clear();
    }
    s
}

/// Wi-Fi only, joined: what most frames look like.
pub fn sample_wifi_only() -> NetSnapshot {
    let mut s = sample();
    s.links = vec![Link {
        name: "wlan0".into(),
        kind: LinkKind::Wifi,
        connected: true,
        default_route: true,
    }];
    s.address = Some("192.168.1.31".into());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passphrase_is_8_to_63_bytes_without_control_characters() {
        assert_eq!(key_problem("1234567"), Some("At least 8 characters"));
        assert_eq!(key_problem("12345678"), None);
        assert_eq!(key_problem(&"x".repeat(63)), None);
        assert_eq!(key_problem(&"x".repeat(64)), Some("At most 63 characters"));
        // Bytes, not characters: "é" is two.
        assert_eq!(key_problem("café123"), None);
        assert_eq!(key_problem("abc\"def gh"), None);
        assert!(key_problem("abcd\tefgh").is_some());
    }

    #[test]
    fn bars_step_evenly_and_a_lock_shows_only_with_a_password() {
        assert_eq!([-50, -55, -60, -70, -80, -90].map(bars), [4, 4, 3, 2, 1, 0]);
        assert_eq!(signal_icon(3, true), icons::NETWORK_WIFI_3_BAR_LOCKED);
        assert_eq!(signal_icon(4, false), icons::SIGNAL_WIFI_4_BAR);
    }

    #[test]
    fn a_join_command_never_shows_its_key() {
        let c = NetCommand::Join {
            ssid: Ssid::new("Home"),
            security: Security::Wpa2,
            key: "hunter2hunter2".into(),
            hidden: false,
        };
        let s = format!("{c:?}");
        assert!(!s.contains("hunter2"), "{s}");
        assert!(s.contains("Home"));
    }
}
