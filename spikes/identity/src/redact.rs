// SPDX-License-Identifier: Apache-2.0
//! Redaction: every identity-bearing value goes through [`Redactor::show`] before it is placed in
//! a report. With redaction on, `show` returns a stable placeholder (`<user-1>`, `<peer-A>`,
//! `100.x.y.z`) and remembers the real value so that [`Redactor::scrub`] can clean free text
//! (error messages) and [`Redactor::leak_check`] can refuse to emit output that still contains
//! one. With redaction off, `show` is the identity function.

use anyhow::{Result, bail};
use std::collections::{BTreeSet, HashMap};

/// What a value is, which decides its placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    SelfNode,
    Peer,
    User,
    UserId,
    NodeId,
    NodeKey,
    Tailnet,
    Region,
    Endpoint,
    Tag,
    IpV4,
    IpV6,
    WinUser,
}

/// Values that are protocol constants, not identities, and stay readable in redacted output.
const KEEP_LITERAL: &[&str] = &["tagged-devices"];

#[derive(Debug)]
pub struct Redactor {
    enabled: bool,
    map: HashMap<(Kind, String), String>,
    counts: HashMap<Kind, usize>,
    guard: BTreeSet<String>,
}

impl Redactor {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            map: HashMap::new(),
            counts: HashMap::new(),
            guard: BTreeSet::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The text to put in a report for `real`.
    pub fn show(&mut self, kind: Kind, real: &str) -> String {
        if !self.enabled || real.is_empty() || KEEP_LITERAL.contains(&real) {
            return real.to_string();
        }
        self.remember(real);
        if let Some(p) = self.map.get(&(kind, real.to_string())) {
            return p.clone();
        }
        let n = {
            let c = self.counts.entry(kind).or_insert(0);
            *c += 1;
            *c
        };
        // A value seen before under the same kind and placeholder family reuses its number.
        let p = placeholder(kind, n);
        self.map.insert((kind, real.to_string()), p.clone());
        p
    }

    /// Add a value that must never appear in output but that has no placeholder of its own
    /// (display names, DNS names, endpoint candidates, profile picture URLs).
    pub fn forbid(&mut self, real: &str) {
        if self.enabled {
            self.remember(real);
        }
    }

    fn remember(&mut self, real: &str) {
        // Very short values (a 1-2 character region code, a small number) would flag harmless
        // text; they are still replaced by `show`, just not guarded.
        let numeric = real.bytes().all(|b| b.is_ascii_digit());
        let min = if numeric { 5 } else { 3 };
        if real.len() >= min {
            self.guard.insert(real.to_string());
        }
    }

    /// Replace every remembered real value inside free text with its placeholder, and mask
    /// anything else that looks like an address. No-op when redaction is off.
    pub fn scrub(&self, text: &str) -> String {
        if !self.enabled {
            return text.to_string();
        }
        let mut out = text.to_string();
        let mut entries: Vec<(&String, &String)> =
            self.map.iter().map(|((_, real), ph)| (real, ph)).collect();
        entries.sort_by_key(|(real, _)| std::cmp::Reverse(real.len()));
        for (real, ph) in entries {
            out = replace_token(&out, real, ph);
        }
        for real in &self.guard {
            if !self.map.keys().any(|(_, r)| r == real) {
                out = replace_token(&out, real, "<redacted>");
            }
        }
        out
    }

    /// Refuse to emit `text` if it still contains anything identifying. Does not echo the
    /// offending value.
    pub fn leak_check(&self, text: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        for real in &self.guard {
            if find_token(text, real).is_some() {
                bail!(
                    "redaction leak: a registered identity value ({} chars) is present",
                    real.len()
                );
            }
        }
        if text.contains('@') {
            bail!("redaction leak: '@' present (possible login name)");
        }
        if text.contains(".ts.net") {
            bail!("redaction leak: a MagicDNS name is present");
        }
        if has_tailnet_ipv4(text) {
            bail!("redaction leak: a 100.64.0.0/10 address is present");
        }
        if has_nodekey(text) {
            bail!("redaction leak: a node key is present");
        }
        if has_unmasked_ula(text) {
            bail!("redaction leak: an fd7a:115c:a1e0:: address is present");
        }
        if text.contains("S-1-5-21-") {
            bail!("redaction leak: a user or domain SID is present");
        }
        Ok(())
    }
}

