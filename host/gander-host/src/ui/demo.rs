// SPDX-License-Identifier: Apache-2.0
//! [`DemoController`]: an in-memory [`HostController`] with sample data.
//!
//! Used by `gander-host --ui-smoke` (so the smoke run starts no server, binds no port and reads no
//! real config), by the screenshots, and as the fake in the UI tests: every action is recorded as a
//! [`Call`] and changes the sample state the way the real host would, so a test can click a
//! button and assert on both.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use gander_core::config::{ShareConfig, ShareGrant};
use gander_core::net::firewall::FirewallRule;
use gander_core::net::tailscale::PeerIdentity;
use gander_core::pairing::NONCE_LEN;
use gander_core::pairing::qr::QrPayload;
use gander_core::pairing::sas::derive_sas;
use gander_core::pairing::window::{CloseReason, Flow, PendingView, Snapshot};
use gander_core::protocol::files::{Permission, StreamDecision};
use gander_core::protocol::host::{RelayState, TrustAdapterKind};
use gander_core::protocol::pairing::PairingDevice;
use gander_core::protocol::{DeviceId, HostId, PairingId, PairingSecret, ShareId};
use time::OffsetDateTime;

use super::controller::{
    Applied, AutostartView, ConnectedDevice, ControlError, ControlResult, DeviceRow,
    HostController, HostStatus, NetworkView, PairingView, PeerPath, SelfCheckView, SettingsView,
    StreamSessionView,
};
use crate::audit::{AuditEvent, Outcome, kind};

/// An action the UI asked the controller to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// `apply_shares` with this many shares.
    ApplyShares(usize),
    /// `revoke_device`.
    Revoke(DeviceId),
    /// `open_pairing_window`.
    OpenPairing,
    /// `close_pairing_window`.
    ClosePairing,
    /// `approve_pairing`.
    Approve(PairingId),
    /// `deny_pairing`.
    Deny(PairingId),
    /// `refresh_network`.
    RefreshNetwork,
    /// `set_port`.
    SetPort(u16),
    /// `set_cache_cap_mib`.
    SetCacheCap(u32),
    /// `purge_cache`.
    PurgeCache,
    /// `set_autostart`.
    SetAutostart(bool),
    /// `set_theme`.
    SetTheme(super::theme::ThemeChoice),
    /// `set_clipboard_mode`.
    SetClipboard(gander_core::clipboard::ClipboardMode),
    /// `pause_listener` for this long.
    Pause(Duration),
    /// `resume_listener`.
    Resume,
    /// `kill_switch`.
    KillSwitch,
}

#[derive(Debug)]
struct State {
    status: HostStatus,
    shares: Vec<ShareConfig>,
    devices: Vec<DeviceRow>,
    pairing: PairingView,
    qr: Option<QrPayload>,
    audit: Vec<AuditEvent>,
    network: NetworkView,
    settings: SettingsView,
    fail_apply: Option<String>,
    calls: Vec<Call>,
    notices: Vec<crate::clipboard::Notice>,
}

/// See the module docs.
#[derive(Debug)]
pub struct DemoController {
    state: Mutex<State>,
}

/// The device ids of the sample data.
pub const PIXEL: &str = "dev_01JC0000000000000000000PX9";
/// See [`PIXEL`].
pub const TABLET: &str = "dev_01JC0000000000000000000TAB";
/// See [`PIXEL`].
pub const OLD_PHONE: &str = "dev_01JC0000000000000000000OLD";

/// A QR payload with a recognisably fake secret.
pub fn sample_qr() -> QrPayload {
    QrPayload {
        host_id: HostId::from("kVq3x0Wm4n7sJd2R9pLhYtZb8cEoUaFgNiXwSvTrQeM"),
        addresses: vec!["100.101.102.103".parse().unwrap()],
        port: 42637,
        host_name: "STUDIO-PC".into(),
        secret: PairingSecret::new("demo-secret-not-a-real-one-demo-secret-not-a-real-one"),
        magic_dns: Some("studio-pc.tail1234.ts.net".into()),
    }
}

