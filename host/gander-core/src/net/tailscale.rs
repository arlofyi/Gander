// SPDX-License-Identifier: Apache-2.0
//! Tailscale identity and path classification, independent of how tailscaled is reached.
//!
//! The platform crate provides backends for [`TailscaleLocal`] (on Windows: the LocalAPI named
//! pipe and the `tailscale` CLI, `gander_win::net`). This module holds what does not depend on the
//! transport:
//!
//! - the JSON shapes of `whois`, `status` and `prefs` (only the fields Gander reads; everything else
//!   is ignored, because `status --json` is documented as "subject to change"),
//! - the WhoIs semantics of the pairing spec: tagged nodes, shared-in nodes, not-found,
//! - relay classification (direct / peer relay / DERP),
//! - [`Fallback`], which tries a second backend when the first one fails,
//! - [`WhoIsCache`], the 60 s answer cache,
//! - [`FakeTailscale`], a test double.
//!
//! Gander only ever talks to the LOCAL tailscaled. Nothing here contacts a peer.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config::{RecordedIdentity, TrustConfig};
use crate::protocol::host::{NetworkIdentity, RelayState};

/// Pseudo-user login Tailscale reports for tagged nodes.
pub const TAGGED_DEVICES_LOGIN: &str = "tagged-devices";

/// How long a WhoIs answer is reused.
pub const WHOIS_CACHE_TTL: Duration = Duration::from_secs(60);

/// Most cached answers kept (a tailnet rarely has more peers talking to one host).
pub const WHOIS_CACHE_MAX: usize = 1024;

/// Errors from a [`TailscaleLocal`] backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TailscaleError {
    /// The backend is not present on this machine (no pipe, no CLI).
    #[error("tailscale backend unavailable: {0}")]
    Unavailable(String),
    /// Talking to tailscaled failed (I/O, timeout, non-success status).
    #[error("tailscale request failed: {0}")]
    Request(String),
    /// tailscaled answered with something Gander cannot parse.
    #[error("tailscale response not understood: {0}")]
    Parse(String),
}

/// The local tailscaled, reached by some transport.
///
/// Methods block; async callers run them on a blocking thread with a timeout.
pub trait TailscaleLocal: Send + Sync + fmt::Debug {
    /// WhoIs for a peer address (the TCP source of a connection). `Ok(NotFound)` for loopback,
    /// LAN and subnet-routed sources (LocalAPI 404, CLI `peer not found`).
    fn whois(&self, addr: SocketAddr) -> Result<WhoIsAnswer, TailscaleError>;
    /// `status` (peers and paths).
    fn status(&self) -> Result<TailscaleStatus, TailscaleError>;
    /// The `ShieldsUp` preference ("Allow incoming connections" off).
    fn shields_up(&self) -> Result<bool, TailscaleError>;
}

/// Result of a WhoIs lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhoIsAnswer {
    /// The address is a tailnet node.
    Found(PeerIdentity),
    /// Not a tailnet node address: no network identity.
    NotFound,
}

/// A tailnet peer as WhoIs describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Login name, opaque (email on Tailscale, plain user name on Headscale, `tagged-devices`).
    pub login: String,
    /// Stable node id (`StableID`, or the numeric `ID` when absent).
    pub node_id: String,
    /// Short node name, for example `pixel-9`.
    pub node_name: String,
    /// ACL tags; non-empty means a tagged node.
    pub tags: Vec<String>,
    /// The node was shared into this tailnet from another one (`Node.Sharer` set).
    pub shared_in: bool,
}

impl PeerIdentity {
    /// Tagged nodes have tags or the `tagged-devices` pseudo-user.
    pub fn is_tagged(&self) -> bool {
        !self.tags.is_empty() || self.login == TAGGED_DEVICES_LOGIN
    }

    /// The identity reported in `/v1/me`.
    pub fn to_network_identity(&self) -> NetworkIdentity {
        NetworkIdentity {
            login: self.login.clone(),
            node: self.node_name.clone(),
        }
    }

    /// The identity recorded on the device at pairing approval.
    pub fn to_recorded(&self) -> RecordedIdentity {
        RecordedIdentity {
            login: self.login.clone(),
            node_id: self.node_id.clone(),
            node_name: self.node_name.clone(),
        }
    }
}

// ---------- WhoIs JSON ----------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoIsJson {
    node: Option<NodeJson>,
    user_profile: Option<UserProfileJson>,
}

