// SPDX-License-Identifier: Apache-2.0
//! The boundary between the stream engine's data channels and the injector.
//!
//! The engine (WP-50) owns the WebRTC data channels. For every message it calls the injector
//! with the raw bytes and the channel they came on ([`ChannelKind`]); the injector decodes
//! ([`decode`]), checks the channel ([`channel_allows`]), and reports back through an
//! [`InputSink`] the engine and lifecycle (WP-53) implement:
//!
//! - [`InputSink::state_changed`]: the [`GateState`] changed; turn it into `stream.state`
//!   ([`stream_state_event`]) and pause or resume whatever the lifecycle pauses.
//! - [`InputSink::refused`]: one message was not injected, with the [`Refusal`]. Consecutive
//!   identical refusals are reported once (a covered window would otherwise report every pointer
//!   move). Tell the phone in words; do not forward another application's pid or class.

use std::sync::Mutex;

use crate::protocol::SessionId;
use crate::protocol::events::{StreamStateEvent, StreamWindow};
use crate::protocol::input::InputMessage;

use super::gate::{GateState, PauseCause};
use super::keys::{KeyRefusal, TextRefusal};
use super::mapping::MapError;

/// Which data channel a message arrived on (protocol v1, "Stream").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    /// `input`: unordered, `maxRetransmits: 0`; pointer moves (and any pointer message).
    Input,
    /// `control`: ordered, reliable; keys, text, clipboard, and any pointer message.
    Control,
}

/// Largest accepted message (a `text` of [`super::keys::MAX_TEXT_UNITS`] fits with room).
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// Why bytes did not decode to an [`InputMessage`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// Over [`MAX_MESSAGE_BYTES`].
    #[error("message of {0} bytes is too large")]
    TooLarge(usize),
    /// Not a known input message.
    #[error("not an input message: {0}")]
    Invalid(String),
}

/// Decodes one data-channel message.
pub fn decode(bytes: &[u8]) -> Result<InputMessage, DecodeError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::TooLarge(bytes.len()));
    }
    serde_json::from_slice(bytes).map_err(|e| DecodeError::Invalid(e.to_string()))
}

/// Keys and text must come on the ordered, reliable `control` channel: on the lossy `input`
/// channel a lost key-up would leave a key held down on the host. Pointer messages are accepted
/// on both.
pub fn channel_allows(kind: ChannelKind, message: &InputMessage) -> bool {
    match message {
        InputMessage::Move { .. }
        | InputMessage::Down { .. }
        | InputMessage::Up { .. }
        | InputMessage::Scroll { .. } => true,
        InputMessage::Key { .. }
        | InputMessage::Text { .. }
        | InputMessage::ClipboardGet {}
        | InputMessage::Clipboard { .. } => kind == ChannelKind::Control,
    }
}

/// The kind of a message, for reports (carries no content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// `move`
    Move,
    /// `down`
    Down,
    /// `up`
    Up,
    /// `scroll`
    Scroll,
    /// `key`
    Key,
    /// `text`
    Text,
    /// `clipboard.get` / `clipboard`
    Clipboard,
    /// Bytes that did not decode.
    Undecodable,
}

impl MessageKind {
    /// The kind of `m`.
    pub fn of(m: &InputMessage) -> Self {
        match m {
            InputMessage::Move { .. } => MessageKind::Move,
            InputMessage::Down { .. } => MessageKind::Down,
            InputMessage::Up { .. } => MessageKind::Up,
            InputMessage::Scroll { .. } => MessageKind::Scroll,
            InputMessage::Key { .. } => MessageKind::Key,
            InputMessage::Text { .. } => MessageKind::Text,
            InputMessage::ClipboardGet {} | InputMessage::Clipboard { .. } => {
                MessageKind::Clipboard
            }
        }
    }
}