/// A pending pairing request for the sample data and tests. `identity` adds a Tailscale peer.
pub fn sample_pending(id: &str, name: &str, flow: Flow, identity: bool) -> PendingView {
    PendingView {
        pairing_id: PairingId::from(id),
        device: PairingDevice {
            name: name.into(),
            platform: "android".into(),
            model: name.into(),
            app_version: "0.1.0".into(),
        },
        flow,
        identity: identity.then(|| PeerIdentity {
            login: "alice@example.com".into(),
            node_id: "nABC123".into(),
            node_name: "pixel-9".into(),
            tags: Vec::new(),
            shared_in: false,
        }),
        sas: derive_sas(b"demo-spki", &[7; NONCE_LEN], &[9; NONCE_LEN]),
        source: "100.101.102.104".parse().unwrap(),
        expires_in: Duration::from_secs(104),
    }
}

fn share(
    id: &str,
    name: &str,
    root: &str,
    grants: Vec<(&str, Vec<Permission>)>,
    hidden: &[&str],
) -> ShareConfig {
    ShareConfig {
        id: ShareId::from(id),
        name: name.into(),
        root: PathBuf::from(root),
        show_hidden: false,
        show_path: false,
        hidden_patterns: hidden.iter().map(|s| (*s).to_owned()).collect(),
        stream_default: StreamDecision::Ask,
        grants: grants
            .into_iter()
            .map(|(d, permissions)| ShareGrant {
                device: DeviceId::from(d),
                permissions,
            })
            .collect(),
    }
}

fn row(
    now: OffsetDateTime,
    id: &str,
    name: &str,
    model: &str,
    seen_min: Option<i64>,
    connected: bool,
) -> DeviceRow {
    DeviceRow {
        id: DeviceId::from(id),
        name: name.into(),
        platform: "android".into(),
        model: model.into(),
        app_version: "0.1.0".into(),
        identity: Some((
            "alice@example.com".into(),
            name.to_lowercase().replace(' ', "-"),
        )),
        created_at: now - time::Duration::days(12),
        last_seen_at: seen_min.map(|m| now - time::Duration::minutes(m)),
        last_address: seen_min.map(|_| "100.101.102.104".parse::<IpAddr>().unwrap()),
        connected,
        revoked_at: None,
    }
}

impl DemoController {
    /// A host with nothing configured: no shares, no devices, nothing connected.
    pub fn empty() -> Self {
        let st = State {
            status: HostStatus {
                host_name: "STUDIO-PC".into(),
                paused_for: None,
                connected: Vec::new(),
                stream: None,
            },
            shares: Vec::new(),
            devices: Vec::new(),
            pairing: closed_pairing(None),
            qr: None,
            audit: Vec::new(),
            network: NetworkView {
                adapter: TrustAdapterKind::Tailscale,
                interface: Some("Tailscale".into()),
                port: 42637,
                bound: vec![
                    "127.0.0.1:42637".parse().unwrap(),
                    "100.101.102.103:42637".parse().unwrap(),
                ],
                paused_for: None,
                peers: Vec::new(),
                self_check: SelfCheckView {
                    running: false,
                    done: true,
                    ..SelfCheckView::default()
                },
            },
            settings: SettingsView {
                port: 42637,
                cache_used: Some(0),
                cache_cap_mib: 2048,
                autostart: AutostartView {
                    enabled: false,
                    note: None,
                },
                version: "0.0.0".into(),
                theme: super::theme::ThemeChoice::System,
                previews_problem: None,
                clipboard: gander_core::clipboard::ClipboardMode::Off,
            },
            fail_apply: None,
            calls: Vec::new(),
            notices: Vec::new(),
        };
        Self {
            state: Mutex::new(st),
        }
    }

    /// A lived-in host: three shares, three devices (one connected, one revoked), activity, a
    /// Block rule and Shields Up on the Network window.
    pub fn sample() -> Self {
        Self::sample_at(OffsetDateTime::now_utc())
    }

