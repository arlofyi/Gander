// SPDX-License-Identifier: Apache-2.0
//! Interpretation of the LocalAPI JSON: nodes out of `status`, the path class of a peer, and a
//! structural summary of a `whois` answer. Everything here works on real values; the report
//! layer decides what to show (see `redact`).

use serde_json::Value;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    SelfNode,
    Peer,
}

/// One node of `status` (self or a peer), real values.
#[derive(Debug, Clone)]
pub struct NodeView {
    pub role: Role,
    pub key: String,
    pub host_name: String,
    pub dns_name: String,
    pub os: String,
    pub node_id: String,
    pub user_id: Option<i64>,
    pub ips: Vec<String>,
    /// `None` when the `Tags` field is absent from the status entry.
    pub tags: Option<Vec<String>>,
    pub online: bool,
    pub active: bool,
    pub exit_node: bool,
    pub sharee_node: bool,
    pub cur_addr: String,
    pub relay: String,
    pub peer_relay: String,
    pub addrs: Vec<String>,
}

impl NodeView {
    pub fn first_v4(&self) -> Option<&str> {
        self.ips
            .iter()
            .map(String::as_str)
            .find(|s| s.parse::<std::net::Ipv4Addr>().is_ok())
    }

    pub fn first_v6(&self) -> Option<&str> {
        self.ips
            .iter()
            .map(String::as_str)
            .find(|s| s.parse::<std::net::Ipv6Addr>().is_ok())
    }
}

fn s(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn b(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn keys_of(v: &Value) -> Vec<String> {
    v.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// JSON type of each field of an object (`null`, `bool`, `number`, `string`, `array`, `object`).
pub fn types_of(v: &Value) -> BTreeMap<String, &'static str> {
    v.as_object()
        .map(|o| o.iter().map(|(k, x)| (k.clone(), json_type(x))).collect())
        .unwrap_or_default()
}

pub fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub fn parse_node(role: Role, key_hint: &str, v: &Value) -> NodeView {
    let key = if key_hint.is_empty() {
        s(v, "PublicKey")
    } else {
        key_hint.to_string()
    };
    NodeView {
        role,
        key,
        host_name: s(v, "HostName"),
        dns_name: s(v, "DNSName"),
        os: s(v, "OS"),
        node_id: if v.get("NodeID").is_some() {
            s(v, "NodeID")
        } else {
            s(v, "ID")
        },
        user_id: v.get("UserID").and_then(Value::as_i64),
        ips: strings(v, "TailscaleIPs"),
        tags: v.get("Tags").map(|_| strings(v, "Tags")),
        online: b(v, "Online"),
        active: b(v, "Active"),
        exit_node: b(v, "ExitNode"),
        sharee_node: b(v, "ShareeNode"),
        cur_addr: s(v, "CurAddr"),
        relay: s(v, "Relay"),
        peer_relay: s(v, "PeerRelay"),
        addrs: strings(v, "Addrs"),
    }
}

/// How traffic to a peer currently flows, derived from `status` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    /// The control plane reports the peer offline. `Relay` is then only its home DERP region.
    Offline,
    /// Direct UDP path (`CurAddr` set).
    Direct,
    /// Via a peer relay (`PeerRelay` set, `CurAddr` empty).
    PeerRelay,
    /// Via a DERP relay: online and recently active with `Relay` set and no direct path.
    Derp,
    /// Online, no recent traffic, `Relay` is only the home region. The current path is unknown
    /// until traffic flows; a Stream must not treat this as "direct".
    IdleUnknown,
    /// Online but no path information at all.
    NoPath,
}

impl PathClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Direct => "direct",
            Self::PeerRelay => "peer_relay",
            Self::Derp => "derp",
            Self::IdleUnknown => "idle_unknown",
            Self::NoPath => "no_path",
        }
    }

    /// Whether a Stream should warn about throughput.
    pub fn relayed(self) -> bool {
        matches!(self, Self::Derp | Self::PeerRelay)
    }
}

