// SPDX-License-Identifier: Apache-2.0
//! Application-level liveness and pause over the `control` data channel (condition (c) of the
//! D12 acceptance).
//!
//! # Why
//!
//! WP-44 measured that the transport alone does not notice a phone that stopped looking: with
//! the app backgrounded or the screen off the host kept encoding for 20 s, and a killed app was
//! noticed only after 29.95 s (ICE consent). The phone therefore says so itself.
//!
//! # Channels
//!
//! The host creates two data channels before the offer (protocol spec, "Stream"):
//!
//! - `input`: unordered, `maxRetransmits: 0`. Pointer messages (`gander_core::protocol::input`).
//!   The engine passes every message to the session's input handler (WP-52).
//! - `control`: ordered, reliable. The liveness and pause messages below, plus key, text and
//!   clipboard messages, which the engine passes to the input handler like `input` (the clipboard
//!   ones are up to 256 KiB, [`crate::peer::MAX_DATA_CHANNEL_MESSAGE`]; every other message is
//!   small).
//!
//! # Messages on `control` (JSON text, tagged by `t`, like the input messages)
//!
//! | Direction | Message | Meaning |
//! | --- | --- | --- |
//! | phone -> host | `{"t":"ping","seq":<u64>}` | Liveness, every 1 s while the stream is on screen. The host answers `pong` with the same `seq`. |
//! | host -> phone | `{"t":"pong","seq":<u64>}` | Answer; the phone may use the round trip as a latency probe. |
//! | phone -> host | `{"t":"pause"}` | The stream is not visible (app backgrounded, screen off). The host stops capturing and encoding at once and reports `stream.state paused` with reason `phone_background`. |
//! | phone -> host | `{"t":"resume"}` | Visible again. The host resumes and sends an IDR first. |
//! | phone -> host | `{"t":"keyframe"}` | Ask for an IDR (PLI is the normal path; this one survives a broken RTCP path). |
//! | host -> phone | `{"t":"state","state":"streaming"\|"paused","reason":<reason or null>}` | What the host is doing, mirrored from `stream.state` so the phone needs no `/events` round trip. Reasons as in `stream.state` (`phone_background`, `network`; WP-52 adds the input-only `focus_lost` and `local_user_active`, during which video keeps flowing). |
//! | phone -> host | `{"t":"front"}` | WP-52 "Bring to front": the host runs the foreground procedure ([`crate::input`]). |
//! | host -> phone | `{"t":"front.result","acquired":<bool>}` | WP-52: whether the window is now in front. |
//! | host -> phone | `{"t":"input.refused","kind":<t of the message>,"reason":<code>}` | WP-52: one input message was not injected (`covered`, `outside_picture`, `wrong_channel`, `shortcut_refused`, ...; `gander_core::input::Refusal::code`). Not sent for pauses, which `state` already explains. |
//! | phone -> host | `{"t":"clipboard.get"}` | WP-56: read the computer's clipboard text. Served only when the owner turned `[stream] clipboard` on for that direction, the stream is as active as input needs it to be, and the text fits 64 KiB ([`gander_core::clipboard`]). |
//! | host -> phone | `{"t":"clipboard","s":<text>}` | WP-56: the answer to `clipboard.get`. |
//! | phone -> host | `{"t":"clipboard","s":<text>}` | WP-56: put this text (at most 64 KiB of UTF-8) on the computer's clipboard. |
//! | host -> phone | `{"t":"clipboard.ack","bytes":<n>}` | WP-56: the written text is on the clipboard. |
//! | host -> phone | `{"t":"clipboard.refused","op":"get"\|"set","reason":<code>}` | WP-56: nothing was read or written (`disabled`, `host_locked`, `focus_lost`, `too_large`, `no_text`, `busy`, ...; [`gander_core::clipboard::ClipboardRefusal::code`]). |
//! | phone -> host | `{"t":"mute","on":<bool>}` | WP-55: stop (`true`) or restart (`false`) the window's sound. The host stops capturing at once; unmuted is the default. |
//! | host -> phone | `{"t":"audio","on":<bool>,"reason":<code or null>}` | WP-55: whether the window's sound is being sent now, and why not: `muted` (the phone muted it), `paused` (the stream is paused or not connected yet), `unavailable` (no audio this session: process loopback could not start; there is no system-wide fallback), `none` (this host streams no audio). Sent on every change, the first time when the stream starts. |
//!
//! Keys and text go on `control` (ordered, reliable); the injector refuses them on `input`
//! (WP-52 decision, see [`crate::input`]).
//!
//! Scroll sign (`{"t":"scroll","dx":..,"dy":..}`, wheel-delta units, 120 per notch): the Windows
//! wheel convention (Conductor decision, 2026-11-02). Positive `dy` means the wheel turned away
//! from the user, so the content scrolls UP (`+WHEEL_DELTA`); positive `dx` means a tilt to the
//! right. The host passes both through unchanged (`gander_core::input::mapping::wheel_data`).
//!
//! Liveness rule: once the host has received the first `ping`, a silence longer than
//! [`crate::EngineConfig::liveness_timeout`] (default 5 s) pauses the stream with reason
//! `network` (encoding stops); any later message on `control` resumes it with an IDR. A phone that
//! never pings is not paused by this rule (older clients), only by ICE.
//!
//! Unknown `t` values are not errors: they are passed to the input handler (compatibility rule).

