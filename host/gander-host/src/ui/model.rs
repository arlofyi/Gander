// SPDX-License-Identifier: Apache-2.0
//! What the windows read each frame: one cached poll of the controller, and the page list.
//!
//! The controller is asked at most about twice a second (see `app::POLL_INTERVAL`), never once
//! per frame: hovering the mouse repaints at 60 Hz and every call takes locks and clones data.

use std::time::Instant;

use gander_core::config::ShareConfig;
use gander_core::pairing::qr::QrPayload;
use time::{OffsetDateTime, UtcOffset};

use super::controller::{
    DeviceRow, HostController, HostStatus, NetworkView, PairingView, SettingsView,
};
use crate::audit::AuditEvent;

/// How many audit events the Activity window loads.
pub const ACTIVITY_ROWS: usize = 300;

/// The pages of the settings window. Add device is its own window ([`super::pages::add_device`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    /// Shared folders and per-device permissions.
    Shares,
    /// Paired devices.
    Devices,
    /// The audit log tail.
    Activity,
    /// Listener, relay paths and the connectivity self-check.
    Network,
    /// Port, cache, autostart.
    Settings,
}

impl Page {
    /// The pages in sidebar order.
    pub const ALL: [Page; 5] = [
        Page::Shares,
        Page::Devices,
        Page::Activity,
        Page::Network,
        Page::Settings,
    ];

    /// Sidebar and window title.
    pub fn title(self) -> &'static str {
        match self {
            Page::Shares => "Shares",
            Page::Devices => "Devices",
            Page::Activity => "Activity",
            Page::Network => "Network",
            Page::Settings => "Settings",
        }
    }
}

/// Everything the windows draw from, as of one poll.
#[derive(Debug, Clone)]
pub struct UiData {
    /// Tray state inputs.
    pub status: HostStatus,
    /// The pairing window.
    pub pairing: PairingView,
    /// The QR payload while the pairing window is open (a full poll only).
    pub qr: Option<QrPayload>,
    /// Saved shares.
    pub shares: Vec<ShareConfig>,
    /// Device records.
    pub devices: Vec<DeviceRow>,
    /// Audit tail, oldest first.
    pub activity: Vec<AuditEvent>,
    /// Network window data.
    pub network: NetworkView,
    /// Settings window data.
    pub settings: SettingsView,
    /// When this poll ran.
    pub fetched_at: Instant,
    /// Wall clock for "5 min ago".
    pub now: OffsetDateTime,
    /// Local UTC offset for clock times.
    pub offset: UtcOffset,
    /// How long the Stream session in progress has run (set by the application from its own
    /// clock; `None` when nothing streams or the time is not known).
    pub stream_elapsed: Option<std::time::Duration>,
}

impl UiData {
    /// A poll of everything, including the QR payload (tests and the screenshots).
    pub fn fetch(ctrl: &dyn HostController) -> Self {
        Self::fetch_with(ctrl, true)
    }

    /// A poll of everything (a window is open). The QR payload carries the pairing secret, so it
    /// is only read while the Add a device window is open (`want_qr`).
    pub fn fetch_with(ctrl: &dyn HostController, want_qr: bool) -> Self {
        let pairing = ctrl.pairing();
        let qr = if want_qr && pairing.open {
            ctrl.pairing_qr()
        } else {
            None
        };
        Self {
            status: ctrl.status(),
            pairing,
            qr,
            shares: ctrl.shares(),
            devices: ctrl.devices(),
            activity: ctrl.activity(ACTIVITY_ROWS),
            network: ctrl.network(),
            settings: ctrl.settings(),
            fetched_at: Instant::now(),
            now: OffsetDateTime::now_utc(),
            offset: super::format::local_offset(),
            stream_elapsed: None,
        }
    }

    /// Updates only what the tray and the toasts need (no window is open): status and pairing.
    pub fn refresh_light(&mut self, ctrl: &dyn HostController) {
        self.status = ctrl.status();
        self.pairing = ctrl.pairing();
        self.fetched_at = Instant::now();
        self.now = OffsetDateTime::now_utc();
    }
}