#[derive(Debug, Deserialize)]
struct NodeJson {
    #[serde(rename = "ID", default)]
    id: Option<i64>,
    #[serde(rename = "StableID", default)]
    stable_id: Option<String>,
    #[serde(rename = "Name", default)]
    name: Option<String>,
    #[serde(rename = "ComputedName", default)]
    computed_name: Option<String>,
    #[serde(rename = "Tags", default)]
    tags: Option<Vec<String>>,
    #[serde(rename = "Sharer", default)]
    sharer: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UserProfileJson {
    #[serde(default)]
    login_name: Option<String>,
}

/// Parses the body of LocalAPI `/localapi/v0/whois` or `tailscale whois --json`.
pub fn parse_whois(json: &[u8]) -> Result<WhoIsAnswer, TailscaleError> {
    let parsed: WhoIsJson =
        serde_json::from_slice(json).map_err(|e| TailscaleError::Parse(format!("whois: {e}")))?;
    let Some(node) = parsed.node else {
        return Ok(WhoIsAnswer::NotFound);
    };
    let login = parsed
        .user_profile
        .and_then(|u| u.login_name)
        .unwrap_or_default();
    let node_id = node
        .stable_id
        .filter(|s| !s.is_empty())
        .or_else(|| node.id.map(|i| i.to_string()))
        .unwrap_or_default();
    let node_name = node
        .computed_name
        .filter(|s| !s.is_empty())
        .or_else(|| {
            node.name
                .as_deref()
                .and_then(|n| n.split('.').next())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let shared_in = match node.sharer {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Number(n)) => n.as_i64() != Some(0),
        Some(serde_json::Value::String(s)) => !s.is_empty() && s != "0",
        Some(_) => true,
    };
    Ok(WhoIsAnswer::Found(PeerIdentity {
        login,
        node_id,
        node_name,
        tags: node.tags.unwrap_or_default(),
        shared_in,
    }))
}

/// Whether a LocalAPI/CLI error text means "this address is not a tailnet node".
pub fn is_not_found_message(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("no match for") || t.contains("not found") || t.contains("no such peer")
}

// ---------- pairing and binding policy ----------

/// Why a peer may not pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRefusal {
    /// Tagged node and `allow_tagged_peers` is off.
    TaggedPeer,
}

/// Whether a peer with this WhoIs answer may pair (pairing spec: tagged refused by default).
/// Not-found peers may pair; the approval dialog says "identity unknown".
pub fn pairing_admissible(answer: &WhoIsAnswer, trust: &TrustConfig) -> Result<(), PairingRefusal> {
    match answer {
        WhoIsAnswer::Found(p) if p.is_tagged() && !trust.allow_tagged_peers => {
            Err(PairingRefusal::TaggedPeer)
        }
        _ => Ok(()),
    }
}

/// Outcome of comparing a request's WhoIs with the identity recorded at pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingCheck {
    /// No recorded identity, or login and node match.
    Ok,
    /// Login matches, node id differs; log a warning (phones get new ids on reinstall).
    NodeChanged,
    /// Refuse with 403 `network_identity_mismatch`.
    Mismatch,
}

/// The per-request network binding rule (pairing spec, "Network binding").
pub fn check_binding(
    recorded: Option<&RecordedIdentity>,
    current: &WhoIsAnswer,
    trust: &TrustConfig,
) -> BindingCheck {
    let Some(recorded) = recorded else {
        return BindingCheck::Ok;
    };
    match current {
        WhoIsAnswer::NotFound => BindingCheck::Mismatch,
        WhoIsAnswer::Found(p) if p.login != recorded.login => BindingCheck::Mismatch,
        WhoIsAnswer::Found(p) if p.node_id != recorded.node_id => {
            if trust.strict_node_binding {
                BindingCheck::Mismatch
            } else {
                BindingCheck::NodeChanged
            }
        }
        WhoIsAnswer::Found(_) => BindingCheck::Ok,
    }
}

// ---------- status JSON ----------

/// The parts of `tailscale status --json` (LocalAPI `/localapi/v0/status`) Gander reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailscaleStatus {
    /// `BackendState`, for example `Running`.
    pub backend_state: String,
    /// This node's MagicDNS name without the trailing dot.
    pub self_dns_name: Option<String>,
    /// This node's tailnet addresses.
    pub self_addresses: Vec<IpAddr>,
    /// Peers.
    pub peers: Vec<PeerStatus>,
    /// Health warnings, verbatim.
    pub health: Vec<String>,
}