/// Why one message was not injected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refusal {
    /// The gate is paused (lock, client pause, focus, local user).
    #[error("input is paused ({0:?})")]
    Paused(PauseCause),
    /// Another application's window is on top at the point (pre-click check). For local logs
    /// only: do not send the pid or class to the phone.
    #[error("another window covers the point (pid {pid}, class {class})")]
    Covered {
        /// The covering window's process.
        pid: u32,
        /// Its window class.
        class: String,
    },
    /// No window under the point (off every monitor).
    #[error("no window under the point")]
    NoWindowUnderPoint,
    /// The captured window is closed, or its handle now belongs to another process.
    #[error("the window is gone")]
    WindowGone,
    /// The application is not responding; nothing is sent to it.
    #[error("the app is not responding")]
    NotResponding,
    /// The pointer position could not be mapped.
    #[error(transparent)]
    Map(#[from] MapError),
    /// The key is unknown or a desktop shortcut.
    #[error(transparent)]
    Key(#[from] KeyRefusal),
    /// The text is not injectable.
    #[error(transparent)]
    Text(#[from] TextRefusal),
    /// A key or text message on the lossy `input` channel.
    #[error("keys and text must use the control channel")]
    WrongChannel,
    /// Not injectable: a clipboard message (served by `gander_core::clipboard` before the
    /// injector sees it, so it only arrives here on a link without the bridge) or a message the
    /// phone never sends.
    #[error("not an injectable message")]
    Unsupported,
    /// The bytes were not an input message.
    #[error(transparent)]
    Decode(#[from] DecodeError),
    /// `SendInput` accepted fewer events than given (blocked by UIPI, a secure desktop, or
    /// another thread's input). `error` is `GetLastError`.
    #[error("SendInput accepted {accepted} of {expected} events (error {error})")]
    Blocked {
        /// Events accepted.
        accepted: u32,
        /// Events given.
        expected: u32,
        /// `GetLastError`.
        error: u32,
    },
}

impl Refusal {
    /// A short code for the phone (`input.refused` on the `control` channel). Carries no
    /// detail about other applications.
    pub fn code(&self) -> &'static str {
        use super::keys::KeyRefusal as K;
        match self {
            Refusal::Paused(c) => match c {
                PauseCause::Locked => "host_locked",
                PauseCause::PhoneBackground => "phone_background",
                PauseCause::FocusLost => "focus_lost",
                PauseCause::LocalUserActive => "local_user_active",
            },
            Refusal::Covered { .. } => "covered",
            Refusal::NoWindowUnderPoint => "no_window",
            Refusal::WindowGone => "window_gone",
            Refusal::NotResponding => "not_responding",
            Refusal::Map(MapError::OutsidePicture) => "outside_picture",
            Refusal::Map(MapError::NotFinite) => "bad_coordinate",
            Refusal::Map(MapError::EmptyWindow) => "window_hidden",
            Refusal::Key(K::Unknown(_)) => "unknown_key",
            Refusal::Key(K::Shortcut(_)) => "shortcut_refused",
            Refusal::Text(_) => "text_too_long",
            Refusal::WrongChannel => "wrong_channel",
            Refusal::Unsupported => "unsupported",
            Refusal::Decode(_) => "undecodable",
            Refusal::Blocked { .. } => "blocked",
        }
    }
}

impl MessageKind {
    /// The message's `t` value (`undecodable` for bytes that did not decode).
    pub fn wire(self) -> &'static str {
        match self {
            MessageKind::Move => "move",
            MessageKind::Down => "down",
            MessageKind::Up => "up",
            MessageKind::Scroll => "scroll",
            MessageKind::Key => "key",
            MessageKind::Text => "text",
            MessageKind::Clipboard => "clipboard",
            MessageKind::Undecodable => "undecodable",
        }
    }
}

/// Where the injector reports. Implemented by the stream engine / lifecycle; calls come from the
/// thread that handles the message or polls the gate and must return quickly.
pub trait InputSink: Send + Sync {
    /// The gate changed state (also called once when the injector starts).
    fn state_changed(&self, state: GateState);
    /// One message was not injected.
    fn refused(&self, kind: MessageKind, refusal: &Refusal);
}

/// The `stream.state` event for a gate state, `None` when protocol v1 has no reason for it
/// ([`PauseCause::Locked`]: the lifecycle ends the session instead, `412 host_locked` for new
/// ones).
pub fn stream_state_event(
    session: SessionId,
    state: GateState,
    window: Option<StreamWindow>,
) -> Option<StreamStateEvent> {
    if state == GateState::Paused(PauseCause::Locked) {
        return None;
    }
    Some(StreamStateEvent {
        session,
        state: state.stream_state(),
        reason: state.stream_reason(),
        window,
        candidates: Vec::new(),
    })
}

/// One report a [`RecordingSink`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkRecord {
    /// [`InputSink::state_changed`].
    State(GateState),
    /// [`InputSink::refused`].
    Refused(MessageKind, Refusal),
}

