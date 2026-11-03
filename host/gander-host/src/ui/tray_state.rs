// SPDX-License-Identifier: Apache-2.0
//! The tray indicator: which state the host is in, its tooltip and its icon pixels.
//!
//! security.md principle 7: "Tray icon changes state while any device is connected and again
//! while streaming". The icons are the Gander lens (a goose head in a magnifying-glass ring, see
//! `brand/`) and differ in shape as well as colour, so the state is readable without telling green
//! from orange: a hollow grey lens (idle), a green lens with tinted glass (connected), a solid
//! orange lens with the goose cut out (streaming) and a grey lens with two bars (paused). The
//! pixels come from [`super::icons`].

use super::controller::HostStatus;

/// What the tray icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrayState {
    /// Nobody is connected.
    Idle,
    /// At least one paired device has a connection open.
    Connected,
    /// A device is streaming an app window (Phase 2).
    Streaming,
    /// The listener is paused (tray menu "Pause listener"): nothing is reachable.
    Paused,
}

impl TrayState {
    /// Every state, for tests and the smoke run.
    pub const ALL: [TrayState; 4] = [
        TrayState::Idle,
        TrayState::Connected,
        TrayState::Streaming,
        TrayState::Paused,
    ];

    /// The state for a status. A pause wins (nothing is served), then streaming, then connected.
    pub fn from_status(status: &HostStatus) -> Self {
        if status.paused_for.is_some() {
            Self::Paused
        } else if status.stream.is_some() {
            Self::Streaming
        } else if !status.connected.is_empty() {
            Self::Connected
        } else {
            Self::Idle
        }
    }

    /// Short name for logs and the smoke report.
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connected => "connected",
            Self::Streaming => "streaming",
            Self::Paused => "paused",
        }
    }

    /// The tooltip for a status in this state (Windows truncates tooltips at 127 characters).
    pub fn tooltip(self, status: &HostStatus) -> String {
        let text = match self {
            Self::Idle => "Gander: ready, no device connected".to_owned(),
            Self::Connected => {
                let names: Vec<&str> = status.connected.iter().map(|d| d.name.as_str()).collect();
                format!("Gander: connected to {}", names.join(", "))
            }
            Self::Streaming => match &status.stream {
                Some(s) => format!(
                    "Gander: streaming {} to {}",
                    s.viewing(&s.file_name),
                    s.device_name
                ),
                None => "Gander: streaming".to_owned(),
            },
            Self::Paused => {
                let mins = status
                    .paused_for
                    .map_or(0, |d| d.as_secs().div_ceil(60).max(1));
                format!("Gander: listener paused, {mins} min left")
            }
        };
        truncate_chars(&text, 120)
    }

    /// The icon as RGBA8 at the size the notification area wants right now (16, 20, 24 or 32 px,
    /// square).
    pub fn icon(self) -> super::icons::Pixels {
        super::icons::tray_icon(self, super::icons::tray_size())
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('\u{2026}');
    s
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gander_core::protocol::DeviceId;

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

    fn phone() -> ConnectedDevice {
        ConnectedDevice {
            id: DeviceId::from("dev_1"),
            name: "Pixel 9".into(),
        }
    }

    #[test]
    fn state_follows_the_status_with_pause_first() {
        let mut s = status();
        assert_eq!(TrayState::from_status(&s), TrayState::Idle);
        s.connected.push(phone());
        assert_eq!(TrayState::from_status(&s), TrayState::Connected);
        s.stream = Some(StreamSessionView {
            device_name: "Pixel 9".into(),
            file_name: "deck.pptx".into(),
            app_name: "PowerPoint".into(),
            audio: false,
        });
        assert_eq!(TrayState::from_status(&s), TrayState::Streaming);
        s.paused_for = Some(Duration::from_secs(300));
        assert_eq!(TrayState::from_status(&s), TrayState::Paused);
    }

    #[test]
    fn tooltips_name_the_device_and_stay_short() {
        let mut s = status();
        assert!(TrayState::Idle.tooltip(&s).contains("no device"));
        s.connected.push(phone());
        assert_eq!(
            TrayState::Connected.tooltip(&s),
            "Gander: connected to Pixel 9"
        );
        s.stream = Some(StreamSessionView {
            device_name: "Pixel 9".into(),
            file_name: "deck.pptx".into(),
            app_name: "PowerPoint".into(),
            audio: false,
        });
        assert_eq!(
            TrayState::Streaming.tooltip(&s),
            "Gander: streaming deck.pptx to Pixel 9"
        );
        if let Some(v) = s.stream.as_mut() {
            v.audio = true;
        }
        assert_eq!(
            TrayState::Streaming.tooltip(&s),
            "Gander: streaming deck.pptx with sound to Pixel 9"
        );
        s.paused_for = Some(Duration::from_secs(61));
        assert_eq!(
            TrayState::Paused.tooltip(&s),
            "Gander: listener paused, 2 min left"
        );
        s.connected = (0..40)
            .map(|i| ConnectedDevice {
                id: DeviceId::from(format!("dev_{i}")),
                name: format!("A very long device name {i}"),
            })
            .collect();
        assert!(TrayState::Connected.tooltip(&s).chars().count() <= 120);
    }
}