/// Replace machine, domain and user SIDs (`S-1-5-21-...`) in an SDDL string with a placeholder.
/// Well-known SIDs (`SY`, `BA`, `S-1-5-18`, ...) are not identities and stay.
pub fn mask_sids(sddl: &str) -> String {
    const PREFIX: &str = "S-1-5-21-";
    let mut out = String::with_capacity(sddl.len());
    let mut rest = sddl;
    while let Some(i) = rest.find(PREFIX) {
        out.push_str(&rest[..i]);
        out.push_str("<machine-or-user-sid>");
        let tail = &rest[i + PREFIX.len()..];
        let consumed = tail
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'-')
            .count();
        rest = &tail[consumed..];
    }
    out.push_str(rest);
    out
}

fn placeholder(kind: Kind, n: usize) -> String {
    match kind {
        Kind::SelfNode => "<self>".to_string(),
        Kind::Peer => format!("<peer-{}>", letters(n)),
        Kind::User => format!("<user-{n}>"),
        Kind::UserId => format!("<user-id-{n}>"),
        Kind::NodeId => format!("<node-id-{n}>"),
        Kind::NodeKey => "nodekey:<redacted>".to_string(),
        Kind::Tailnet => "<tailnet>".to_string(),
        Kind::Region => format!("<derp-region-{n}>"),
        Kind::Endpoint => "<endpoint>".to_string(),
        Kind::Tag => format!("<tag-{n}>"),
        Kind::IpV4 => "100.x.y.z".to_string(),
        Kind::IpV6 => "fd7a:115c:a1e0::x".to_string(),
        Kind::WinUser => "<winuser>".to_string(),
    }
}

/// 1 -> A, 26 -> Z, 27 -> AA.
fn letters(mut n: usize) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'A' + u8::try_from(n % 26).unwrap_or(0));
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// First index of `needle` in `hay` that is not embedded in a longer alphanumeric run.
fn find_token(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let hb = hay.as_bytes();
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word(hb[start - 1]) || !is_word(needle.as_bytes()[0]);
        let after_ok =
            end == hb.len() || !is_word(hb[end]) || !is_word(needle.as_bytes()[needle.len() - 1]);
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + needle.len().max(1);
        if from >= hay.len() {
            break;
        }
    }
    None
}

