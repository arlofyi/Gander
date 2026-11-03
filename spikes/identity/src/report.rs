// SPDX-License-Identifier: Apache-2.0
//! Runs the probes against the local tailscaled and assembles the report (JSON plus a text
//! summary). All identity-bearing values pass through the [`Redactor`]; with redaction on, the
//! finished output is leak-checked before it is returned.

use crate::analysis::{
    NodeView, PathClass, Role, WhoisSummary, cap_label, classify_path, endpoint_scope, keys_of,
    parse_node, summarize_whois,
};
use crate::context;
use crate::localapi::{self, Call, LOCALAPI_HOST, Qos};
use crate::redact::{Kind, Redactor, mask_sids};
use anyhow::Result;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// A header probe: name, `Host` value (`None` = HTTP/1.0 without one) and extra headers.
type HeaderProbe<'a> = (&'a str, Option<&'a str>, &'a [(&'a str, &'a str)]);

/// Rounds of the QoS matrix (each round tries every level once, alternating direction).
const QOS_ROUNDS: usize = 4;

/// An arbitrary source port for the lookups (the host will pass the real peer port).
const PROBE_PORT: u16 = 40000;

#[derive(Debug)]
pub struct Options {
    pub redact: bool,
    pub pipe_path: String,
    pub bench: usize,
    pub probes: bool,
}

#[derive(Debug)]
pub struct Outcome {
    pub json: Value,
    pub summary: String,
    pub status_parsed: bool,
    pub peers_classified: usize,
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn addr_string(ip: &str, port: Option<u16>) -> String {
    match (ip.contains(':'), port) {
        (true, Some(p)) => format!("[{ip}]:{p}"),
        (false, Some(p)) => format!("{ip}:{p}"),
        (_, None) => ip.to_string(),
    }
}

fn whois_target(addr: &str, proto_tcp: bool) -> String {
    let proto = if proto_tcp { "&proto=tcp" } else { "" };
    format!("/localapi/v0/whois?addr={}{proto}", pct(addr))
}

fn pipe_whois(opts: &Options, qos: Qos, addr: &str, proto_tcp: bool) -> Call {
    localapi::pipe_get(
        &opts.pipe_path,
        &whois_target(addr, proto_tcp),
        Some(LOCALAPI_HOST),
        &[],
        qos,
    )
}

fn ip_kind(ip: &str) -> Kind {
    if ip.contains(':') {
        Kind::IpV6
    } else {
        Kind::IpV4
    }
}

fn call_brief(red: &Redactor, c: &Call) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("http_status".into(), json!(c.http_status));
    m.insert("exit_code".into(), json!(c.exit_code));
    m.insert("json_parsed".into(), json!(c.json.is_some()));
    m.insert("body_len".into(), json!(c.body_len));
    m.insert(
        "text_head".into(),
        json!(c.text_head.as_deref().map(|t| red.scrub(t))),
    );
    m.insert(
        "error".into(),
        json!(c.error.as_deref().map(|e| red.scrub(e))),
    );
    m.insert("os_error".into(), json!(c.os_error));
    m.insert("latency_us".into(), json!(c.micros));
    m
}

