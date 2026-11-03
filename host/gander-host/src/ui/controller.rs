// SPDX-License-Identifier: Apache-2.0
//! [`HostController`]: the one seam between the tray/settings UI and the running host.
//!
//! The UI never touches the server, the pairing service, the registry or the config files
//! directly. It reads plain view values and calls the actions below, so every window is unit
//! testable against [`super::demo::DemoController`] and the real implementation
//! ([`super::live::LiveController`]) is a thin adapter over the WP-12/13/17 APIs.
//!
//! Every method is synchronous and cheap or short: the UI calls them from its frame loop (reads at
//! most every 400 ms). Anything slow (the firewall self-check, WhoIs) is started with a
//! `refresh_*` call and read back through the next [`HostController::network`].
//!
//! Secrets: [`QrPayload`] redacts itself in `Debug` and the UI only ever renders it as a QR code;
//! no view type below carries a token, a token hash or a pairing secret.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use gander_core::clipboard::ClipboardMode;
use gander_core::config::{DeviceRecord, ShareConfig};
use gander_core::net::firewall::FirewallRule;
use gander_core::pairing::qr::QrPayload;
use gander_core::pairing::window::Snapshot as PairingSnapshot;
use gander_core::protocol::host::{RelayState, TrustAdapterKind};
use gander_core::protocol::{DeviceId, PairingId, Timestamp};

pub use super::theme::ThemeChoice;
use crate::audit::AuditEvent;
use crate::clipboard::Notice;

/// The pairing window as the Add device window shows it. Re-exported from the pairing state
/// machine: `open`, `expires_in`, `failures`, `rate_limited_sources`, `pending`, `closed_reason`.
pub type PairingView = PairingSnapshot;

/// A failed action, with text fit to show to the owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ControlError(pub String);

impl ControlError {
    /// An error with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Result of a [`HostController`] action.
pub type ControlResult<T> = Result<T, ControlError>;

/// A device with an open connection right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedDevice {
    /// Device id.
    pub id: DeviceId,
    /// Device name from pairing.
    pub name: String,
}

/// A Stream session in progress (WP-53): from the moment its window is found until it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSessionView {
    /// Which device is watching.
    pub device_name: String,
    /// The file name being streamed.
    pub file_name: String,
    /// The app it is open in.
    pub app_name: String,
    /// The window's sound is being sent too (WP-55).
    pub audio: bool,
}

impl StreamSessionView {
    /// What the device gets: the app, plus " with sound" while its sound is sent (WP-55).
    pub fn viewing(&self, what: &str) -> String {
        if self.audio {
            format!("{what} with sound")
        } else {
            what.to_owned()
        }
    }
}

/// A Stream prompt waiting for the owner (approval or "someone is using this computer").
pub type StreamPromptView = crate::lifecycle::PromptView;

/// The owner's answer to a Stream prompt.
pub type StreamPromptAnswer = crate::lifecycle::PromptAnswer;

/// What the tray icon, its tooltip and the toasts are derived from. Polled about twice a second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStatus {
    /// This computer's name.
    pub host_name: String,
    /// Time left of a listener pause, `None` when the listener is running.
    pub paused_for: Option<Duration>,
    /// Devices with an open connection, by name.
    pub connected: Vec<ConnectedDevice>,
    /// The Stream session in progress, if any.
    pub stream: Option<StreamSessionView>,
}

/// One row of the Devices window. Carries no token or token hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// Device id.
    pub id: DeviceId,
    /// Name from pairing.
    pub name: String,
    /// Platform from pairing (`android`).
    pub platform: String,
    /// Model from pairing.
    pub model: String,
    /// Client version.
    pub app_version: String,
    /// `login` and node name recorded at approval (Tailscale), `None` when there was no identity.
    pub identity: Option<(String, String)>,
    /// When it was approved.
    pub created_at: Timestamp,
    /// Last authenticated request.
    pub last_seen_at: Option<Timestamp>,
    /// Source address of the last request.
    pub last_address: Option<IpAddr>,
    /// Whether it has a connection open now.
    pub connected: bool,
    /// When it was revoked (kept 30 days).
    pub revoked_at: Option<Timestamp>,
}