/// An [`InputSink`] that keeps everything (tests and harnesses).
#[derive(Debug, Default)]
pub struct RecordingSink {
    records: Mutex<Vec<SinkRecord>>,
}

impl RecordingSink {
    /// Everything received so far.
    pub fn records(&self) -> Vec<SinkRecord> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The state changes received so far.
    pub fn states(&self) -> Vec<GateState> {
        self.records()
            .into_iter()
            .filter_map(|r| match r {
                SinkRecord::State(s) => Some(s),
                SinkRecord::Refused(..) => None,
            })
            .collect()
    }

    /// The refusals received so far.
    pub fn refusals(&self) -> Vec<(MessageKind, Refusal)> {
        self.records()
            .into_iter()
            .filter_map(|r| match r {
                SinkRecord::Refused(k, why) => Some((k, why)),
                SinkRecord::State(_) => None,
            })
            .collect()
    }

    fn push(&self, r: SinkRecord) {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(r);
    }
}

impl InputSink for RecordingSink {
    fn state_changed(&self, state: GateState) {
        self.push(SinkRecord::State(state));
    }

    fn refused(&self, kind: MessageKind, refusal: &Refusal) {
        self.push(SinkRecord::Refused(kind, refusal.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::input::{Modifier, MouseButton};
    use crate::protocol::stream::{StreamReason, StreamState};

    #[test]
    fn decodes_the_protocol_examples() {
        assert_eq!(
            decode(br#"{ "t": "move", "x": 0.4321, "y": 0.7788 }"#),
            Ok(InputMessage::Move {
                x: 0.4321,
                y: 0.7788
            })
        );
        assert_eq!(
            decode(br#"{ "t": "down", "b": "left", "x": 0.5, "y": 0.25 }"#),
            Ok(InputMessage::Down {
                b: MouseButton::Left,
                x: 0.5,
                y: 0.25
            })
        );
        assert_eq!(
            decode(br#"{ "t": "key", "code": "KeyA", "down": true, "mods": ["ctrl"] }"#),
            Ok(InputMessage::Key {
                code: "KeyA".into(),
                down: true,
                mods: vec![Modifier::Ctrl]
            })
        );
        assert!(matches!(
            decode(br#"{ "t": "launch", "cmd": "calc" }"#),
            Err(DecodeError::Invalid(_))
        ));
        assert!(matches!(decode(b"\xff"), Err(DecodeError::Invalid(_))));
        let big = vec![b' '; MAX_MESSAGE_BYTES + 1];
        assert_eq!(
            decode(&big),
            Err(DecodeError::TooLarge(MAX_MESSAGE_BYTES + 1))
        );
    }

    #[test]
    fn keys_only_on_the_control_channel() {
        let key = InputMessage::Key {
            code: "KeyA".into(),
            down: true,
            mods: vec![],
        };
        let mv = InputMessage::Move { x: 0.0, y: 0.0 };
        assert!(!channel_allows(ChannelKind::Input, &key));
        assert!(channel_allows(ChannelKind::Control, &key));
        assert!(channel_allows(ChannelKind::Input, &mv));
        assert!(channel_allows(ChannelKind::Control, &mv));
        assert!(!channel_allows(
            ChannelKind::Input,
            &InputMessage::Text { s: "x".into() }
        ));
    }

    #[test]
    fn stream_state_events() {
        let id = SessionId::from("ses_1".to_owned());
        let e =
            stream_state_event(id.clone(), GateState::Paused(PauseCause::FocusLost), None).unwrap();
        assert_eq!(e.state, StreamState::Paused);
        assert_eq!(e.reason, Some(StreamReason::FocusLost));
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["reason"], "focus_lost");
        let e = stream_state_event(id.clone(), GateState::Active, None).unwrap();
        assert_eq!(e.state, StreamState::Streaming);
        assert_eq!(e.reason, None);
        assert!(stream_state_event(id, GateState::Paused(PauseCause::Locked), None).is_none());
    }

    #[test]
    fn recording_sink_keeps_order() {
        let s = RecordingSink::default();
        s.state_changed(GateState::Active);
        s.refused(MessageKind::Down, &Refusal::NoWindowUnderPoint);
        assert_eq!(s.states(), vec![GateState::Active]);
        assert_eq!(
            s.refusals(),
            vec![(MessageKind::Down, Refusal::NoWindowUnderPoint)]
        );
    }
}