/// Report entry for a `whois` call plus its structural summary when it succeeded.
fn whois_json(red: &mut Redactor, c: &Call) -> (Value, Option<WhoisSummary>) {
    let mut m = call_brief(red, c);
    let summary = c.json.as_ref().filter(|_| c.ok()).map(summarize_whois);
    if let Some(w) = &summary {
        m.insert("top_field_types".into(), json!(w.top_types));
        m.insert("node_field_types".into(), json!(w.node_types));
        m.insert(
            "user_profile_field_types".into(),
            json!(w.user_profile_types),
        );
        m.insert("node_tags_present".into(), json!(w.node_tags.is_some()));
        let tags: Option<Vec<String>> = w
            .node_tags
            .as_ref()
            .map(|t| t.iter().map(|x| red.show(Kind::Tag, x)).collect());
        m.insert("node_tags".into(), json!(tags));
        m.insert("login_name_present".into(), json!(w.login_name.is_some()));
        let login = w.login_name.as_deref().map(|l| red.show(Kind::User, l));
        m.insert("login_name".into(), json!(login));
        m.insert(
            "login_is_tagged_devices".into(),
            json!(w.login_name.as_deref() == Some("tagged-devices")),
        );
        m.insert("display_name_present".into(), json!(w.display_name_present));
        m.insert("node_sharer_present".into(), json!(w.sharer_present));
        m.insert(
            "node_sharer_nonzero".into(),
            json!(w.sharer.is_some_and(|s| s != 0)),
        );
        m.insert(
            "node_cap_map_present".into(),
            json!(w.node_cap_map.is_some()),
        );
        m.insert(
            "node_cap_map".into(),
            json!(cap_names(w.node_cap_map.as_deref())),
        );
        m.insert("top_cap_map_present".into(), json!(w.top_cap_map.is_some()));
        m.insert(
            "top_cap_map".into(),
            json!(cap_names(w.top_cap_map.as_deref())),
        );
        m.insert(
            "node_user_matches_profile_id".into(),
            json!(w.node_user_id.is_some() && w.node_user_id == w.profile_user_id),
        );
    }
    (Value::Object(m), summary)
}

fn cap_names(caps: Option<&[String]>) -> Option<Vec<String>> {
    caps.map(|c| {
        let mut v: Vec<String> = c.iter().map(|n| cap_label(n)).collect();
        v.sort();
        v
    })
}

fn register_node(red: &mut Redactor, n: &NodeView) -> String {
    let kind = if n.role == Role::SelfNode {
        Kind::SelfNode
    } else {
        Kind::Peer
    };
    let label = red.show(kind, &n.key);
    red.show(Kind::NodeKey, &n.key);
    red.forbid(&n.host_name);
    red.forbid(&n.dns_name);
    if let Some(first) = n.dns_name.split('.').next() {
        red.forbid(first);
    }
    red.show(Kind::NodeId, &n.node_id);
    for ip in &n.ips {
        red.show(ip_kind(ip), ip);
    }
    for a in &n.addrs {
        red.forbid(a);
        if let Some((ip, _)) = a.rsplit_once(':') {
            red.forbid(ip.trim_matches(['[', ']']));
        }
    }
    if !n.cur_addr.is_empty() {
        red.show(Kind::Endpoint, &n.cur_addr);
        if let Some((ip, _)) = n.cur_addr.rsplit_once(':') {
            red.forbid(ip.trim_matches(['[', ']']));
        }
    }
    if !n.peer_relay.is_empty() {
        red.show(Kind::Endpoint, &n.peer_relay);
        if let Some((ip, _)) = n.peer_relay.rsplit_once(':') {
            red.forbid(ip.trim_matches(['[', ']']));
        }
    }
    if !n.relay.is_empty() {
        red.show(Kind::Region, &n.relay);
    }
    if let Some(uid) = n.user_id {
        red.show(Kind::UserId, &uid.to_string());
    }
    for t in n.tags.iter().flatten() {
        red.show(Kind::Tag, t);
    }
    label
}

fn register_status_extras(red: &mut Redactor, status: &Value) {
    if let Some(users) = status.get("User").and_then(Value::as_object) {
        for (id, u) in users {
            red.show(Kind::UserId, id);
            if let Some(l) = u.get("LoginName").and_then(Value::as_str) {
                red.show(Kind::User, l);
            }
            for k in ["DisplayName", "ProfilePicURL"] {
                if let Some(x) = u.get(k).and_then(Value::as_str) {
                    red.forbid(x);
                }
            }
        }
    }
    if let Some(t) = status.get("CurrentTailnet") {
        for k in ["Name", "MagicDNSSuffix", "StableID"] {
            if let Some(x) = t.get(k).and_then(Value::as_str) {
                red.show(Kind::Tailnet, x);
            }
        }
    }
    if let Some(x) = status.get("MagicDNSSuffix").and_then(Value::as_str) {
        red.show(Kind::Tailnet, x);
    }
    for k in ["CertDomains", "TailscaleIPs"] {
        for x in status
            .get(k)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(x) = x.as_str() {
                if k == "TailscaleIPs" {
                    red.show(ip_kind(x), x);
                } else {
                    red.forbid(x);
                }
            }
        }
    }
}