/// One peer's path state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerStatus {
    /// The peer's tailnet addresses.
    pub addresses: Vec<IpAddr>,
    /// `CurAddr`: set when the path is direct.
    pub cur_addr: String,
    /// `Relay`: home DERP region.
    pub relay: String,
    /// `PeerRelay`: set when a Tailscale peer relay carries the traffic.
    pub peer_relay: String,
    /// `Online`.
    pub online: bool,
    /// `Active` (recent traffic).
    pub active: bool,
}

impl PeerStatus {
    /// Path classification, corrected by the identity spike (WP-46 findings): `Relay` is the
    /// peer's HOME DERP region and stays set when the peer is offline or idle, so it alone does not
    /// prove relaying. Offline -> unknown; `CurAddr` set -> direct; `PeerRelay` set -> peer relay;
    /// `Relay` set and `Active` -> DERP; anything else (idle, no path) -> unknown (`None`).
    pub fn relay_state(&self) -> Option<RelayState> {
        if !self.online {
            None
        } else if !self.cur_addr.is_empty() {
            Some(RelayState::Direct)
        } else if !self.peer_relay.is_empty() {
            Some(RelayState::PeerRelay)
        } else if !self.relay.is_empty() && self.active {
            Some(RelayState::Derp)
        } else {
            None
        }
    }
}

impl TailscaleStatus {
    /// The path to the peer owning `addr`, if it is a known peer.
    pub fn relay_state_for(&self, addr: IpAddr) -> Option<RelayState> {
        self.peers
            .iter()
            .find(|p| p.addresses.contains(&addr))
            .and_then(PeerStatus::relay_state)
    }

