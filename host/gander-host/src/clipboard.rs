// SPDX-License-Identifier: Apache-2.0
//! The host side of the clipboard text bridge (WP-56): what the owner is told and what the audit
//! log keeps. The policy, the caps and the codec are `gander_core::clipboard`; the Windows
//! clipboard is `gander_win::clipboard`; the data channel wiring is `gander_stream::input`.
//!
//! [`HostClipboard`] holds the two things the tray, the lifecycle and the audit log share: the
//! owner's live setting ([`ClipboardSetting`], changed from Settings, read by every session on
//! every request) and the queue of notices the tray turns into native notifications
//! ([`Notices`]). Its [`HostClipboard::observer`] hears every request, refused or not:
//!
//! - **Audit** ([`kind::CLIPBOARD`]): `detail.direction` (`to_phone` when the phone read the
//!   computer's clipboard, `from_phone` when it wrote it), `detail.session`, and either
//!   `detail.bytes` (outcome `ok`) or `detail.reason` (outcome `refused`, the code the phone
//!   got). **Never the text.**
//! - **Notice**, for a transfer that happened: "Pixel 9 copied text from this computer" when the
//!   phone read the clipboard, "Pixel 9 put text on this computer's clipboard" when it wrote it,
//!   with the size. At most one per device and direction in [`NOTICE_QUIET`], so a burst of taps
//!   is one notification; every transfer is audited regardless.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gander_core::clipboard::{
    ClipboardEvent, ClipboardMode, ClipboardObserver, ClipboardSetting, Direction,
};
use gander_core::protocol::DeviceId;

use crate::audit::{AuditEvent, AuditLog, Outcome, kind};
use crate::lifecycle::DeviceNames;

/// Notices kept for the tray; older ones are dropped first.
pub const MAX_NOTICES: usize = 8;

/// The shortest time between two notices for the same device and direction.
pub const NOTICE_QUIET: Duration = Duration::from_secs(10);

/// One thing the owner should be told on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Bold first line.
    pub title: String,
    /// Detail line.
    pub body: String,
}

/// The queue between the stream's input thread (which pushes) and the tray (which takes them on
/// its next poll). Cheap to clone; all clones share the queue.
#[derive(Debug, Clone, Default)]
pub struct Notices(Arc<Mutex<VecDeque<Notice>>>);

impl Notices {
    /// Adds a notice (the oldest goes when there are more than [`MAX_NOTICES`]).
    pub fn push(&self, notice: Notice) {
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(notice);
        while q.len() > MAX_NOTICES {
            q.pop_front();
        }
    }

    /// Takes every queued notice, oldest first.
    pub fn take(&self) -> Vec<Notice> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
}

/// The owner's clipboard setting and notices (module documentation). Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct HostClipboard {
    setting: ClipboardSetting,
    notices: Notices,
}

impl HostClipboard {
    /// Starting at the configured `mode`.
    pub fn new(mode: ClipboardMode) -> Self {
        Self {
            setting: ClipboardSetting::new(mode),
            notices: Notices::default(),
        }
    }

    /// The live setting: every session reads it on each request.
    pub fn setting(&self) -> &ClipboardSetting {
        &self.setting
    }

    /// The queue the tray drains.
    pub fn notices(&self) -> &Notices {
        &self.notices
    }

    /// The observer that audits every request and queues the notices.
    pub fn observer(&self, audit: Arc<AuditLog>, names: DeviceNames) -> Arc<dyn ClipboardObserver> {
        Arc::new(Observer {
            audit,
            names,
            notices: self.notices.clone(),
            last: Mutex::new(HashMap::new()),
        })
    }
}

struct Observer {
    audit: Arc<AuditLog>,
    names: DeviceNames,
    notices: Notices,
    last: Mutex<HashMap<(DeviceId, &'static str), Instant>>,
}

impl std::fmt::Debug for Observer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Observer").finish_non_exhaustive()
    }
}

impl Observer {
    /// Whether a notice for this device and direction is due now (and marks it shown).
    fn due(&self, device: &DeviceId, direction: Direction, now: Instant) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let key = (device.clone(), direction.as_str());
        match last.get(&key) {
            Some(t) if now.saturating_duration_since(*t) < NOTICE_QUIET => false,
            _ => {
                last.insert(key, now);
                true
            }
        }
    }
}