fn status_shape(status: &Value) -> Value {
    let mut presence: BTreeMap<String, usize> = BTreeMap::new();
    let peers = status.get("Peer").and_then(Value::as_object);
    for p in peers.into_iter().flat_map(|m| m.values()) {
        for k in keys_of(p) {
            *presence.entry(k).or_insert(0) += 1;
        }
    }
    let user_entry_keys: BTreeSet<String> = status
        .get("User")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values().flat_map(keys_of))
        .collect();
    json!({
        "top_level_keys": keys_of(status),
        "self_keys": keys_of(status.get("Self").unwrap_or(&Value::Null)),
        "peer_count": peers.map_or(0, Map::len),
        "peer_key_presence_counts": presence,
        "user_map_entry_keys": user_entry_keys,
        "current_tailnet_keys": keys_of(status.get("CurrentTailnet").unwrap_or(&Value::Null)),
    })
}

fn same_status_shape(a: &Value, b: &Value) -> bool {
    let sa = status_shape(a);
    let sb = status_shape(b);
    [
        "top_level_keys",
        "self_keys",
        "peer_key_presence_counts",
        "user_map_entry_keys",
    ]
    .iter()
    .all(|k| sa[k] == sb[k])
}

fn same_whois_shape(a: &WhoisSummary, b: &WhoisSummary) -> bool {
    a.top_types == b.top_types
        && a.node_types == b.node_types
        && a.user_profile_types == b.user_profile_types
        && a.node_cap_map.is_some() == b.node_cap_map.is_some()
        && a.top_cap_map.is_some() == b.top_cap_map.is_some()
}

fn probe_row(red: &Redactor, name: &str, c: &Call) -> Value {
    let mut m = call_brief(red, c);
    m.insert("name".into(), json!(name));
    m.insert("ok".into(), json!(c.ok()));
    Value::Object(m)
}

fn stats(mut micros: Vec<u128>) -> Value {
    micros.sort_unstable();
    match (micros.first(), micros.get(micros.len() / 2), micros.last()) {
        (Some(min), Some(med), Some(max)) => {
            json!({"runs": micros.len(), "min_us": min, "median_us": med, "max_us": max})
        }
        _ => Value::Null,
    }
}