    /// Whether a health warning mentions Shields Up. `status` carries no `ShieldsUp` field
    /// (measured on Tailscale 1.104.4); [`TailscaleLocal::shields_up`] reads the preference.
    pub fn health_mentions_shields_up(&self) -> bool {
        self.health
            .iter()
            .any(|h| h.to_ascii_lowercase().contains("shields"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StatusJson {
    #[serde(default)]
    backend_state: String,
    #[serde(rename = "Self", default)]
    self_node: Option<PeerJson>,
    #[serde(default)]
    peer: Option<HashMap<String, PeerJson>>,
    #[serde(default)]
    health: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PeerJson {
    #[serde(rename = "DNSName", default)]
    dns_name: Option<String>,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Option<Vec<String>>,
    #[serde(default)]
    cur_addr: Option<String>,
    #[serde(default)]
    relay: Option<String>,
    #[serde(default)]
    peer_relay: Option<String>,
    #[serde(default)]
    online: Option<bool>,
    #[serde(default)]
    active: Option<bool>,
}

fn parse_ips(ips: Option<Vec<String>>) -> Vec<IpAddr> {
    ips.unwrap_or_default()
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// Parses `tailscale status --json` / LocalAPI `/localapi/v0/status`.
pub fn parse_status(json: &[u8]) -> Result<TailscaleStatus, TailscaleError> {
    let parsed: StatusJson =
        serde_json::from_slice(json).map_err(|e| TailscaleError::Parse(format!("status: {e}")))?;
    let (self_dns_name, self_addresses) = match parsed.self_node {
        Some(s) => (
            s.dns_name
                .map(|n| n.trim_end_matches('.').to_owned())
                .filter(|n| !n.is_empty()),
            parse_ips(s.tailscale_ips),
        ),
        None => (None, Vec::new()),
    };
    let mut peers: Vec<PeerStatus> = parsed
        .peer
        .unwrap_or_default()
        .into_values()
        .map(|p| PeerStatus {
            addresses: parse_ips(p.tailscale_ips),
            cur_addr: p.cur_addr.unwrap_or_default(),
            relay: p.relay.unwrap_or_default(),
            peer_relay: p.peer_relay.unwrap_or_default(),
            online: p.online.unwrap_or(false),
            active: p.active.unwrap_or(false),
        })
        .collect();
    peers.sort_by(|a, b| a.addresses.cmp(&b.addresses));
    Ok(TailscaleStatus {
        backend_state: parsed.backend_state,
        self_dns_name,
        self_addresses,
        peers,
        health: parsed.health.unwrap_or_default(),
    })
}

/// Reads `ShieldsUp` from LocalAPI `/localapi/v0/prefs` or `tailscale debug prefs`.
pub fn parse_shields_up(json: &[u8]) -> Result<bool, TailscaleError> {
    #[derive(Deserialize)]
    struct Prefs {
        #[serde(rename = "ShieldsUp", default)]
        shields_up: bool,
    }
    let p: Prefs =
        serde_json::from_slice(json).map_err(|e| TailscaleError::Parse(format!("prefs: {e}")))?;
    Ok(p.shields_up)
}

// ---------- fallback ----------

/// Tries `primary`; on an error (not on `NotFound`) tries `secondary`.
#[derive(Debug)]
pub struct Fallback<A, B> {
    primary: A,
    secondary: B,
}

impl<A: TailscaleLocal, B: TailscaleLocal> Fallback<A, B> {
    /// Primary first, then secondary.
    pub fn new(primary: A, secondary: B) -> Self {
        Self { primary, secondary }
    }
}

impl<A: TailscaleLocal, B: TailscaleLocal> TailscaleLocal for Fallback<A, B> {
    fn whois(&self, addr: SocketAddr) -> Result<WhoIsAnswer, TailscaleError> {
        self.primary
            .whois(addr)
            .or_else(|_| self.secondary.whois(addr))
    }

    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        self.primary.status().or_else(|_| self.secondary.status())
    }

    fn shields_up(&self) -> Result<bool, TailscaleError> {
        self.primary
            .shields_up()
            .or_else(|_| self.secondary.shields_up())
    }
}

// ---------- cache ----------

/// WhoIs answers cached for [`WHOIS_CACHE_TTL`]. Errors are never cached. The clock is passed in
/// so tests control time.
#[derive(Debug)]
pub struct WhoIsCache {
    ttl: Duration,
    entries: Mutex<HashMap<IpAddr, (Instant, WhoIsAnswer)>>,
}

impl Default for WhoIsCache {
    fn default() -> Self {
        Self::new(WHOIS_CACHE_TTL)
    }
}

impl WhoIsCache {
    /// A cache with the given time to live.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// A fresh answer for `addr`, if one is cached.
    pub fn get(&self, addr: IpAddr, now: Instant) -> Option<WhoIsAnswer> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(&addr)
            .filter(|(at, _)| now.saturating_duration_since(*at) < self.ttl)
            .map(|(_, a)| a.clone())
    }

    /// Stores an answer.
    pub fn insert(&self, addr: IpAddr, answer: WhoIsAnswer, now: Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= WHOIS_CACHE_MAX {
            let ttl = self.ttl;
            entries.retain(|_, (at, _)| now.saturating_duration_since(*at) < ttl);
            if entries.len() >= WHOIS_CACHE_MAX
                && let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| *k)
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(addr, (now, answer));
    }

    /// Drops every entry (for example after the network changed).
    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Cached lookup: answers from the cache when fresh, else asks `backend` and caches the answer.
    /// Keyed by IP: the identity of a node does not depend on the source port.
    pub fn lookup(
        &self,
        backend: &dyn TailscaleLocal,
        addr: SocketAddr,
        now: Instant,
    ) -> Result<WhoIsAnswer, TailscaleError> {
        if let Some(hit) = self.get(addr.ip(), now) {
            return Ok(hit);
        }
        let answer = backend.whois(addr)?;
        self.insert(addr.ip(), answer.clone(), now);
        Ok(answer)
    }
}

// ---------- fake ----------

/// A [`TailscaleLocal`] test double: answers from tables, counts WhoIs calls.
#[derive(Debug, Default)]
pub struct FakeTailscale {
    /// WhoIs answers by address; addresses not present are `NotFound`.
    pub peers: HashMap<IpAddr, PeerIdentity>,
    /// Returned by `status`; `None` makes `status` fail.
    pub status: Option<TailscaleStatus>,
    /// Returned by `shields_up`.
    pub shields_up: bool,
    /// When true every call fails with `Unavailable`.
    pub unavailable: bool,
    calls: std::sync::atomic::AtomicUsize,
}

impl FakeTailscale {
    /// An empty fake (every address not found).
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a peer.
    pub fn with_peer(mut self, addr: IpAddr, peer: PeerIdentity) -> Self {
        self.peers.insert(addr, peer);
        self
    }

    /// A fake whose every call fails with `Unavailable`.
    pub fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::default()
        }
    }

    /// Sets the `shields_up` answer.
    pub fn with_shields_up(mut self, on: bool) -> Self {
        self.shields_up = on;
        self
    }

    /// Sets the `status` answer.
    pub fn with_status(mut self, status: TailscaleStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Number of `whois` calls so far.
    pub fn whois_calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl TailscaleLocal for FakeTailscale {
    fn whois(&self, addr: SocketAddr) -> Result<WhoIsAnswer, TailscaleError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.unavailable {
            return Err(TailscaleError::Unavailable("fake".into()));
        }
        Ok(self
            .peers
            .get(&addr.ip())
            .cloned()
            .map_or(WhoIsAnswer::NotFound, WhoIsAnswer::Found))
    }

    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        if self.unavailable {
            return Err(TailscaleError::Unavailable("fake".into()));
        }
        self.status
            .clone()
            .ok_or_else(|| TailscaleError::Request("fake: no status".into()))
    }

    fn shields_up(&self) -> Result<bool, TailscaleError> {
        if self.unavailable {
            return Err(TailscaleError::Unavailable("fake".into()));
        }
        Ok(self.shields_up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic fixtures in the shape tailscaled returns; no real tailnet data.
    const WHOIS_USER: &str = r#"{
        "Node": {"ID": 123, "StableID": "nABC123CNTRL", "Name": "pixel-9.tail0000.ts.net.",
                 "ComputedName": "pixel-9", "User": 42, "Addresses": ["100.100.1.2/32"]},
        "UserProfile": {"ID": 42, "LoginName": "alice@example.com", "DisplayName": "Alice"},
        "CapMap": null
    }"#;
    const WHOIS_TAGGED: &str = r#"{
        "Node": {"ID": 7, "StableID": "nTAG", "Name": "ci-runner.tail0000.ts.net.",
                 "Tags": ["tag:ci"], "User": 99},
        "UserProfile": {"ID": 99, "LoginName": "tagged-devices", "DisplayName": "Tagged Devices"}
    }"#;
    const WHOIS_SHARED: &str = r#"{
        "Node": {"ID": 8, "StableID": "nSHR", "Name": "bobs-phone.other.ts.net.", "Sharer": 555},
        "UserProfile": {"ID": 77, "LoginName": "bob@elsewhere.example"}
    }"#;
    const WHOIS_HEADSCALE: &str = r#"{
        "Node": {"ID": 3, "Name": "tablet.hs.example.", "Sharer": 0},
        "UserProfile": {"ID": 1, "LoginName": "alice"}
    }"#;