impl DeviceRow {
    /// Builds a row from a registry record (dropping the token hash).
    pub fn from_record(record: &DeviceRecord, connected: bool) -> Self {
        Self {
            id: record.id.clone(),
            name: record.name.clone(),
            platform: record.platform.clone(),
            model: record.model.clone(),
            app_version: record.app_version.clone(),
            identity: record
                .network_identity
                .as_ref()
                .map(|i| (i.login.clone(), i.node_name.clone())),
            created_at: record.created_at,
            last_seen_at: record.last_seen_at,
            last_address: record.last_address,
            connected: connected && record.revoked_at.is_none(),
            revoked_at: record.revoked_at,
        }
    }
}

/// How a connected device reaches this computer (Network window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerPath {
    /// Device name.
    pub device_name: String,
    /// Its last address.
    pub address: IpAddr,
    /// Direct, peer relay or DERP; `None` when unknown (not on Tailscale).
    pub path: Option<RelayState>,
}

/// The firewall and Tailscale checks (pairing spec, "Windows Firewall").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelfCheckView {
    /// Whether a check is running now.
    pub running: bool,
    /// Whether one has finished since the host started.
    pub done: bool,
    /// Inbound Block rules that name Gander (they override every Allow rule).
    pub blocking_rules: Vec<FirewallRule>,
    /// Tailscale "Allow incoming connections" is off. `None` when unknown.
    pub shields_up: Option<bool>,
    /// Problems running the check itself.
    pub errors: Vec<String>,
}

impl SelfCheckView {
    /// Whether the check found something that stops inbound connections.
    pub fn has_problems(&self) -> bool {
        !self.blocking_rules.is_empty() || self.shields_up == Some(true)
    }
}

/// The Network window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkView {
    /// Which trust adapter picked the addresses.
    pub adapter: TrustAdapterKind,
    /// The network interface the private-network addresses belong to (Windows adapter alias).
    pub interface: Option<String>,
    /// Port every socket uses.
    pub port: u16,
    /// Sockets bound now. Loopback first.
    pub bound: Vec<SocketAddr>,
    /// Whether the listener is paused (only loopback is bound).
    pub paused_for: Option<Duration>,
    /// Path to each connected device. The relay information.
    pub peers: Vec<PeerPath>,
    /// The latest self-check.
    pub self_check: SelfCheckView,
}

/// Autostart (a Task Scheduler logon task, registered by the installer, WP-1A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartView {
    /// Whether Gander starts when the owner signs in.
    pub enabled: bool,
    /// A caveat to show under the switch (the stub says nothing is registered yet).
    pub note: Option<String>,
}

/// The Settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsView {
    /// The configured listener port (a change needs a restart).
    pub port: u16,
    /// Thumbnail and preview caches: bytes used now, `None` when the caches are unavailable.
    pub cache_used: Option<u64>,
    /// Configured total cap in MiB (read at start).
    pub cache_cap_mib: u32,
    /// Autostart state.
    pub autostart: AutostartView,
    /// Host version, shown at the bottom.
    pub version: String,
    /// The owner's theme choice (System, Light, Dark).
    pub theme: ThemeChoice,
    /// Why previews and thumbnails are off (render worker missing, cache unavailable), shown as
    /// a warning at the top of the window. `None` when they work.
    pub previews_problem: Option<String>,
    /// Which way clipboard text may cross during a stream (`[stream] clipboard`, in force now).
    pub clipboard: ClipboardMode,
}

/// What an applied setting needs before it takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// It is in force now.
    Now,
    /// It is saved and takes effect when Gander restarts.
    AfterRestart,
}

