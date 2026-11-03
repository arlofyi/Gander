// SPDX-License-Identifier: Apache-2.0
//! Toasts: what the owner is told when they are not looking at the Gander window.
//!
//! - **A pairing request** arrives (a device scanned the QR code or typed the address): the owner
//!   must go to the Add device window and compare the code. The toast says so and carries an
//!   Open button, and goes away when the request is decided or times out.
//! - **A session starts** (security.md principle 7: "a toast on session start naming the device
//!   and the file"): a paired device connected, or started streaming a file ("Pixel 9 is viewing
//!   Deck.pptx in PowerPoint", with Stop).
//! - **A Stream prompt** (WP-53) waits for the owner: "Allow once / Always / Deny" for an app
//!   not yet approved, "Allow / Not now" when someone is using the computer. It goes when the
//!   prompt is answered or times out ([`prompt_toast`], [`ToastQueue::prune_prompts`]).
//!
//! [`Watcher::observe`] turns two consecutive polls of the host into toasts; [`ToastQueue`] keeps
//! the ones on screen. Both are plain data, tested without a window.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use gander_core::protocol::{DeviceId, PairingId};

use super::controller::{HostStatus, PairingView, StreamPromptAnswer, StreamPromptView};

/// Toasts on screen at once; older ones are dropped first.
pub const MAX_TOASTS: usize = 4;

/// How long a session toast stays.
pub const SESSION_TTL: Duration = Duration::from_secs(10);

/// How long a confirmation of a tray action stays.
pub const NOTICE_TTL: Duration = Duration::from_secs(6);

/// How long a pairing request toast stays at most (it also goes when the request is decided).
pub const PAIRING_TTL: Duration = Duration::from_secs(40);

/// How long a Stream prompt toast stays at most (it goes when the prompt is answered or times
/// out; the host's own timeout is shorter).
pub const PROMPT_TTL: Duration = Duration::from_secs(65);

/// What a toast is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastKind {
    /// A device asked to pair.
    PairingRequest(PairingId),
    /// A paired device connected.
    DeviceConnected,
    /// A device started streaming.
    StreamStarted,
    /// A Stream prompt (its id); `approval`: Allow once / Always / Deny, else Allow / Not now.
    StreamPrompt {
        /// The prompt id.
        id: u64,
        /// The approval prompt (an app not yet approved) rather than the attended one.
        approval: bool,
    },
    /// Something the owner just did from the tray (kill switch, pause, resume), confirmed on screen.
    Notice,
}

/// What the toast's Open button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastAction {
    /// Open the Add device window.
    OpenAddDevice,
    /// Open the Devices window.
    OpenDevices,
    /// End the Stream session (the "is viewing" toast).
    StopStream,
    /// Answer a Stream prompt.
    Answer(u64, StreamPromptAnswer),
    /// Nothing to open: the toast only informs.
    None,
}

/// One toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// Unique per queue, for Dismiss.
    pub id: u64,
    /// What it is about.
    pub kind: ToastKind,
    /// Bold first line.
    pub title: String,
    /// Detail line.
    pub body: String,
    /// What Open does.
    pub action: ToastAction,
    /// When it appeared.
    pub created: Instant,
    /// How long it stays.
    pub ttl: Duration,
}

/// A toast before it is queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewToast {
    /// What it is about.
    pub kind: ToastKind,
    /// Bold first line.
    pub title: String,
    /// Detail line.
    pub body: String,
    /// What Open does.
    pub action: ToastAction,
}

impl NewToast {
    fn ttl(&self) -> Duration {
        match self.kind {
            ToastKind::PairingRequest(_) => PAIRING_TTL,
            ToastKind::DeviceConnected | ToastKind::StreamStarted => SESSION_TTL,
            ToastKind::StreamPrompt { .. } => PROMPT_TTL,
            ToastKind::Notice => NOTICE_TTL,
        }
    }
}

/// The toasts on screen.
#[derive(Debug, Default)]
pub struct ToastQueue {
    items: Vec<Toast>,
    next_id: u64,
}