/// Run every probe and build the report.
#[allow(clippy::too_many_lines)]
pub fn run(opts: &Options) -> Result<Outcome> {
    let mut red = Redactor::new(opts.redact);
    let ctx = context::detect();
    if let Some(u) = &ctx.windows_user {
        red.show(Kind::WinUser, u);
    }
    let cli = localapi::cli_locate();

    // --- QoS matrix: which impersonation levels does tailscaled accept? Interleaved and
    // alternating in direction so that an ordering effect would show up as a mixed result. ---
    let mut qos_counts: BTreeMap<&'static str, BTreeMap<String, usize>> = BTreeMap::new();
    for round in 0..QOS_ROUNDS {
        let mut order = Qos::ALL.to_vec();
        if round % 2 == 1 {
            order.reverse();
        }
        for q in order {
            let c = localapi::pipe_get(
                &opts.pipe_path,
                "/localapi/v0/status",
                Some(LOCALAPI_HOST),
                &[],
                q,
            );
            let key = match (c.http_status, c.os_error) {
                (Some(h), _) => format!("http_{h}"),
                (None, Some(e)) => format!("os_error_{e}"),
                (None, None) => "error".to_string(),
            };
            *qos_counts
                .entry(q.name())
                .or_default()
                .entry(key)
                .or_insert(0) += 1;
        }
    }
    let all_ok =
        |q: Qos| qos_counts.get(q.name()).and_then(|m| m.get("http_200")) == Some(&QOS_ROUNDS);
    let any_ok = |q: Qos| {
        qos_counts
            .get(q.name())
            .is_some_and(|m| m.contains_key("http_200"))
    };
    let preference = [
        Qos::Anonymous,
        Qos::Identification,
        Qos::Impersonation,
        Qos::Default,
    ];
    let qos = preference
        .iter()
        .copied()
        .find(|q| all_ok(*q))
        .or_else(|| preference.iter().copied().find(|q| any_ok(*q)))
        .unwrap_or(Qos::Default);
    let pipe_status = localapi::pipe_get(
        &opts.pipe_path,
        "/localapi/v0/status",
        Some(LOCALAPI_HOST),
        &[],
        qos,
    );
    let cli_status = cli
        .as_deref()
        .map(|p| localapi::cli_run(p, &["status", "--json"]));
    let sddl = localapi::pipe_sddl(&opts.pipe_path);

    let (status, source) = if pipe_status.ok() {
        (pipe_status.json.clone(), "pipe")
    } else if let Some(c) = cli_status.as_ref().filter(|c| c.ok()) {
        (c.json.clone(), "cli")
    } else {
        (None, "none")
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "spike-identity (WP-46): Tailscale identity probe, redaction {}",
        if opts.redact {
            "on"
        } else {
            "OFF (real values on console)"
        }
    ));
    let user_shown = ctx
        .windows_user
        .as_deref()
        .map(|u| red.show(Kind::WinUser, u));
    lines.push(format!(
        "process: elevated={:?} elevation_type={:?} integrity={:?} user={}",
        ctx.elevated,
        ctx.elevation_type,
        ctx.integrity,
        user_shown.as_deref().unwrap_or("?")
    ));
    lines.push(format!("tailscale CLI found: {}", cli.is_some()));
    let qos_cells: Vec<String> = Qos::ALL
        .iter()
        .map(|q| {
            format!(
                "{}={:?}",
                q.name(),
                qos_counts.get(q.name()).cloned().unwrap_or_default()
            )
        })
        .collect();
    lines.push(format!(
        "pipe QoS matrix ({QOS_ROUNDS} rounds, status): {}",
        qos_cells.join(" ")
    ));
    lines.push(format!(
        "pipe status at QoS {}: {} ({})",
        qos.name(),
        outcome_word(&pipe_status),
        detail(&red, &pipe_status)
    ));
    if let Some(c) = &cli_status {
        lines.push(format!(
            "cli status --json: {} ({})",
            outcome_word(c),
            detail(&red, c)
        ));
    }

    let sddl_shown = sddl.as_ref().ok().map(|s| mask_sids(s));
    lines.push(match &sddl_shown {
        Some(s) => format!("pipe security descriptor (SIDs masked): {s}"),
        None => "pipe security descriptor: unavailable".to_string(),
    });

    let Some(status) = status else {
        lines.push("VERIFY: status could not be read from the pipe or the CLI".to_string());
        let json = json!({
            "spike": "identity", "wp": "WP-46", "redacted": opts.redact,
            "status_source": "none",
            "process": process_json(&ctx, &mut red),
            "pipe_status_call": Value::Object(call_brief(&red, &pipe_status)),
            "cli_status_call": cli_status.as_ref().map(|c| Value::Object(call_brief(&red, c))),
        });
        return finish(&red, json, lines, false, 0);
    };

    // --- nodes ---
    register_status_extras(&mut red, &status);
    let self_node = parse_node(
        Role::SelfNode,
        "",
        status.get("Self").unwrap_or(&Value::Null),
    );
    let mut peers: Vec<NodeView> = status
        .get("Peer")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| parse_node(Role::Peer, k, v))
                .collect()
        })
        .unwrap_or_default();
    peers.sort_by(|a, b| (!a.online, &a.key).cmp(&(!b.online, &b.key)));
    let self_label = register_node(&mut red, &self_node);
    let peer_labels: Vec<String> = peers.iter().map(|p| register_node(&mut red, p)).collect();
    let self_parsed = !self_node.ips.is_empty() || !self_node.key.is_empty();

    lines.push(format!(
        "status source: {source}; client version {}; backend {}; peers {}",
        status.get("Version").and_then(Value::as_str).unwrap_or("?"),
        status
            .get("BackendState")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        peers.len()
    ));

    // --- per-node whois (pipe and CLI) and path classification ---
    let status_logins: BTreeMap<String, String> = status
        .get("User")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(id, u)| Some((id.clone(), u.get("LoginName")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let mut node_reports: Vec<Value> = Vec::new();
    let mut path_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut whois_pipe_ok, mut whois_cli_ok, mut shape_same, mut shape_checked) = (0, 0, 0, 0);
    let mut peers_classified = 0;

    let all: Vec<(&NodeView, &String)> = std::iter::once((&self_node, &self_label))
        .chain(peers.iter().zip(peer_labels.iter()))
        .collect();
    for (n, label) in all {
        let class = (n.role == Role::Peer).then(|| classify_path(n));
        if let Some(c) = class {
            *path_counts.entry(c.as_str()).or_insert(0) += 1;
            if !matches!(c, PathClass::Offline | PathClass::NoPath) {
                peers_classified += 1;
            }
        }
        let v4 = n.first_v4().map(str::to_string);
        let pipe_call = v4
            .as_deref()
            .map(|ip| pipe_whois(opts, qos, &addr_string(ip, Some(PROBE_PORT)), true));
        let v6_call = n
            .first_v6()
            .map(|ip| pipe_whois(opts, qos, &addr_string(ip, Some(PROBE_PORT)), true));
        let cli_call = match (&cli, v4.as_deref()) {
            (Some(p), Some(ip)) => Some(localapi::cli_run(p, &["whois", "--json", ip])),
            _ => None,
        };
        let (pipe_json, pipe_sum) = match &pipe_call {
            Some(c) => {
                let (j, s) = whois_json(&mut red, c);
                (Some(j), s)
            }
            None => (None, None),
        };
        let (cli_json, cli_sum) = match &cli_call {
            Some(c) => {
                let (j, s) = whois_json(&mut red, c);
                (Some(j), s)
            }
            None => (None, None),
        };
        whois_pipe_ok += usize::from(pipe_sum.is_some());
        whois_cli_ok += usize::from(cli_sum.is_some());
        if let (Some(a), Some(b)) = (&pipe_sum, &cli_sum) {
            shape_checked += 1;
            shape_same += usize::from(same_whois_shape(a, b));
        }

        // Does the login in `status`'s User map agree with whois?
        let status_login = n.user_id.and_then(|id| status_logins.get(&id.to_string()));
        let login_agrees = match (&pipe_sum, status_login) {
            (Some(w), Some(l)) => Some(w.login_name.as_deref() == Some(l.as_str())),
            _ => None,
        };

        let mut m = Map::new();
        m.insert("label".into(), json!(label));
        m.insert("os".into(), json!(n.os));
        m.insert("online".into(), json!(n.online));
        m.insert("active".into(), json!(n.active));
        m.insert("exit_node".into(), json!(n.exit_node));
        m.insert("ip_count".into(), json!(n.ips.len()));
        m.insert("status_tags_present".into(), json!(n.tags.is_some()));
        m.insert("status_sharee_node".into(), json!(n.sharee_node));
        m.insert(
            "status_user_login_matches_whois".into(),
            json!(login_agrees),
        );
        if let Some(c) = class {
            m.insert(
                "path".into(),
                json!({
                    "class": c.as_str(),
                    "relayed": c.relayed(),
                    "cur_addr_set": !n.cur_addr.is_empty(),
                    "cur_addr_scope": endpoint_scope(&n.cur_addr),
                    "peer_relay_set": !n.peer_relay.is_empty(),
                    "relay_set": !n.relay.is_empty(),
                    "relay_region": red.show(Kind::Region, &n.relay),
                }),
            );
        }
        m.insert("whois_pipe".into(), json!(pipe_json));
        m.insert(
            "whois_pipe_ipv6".into(),
            json!(v6_call.as_ref().map(|c| Value::Object(call_brief(&red, c)))),
        );
        m.insert("whois_cli".into(), json!(cli_json));
        m.insert(
            "whois_pipe_vs_cli_same_shape".into(),
            json!(
                pipe_sum
                    .as_ref()
                    .zip(cli_sum.as_ref())
                    .map(|(a, b)| same_whois_shape(a, b))
            ),
        );
        node_reports.push(Value::Object(m));

        let who = pipe_sum.as_ref().map_or_else(
            || "whois(pipe)=FAILED".to_string(),
            |w| {
                format!(
                    "whois(pipe)=ok login={} tags={} sharer={} nodecaps={} topcaps={}",
                    w.login_name
                        .as_deref()
                        .map_or("absent".to_string(), |l| red.show(Kind::User, l)),
                    w.node_tags
                        .as_ref()
                        .map_or("absent".to_string(), |t| format!("{} entries", t.len())),
                    if w.sharer_present {
                        "present"
                    } else {
                        "absent"
                    },
                    cap_cell(
                        w.node_types.get("CapMap").copied(),
                        w.node_cap_map.as_deref()
                    ),
                    cap_cell(w.top_types.get("CapMap").copied(), w.top_cap_map.as_deref()),
                )
            },
        );
        let cli_word = cli_sum.as_ref().map_or("cli=FAILED/absent", |_| "cli=ok");
        let path = class.map_or("self".to_string(), |c| c.as_str().to_string());
        lines.push(format!(
            "  {label} os={} online={} active={} path={path} {who} {cli_word}",
            n.os, n.online, n.active
        ));
    }

    // --- variants, negative and header probes against the self address ---
    let mut variants = Vec::new();
    let mut negatives = Vec::new();
    let mut headers = Vec::new();
    if opts.probes
        && let Some(ip) = self_node.first_v4()
    {
        let rows: Vec<(&str, String)> = vec![
            ("ip_only", whois_target(ip, false)),
            ("ip_proto_tcp_spec_form", whois_target(ip, true)),
            ("ip_port0", whois_target(&addr_string(ip, Some(0)), false)),
            (
                "ip_port",
                whois_target(&addr_string(ip, Some(PROBE_PORT)), false),
            ),
            (
                "ip_port_proto_tcp",
                whois_target(&addr_string(ip, Some(PROBE_PORT)), true),
            ),
        ];
        for (name, target) in rows {
            let c = localapi::pipe_get(&opts.pipe_path, &target, Some(LOCALAPI_HOST), &[], qos);
            variants.push(probe_row(&red, name, &c));
        }
        if let Some(v6) = self_node.first_v6() {
            for (name, target) in [
                ("ipv6_no_port", whois_target(&addr_string(v6, None), false)),
                (
                    "ipv6_bracketed_port_proto_tcp",
                    whois_target(&addr_string(v6, Some(PROBE_PORT)), true),
                ),
            ] {
                let c = localapi::pipe_get(&opts.pipe_path, &target, Some(LOCALAPI_HOST), &[], qos);
                variants.push(probe_row(&red, name, &c));
            }
        }
        let neg: Vec<(&str, String)> = vec![
            (
                "loopback",
                "/localapi/v0/whois?addr=127.0.0.1%3A40000&proto=tcp".to_string(),
            ),
            (
                "documentation_ip_192_0_2_1",
                "/localapi/v0/whois?addr=192.0.2.1%3A40000&proto=tcp".to_string(),
            ),
            (
                "not_an_address",
                "/localapi/v0/whois?addr=not-an-address".to_string(),
            ),
            ("missing_addr", "/localapi/v0/whois".to_string()),
            (
                "private_lan_192_168_0_1",
                "/localapi/v0/whois?addr=192.168.0.1%3A40000&proto=tcp".to_string(),
            ),
        ];
        for (name, target) in neg {
            let c = localapi::pipe_get(&opts.pipe_path, &target, Some(LOCALAPI_HOST), &[], qos);
            negatives.push(probe_row(&red, name, &c));
        }
        let target = whois_target(&addr_string(ip, Some(PROBE_PORT)), true);
        let origin: &[(&str, &str)] = &[("Origin", "http://example.invalid")];
        let referer: &[(&str, &str)] = &[("Referer", "http://example.invalid/")];
        let hdr: Vec<HeaderProbe<'_>> = vec![
            ("host_local_tailscaled_sock", Some(LOCALAPI_HOST), &[]),
            ("host_absent_http10", None, &[]),
            ("host_localhost", Some("localhost"), &[]),
            ("host_foreign", Some("example.invalid"), &[]),
            ("origin_header", Some(LOCALAPI_HOST), origin),
            ("referer_header", Some(LOCALAPI_HOST), referer),
        ];
        for (name, host, extra) in hdr {
            let c = localapi::pipe_get(&opts.pipe_path, &target, host, extra, qos);
            headers.push(probe_row(&red, name, &c));
        }
    }

    // --- CLI whois edge cases (local lookups only) ---
    let mut cli_probes = Vec::new();
    if opts.probes
        && let (Some(exe), Some(ip)) = (cli.as_deref(), self_node.first_v4())
    {
        let with_port = addr_string(ip, Some(PROBE_PORT));
        let cases: Vec<(&str, Vec<&str>)> = vec![
            ("whois_json_ip_port", vec!["whois", "--json", &with_port]),
            ("whois_json_loopback", vec!["whois", "--json", "127.0.0.1"]),
            (
                "whois_json_documentation_ip",
                vec!["whois", "--json", "192.0.2.1"],
            ),
            (
                "whois_json_not_an_address",
                vec!["whois", "--json", "not-an-address"],
            ),
        ];
        for (name, args) in cases {
            let c = localapi::cli_run(exe, &args);
            cli_probes.push(probe_row(&red, name, &c));
        }
    }

    // --- latency ---
    let bench_ip = peers
        .iter()
        .find(|p| p.online)
        .or(peers.first())
        .unwrap_or(&self_node)
        .first_v4()
        .map(str::to_string);
    let latency = match (opts.bench, bench_ip) {
        (n, Some(ip)) if n > 0 => {
            let addr = addr_string(&ip, Some(PROBE_PORT));
            let pipe: Vec<u128> = (0..n)
                .map(|_| pipe_whois(opts, qos, &addr, true).micros)
                .collect();
            let cli_runs: Vec<u128> = cli
                .as_deref()
                .map(|p| {
                    (0..3)
                        .map(|_| localapi::cli_run(p, &["whois", "--json", &ip]).micros)
                        .collect()
                })
                .unwrap_or_default();
            json!({"pipe_whois": stats(pipe), "cli_whois": stats(cli_runs)})
        }
        _ => Value::Null,
    };
    if !latency.is_null() {
        lines.push(format!(
            "latency: pipe whois median {} us; cli whois median {} us",
            latency["pipe_whois"]["median_us"], latency["cli_whois"]["median_us"]
        ));
    }

    if opts.probes {
        for (title, rows) in [
            ("variants", &variants),
            ("negative", &negatives),
            ("headers", &headers),
            ("cli", &cli_probes),
        ] {
            let cells: Vec<String> = rows
                .iter()
                .map(|r| {
                    let code = if r["http_status"].is_null() {
                        format!("exit{}", r["exit_code"])
                    } else {
                        r["http_status"].to_string()
                    };
                    format!("{}={code}", r["name"].as_str().unwrap_or("?"))
                })
                .collect();
            lines.push(format!("probes {title}: {}", cells.join(" ")));
        }
    }
    let cli_shape_same = cli_status
        .as_ref()
        .and_then(|c| c.json.as_ref())
        .zip(pipe_status.json.as_ref())
        .map(|(c, p)| same_status_shape(c, p));
    lines.push(format!(
        "cli vs pipe: status shape same={cli_shape_same:?}; whois shape same {shape_same}/{shape_checked}"
    ));
    lines.push(format!(
        "VERIFY: self parsed={self_parsed}; peers={}; peers path-classified={peers_classified}; path counts={path_counts:?}",
        peers.len()
    ));

    let json = json!({
        "spike": "identity",
        "wp": "WP-46",
        "tool": {"name": "spike-identity", "version": env!("CARGO_PKG_VERSION")},
        "redacted": opts.redact,
        "platform": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH},
        "process": process_json(&ctx, &mut red),
        "tailscale": {
            "cli_found": cli.is_some(),
            "client_version": status.get("Version"),
            "backend_state": status.get("BackendState"),
            "health_entries": status.get("Health").and_then(Value::as_array).map(Vec::len),
            "tun": status.get("TUN"),
        },
        "pipe": {
            "path": localapi::PIPE_PATH,
            "opens_in_this_run": localapi::PIPE_OPENS.load(std::sync::atomic::Ordering::Relaxed),
            "error_pipe_busy_retries": localapi::BUSY_RETRIES.load(std::sync::atomic::Ordering::Relaxed),
            "qos_matrix_status_rounds": QOS_ROUNDS,
            "qos_matrix_status_outcomes": qos_counts,
            "qos_used_for_other_calls": qos.name(),
            "status_call": Value::Object(call_brief(&red, &pipe_status)),
            "security_descriptor_sddl_masked": sddl_shown,
            "security_descriptor_error": sddl.as_ref().err().map(|e| red.scrub(&format!("{e:#}"))),
        },
        "cli": {
            "status_call": cli_status.as_ref().map(|c| Value::Object(call_brief(&red, c))),
            "status_shape_same_as_pipe": cli_shape_same,
        },
        "status_source": source,
        "status_shape": status_shape(&status),
        "nodes": node_reports,
        "whois_variants_self": variants,
        "whois_negative_probes": negatives,
        "header_probes": headers,
        "cli_probes": cli_probes,
        "latency": latency,
        "summary": {
            "self_parsed": self_parsed,
            "peer_count": peers.len(),
            "peers_path_classified": peers_classified,
            "path_counts": path_counts,
            "whois_pipe_ok_nodes": whois_pipe_ok,
            "whois_cli_ok_nodes": whois_cli_ok,
            "whois_shape_same_pipe_vs_cli": format!("{shape_same}/{shape_checked}"),
        },
    });
    finish(&red, json, lines, self_parsed, peers_classified)
}

