// SPDX-License-Identifier: Apache-2.0
//! Text for the windows: times, sizes, audit event names, identity lines. Pure functions, so the
//! wording is tested without a window.

use std::net::IpAddr;
use std::time::Duration;

use gander_core::protocol::Timestamp;
use gander_core::protocol::host::RelayState;
use serde_json::{Map, Value};
use time::{OffsetDateTime, UtcOffset};

/// The local UTC offset, or UTC when the platform cannot tell.
pub fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

/// `14:03:22` in `offset`.
pub fn clock(ts: Timestamp, offset: UtcOffset) -> String {
    let t = ts.to_offset(offset);
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

/// `2026-11-02 14:03` in `offset`.
pub fn date_time(ts: Timestamp, offset: UtcOffset) -> String {
    let t = ts.to_offset(offset);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    )
}

/// "just now", "5 min ago", "3 h ago", "2 days ago", or the date for older times.
pub fn ago(now: OffsetDateTime, then: Timestamp, offset: UtcOffset) -> String {
    let secs = (now - then).whole_seconds();
    if secs < 0 {
        return "just now".to_owned();
    }
    match secs {
        0..=44 => "just now".to_owned(),
        45..=3_599 => format!("{} min ago", (secs / 60).max(1)),
        3_600..=86_399 => format!("{} h ago", secs / 3_600),
        86_400..=1_209_599 => {
            let d = secs / 86_400;
            format!("{d} day{} ago", if d == 1 { "" } else { "s" })
        }
        _ => date_time(then, offset),
    }
}

/// `1.5 GiB`, `320 MiB`, `12 KiB`, `0 B`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `4:07` for a countdown.
pub fn countdown(left: Duration) -> String {
    let s = left.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The identity line of a device or a pending request. `identity` is `(login, node name)`.
pub fn identity_line(identity: Option<(&str, &str)>) -> String {
    match identity {
        Some((login, node)) => format!("{login}'s {node}"),
        None => "identity unknown (not a Tailscale peer)".to_owned(),
    }
}

/// How the path from a device reaches this computer.
pub fn relay_text(path: Option<RelayState>) -> &'static str {
    match path {
        Some(RelayState::Direct) => "direct",
        Some(RelayState::PeerRelay) => "through a peer relay",
        Some(RelayState::Derp) => "through a DERP relay (slow: a few Mbit/s)",
        None => "unknown",
    }
}

/// A readable name for an audit event kind; unknown kinds are shown as they are.
pub fn event_label(kind: &str) -> String {
    let known = match kind {
        "pair.window_opened" => "Add a device opened",
        "pair.window_closed" => "Add a device closed",
        "pair.attempt" => "Pairing attempt",
        "pair.approved" => "Device approved",
        "pair.denied" => "Pairing denied",
        "pair.expired" => "Pairing request timed out",
        "pair.collected" => "Device collected its token",
        "pair.abandoned" => "Approval never collected",
        "pair.test_approved" => "Test hook approved a device",
        "device.revoked" => "Device revoked",
        "device.purged" => "Old revoked devices removed",
        "token.rotated" => "Token rotated",
        "auth.refused" => "Request refused (no valid token)",
        "identity.mismatch" => "Network identity mismatch",
        "identity.node_changed" => "Device node changed",
        "listener.changed" => "Listener sockets changed",
        "listener.paused" => "Listener paused",
        "listener.resumed" => "Listener resumed",
        "kill_switch" => "Kill switch used",
        "access" => "File access",
        "path.resolver_bug" => "Path resolver bug",
        "shares.changed" => "Shares changed",
        "settings.changed" => "Setting changed",
        "cache.purged" => "Cache emptied",
        other => return other.to_owned(),
    };
    known.to_owned()
}

/// Detail fields as `key value` pairs for one line, secrets already redacted by the audit log.
pub fn detail_summary(detail: &Map<String, Value>) -> String {
    let mut parts = Vec::new();
    for (key, value) in detail {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Null => continue,
            other => other.to_string(),
        };
        parts.push(format!("{key} {text}"));
    }
    parts.join("  ")
}

/// `100.101.102.103:42637`, with brackets for IPv6.
pub fn socket(addr: std::net::SocketAddr) -> String {
    addr.to_string()
}