impl ToastQueue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a toast (newest last); the oldest goes when there are more than [`MAX_TOASTS`].
    pub fn push(&mut self, new: NewToast, now: Instant) {
        self.next_id += 1;
        let ttl = new.ttl();
        self.items.push(Toast {
            id: self.next_id,
            kind: new.kind,
            title: new.title,
            body: new.body,
            action: new.action,
            created: now,
            ttl,
        });
        while self.items.len() > MAX_TOASTS {
            self.items.remove(0);
        }
    }

    /// Removes expired toasts, and pairing-request toasts whose request is no longer the pending
    /// one. Returns whether anything went.
    pub fn prune(&mut self, now: Instant, pending: Option<&PairingId>) -> bool {
        let before = self.items.len();
        self.items.retain(|t| {
            let alive = now.saturating_duration_since(t.created) < t.ttl;
            let current = match &t.kind {
                ToastKind::PairingRequest(id) => pending == Some(id),
                _ => true,
            };
            alive && current
        });
        self.items.len() != before
    }

    /// Removes Stream prompt toasts other than `current` (answered or timed out). Returns
    /// whether anything went.
    pub fn prune_prompts(&mut self, current: Option<u64>) -> bool {
        let before = self.items.len();
        self.items.retain(|t| match t.kind {
            ToastKind::StreamPrompt { id, .. } => current == Some(id),
            _ => true,
        });
        self.items.len() != before
    }

    /// Whether a toast for the Stream prompt `id` is queued.
    pub fn has_prompt(&self, id: u64) -> bool {
        self.items
            .iter()
            .any(|t| matches!(t.kind, ToastKind::StreamPrompt { id: i, .. } if i == id))
    }

    /// Removes one toast (Dismiss, or after Open).
    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|t| t.id != id);
    }

    /// The toasts, oldest first.
    pub fn items(&self) -> &[Toast] {
        &self.items
    }

    /// Whether nothing is on screen.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// When the next toast expires, to schedule a repaint.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.items.iter().map(|t| t.created + t.ttl).min()
    }
}

/// The parts of two consecutive polls that toasts are made from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Observed {
    pending: Option<PairingId>,
    connected: BTreeSet<DeviceId>,
    streaming: Option<(String, String, String)>,
}

/// Compares each poll with the previous one and returns the toasts the change deserves. The very
/// first poll only sets the baseline: a device that was already connected when the window opened is
/// not news.
#[derive(Debug, Default)]
pub struct Watcher {
    previous: Option<Observed>,
}

impl Watcher {
    /// A watcher with no baseline yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one poll; returns the new toasts, in the order they should appear.
    pub fn observe(&mut self, status: &HostStatus, pairing: &PairingView) -> Vec<NewToast> {
        let now = Observed {
            pending: pairing.pending.as_ref().map(|p| p.pairing_id.clone()),
            connected: status.connected.iter().map(|d| d.id.clone()).collect(),
            streaming: status.stream.as_ref().map(|s| {
                (
                    s.device_name.clone(),
                    s.file_name.clone(),
                    s.app_name.clone(),
                )
            }),
        };
        let Some(before) = self.previous.replace(now.clone()) else {
            // Baseline. A request that is already waiting still deserves its toast.
            return pending_toast(pairing, None).into_iter().collect();
        };
        let mut out = Vec::new();
        if now.pending != before.pending {
            out.extend(pending_toast(pairing, before.pending.as_ref()));
        }
        for device in status
            .connected
            .iter()
            .filter(|d| !before.connected.contains(&d.id))
        {
            out.push(NewToast {
                kind: ToastKind::DeviceConnected,
                title: format!("{} connected", device.name),
                body: format!("A paired device is now using {}.", status.host_name),
                action: ToastAction::OpenDevices,
            });
        }
        if now.streaming != before.streaming
            && let Some((device, file, app)) = &now.streaming
        {
            out.push(NewToast {
                kind: ToastKind::StreamStarted,
                title: format!("{device} is viewing {file}"),
                body: format!(
                    "{file} is open in {app} on this computer and shown on {device}. Stop ends it."
                ),
                action: ToastAction::StopStream,
            });
        }
        out
    }
}