    /// [`DemoController::sample`] with every timestamp relative to `now`, so a screenshot can be
    /// made reproducible.
    pub fn sample_at(now: OffsetDateTime) -> Self {
        let demo = Self::empty();
        {
            let mut s = demo.lock();
            s.shares = vec![
                share(
                    "shr_01JC000000000000000000PROJ",
                    "Projects",
                    "D:\\Projects",
                    vec![
                        (
                            PIXEL,
                            vec![Permission::List, Permission::Preview, Permission::Fetch],
                        ),
                        (TABLET, vec![Permission::List, Permission::Preview]),
                    ],
                    &["**/.git", "**/node_modules"],
                ),
                share(
                    "shr_01JC00000000000000000DOCS",
                    "Documents",
                    "C:\\Users\\alice\\Documents",
                    vec![(
                        PIXEL,
                        vec![
                            Permission::List,
                            Permission::Preview,
                            Permission::Fetch,
                            Permission::Stream,
                        ],
                    )],
                    &[],
                ),
                share(
                    "shr_01JC000000000000000000PICS",
                    "Photos",
                    "D:\\Photos",
                    Vec::new(),
                    &[],
                ),
            ];
            let mut old = row(
                now,
                OLD_PHONE,
                "Old Galaxy",
                "SM-S911B",
                Some(60 * 24 * 9),
                false,
            );
            old.revoked_at = Some(now - time::Duration::days(2));
            s.devices = vec![
                row(now, PIXEL, "Pixel 9", "Pixel 9", Some(0), true),
                row(now, TABLET, "Galaxy Tab S9", "SM-X710", Some(185), false),
                old,
            ];
            s.status.connected = vec![ConnectedDevice {
                id: DeviceId::from(PIXEL),
                name: "Pixel 9".into(),
            }];
            let ev = |mins: i64, e: AuditEvent| {
                let mut e = e;
                e.ts = now - time::Duration::minutes(mins);
                e
            };
            let pixel = DeviceId::from(PIXEL);
            s.audit = vec![
                ev(
                    240,
                    AuditEvent::new(kind::PAIR_WINDOW_OPENED).detail("generation", 1),
                ),
                ev(
                    239,
                    AuditEvent::new(kind::PAIR_APPROVED)
                        .device(&pixel)
                        .source("100.101.102.104".parse().unwrap())
                        .detail("flow", "qr")
                        .detail("name", "Pixel 9")
                        .detail("login", "alice@example.com"),
                ),
                ev(
                    120,
                    AuditEvent::new(kind::ACCESS)
                        .device(&pixel)
                        .detail("action", "list")
                        .detail("share", "Projects")
                        .detail("path", "/clients/acme"),
                ),
                ev(
                    60,
                    AuditEvent::new(kind::AUTH_REFUSED)
                        .source("100.101.102.77".parse().unwrap())
                        .outcome(Outcome::Refused)
                        .detail("reason", "unknown token"),
                ),
                ev(
                    5,
                    AuditEvent::new(kind::ACCESS)
                        .device(&pixel)
                        .detail("action", "fetch")
                        .detail("share", "Documents")
                        .detail("path", "/Quarterly review.pptx"),
                ),
                ev(
                    1,
                    AuditEvent::new(kind::IDENTITY_NODE_CHANGED)
                        .device(&DeviceId::from(TABLET))
                        .outcome(Outcome::Warning)
                        .detail("node", "tab-s9-2"),
                ),
            ];
            s.network.peers = vec![PeerPath {
                device_name: "Pixel 9".into(),
                address: "100.101.102.104".parse().unwrap(),
                path: Some(RelayState::Derp),
            }];
            s.network.self_check = SelfCheckView {
                running: false,
                done: true,
                blocking_rules: vec![FirewallRule {
                    id: "TCP Query User{3F1A}C:\\program files\\gander\\gander-host.exe".into(),
                    name: "gander-host.exe".into(),
                    block: true,
                    active: true,
                    inbound: true,
                    app: Some("C:\\Program Files\\Gander\\gander-host.exe".into()),
                    profiles: vec!["Public".into()],
                    protocol: Some(6),
                }],
                shields_up: Some(true),
                errors: Vec::new(),
            };
            s.settings.cache_used = Some(412 * 1024 * 1024);
            s.settings.autostart = AutostartView {
                enabled: true,
                note: Some(
                    "Not connected to Windows yet: the installer will register the sign-in task. \
                     This switch is not saved."
                        .into(),
                ),
            };
        }
        demo
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Everything the UI asked for so far, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Makes the next `apply_shares` fail with `message`.
    pub fn fail_next_apply(&self, message: &str) {
        self.lock().fail_apply = Some(message.to_owned());
    }

    /// A device sends a pairing request while the window is open.
    pub fn receive_pairing_request(&self, pending: PendingView) {
        let mut s = self.lock();
        s.pairing.pending = Some(pending);
    }

    /// A refused attempt.
    pub fn record_failed_attempt(&self) {
        self.lock().pairing.failures += 1;
    }

    /// Replaces the status (tray state tests).
    pub fn set_status(&self, status: HostStatus) {
        self.lock().status = status;
    }

    /// Queues a notice for the next `take_notices` (clipboard notification tests).
    pub fn push_notice(&self, notice: crate::clipboard::Notice) {
        self.lock().notices.push(notice);
    }

    /// Sets a stream session.
    pub fn set_stream(&self, stream: Option<StreamSessionView>) {
        self.lock().status.stream = stream;
    }

    /// Closes the pairing window as if it timed out.
    pub fn expire_pairing_window(&self) {
        let mut s = self.lock();
        s.pairing = closed_pairing(Some(CloseReason::Expired));
        s.qr = None;
    }

    /// Mutates the network view (tests).
    pub fn edit_network(&self, f: impl FnOnce(&mut NetworkView)) {
        f(&mut self.lock().network);
    }
}

fn closed_pairing(reason: Option<CloseReason>) -> PairingView {
    Snapshot {
        open: false,
        generation: 0,
        expires_in: None,
        failures: 0,
        rate_limited_sources: 0,
        pending: None,
        closed_reason: reason,
    }
}

impl HostController for DemoController {
    fn status(&self) -> HostStatus {
        self.lock().status.clone()
    }