/// The seam between the UI and the host. See the module docs.
pub trait HostController: Send + Sync + 'static {
    // ---- status (polled) ----

    /// Tray state inputs.
    fn status(&self) -> HostStatus;

    // ---- shares ----

    /// The configured shares, in display order, with their per-device grants.
    fn shares(&self) -> Vec<ShareConfig>;

    /// Replaces the shares: validates, saves `config.toml`, tells the API and the events channel
    /// (`shares.updated`). Nothing changes when it fails.
    fn apply_shares(&self, shares: Vec<ShareConfig>) -> ControlResult<()>;

    // ---- devices ----

    /// Every device record, active and revoked, without secrets.
    fn devices(&self) -> Vec<DeviceRow>;

    /// Revokes a device now: its connections close and its token stops working.
    fn revoke_device(&self, id: &DeviceId) -> ControlResult<()>;

    // ---- pairing (Add device) ----

    /// Opens the pairing window (or re-opens it with a fresh secret) and returns the QR payload.
    /// Only the Add device window calls this; the pairing endpoints answer only while it is open.
    fn open_pairing_window(&self) -> QrPayload;

    /// The QR payload of the window that is open now, if any (same secret as the last
    /// [`HostController::open_pairing_window`]).
    fn pairing_qr(&self) -> Option<QrPayload>;

    /// The window and the pending request, for the dialog.
    fn pairing(&self) -> PairingView;

    /// Closes the pairing window (the owner closed the dialog). Cancels a pending request.
    fn close_pairing_window(&self);

    /// The owner clicked Allow.
    fn approve_pairing(&self, id: &PairingId) -> ControlResult<DeviceId>;

    /// The owner clicked Deny.
    fn deny_pairing(&self, id: &PairingId) -> ControlResult<()>;

    // ---- activity ----

    /// The last `n` audit events, oldest first.
    fn activity(&self, n: usize) -> Vec<AuditEvent>;

    // ---- windows ----

    /// `[ui] gpu`: which GPU draws the host's windows on a hybrid-GPU laptop.
    fn ui_gpu(&self) -> gander_core::config::UiGpu {
        gander_core::config::UiGpu::default()
    }

    // ---- network ----

    /// The Network window's data (the self-check part is the last finished result).
    fn network(&self) -> NetworkView;

    /// Starts the firewall and Tailscale self-check in the background and reads the peer paths
    /// again. Read-only: it never changes a rule.
    fn refresh_network(&self);

    // ---- settings ----

    /// The Settings window's data.
    fn settings(&self) -> SettingsView;

    /// Saves a new listener port. Takes effect after a restart.
    fn set_port(&self, port: u16) -> ControlResult<Applied>;

    /// Saves a new total cache cap in MiB. Takes effect after a restart.
    fn set_cache_cap_mib(&self, mib: u32) -> ControlResult<Applied>;

    /// Empties the thumbnail and preview caches; returns the bytes freed.
    fn purge_cache(&self) -> ControlResult<u64>;

    /// Turns the logon autostart on or off.
    fn set_autostart(&self, enabled: bool) -> ControlResult<()>;

    /// Saves the theme choice (applied by the UI at once).
    fn set_theme(&self, choice: ThemeChoice) -> ControlResult<()>;

    /// Saves which way clipboard text may cross during a stream (`[stream] clipboard`). In force
    /// at once: a running stream refuses the next request the new mode does not allow.
    fn set_clipboard_mode(&self, mode: ClipboardMode) -> ControlResult<Applied>;

    // ---- tray menu ----

    /// Pauses the listener for `duration`: drops every session, closes the pairing window and
    /// stops serving the private network until the time is up or [`HostController::resume_listener`].
    fn pause_listener(&self, duration: Duration);

    /// Ends a pause now.
    fn resume_listener(&self);

    /// Kill switch: drops every session and closes the pairing window. Devices stay paired and
    /// can reconnect; revoke a device to lock it out.
    fn kill_switch(&self);

    // ---- stream (WP-53) ----

    /// The Stream prompt waiting for the owner, if any (shown as a toast with buttons).
    fn stream_prompt(&self) -> Option<StreamPromptView> {
        None
    }

    /// Answers the Stream prompt `id` (no-op when it is no longer waiting).
    fn answer_stream_prompt(&self, id: u64, answer: StreamPromptAnswer) {
        let _ = (id, answer);
    }

    /// Ends the Stream session now (the Stop button of the "is viewing" toast).
    fn stop_stream(&self) {}

    // ---- clipboard (WP-56) ----

    /// Notices queued since the last call, oldest first ("Pixel 9 copied text from this
    /// computer"): the tray shows each as a native notification. Draining, so call it once per
    /// poll.
    fn take_notices(&self) -> Vec<Notice> {
        Vec::new()
    }
}

/// Picks a folder for a new share. Behind a trait so tests never open a dialog.
pub trait FolderPicker: Send + Sync + 'static {
    /// Shows the folder dialog and blocks until it closes. `None` when cancelled. Called on a
    /// worker thread, never on the UI thread.
    fn pick_folder(&self) -> Option<PathBuf>;
}