pub fn classify_path(n: &NodeView) -> PathClass {
    if !n.online {
        PathClass::Offline
    } else if !n.cur_addr.is_empty() {
        PathClass::Direct
    } else if !n.peer_relay.is_empty() {
        PathClass::PeerRelay
    } else if !n.relay.is_empty() && n.active {
        PathClass::Derp
    } else if !n.relay.is_empty() {
        PathClass::IdleUnknown
    } else {
        PathClass::NoPath
    }
}

/// `lan` for a private, link-local or ULA endpoint, `wan` for anything else, `none` if absent.
pub fn endpoint_scope(addr: &str) -> &'static str {
    let Ok(sa) = addr.parse::<SocketAddr>() else {
        return "none";
    };
    let private = match sa.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        IpAddr::V6(ip) => {
            let seg0 = ip.segments()[0];
            (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80 || ip.is_loopback()
        }
    };
    if private { "lan" } else { "wan" }
}

/// Structural summary of a `whois` answer, real values.
#[derive(Debug, Clone, Default)]
pub struct WhoisSummary {
    pub top_types: BTreeMap<String, &'static str>,
    pub node_types: BTreeMap<String, &'static str>,
    pub user_profile_types: BTreeMap<String, &'static str>,
    /// `None` = `Node.Tags` absent.
    pub node_tags: Option<Vec<String>>,
    pub login_name: Option<String>,
    pub display_name_present: bool,
    pub sharer: Option<i64>,
    pub sharer_present: bool,
    /// Capability names from `Node.CapMap`; `None` = absent.
    pub node_cap_map: Option<Vec<String>>,
    /// Capability names from the top-level `CapMap`; `None` = absent.
    pub top_cap_map: Option<Vec<String>>,
    pub node_user_id: Option<i64>,
    pub profile_user_id: Option<i64>,
}

pub fn summarize_whois(v: &Value) -> WhoisSummary {
    let node = v.get("Node").unwrap_or(&Value::Null);
    let prof = v.get("UserProfile").unwrap_or(&Value::Null);
    WhoisSummary {
        top_types: types_of(v),
        node_types: types_of(node),
        user_profile_types: types_of(prof),
        node_tags: node.get("Tags").map(|_| strings(node, "Tags")),
        login_name: prof
            .get("LoginName")
            .and_then(Value::as_str)
            .map(str::to_string),
        display_name_present: prof
            .get("DisplayName")
            .is_some_and(|d| d.as_str().is_some_and(|s| !s.is_empty())),
        sharer: node.get("Sharer").and_then(Value::as_i64),
        sharer_present: node.get("Sharer").is_some(),
        node_cap_map: node.get("CapMap").map(keys_of),
        top_cap_map: v.get("CapMap").map(keys_of),
        node_user_id: node.get("User").and_then(Value::as_i64),
        profile_user_id: prof.get("ID").and_then(Value::as_i64),
    }
}

