// SPDX-License-Identifier: Apache-2.0
//! Connectivity self-check (read-only): Windows Firewall Block rules that name the Gander
//! executable, and Tailscale Shields Up.
//!
//! The tray UI (WP-18) shows the report and offers removal of Block rules with elevation; this
//! module never changes a rule or a Tailscale preference.

use gander_core::net::firewall::SelfCheckReport;

use super::network::TrustNetwork;

/// Rule-name marker: a rule whose name mentions it counts even without an `App=` path.
pub const RULE_NAME_MARKER: &str = "gander";

/// Runs the self-check for the current executable.
pub async fn connectivity_self_check(network: Option<&TrustNetwork>) -> SelfCheckReport {
    let mut report = SelfCheckReport::default();
    match std::env::current_exe() {
        Ok(exe) => {
            let exe = exe.to_string_lossy().into_owned();
            let (rules, errors) = blocking_rules_for(exe).await;
            report.blocking_rules = rules;
            report.errors.extend(errors);
        }
        Err(e) => report
            .errors
            .push(format!("cannot resolve own executable: {e}")),
    }
    if let Some(n) = network {
        report.shields_up = n.shields_up().await;
    }
    for r in &report.blocking_rules {
        tracing::warn!(rule = %r.id, name = %r.name, "firewall Block rule names Gander");
    }
    if report.shields_up == Some(true) {
        tracing::warn!("Tailscale Shields Up is on: inbound tailnet connections are blocked");
    }
    report
}

#[cfg(windows)]
async fn blocking_rules_for(
    exe: String,
) -> (Vec<gander_core::net::firewall::FirewallRule>, Vec<String>) {
    tokio::task::spawn_blocking(move || {
        gander_win::net::firewall::find_blocking_rules(&exe, RULE_NAME_MARKER)
    })
    .await
    .unwrap_or_else(|e| (Vec::new(), vec![format!("firewall check failed: {e}")]))
}

#[cfg(not(windows))]
async fn blocking_rules_for(
    _exe: String,
) -> (Vec<gander_core::net::firewall::FirewallRule>, Vec<String>) {
    (Vec::new(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gander_core::net::tailscale::FakeTailscale;
    use std::sync::Arc;

    #[tokio::test]
    async fn reports_shields_up_and_reads_rules() {
        let net = TrustNetwork::new(Arc::new(FakeTailscale::new().with_shields_up(true)));
        let r = connectivity_self_check(Some(&net)).await;
        assert_eq!(r.shields_up, Some(true));
        assert!(r.has_problems());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        // The test binary is not named in any firewall rule on a clean machine.
        assert!(r.blocking_rules.is_empty(), "{:?}", r.blocking_rules);
    }
}