/// A short, human label for where an address is: "this computer", "private network" or "LAN".
pub fn address_scope(ip: IpAddr) -> &'static str {
    if ip.is_loopback() {
        "this computer only"
    } else if gander_core::net::plan::is_tailscale_address(ip) {
        "Tailscale"
    } else {
        "private network"
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn clock_and_date_use_the_offset() {
        let ts = datetime!(2026-11-02 14:03:22 UTC);
        assert_eq!(clock(ts, UtcOffset::UTC), "14:03:22");
        let plus1 = UtcOffset::from_hms(1, 0, 0).unwrap();
        assert_eq!(clock(ts, plus1), "15:03:22");
        assert_eq!(date_time(ts, plus1), "2026-11-02 15:03");
    }

    #[test]
    fn ago_steps_through_units() {
        let now = datetime!(2026-11-02 12:00:00 UTC);
        let off = UtcOffset::UTC;
        assert_eq!(ago(now, now, off), "just now");
        assert_eq!(ago(now, now - time::Duration::minutes(5), off), "5 min ago");
        assert_eq!(ago(now, now - time::Duration::hours(3), off), "3 h ago");
        assert_eq!(ago(now, now - time::Duration::days(1), off), "1 day ago");
        assert_eq!(ago(now, now - time::Duration::days(3), off), "3 days ago");
        assert_eq!(
            ago(now, now - time::Duration::days(30), off),
            "2026-10-03 12:00"
        );
        assert_eq!(ago(now, now + time::Duration::minutes(1), off), "just now");
    }

    #[test]
    fn bytes_are_binary_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(320 * 1024 * 1024), "320 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GiB");
    }

    #[test]
    fn countdown_pads_seconds() {
        assert_eq!(countdown(Duration::from_secs(247)), "4:07");
        assert_eq!(countdown(Duration::from_secs(0)), "0:00");
    }

    #[test]
    fn identity_lines_follow_the_spec_wording() {
        assert_eq!(
            identity_line(Some(("alice@example.com", "pixel-9"))),
            "alice@example.com's pixel-9"
        );
        assert_eq!(
            identity_line(None),
            "identity unknown (not a Tailscale peer)"
        );
    }

    #[test]
    fn labels_know_the_audit_kinds_and_pass_unknown_ones_through() {
        assert_eq!(event_label("pair.approved"), "Device approved");
        assert_eq!(event_label("kill_switch"), "Kill switch used");
        assert_eq!(event_label("something.new"), "something.new");
        // Every kind the audit module defines has a label (not echoed back).
        for kind in [
            crate::audit::kind::PAIR_WINDOW_OPENED,
            crate::audit::kind::PAIR_WINDOW_CLOSED,
            crate::audit::kind::PAIR_ATTEMPT,
            crate::audit::kind::PAIR_APPROVED,
            crate::audit::kind::PAIR_DENIED,
            crate::audit::kind::PAIR_EXPIRED,
            crate::audit::kind::PAIR_COLLECTED,
            crate::audit::kind::PAIR_ABANDONED,
            crate::audit::kind::DEVICE_REVOKED,
            crate::audit::kind::DEVICE_PURGED,
            crate::audit::kind::TOKEN_ROTATED,
            crate::audit::kind::AUTH_REFUSED,
            crate::audit::kind::IDENTITY_MISMATCH,
            crate::audit::kind::IDENTITY_NODE_CHANGED,
            crate::audit::kind::LISTENER_CHANGED,
            crate::audit::kind::KILL_SWITCH,
            crate::audit::kind::ACCESS,
            crate::audit::kind::RESOLVER_BUG,
            crate::audit::kind::TEST_APPROVE,
            crate::audit::kind::SHARES_CHANGED,
            crate::audit::kind::LISTENER_PAUSED,
            crate::audit::kind::LISTENER_RESUMED,
            crate::audit::kind::SETTINGS_CHANGED,
            crate::audit::kind::CACHE_PURGED,
        ] {
            assert_ne!(event_label(kind), kind, "{kind} has no label");
        }
    }

    #[test]
    fn detail_summary_skips_nulls_and_joins_pairs() {
        let mut m = Map::new();
        m.insert("flow".into(), Value::String("qr".into()));
        m.insert("login".into(), Value::Null);
        m.insert("failures".into(), Value::from(2));
        let s = detail_summary(&m);
        assert!(s.contains("flow qr"));
        assert!(s.contains("failures 2"));
        assert!(!s.contains("login"));
    }

    #[test]
    fn address_scope_labels() {
        assert_eq!(
            address_scope("127.0.0.1".parse().unwrap()),
            "this computer only"
        );
        assert_eq!(
            address_scope("100.101.102.103".parse().unwrap()),
            "Tailscale"
        );
        assert_eq!(
            address_scope("192.168.1.5".parse().unwrap()),
            "private network"
        );
    }
}
