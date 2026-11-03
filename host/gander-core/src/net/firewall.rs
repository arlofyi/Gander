// SPDX-License-Identifier: Apache-2.0
//! Windows Firewall rule strings and the connectivity self-check's matching logic.
//!
//! Windows stores firewall rules as registry strings of the form
//! `v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=6|Profile=Public|App=C:\...\gander-host.exe|Name=gander-host.exe|`.
//! Keys may repeat (`Profile=Domain|Profile=Private`). The platform crate reads the strings
//! (read-only, `gander_win::net::firewall`); this module parses them and finds the rules that would
//! silently block Gander: inbound **Block** rules that name the Gander executable. A Block rule
//! overrides every Allow rule, so a single one created by a dismissed first-listen prompt defeats
//! the installer's allow rule.
//!
//! The self-check only reports. It never creates, edits or removes rules; removal is offered in the
//! tray UI with elevation (pairing spec, "Windows Firewall").

/// A parsed firewall rule (only the fields the self-check needs).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FirewallRule {
    /// Registry value name (the rule id).
    pub id: String,
    /// `Name=` (may be an indirect string such as `@FirewallAPI.dll,-1`).
    pub name: String,
    /// `Action=Block`.
    pub block: bool,
    /// `Active=TRUE`.
    pub active: bool,
    /// `Dir=In`.
    pub inbound: bool,
    /// `App=` with environment variables expanded by the caller, if any.
    pub app: Option<String>,
    /// `Profile=` values (empty means all profiles).
    pub profiles: Vec<String>,
    /// `Protocol=` number (6 TCP, 17 UDP), if restricted.
    pub protocol: Option<u8>,
}

/// Parses one registry rule string. Returns `None` for strings that are not `v2.x` rules.
pub fn parse_rule(id: &str, data: &str) -> Option<FirewallRule> {
    let mut fields = data.split('|');
    let version = fields.next()?;
    if !version.starts_with("v2.") {
        return None;
    }
    let mut rule = FirewallRule {
        id: id.to_owned(),
        ..FirewallRule::default()
    };
    for field in fields {
        let Some((k, v)) = field.split_once('=') else {
            continue;
        };
        match k {
            "Action" => rule.block = v.eq_ignore_ascii_case("Block"),
            "Active" => rule.active = v.eq_ignore_ascii_case("TRUE"),
            "Dir" => rule.inbound = v.eq_ignore_ascii_case("In"),
            "App" => rule.app = Some(v.to_owned()),
            "Name" => rule.name = v.to_owned(),
            "Profile" => rule.profiles.push(v.to_owned()),
            "Protocol" => rule.protocol = v.parse().ok(),
            _ => {}
        }
    }
    Some(rule)
}

/// Normalises a Windows path for comparison: forward slashes to backslashes, `\\?\` prefix
/// removed, ASCII case folded.
pub fn normalize_windows_path(p: &str) -> String {
    let p = p.trim().trim_matches('"').replace('/', "\\");
    let p = p.strip_prefix(r"\\?\").unwrap_or(&p);
    p.to_ascii_lowercase()
}

/// Inbound, active Block rules that name `exe` (by full path) or whose name mentions `marker`
/// (case-insensitive, for example `gander`). `app` values must already have environment
/// variables expanded.
pub fn blocking_rules<'a>(
    rules: &'a [FirewallRule],
    exe: &str,
    marker: &str,
) -> Vec<&'a FirewallRule> {
    let exe = normalize_windows_path(exe);
    let marker = marker.to_ascii_lowercase();
    rules
        .iter()
        .filter(|r| r.block && r.active && r.inbound)
        .filter(|r| {
            let by_app = r
                .app
                .as_deref()
                .is_some_and(|a| normalize_windows_path(a) == exe);
            let by_name = !marker.is_empty() && r.name.to_ascii_lowercase().contains(&marker);
            by_app || by_name
        })
        .collect()
}

/// Result of the connectivity self-check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfCheckReport {
    /// Block rules that would stop inbound connections to Gander (ids and names).
    pub blocking_rules: Vec<FirewallRule>,
    /// Tailscale "Allow incoming connections" is off. `None` when unknown (not on Tailscale, or
    /// tailscaled not reachable).
    pub shields_up: Option<bool>,
    /// Problems running the check itself (for example the rule store could not be read).
    pub errors: Vec<String>,
}

impl SelfCheckReport {
    /// Whether anything was found that blocks inbound connections.
    pub fn has_problems(&self) -> bool {
        !self.blocking_rules.is_empty() || self.shields_up == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Program Files\Gander\gander-host.exe";

    fn rules() -> Vec<FirewallRule> {
        [
            (
                "TCP Query User{A}",
                r"v2.33|Action=Block|Active=TRUE|Dir=In|Protocol=6|Profile=Public|App=C:\Program Files\Gander\gander-host.exe|Name=gander-host.exe|Desc=gander-host.exe|",
            ),
            (
                "UDP Query User{B}",
                r"v2.33|Action=Block|Active=FALSE|Dir=In|Protocol=17|Profile=Public|App=C:\Program Files\Gander\gander-host.exe|Name=gander-host.exe|",
            ),
            (
                "allow",
                r"v2.33|Action=Allow|Active=TRUE|Dir=In|Protocol=6|Profile=Domain|Profile=Private|App=c:/program files/gander/GANDER-HOST.EXE|Name=Gander|",
            ),
            (
                "out",
                r"v2.33|Action=Block|Active=TRUE|Dir=Out|App=C:\Program Files\Gander\gander-host.exe|Name=x|",
            ),
            (
                "named",
                r"v2.30|Action=Block|Active=TRUE|Dir=In|Name=Block Gander test|",
            ),
            (
                "other",
                r"v2.33|Action=Block|Active=TRUE|Dir=In|App=C:\Windows\notepad.exe|Name=Notepad|",
            ),
        ]
        .iter()
        .filter_map(|(id, d)| parse_rule(id, d))
        .collect()
    }

    #[test]
    fn parses_repeated_keys_and_flags() {
        let r = parse_rule(
            "x",
            "v2.33|Action=Allow|Active=TRUE|Dir=In|Profile=Domain|Profile=Private|LA4=100.64.0.1|Name=Tailscale-In|",
        )
        .unwrap();
        assert_eq!(r.profiles, vec!["Domain", "Private"]);
        assert!(!r.block && r.active && r.inbound);
        assert_eq!(r.name, "Tailscale-In");
        assert!(parse_rule("x", "garbage").is_none());
    }

    #[test]
    fn finds_active_inbound_block_rules_naming_the_exe() {
        let rules = rules();
        let found: Vec<&str> = blocking_rules(&rules, EXE, "gander")
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(found, vec!["TCP Query User{A}", "named"]);
        let found = blocking_rules(&rules, r"\\?\C:/PROGRAM FILES/gander/gander-host.exe", "");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn report_problems() {
        assert!(!SelfCheckReport::default().has_problems());
        let r = SelfCheckReport {
            shields_up: Some(true),
            ..SelfCheckReport::default()
        };
        assert!(r.has_problems());
    }
}