/// Capability names are kept when they belong to Tailscale's own namespace; tailnet-specific
/// ones (set in the policy file) are collapsed because they can name internal systems, and the
/// owner/admin role markers are collapsed because they describe the account, not the protocol.
pub fn cap_label(name: &str) -> String {
    const KNOWN: &[&str] = &[
        "https://tailscale.com/",
        "tailscale.com/",
        "https://login.tailscale.com/",
    ];
    if name.ends_with("/is-admin") || name.ends_with("/is-owner") {
        "<role-cap>".to_string()
    } else if KNOWN.iter().any(|p| name.starts_with(p)) {
        name.to_string()
    } else {
        "<custom-cap>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn peer(v: Value) -> NodeView {
        parse_node(Role::Peer, "nodekey:test", &v)
    }

    #[test]
    fn classification_matrix() {
        let direct = peer(
            json!({"Online": true, "Active": true, "CurAddr": "192.168.1.5:41641", "Relay": "fra"}),
        );
        assert_eq!(classify_path(&direct), PathClass::Direct);
        let derp = peer(json!({"Online": true, "Active": true, "CurAddr": "", "Relay": "fra"}));
        assert_eq!(classify_path(&derp), PathClass::Derp);
        assert!(classify_path(&derp).relayed());
        let idle = peer(json!({"Online": true, "Active": false, "CurAddr": "", "Relay": "fra"}));
        assert_eq!(classify_path(&idle), PathClass::IdleUnknown);
        assert!(!classify_path(&idle).relayed());
        let offline = peer(json!({"Online": false, "Active": false, "Relay": "fra"}));
        assert_eq!(classify_path(&offline), PathClass::Offline);
        let pr = peer(
            json!({"Online": true, "Active": true, "PeerRelay": "203.0.113.9:3478", "Relay": "fra"}),
        );
        assert_eq!(classify_path(&pr), PathClass::PeerRelay);
        let none = peer(json!({"Online": true}));
        assert_eq!(classify_path(&none), PathClass::NoPath);
    }

    #[test]
    fn endpoint_scope_cases() {
        assert_eq!(endpoint_scope("192.168.1.5:41641"), "lan");
        assert_eq!(endpoint_scope("10.1.2.3:1"), "lan");
        assert_eq!(endpoint_scope("203.0.113.9:41641"), "wan");
        assert_eq!(endpoint_scope("[fd00::1]:1"), "lan");
        assert_eq!(endpoint_scope("[2001:db8::1]:1"), "wan");
        assert_eq!(endpoint_scope(""), "none");
    }

    #[test]
    fn node_fields_and_absent_tags() {
        let n = peer(json!({
            "HostName": "pixel", "DNSName": "pixel.example.ts.net.", "OS": "android",
            "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::1"], "UserID": 42,
            "Online": true, "Active": true, "Relay": "fra"
        }));
        assert_eq!(n.first_v4(), Some("100.101.102.103"));
        assert_eq!(n.first_v6(), Some("fd7a:115c:a1e0::1"));
        assert_eq!(n.user_id, Some(42));
        assert!(n.tags.is_none());
        assert!(!n.sharee_node);
        let tagged = peer(json!({"Tags": ["tag:server"]}));
        assert_eq!(tagged.tags, Some(vec!["tag:server".to_string()]));
    }

    #[test]
    fn whois_summary_untagged_user_node() {
        let v = json!({
            "Node": {"ID": 1, "Name": "pixel.example.ts.net.", "User": 42, "Addresses": ["100.101.102.103/32"]},
            "UserProfile": {"ID": 42, "LoginName": "alice@example.com", "DisplayName": "Alice"},
            "CapMap": {"https://tailscale.com/cap/file-sharing": null, "acme/internal": []}
        });
        let w = summarize_whois(&v);
        assert!(w.node_tags.is_none());
        assert!(!w.sharer_present);
        assert_eq!(w.login_name.as_deref(), Some("alice@example.com"));
        assert!(w.display_name_present);
        assert_eq!(w.top_cap_map.as_ref().map(Vec::len), Some(2));
        assert!(w.node_cap_map.is_none());
        assert_eq!(w.node_user_id, w.profile_user_id);
    }

    #[test]
    fn whois_summary_tagged_and_shared() {
        let tagged = json!({
            "Node": {"Tags": ["tag:server"], "User": 7},
            "UserProfile": {"ID": 7, "LoginName": "tagged-devices"}
        });
        let w = summarize_whois(&tagged);
        assert_eq!(w.node_tags, Some(vec!["tag:server".to_string()]));
        assert_eq!(w.login_name.as_deref(), Some("tagged-devices"));
        let shared = json!({"Node": {"Sharer": 99, "User": 5}, "UserProfile": {"ID": 5, "LoginName": "bob@example.org"}});
        let w = summarize_whois(&shared);
        assert!(w.sharer_present);
        assert_eq!(w.sharer, Some(99));
    }

    #[test]
    fn cap_labels() {
        assert_eq!(
            cap_label("https://tailscale.com/cap/file-sharing"),
            "https://tailscale.com/cap/file-sharing"
        );
        assert_eq!(cap_label("acme.example/cap/internal"), "<custom-cap>");
        assert_eq!(
            cap_label("https://tailscale.com/cap/is-admin"),
            "<role-cap>"
        );
        assert_eq!(
            cap_label("https://tailscale.com/cap/is-owner"),
            "<role-cap>"
        );
    }
}