    fn shares(&self) -> Vec<ShareConfig> {
        self.lock().shares.clone()
    }

    fn apply_shares(&self, shares: Vec<ShareConfig>) -> ControlResult<()> {
        let mut s = self.lock();
        s.calls.push(Call::ApplyShares(shares.len()));
        if let Some(message) = s.fail_apply.take() {
            return Err(ControlError::new(message));
        }
        s.shares = shares;
        Ok(())
    }

    fn devices(&self) -> Vec<DeviceRow> {
        self.lock().devices.clone()
    }

    fn revoke_device(&self, id: &DeviceId) -> ControlResult<()> {
        let mut s = self.lock();
        s.calls.push(Call::Revoke(id.clone()));
        let Some(d) = s.devices.iter_mut().find(|d| &d.id == id) else {
            return Err(ControlError::new("no such device"));
        };
        d.revoked_at = Some(OffsetDateTime::now_utc());
        d.connected = false;
        s.status.connected.retain(|c| &c.id != id);
        Ok(())
    }

    fn open_pairing_window(&self) -> QrPayload {
        let mut s = self.lock();
        s.calls.push(Call::OpenPairing);
        s.pairing = Snapshot {
            open: true,
            generation: s.pairing.generation + 1,
            expires_in: Some(Duration::from_secs(5 * 60)),
            failures: 0,
            rate_limited_sources: 0,
            pending: None,
            closed_reason: None,
        };
        let qr = sample_qr();
        s.qr = Some(qr.clone());
        qr
    }

    fn pairing_qr(&self) -> Option<QrPayload> {
        self.lock().qr.clone()
    }

    fn pairing(&self) -> PairingView {
        self.lock().pairing.clone()
    }

    fn close_pairing_window(&self) {
        let mut s = self.lock();
        s.calls.push(Call::ClosePairing);
        if s.pairing.open {
            s.pairing = closed_pairing(Some(CloseReason::Owner));
        }
        s.qr = None;
    }

    fn approve_pairing(&self, id: &PairingId) -> ControlResult<DeviceId> {
        let mut s = self.lock();
        s.calls.push(Call::Approve(id.clone()));
        let Some(p) = s.pairing.pending.take().filter(|p| &p.pairing_id == id) else {
            return Err(ControlError::new("The request is no longer pending."));
        };
        let device_id = DeviceId::from(format!("dev_demo_{}", s.devices.len() + 1));
        let mut r = row(
            OffsetDateTime::now_utc(),
            device_id.as_str(),
            &p.device.name,
            &p.device.model,
            Some(0),
            false,
        );
        r.identity = p.identity.map(|i| (i.login, i.node_name));
        s.devices.push(r);
        s.pairing = closed_pairing(Some(CloseReason::Approved));
        s.qr = None;
        Ok(device_id)
    }

    fn deny_pairing(&self, id: &PairingId) -> ControlResult<()> {
        let mut s = self.lock();
        s.calls.push(Call::Deny(id.clone()));
        if s.pairing
            .pending
            .take()
            .filter(|p| &p.pairing_id == id)
            .is_none()
        {
            return Err(ControlError::new("The request is no longer pending."));
        }
        s.pairing = closed_pairing(Some(CloseReason::Denied));
        s.qr = None;
        Ok(())
    }