    fn sa(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 50000)
    }

    fn found(a: WhoIsAnswer) -> PeerIdentity {
        match a {
            WhoIsAnswer::Found(p) => p,
            WhoIsAnswer::NotFound => panic!("expected found"),
        }
    }

    #[test]
    fn whois_user_node() {
        let p = found(parse_whois(WHOIS_USER.as_bytes()).unwrap());
        assert_eq!(p.login, "alice@example.com");
        assert_eq!(p.node_id, "nABC123CNTRL");
        assert_eq!(p.node_name, "pixel-9");
        assert!(!p.is_tagged());
        assert!(!p.shared_in);
        assert_eq!(p.to_network_identity().node, "pixel-9");
    }

    #[test]
    fn whois_tagged_node() {
        let p = found(parse_whois(WHOIS_TAGGED.as_bytes()).unwrap());
        assert!(p.is_tagged());
        assert_eq!(p.node_name, "ci-runner");
        let a = WhoIsAnswer::Found(p);
        assert_eq!(
            pairing_admissible(&a, &TrustConfig::default()),
            Err(PairingRefusal::TaggedPeer)
        );
        let allow = TrustConfig {
            allow_tagged_peers: true,
            ..TrustConfig::default()
        };
        assert_eq!(pairing_admissible(&a, &allow), Ok(()));
    }

    #[test]
    fn tagged_by_login_only() {
        let p = PeerIdentity {
            login: TAGGED_DEVICES_LOGIN.into(),
            node_id: "n".into(),
            node_name: "x".into(),
            tags: vec![],
            shared_in: false,
        };
        assert!(p.is_tagged());
    }

    #[test]
    fn whois_shared_in_node() {
        let p = found(parse_whois(WHOIS_SHARED.as_bytes()).unwrap());
        assert!(p.shared_in);
        assert_eq!(p.login, "bob@elsewhere.example");
        assert_eq!(
            pairing_admissible(&WhoIsAnswer::Found(p), &TrustConfig::default()),
            Ok(())
        );
    }

    #[test]
    fn whois_headscale_plain_login_and_numeric_id() {
        let p = found(parse_whois(WHOIS_HEADSCALE.as_bytes()).unwrap());
        assert_eq!(p.login, "alice");
        assert_eq!(p.node_id, "3");
        assert_eq!(p.node_name, "tablet");
        assert!(!p.shared_in);
    }

    #[test]
    fn whois_not_found_semantics() {
        assert_eq!(parse_whois(b"{}").unwrap(), WhoIsAnswer::NotFound);
        assert_eq!(
            parse_whois(br#"{"Node": null}"#).unwrap(),
            WhoIsAnswer::NotFound
        );
        assert!(is_not_found_message("no match for IP:port"));
        assert!(!is_not_found_message("access denied"));
        assert!(parse_whois(b"not json").is_err());
        // Not-found peers may pair (dialog says "identity unknown").
        assert_eq!(
            pairing_admissible(&WhoIsAnswer::NotFound, &TrustConfig::default()),
            Ok(())
        );
    }

    #[test]
    fn fake_whois_semantics() {
        let tagged = found(parse_whois(WHOIS_TAGGED.as_bytes()).unwrap());
        let shared = found(parse_whois(WHOIS_SHARED.as_bytes()).unwrap());
        let fake = FakeTailscale::new()
            .with_peer("100.64.0.7".parse().unwrap(), tagged)
            .with_peer("100.64.0.8".parse().unwrap(), shared);
        assert!(found(fake.whois(sa("100.64.0.7")).unwrap()).is_tagged());
        assert!(found(fake.whois(sa("100.64.0.8")).unwrap()).shared_in);
        for addr in ["127.0.0.1", "192.168.1.5", "100.64.0.9"] {
            assert_eq!(fake.whois(sa(addr)).unwrap(), WhoIsAnswer::NotFound);
        }
    }

    #[test]
    fn binding_rules() {
        let me = found(parse_whois(WHOIS_USER.as_bytes()).unwrap());
        let rec = me.to_recorded();
        let t = TrustConfig::default();
        let same = WhoIsAnswer::Found(me.clone());
        assert_eq!(check_binding(Some(&rec), &same, &t), BindingCheck::Ok);
        assert_eq!(
            check_binding(None, &WhoIsAnswer::NotFound, &t),
            BindingCheck::Ok
        );
        assert_eq!(
            check_binding(Some(&rec), &WhoIsAnswer::NotFound, &t),
            BindingCheck::Mismatch
        );
        let mut other_login = me.clone();
        other_login.login = "mallory@example.com".into();
        assert_eq!(
            check_binding(Some(&rec), &WhoIsAnswer::Found(other_login), &t),
            BindingCheck::Mismatch
        );
        let mut new_node = me;
        new_node.node_id = "nNEW".into();
        let new_node = WhoIsAnswer::Found(new_node);
        assert_eq!(
            check_binding(Some(&rec), &new_node, &t),
            BindingCheck::NodeChanged
        );
        let strict = TrustConfig {
            strict_node_binding: true,
            ..TrustConfig::default()
        };
        assert_eq!(
            check_binding(Some(&rec), &new_node, &strict),
            BindingCheck::Mismatch
        );
    }

    #[test]
    fn fallback_uses_secondary_on_error_only() {
        let broken = FakeTailscale {
            unavailable: true,
            ..FakeTailscale::default()
        };
        let good = FakeTailscale {
            shields_up: true,
            ..FakeTailscale::default()
        }
        .with_peer(
            "100.64.0.1".parse().unwrap(),
            found(parse_whois(WHOIS_USER.as_bytes()).unwrap()),
        );
        let fb = Fallback::new(broken, good);
        assert!(matches!(
            fb.whois(sa("100.64.0.1")).unwrap(),
            WhoIsAnswer::Found(_)
        ));
        assert!(fb.shields_up().unwrap());

        // NotFound from the primary is an answer, not a failure.
        let primary = FakeTailscale::new();
        let secondary = FakeTailscale::new().with_peer(
            "100.64.0.1".parse().unwrap(),
            found(parse_whois(WHOIS_USER.as_bytes()).unwrap()),
        );
        let fb = Fallback::new(primary, secondary);
        assert_eq!(fb.whois(sa("100.64.0.1")).unwrap(), WhoIsAnswer::NotFound);
    }

    #[test]
    fn cache_reuses_for_60s_and_skips_errors() {
        let fake = FakeTailscale::new();
        let cache = WhoIsCache::default();
        let t0 = Instant::now();
        let addr = sa("100.64.0.1");
        cache.lookup(&fake, addr, t0).unwrap();
        // Another source port of the same node hits the cache.
        cache
            .lookup(&fake, SocketAddr::new(addr.ip(), 1), t0)
            .unwrap();
        cache
            .lookup(&fake, addr, t0 + Duration::from_secs(59))
            .unwrap();
        assert_eq!(fake.whois_calls(), 1);
        cache
            .lookup(&fake, addr, t0 + Duration::from_secs(60))
            .unwrap();
        assert_eq!(fake.whois_calls(), 2);

        let broken = FakeTailscale {
            unavailable: true,
            ..FakeTailscale::default()
        };
        let other = sa("100.64.0.2");
        assert!(cache.lookup(&broken, other, t0).is_err());
        assert!(cache.get(other.ip(), t0).is_none());
    }

    #[test]
    fn cache_is_bounded() {
        let cache = WhoIsCache::default();
        let t0 = Instant::now();
        for i in 0..(WHOIS_CACHE_MAX as u32 + 10) {
            cache.insert(
                IpAddr::V4(i.into()),
                WhoIsAnswer::NotFound,
                t0 + Duration::from_millis(u64::from(i)),
            );
        }
        assert!(cache.entries.lock().unwrap().len() <= WHOIS_CACHE_MAX);
    }

    const STATUS: &str = r#"{
        "BackendState": "Running",
        "Self": {"DNSName": "studio-pc.tail0000.ts.net.", "TailscaleIPs": ["100.100.0.1", "fd7a:115c:a1e0::1"]},
        "Health": [],
        "Peer": {
            "nodekey:a": {"TailscaleIPs": ["100.100.0.2"], "CurAddr": "192.0.2.10:41641", "Relay": "fra", "PeerRelay": "", "Online": true, "Active": true},
            "nodekey:b": {"TailscaleIPs": ["100.100.0.3"], "CurAddr": "", "Relay": "fra", "PeerRelay": "100.100.0.9:7777", "Online": true, "Active": true},
            "nodekey:c": {"TailscaleIPs": ["100.100.0.4", "fd7a:115c:a1e0::4"], "CurAddr": "", "Relay": "nyc", "Online": true, "Active": true},
            "nodekey:d": {"TailscaleIPs": ["100.100.0.5"], "Relay": "fra", "Online": false, "Active": false},
            "nodekey:e": {"TailscaleIPs": ["100.100.0.6"], "CurAddr": "", "Relay": "fra", "PeerRelay": "", "Online": true, "Active": false}
        },
        "SomethingNew": {"ignored": true}
    }"#;

    #[test]
    fn status_relay_classification() {
        let s = parse_status(STATUS.as_bytes()).unwrap();
        assert_eq!(s.backend_state, "Running");
        assert_eq!(
            s.self_dns_name.as_deref(),
            Some("studio-pc.tail0000.ts.net")
        );
        assert_eq!(s.self_addresses.len(), 2);
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(
            s.relay_state_for(ip("100.100.0.2")),
            Some(RelayState::Direct)
        );
        assert_eq!(
            s.relay_state_for(ip("100.100.0.3")),
            Some(RelayState::PeerRelay)
        );
        assert_eq!(
            s.relay_state_for(ip("fd7a:115c:a1e0::4")),
            Some(RelayState::Derp)
        );
        // Offline peer keeps its home DERP region in `Relay`: not "relayed" (WP-46 finding).
        assert_eq!(s.relay_state_for(ip("100.100.0.5")), None);
        // Online but idle: current path unknown.
        assert_eq!(s.relay_state_for(ip("100.100.0.6")), None);
        assert_eq!(s.relay_state_for(ip("100.100.0.99")), None);
        assert!(!s.health_mentions_shields_up());
    }

    #[test]
    fn shields_up_from_prefs() {
        assert!(parse_shields_up(br#"{"ShieldsUp": true, "WantRunning": true}"#).unwrap());
        assert!(!parse_shields_up(br#"{"WantRunning": true}"#).unwrap());
    }
}