/// The toast for a Stream prompt.
pub fn prompt_toast(p: &StreamPromptView) -> NewToast {
    use crate::lifecycle::PromptKind;
    let approval = p.kind == PromptKind::Approval;
    let (title, body) = if approval {
        let by = p
            .publisher
            .as_deref()
            .map_or_else(|| "unsigned".to_owned(), str::to_owned);
        (
            format!("{} wants to open {}", p.device_name, p.file_name),
            format!(
                "With {} ({by}). Allow this app for streaming? It will be shown on {} and take mouse and keyboard input.",
                p.handler, p.device_name
            ),
        )
    } else {
        (
            format!("{} wants to stream {}", p.device_name, p.file_name),
            format!(
                "It opens in {} and will move the mouse. Allow, or Not now.",
                p.app_name
            ),
        )
    };
    NewToast {
        kind: ToastKind::StreamPrompt { id: p.id, approval },
        title,
        body,
        action: ToastAction::None,
    }
}

fn pending_toast(pairing: &PairingView, previous: Option<&PairingId>) -> Option<NewToast> {
    let pending = pairing.pending.as_ref()?;
    if previous == Some(&pending.pairing_id) {
        return None;
    }
    let who = match &pending.identity {
        Some(p) => format!("{} ({}'s {})", pending.device.name, p.login, p.node_name),
        None => pending.device.name.clone(),
    };
    Some(NewToast {
        kind: ToastKind::PairingRequest(pending.pairing_id.clone()),
        title: format!("{} wants to pair", pending.device.name),
        body: format!(
            "{who}. Open Add a device and compare the code {}.",
            pending.sas
        ),
        action: ToastAction::OpenAddDevice,
    })
}

#[cfg(test)]
mod tests {
    use gander_core::pairing::NONCE_LEN;
    use gander_core::pairing::sas::{Sas, derive_sas};
    use gander_core::pairing::window::{Flow, PendingView, Snapshot};
    use gander_core::protocol::pairing::PairingDevice;

    use super::super::controller::{ConnectedDevice, StreamSessionView};
    use super::*;

    fn status() -> HostStatus {
        HostStatus {
            host_name: "studio-pc".into(),
            paused_for: None,
            connected: Vec::new(),
            stream: None,
        }
    }

    fn sas() -> Sas {
        derive_sas(b"spki", &[1; NONCE_LEN], &[2; NONCE_LEN])
    }

    fn pending(id: &str) -> PendingView {
        PendingView {
            pairing_id: PairingId::from(id),
            device: PairingDevice {
                name: "Pixel 9".into(),
                platform: "android".into(),
                model: "Pixel 9".into(),
                app_version: "0.1.0".into(),
            },
            flow: Flow::Qr,
            identity: None,
            sas: sas(),
            source: "100.101.102.104".parse().unwrap(),
            expires_in: Duration::from_secs(110),
        }
    }

    fn snapshot(p: Option<PendingView>) -> Snapshot {
        Snapshot {
            open: true,
            generation: 1,
            expires_in: Some(Duration::from_secs(200)),
            failures: 0,
            rate_limited_sources: 0,
            pending: p,
            closed_reason: None,
        }
    }

    fn device(id: &str, name: &str) -> ConnectedDevice {
        ConnectedDevice {
            id: DeviceId::from(id),
            name: name.into(),
        }
    }

    #[test]
    fn the_first_poll_is_only_a_baseline() {
        let mut w = Watcher::new();
        let mut s = status();
        s.connected.push(device("dev_1", "Pixel 9"));
        assert!(w.observe(&s, &snapshot(None)).is_empty());
        assert!(
            w.observe(&s, &snapshot(None)).is_empty(),
            "no change, no toast"
        );
    }