    fn activity(&self, n: usize) -> Vec<AuditEvent> {
        let s = self.lock();
        s.audit
            .iter()
            .skip(s.audit.len().saturating_sub(n))
            .cloned()
            .collect()
    }

    fn network(&self) -> NetworkView {
        self.lock().network.clone()
    }

    fn refresh_network(&self) {
        self.lock().calls.push(Call::RefreshNetwork);
    }

    fn settings(&self) -> SettingsView {
        self.lock().settings.clone()
    }

    fn set_port(&self, port: u16) -> ControlResult<Applied> {
        let mut s = self.lock();
        s.calls.push(Call::SetPort(port));
        if port == 0 {
            return Err(ControlError::new("listener.port must not be 0"));
        }
        s.settings.port = port;
        Ok(Applied::AfterRestart)
    }

    fn set_cache_cap_mib(&self, mib: u32) -> ControlResult<Applied> {
        let mut s = self.lock();
        s.calls.push(Call::SetCacheCap(mib));
        s.settings.cache_cap_mib = mib;
        Ok(Applied::AfterRestart)
    }

    fn purge_cache(&self) -> ControlResult<u64> {
        let mut s = self.lock();
        s.calls.push(Call::PurgeCache);
        let freed = s.settings.cache_used.unwrap_or(0);
        s.settings.cache_used = Some(0);
        Ok(freed)
    }

    fn set_autostart(&self, enabled: bool) -> ControlResult<()> {
        let mut s = self.lock();
        s.calls.push(Call::SetAutostart(enabled));
        s.settings.autostart.enabled = enabled;
        Ok(())
    }

    fn set_theme(&self, choice: super::theme::ThemeChoice) -> ControlResult<()> {
        let mut s = self.lock();
        s.calls.push(Call::SetTheme(choice));
        s.settings.theme = choice;
        Ok(())
    }

    fn set_clipboard_mode(
        &self,
        mode: gander_core::clipboard::ClipboardMode,
    ) -> ControlResult<Applied> {
        let mut s = self.lock();
        s.calls.push(Call::SetClipboard(mode));
        s.settings.clipboard = mode;
        Ok(Applied::Now)
    }

    fn take_notices(&self) -> Vec<crate::clipboard::Notice> {
        std::mem::take(&mut self.lock().notices)
    }

    fn pause_listener(&self, duration: Duration) {
        let mut s = self.lock();
        s.calls.push(Call::Pause(duration));
        s.status.paused_for = Some(duration);
        s.network.paused_for = Some(duration);
        s.status.connected.clear();
        s.pairing = closed_pairing(Some(CloseReason::Owner));
    }

    fn resume_listener(&self) {
        let mut s = self.lock();
        s.calls.push(Call::Resume);
        s.status.paused_for = None;
        s.network.paused_for = None;
    }

    fn kill_switch(&self) {
        let mut s = self.lock();
        s.calls.push(Call::KillSwitch);
        s.status.connected.clear();
        s.status.stream = None;
        s.pairing = closed_pairing(Some(CloseReason::Owner));
        s.qr = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_has_the_pieces_every_window_needs() {
        let c = DemoController::sample();
        assert_eq!(c.shares().len(), 3);
        assert_eq!(c.devices().len(), 3);
        assert!(c.devices().iter().any(|d| d.revoked_at.is_some()));
        assert_eq!(c.status().connected.len(), 1);
        assert!(!c.activity(100).is_empty());
        assert!(c.network().self_check.has_problems());
    }

    #[test]
    fn approve_adds_a_device_and_closes_the_window() {
        let c = DemoController::empty();
        c.open_pairing_window();
        c.receive_pairing_request(sample_pending("pair_1", "Pixel 9", Flow::Qr, true));
        let id = c.approve_pairing(&PairingId::from("pair_1")).unwrap();
        assert!(c.devices().iter().any(|d| d.id == id));
        assert!(!c.pairing().open);
        assert_eq!(c.pairing().closed_reason, Some(CloseReason::Approved));
        assert!(c.approve_pairing(&PairingId::from("pair_1")).is_err());
    }

    #[test]
    fn calls_are_recorded_in_order() {
        let c = DemoController::empty();
        c.open_pairing_window();
        c.close_pairing_window();
        c.kill_switch();
        assert_eq!(
            c.calls(),
            vec![Call::OpenPairing, Call::ClosePairing, Call::KillSwitch]
        );
    }
}