fn replace_token(hay: &str, needle: &str, with: &str) -> String {
    let mut out = String::with_capacity(hay.len());
    let mut rest = hay;
    while let Some(i) = find_token(rest, needle) {
        out.push_str(&rest[..i]);
        out.push_str(with);
        rest = &rest[i + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// True if `text` contains an IPv4 literal inside 100.64.0.0/10.
fn has_tailnet_ipv4(text: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find("100.") {
        let start = from + rel;
        from = start + 4;
        if start > 0 && (b[start - 1].is_ascii_digit() || b[start - 1] == b'.') {
            continue;
        }
        let digits: String = text[from..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(second) = digits.parse::<u32>()
            && (64..=127).contains(&second)
            && text[from + digits.len()..].starts_with('.')
        {
            return true;
        }
    }
    false
}

fn has_nodekey(text: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = text[from..].find("nodekey:") {
        let start = from + rel + "nodekey:".len();
        from = start;
        let hex = text[start..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .count();
        if hex >= 8 {
            return true;
        }
    }
    false
}

fn has_unmasked_ula(text: &str) -> bool {
    const PREFIX: &str = "fd7a:115c:a1e0:";
    let mut from = 0;
    while let Some(rel) = text[from..].find(PREFIX) {
        let after = from + rel + PREFIX.len();
        from = after;
        if !text[after..].starts_with(":x") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_identity() {
        let mut r = Redactor::new(false);
        assert_eq!(r.show(Kind::User, "alice@example.com"), "alice@example.com");
        assert!(r.leak_check("alice@example.com").is_ok());
    }

    #[test]
    fn placeholders_are_stable_and_numbered() {
        let mut r = Redactor::new(true);
        assert_eq!(r.show(Kind::User, "alice@example.com"), "<user-1>");
        assert_eq!(r.show(Kind::User, "bob@example.com"), "<user-2>");
        assert_eq!(r.show(Kind::User, "alice@example.com"), "<user-1>");
        assert_eq!(r.show(Kind::Peer, "pixel"), "<peer-A>");
        assert_eq!(r.show(Kind::Peer, "tablet"), "<peer-B>");
        assert_eq!(r.show(Kind::IpV4, "100.101.102.103"), "100.x.y.z");
        assert_eq!(
            r.show(Kind::NodeKey, "nodekey:0123456789abcdef"),
            "nodekey:<redacted>"
        );
    }

    #[test]
    fn letters_roll_over() {
        assert_eq!(letters(1), "A");
        assert_eq!(letters(26), "Z");
        assert_eq!(letters(27), "AA");
        assert_eq!(letters(28), "AB");
    }

    #[test]
    fn tagged_devices_stays_literal() {
        let mut r = Redactor::new(true);
        assert_eq!(r.show(Kind::User, "tagged-devices"), "tagged-devices");
    }

    #[test]
    fn scrub_replaces_known_values_on_token_boundaries() {
        let mut r = Redactor::new(true);
        r.show(Kind::IpV4, "100.101.102.103");
        r.show(Kind::Peer, "pixel");
        let s = r.scrub("no match for 100.101.102.103:443 on pixel; 100.101.102.1030 is other");
        assert!(s.contains("no match for 100.x.y.z:443 on <peer-A>"));
        assert!(s.contains("100.101.102.1030"));
    }

    #[test]
    fn leak_check_catches_registered_and_pattern_leaks() {
        let mut r = Redactor::new(true);
        r.show(Kind::Peer, "my-pixel-9");
        r.forbid("Alice Example");
        assert!(
            r.leak_check("clean <peer-A> 100.x.y.z fd7a:115c:a1e0::x")
                .is_ok()
        );
        assert!(r.leak_check("host my-pixel-9 here").is_err());
        assert!(r.leak_check("name Alice Example").is_err());
        assert!(r.leak_check("someone@example.com").is_err());
        assert!(r.leak_check("host.tailnet-1.ts.net.").is_err());
        assert!(r.leak_check("ip 100.64.1.2 x").is_err());
        assert!(r.leak_check("ip 100.127.255.1 x").is_err());
        assert!(r.leak_check("ip 100.128.0.1 x").is_ok());
        assert!(r.leak_check("version 1100.64.1.2").is_ok());
        assert!(r.leak_check("nodekey:0123456789abcdef").is_err());
        assert!(r.leak_check("fd7a:115c:a1e0:ab12::1").is_err());
    }

    #[test]
    fn sids_are_masked() {
        let s = "O:S-1-5-21-111-222-333-1001D:(A;;FA;;;SY)(A;;FA;;;S-1-5-21-111-222-333-513)";
        let m = mask_sids(s);
        assert!(!m.contains("S-1-5-21-"));
        assert!(m.contains("(A;;FA;;;SY)"));
        assert!(Redactor::new(true).leak_check(s).is_err());
        assert!(Redactor::new(true).leak_check(&m).is_ok());
    }

    #[test]
    fn short_values_are_replaced_but_not_guarded() {
        let mut r = Redactor::new(true);
        assert_eq!(r.show(Kind::Region, "fr"), "<derp-region-1>");
        assert!(r.leak_check("framework fr").is_ok());
    }
}