impl ClipboardObserver for Observer {
    fn transfer(&self, event: &ClipboardEvent) {
        let mut audit = AuditEvent::new(kind::CLIPBOARD)
            .device(&event.device)
            .detail("session", event.session.as_str())
            .detail("direction", event.direction.as_str());
        match &event.outcome {
            Ok(bytes) => audit = audit.detail("bytes", *bytes),
            Err(refusal) => {
                audit = audit
                    .outcome(Outcome::Refused)
                    .detail("reason", refusal.code());
            }
        }
        self.audit.record(audit);
        let Ok(bytes) = event.outcome else {
            return;
        };
        if !self.due(&event.device, event.direction, Instant::now()) {
            return;
        }
        let who = (self.names)(&event.device);
        let (title, what) = match event.direction {
            Direction::ToPhone => (
                format!("{who} copied text from this computer"),
                "The phone read the clipboard",
            ),
            Direction::FromPhone => (
                format!("{who} put text on this computer's clipboard"),
                "The phone replaced the clipboard",
            ),
        };
        self.notices.push(Notice {
            title,
            body: format!(
                "{what} ({bytes} bytes of text). Settings > Streaming turns clipboard sharing off."
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use gander_core::clipboard::ClipboardRefusal;
    use gander_core::input::PauseCause;
    use gander_core::protocol::SessionId;

    use super::*;

    fn rig() -> (HostClipboard, Arc<AuditLog>, Arc<dyn ClipboardObserver>) {
        let hc = HostClipboard::new(ClipboardMode::Both);
        let audit = Arc::new(AuditLog::memory());
        let names: DeviceNames = Arc::new(|_| "Pixel 9".to_owned());
        let observer = hc.observer(Arc::clone(&audit), names);
        (hc, audit, observer)
    }

    fn event(direction: Direction, outcome: Result<usize, ClipboardRefusal>) -> ClipboardEvent {
        ClipboardEvent {
            session: SessionId::from("ses_1"),
            device: DeviceId::from("dev_1"),
            direction,
            outcome,
        }
    }

    #[test]
    fn the_setting_starts_at_the_configured_mode_and_is_shared() {
        assert_eq!(
            HostClipboard::new(ClipboardMode::ToPhone).setting().get(),
            ClipboardMode::ToPhone
        );
        assert_eq!(
            HostClipboard::default().setting().get(),
            ClipboardMode::Off,
            "off unless the owner says otherwise"
        );
        let hc = HostClipboard::new(ClipboardMode::Off);
        hc.clone().setting().set(ClipboardMode::Both);
        assert_eq!(hc.setting().get(), ClipboardMode::Both);
    }

    #[test]
    fn a_read_is_audited_with_direction_and_size_and_notified() {
        let (hc, audit, observer) = rig();
        observer.transfer(&event(Direction::ToPhone, Ok(1234)));
        let events = audit.recent(10);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.event, kind::CLIPBOARD);
        assert_eq!(e.outcome, Outcome::Ok);
        assert_eq!(e.device_id, Some(DeviceId::from("dev_1")));
        assert_eq!(e.detail["direction"], "to_phone");
        assert_eq!(e.detail["bytes"], 1234);
        assert_eq!(e.detail["session"], "ses_1");
        assert!(!e.detail.contains_key("reason"));
        let notices = hc.notices().take();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Pixel 9 copied text from this computer");
        assert!(
            notices[0].body.contains("1234 bytes"),
            "{}",
            notices[0].body
        );
        assert!(hc.notices().take().is_empty(), "taken once");
    }

    #[test]
    fn a_write_is_audited_and_notified_in_its_own_words() {
        let (hc, audit, observer) = rig();
        observer.transfer(&event(Direction::FromPhone, Ok(5)));
        assert_eq!(audit.recent(1)[0].detail["direction"], "from_phone");
        let notices = hc.notices().take();
        assert_eq!(
            notices[0].title,
            "Pixel 9 put text on this computer's clipboard"
        );
    }

    #[test]
    fn a_refusal_is_audited_with_its_reason_and_not_announced() {
        let (hc, audit, observer) = rig();
        observer.transfer(&event(Direction::ToPhone, Err(ClipboardRefusal::Disabled)));
        observer.transfer(&event(
            Direction::FromPhone,
            Err(ClipboardRefusal::Paused(PauseCause::Locked)),
        ));
        let events = audit.recent(10);
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.outcome == Outcome::Refused));
        assert_eq!(events[0].detail["reason"], "disabled");
        assert_eq!(events[1].detail["reason"], "host_locked");
        assert!(events.iter().all(|e| !e.detail.contains_key("bytes")));
        assert!(hc.notices().take().is_empty());
    }

    #[test]
    fn a_burst_is_one_notice_but_every_transfer_is_audited() {
        let (hc, audit, observer) = rig();
        for _ in 0..5 {
            observer.transfer(&event(Direction::ToPhone, Ok(10)));
        }
        assert_eq!(audit.recent(10).len(), 5);
        assert_eq!(hc.notices().take().len(), 1);
        // The other direction is its own notice.
        observer.transfer(&event(Direction::FromPhone, Ok(10)));
        assert_eq!(hc.notices().take().len(), 1);
    }

    #[test]
    fn the_quiet_period_ends() {
        let hc = HostClipboard::new(ClipboardMode::Both);
        let names: DeviceNames = Arc::new(|_| "Pixel 9".to_owned());
        let o = Observer {
            audit: Arc::new(AuditLog::memory()),
            names,
            notices: hc.notices().clone(),
            last: Mutex::new(HashMap::new()),
        };
        let d = DeviceId::from("dev_1");
        let t0 = Instant::now();
        assert!(o.due(&d, Direction::ToPhone, t0));
        assert!(!o.due(&d, Direction::ToPhone, t0 + Duration::from_secs(9)));
        assert!(o.due(&d, Direction::ToPhone, t0 + NOTICE_QUIET));
        assert!(
            o.due(&DeviceId::from("dev_2"), Direction::ToPhone, t0),
            "per device"
        );
    }

    #[test]
    fn the_queue_is_bounded() {
        let n = Notices::default();
        for i in 0..(MAX_NOTICES + 3) {
            n.push(Notice {
                title: i.to_string(),
                body: String::new(),
            });
        }
        let taken = n.take();
        assert_eq!(taken.len(), MAX_NOTICES);
        assert_eq!(taken[0].title, "3");
    }

    #[test]
    fn no_text_can_reach_the_audit_log() {
        // The event type has no field for it; this pins the audit detail keys.
        let (_, audit, observer) = rig();
        observer.transfer(&event(Direction::ToPhone, Ok(9)));
        let keys: Vec<_> = audit.recent(1)[0].detail.keys().cloned().collect();
        let mut keys = keys;
        keys.sort();
        assert_eq!(keys, ["bytes", "direction", "session"]);
    }
}