fn process_json(ctx: &context::ProcessContext, red: &mut Redactor) -> Value {
    json!({
        "elevated": ctx.elevated,
        "elevation_type": ctx.elevation_type,
        "integrity": ctx.integrity,
        "windows_user": ctx.windows_user.as_deref().map(|u| red.show(Kind::WinUser, u)),
    })
}

/// `absent`, `null`, or `object(n keys)` for a capability map.
fn cap_cell(json_type: Option<&'static str>, caps: Option<&[String]>) -> String {
    match (json_type, caps) {
        (None, _) => "absent".to_string(),
        (Some("object"), Some(c)) => format!("object({} keys)", c.len()),
        (Some(t), _) => t.to_string(),
    }
}

fn outcome_word(c: &Call) -> &'static str {
    if c.ok() { "ok" } else { "FAILED" }
}

fn detail(red: &Redactor, c: &Call) -> String {
    match (&c.error, c.http_status, c.exit_code) {
        (Some(e), _, _) => format!("{} os_error={:?}", red.scrub(e), c.os_error),
        (None, Some(h), _) => format!("HTTP {h}, {} us", c.micros),
        (None, None, Some(x)) => format!("exit {x}, {} us", c.micros),
        _ => "no detail".to_string(),
    }
}

fn finish(
    red: &Redactor,
    json: Value,
    lines: Vec<String>,
    status_parsed: bool,
    peers_classified: usize,
) -> Result<Outcome> {
    let summary = lines.join("\n");
    if red.enabled() {
        red.leak_check(&serde_json::to_string(&json)?)?;
        red.leak_check(&summary)?;
    }
    Ok(Outcome {
        json,
        summary,
        status_parsed,
        peers_classified,
    })
}