    #[test]
    fn a_new_pairing_request_toasts_once_with_the_code() {
        let mut w = Watcher::new();
        let s = status();
        assert!(w.observe(&s, &snapshot(None)).is_empty());
        let toasts = w.observe(&s, &snapshot(Some(pending("pair_1"))));
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].action, ToastAction::OpenAddDevice);
        assert!(toasts[0].title.contains("Pixel 9 wants to pair"));
        assert!(
            toasts[0].body.contains(&sas().to_string()),
            "{}",
            toasts[0].body
        );
        assert!(w.observe(&s, &snapshot(Some(pending("pair_1")))).is_empty());
        let second = w.observe(&s, &snapshot(Some(pending("pair_2"))));
        assert_eq!(second.len(), 1, "a different request toasts again");
    }

    #[test]
    fn a_request_already_waiting_at_start_still_toasts() {
        let mut w = Watcher::new();
        let toasts = w.observe(&status(), &snapshot(Some(pending("pair_1"))));
        assert_eq!(toasts.len(), 1);
    }

    #[test]
    fn session_start_toasts_name_the_device_and_the_file() {
        let mut w = Watcher::new();
        let mut s = status();
        w.observe(&s, &snapshot(None));
        s.connected.push(device("dev_1", "Pixel 9"));
        let toasts = w.observe(&s, &snapshot(None));
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].kind, ToastKind::DeviceConnected);
        assert!(toasts[0].title.contains("Pixel 9"));

        s.stream = Some(StreamSessionView {
            device_name: "Pixel 9".into(),
            file_name: "deck.pptx".into(),
            app_name: "PowerPoint".into(),
            audio: false,
        });
        let toasts = w.observe(&s, &snapshot(None));
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].kind, ToastKind::StreamStarted);
        assert_eq!(toasts[0].action, ToastAction::StopStream);
        assert!(toasts[0].title.contains("deck.pptx"));
        assert!(toasts[0].body.contains("PowerPoint"));
        assert!(toasts[0].body.contains("Pixel 9"));

        // A device that stays connected, or disconnects, is not news.
        assert!(w.observe(&s, &snapshot(None)).is_empty());
        s.connected.clear();
        s.stream = None;
        assert!(w.observe(&s, &snapshot(None)).is_empty());
    }

    #[test]
    fn stream_prompts_toast_and_go_when_answered() {
        use crate::lifecycle::{PromptKind, PromptView};
        let view = |id, kind| PromptView {
            id,
            kind,
            device_name: "Pixel 9".into(),
            file_name: "Deck.pptx".into(),
            app_name: "PowerPoint".into(),
            handler: "POWERPNT.EXE".into(),
            publisher: Some("Microsoft Corporation".into()),
            expires_at: Instant::now(),
        };
        let t = prompt_toast(&view(7, PromptKind::Approval));
        assert_eq!(
            t.kind,
            ToastKind::StreamPrompt {
                id: 7,
                approval: true
            }
        );
        assert!(t.body.contains("POWERPNT.EXE") && t.body.contains("Microsoft Corporation"));
        let a = prompt_toast(&view(8, PromptKind::Attended));
        assert!(a.body.contains("move the mouse"));

        let mut q = ToastQueue::new();
        q.push(t, Instant::now());
        assert!(q.has_prompt(7));
        assert!(!q.prune_prompts(Some(7)));
        assert!(q.prune_prompts(None));
        assert!(q.is_empty());
    }

    #[test]
    fn the_queue_expires_caps_and_drops_decided_requests() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        let pairing = NewToast {
            kind: ToastKind::PairingRequest(PairingId::from("pair_1")),
            title: "a".into(),
            body: "b".into(),
            action: ToastAction::OpenAddDevice,
        };
        let session = NewToast {
            kind: ToastKind::DeviceConnected,
            title: "c".into(),
            body: "d".into(),
            action: ToastAction::OpenDevices,
        };
        q.push(pairing.clone(), t0);
        q.push(session.clone(), t0);
        assert_eq!(q.items().len(), 2);

        // Still pending: both stay.
        assert!(!q.prune(
            t0 + Duration::from_secs(5),
            Some(&PairingId::from("pair_1"))
        ));
        // Decided: the pairing toast goes at once.
        assert!(q.prune(t0 + Duration::from_secs(6), None));
        assert_eq!(q.items().len(), 1);
        // The session toast expires after its ttl.
        assert!(q.prune(t0 + SESSION_TTL, None));
        assert!(q.is_empty());

        for _ in 0..MAX_TOASTS + 3 {
            q.push(session.clone(), t0);
        }
        assert_eq!(q.items().len(), MAX_TOASTS);
        let first = q.items()[0].id;
        q.dismiss(first);
        assert_eq!(q.items().len(), MAX_TOASTS - 1);
        assert_eq!(q.next_expiry(), Some(t0 + SESSION_TTL));
    }
}