use serde::{Deserialize, Serialize};

/// Liveness and pause messages on the `control` channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Phone -> host: still watching.
    Ping {
        /// Echoed in the `pong`.
        seq: u64,
    },
    /// Host -> phone: answer to `ping`.
    Pong {
        /// The `seq` of the ping.
        seq: u64,
    },
    /// Phone -> host: stop encoding, the stream is not visible.
    Pause,
    /// Phone -> host: visible again.
    Resume,
    /// Phone -> host: send an IDR.
    Keyframe,
    /// Phone -> host (WP-55): mute or unmute the window's sound.
    Mute {
        /// Muted.
        on: bool,
    },
    /// Host -> phone (WP-55): the sound is (not) being sent, and why not.
    Audio {
        /// Sound is captured and sent now.
        on: bool,
        /// Why not: `muted`, `paused`, `unavailable` or `none`.
        reason: Option<String>,
    },
    /// Host -> phone: the stream's state.
    State {
        /// `streaming` or `paused`.
        state: String,
        /// Why, for `paused`.
        reason: Option<String>,
    },
}

impl ControlMessage {
    /// Parses a control message; `None` for anything else (an input message, garbage).
    pub fn parse(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }

    /// The JSON text.
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Label of the unordered, lossy input channel.
pub const INPUT_LABEL: &str = "input";
/// Label of the ordered, reliable control channel.
pub const CONTROL_LABEL: &str = "control";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes() {
        assert_eq!(
            ControlMessage::Ping { seq: 7 }.to_text(),
            r#"{"t":"ping","seq":7}"#
        );
        assert_eq!(ControlMessage::Pause.to_text(), r#"{"t":"pause"}"#);
        assert_eq!(
            ControlMessage::State {
                state: "paused".into(),
                reason: Some("network".into())
            }
            .to_text(),
            r#"{"t":"state","state":"paused","reason":"network"}"#
        );
        assert_eq!(
            ControlMessage::parse(br#"{"t":"resume"}"#),
            Some(ControlMessage::Resume)
        );
        assert_eq!(ControlMessage::parse(br#"{"t":"key","code":"KeyA"}"#), None);
        assert_eq!(
            ControlMessage::parse(br#"{"t":"mute","on":true}"#),
            Some(ControlMessage::Mute { on: true })
        );
        assert_eq!(
            ControlMessage::Audio {
                on: false,
                reason: Some("muted".into())
            }
            .to_text(),
            r#"{"t":"audio","on":false,"reason":"muted"}"#
        );
    }
}
