// SPDX-License-Identifier: Apache-2.0
//! The clipboard text bridge (WP-56), the platform-free half: the setting, the message codec, the
//! caps, the policy and the service that applies them. The Windows clipboard itself is
//! `gander_win::clipboard`; the stream engine's data channel wiring is `gander_stream::input`.
//!
//! # What it is
//!
//! While a Stream session is running the owner can let the phone read the computer's clipboard
//! text ("Copy from computer") and put text on it ("Paste to computer"). It is **off by default**
//! (`[stream] clipboard = "off"`), **text only**, capped at [`MAX_TEXT_BYTES`] (64 KiB of UTF-8),
//! and refused unless the stream is as active as it must be for a click to be injected: the
//! session unlocked (fail closed), the phone looking, the app in front, nobody using the
//! computer's own keyboard or mouse ([`GateState::Active`]).
//!
//! # Messages (JSON, tagged by `t`, on the ordered `control` data channel only)
//!
//! | Direction | Message | Meaning |
//! | --- | --- | --- |
//! | phone -> host | `{"t":"clipboard.get"}` | Read the computer's clipboard text. |
//! | host -> phone | `{"t":"clipboard","s":"..."}` | The answer: the text, `\n` line ends. |
//! | phone -> host | `{"t":"clipboard","s":"..."}` | Put this text on the computer's clipboard. |
//! | host -> phone | `{"t":"clipboard.ack","bytes":<n>}` | The text (`n` UTF-8 bytes) is on the clipboard. |
//! | host -> phone | `{"t":"clipboard.refused","op":"get"\|"set","reason":<code>}` | Nothing was read or written. |
//!
//! Refusal codes ([`ClipboardRefusal::code`]): `disabled` (the owner's setting does not allow this
//! direction), `host_locked`, `phone_background`, `focus_lost`, `local_user_active` (the stream is
//! not active, the same codes as `input.refused`), `too_large` (over 64 KiB, in either
//! direction; nothing is cut off), `no_text` (empty, or the clipboard holds no text),
//! `invalid_text` (a NUL character), `busy` (another application kept the clipboard open) and
//! `unavailable`.
//!
//! The host advertises the setting as `capabilities.stream.clipboard` (`GET /v1/me`, omitted when
//! `off`); the phone shows its two actions only for the directions that are on.
//!
//! # Privacy rules the code keeps
//!
//! - The clipboard is never read or written outside [`ClipboardService::serve`], and `serve` checks
//!   the setting and the gate first.
//! - Contents are never logged and never audited: [`ClipboardEvent`] carries the direction and the
//!   size only.
//! - Every request, allowed or refused, produces one [`ClipboardEvent`] for the host's audit log
//!   and notifications.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::input::{GateState, PauseCause, Refusal};
use crate::protocol::{DeviceId, SessionId};

/// The most text one transfer may carry, in UTF-8 bytes (64 KiB).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The largest clipboard message on the wire. It is the SCTP data channel's own ceiling
/// (256 KiB), which the stream engine negotiates, so a 64 KiB text still fits when JSON escapes
/// make it several times longer.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// `[stream] clipboard`: what the owner allows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardMode {
    /// Nothing is shared (the default).
    #[default]
    Off,
    /// The phone may read the computer's clipboard text.
    ToPhone,
    /// The phone may put text on the computer's clipboard.
    FromPhone,
    /// Both.
    Both,
}

impl ClipboardMode {
    /// Every mode, in the order the settings list shows them.
    pub const ALL: [ClipboardMode; 4] = [
        ClipboardMode::Off,
        ClipboardMode::ToPhone,
        ClipboardMode::FromPhone,
        ClipboardMode::Both,
    ];

    /// Whether `direction` is allowed.
    pub const fn allows(self, direction: Direction) -> bool {
        matches!(
            (self, direction),
            (ClipboardMode::Both, _)
                | (ClipboardMode::ToPhone, Direction::ToPhone)
                | (ClipboardMode::FromPhone, Direction::FromPhone)
        )
    }

    /// `true` for [`ClipboardMode::Off`] (serde skips the field then).
    pub const fn is_off(&self) -> bool {
        matches!(self, ClipboardMode::Off)
    }

    /// The config and protocol spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            ClipboardMode::Off => "off",
            ClipboardMode::ToPhone => "to_phone",
            ClipboardMode::FromPhone => "from_phone",
            ClipboardMode::Both => "both",
        }
    }

    /// The settings window's wording.
    pub const fn label(self) -> &'static str {
        match self {
            ClipboardMode::Off => "Off",
            ClipboardMode::ToPhone => "Computer to phone",
            ClipboardMode::FromPhone => "Phone to computer",
            ClipboardMode::Both => "Both ways",
        }
    }

    const fn to_u8(self) -> u8 {
        match self {
            ClipboardMode::Off => 0,
            ClipboardMode::ToPhone => 1,
            ClipboardMode::FromPhone => 2,
            ClipboardMode::Both => 3,
        }
    }

    const fn from_u8(v: u8) -> Self {
        match v {
            1 => ClipboardMode::ToPhone,
            2 => ClipboardMode::FromPhone,
            3 => ClipboardMode::Both,
            // Anything else is `Off`: fail closed.
            _ => ClipboardMode::Off,
        }
    }
}

/// The owner's setting as a live value: the settings window writes it, every running session
/// reads it on each request, so turning clipboard sharing off takes effect at once. Cheap to
/// clone; all clones share one value.
#[derive(Debug, Clone, Default)]
pub struct ClipboardSetting(Arc<AtomicU8>);

impl ClipboardSetting {
    /// A setting starting at `mode`.
    pub fn new(mode: ClipboardMode) -> Self {
        Self(Arc::new(AtomicU8::new(mode.to_u8())))
    }

    /// The current mode.
    pub fn get(&self) -> ClipboardMode {
        ClipboardMode::from_u8(self.0.load(Ordering::Acquire))
    }

    /// Changes the mode for every holder.
    pub fn set(&self, mode: ClipboardMode) {
        self.0.store(mode.to_u8(), Ordering::Release);
    }
}

/// Which way the text goes, from the computer's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// The phone reads the computer's clipboard (`clipboard.get`).
    ToPhone,
    /// The phone writes the computer's clipboard (`clipboard`).
    FromPhone,
}

impl Direction {
    /// `to_phone` or `from_phone`, as the audit log and the setting spell it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::ToPhone => "to_phone",
            Direction::FromPhone => "from_phone",
        }
    }

    /// The operation name in `clipboard.refused`.
    pub const fn op(self) -> Op {
        match self {
            Direction::ToPhone => Op::Get,
            Direction::FromPhone => Op::Set,
        }
    }
}

/// `op` of `clipboard.refused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// A `clipboard.get`.
    Get,
    /// A `clipboard` write.
    Set,
}

/// Why a transfer did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ClipboardRefusal {
    /// The owner's setting does not allow this direction.
    #[error("clipboard sharing does not allow this direction")]
    Disabled,
    /// The stream is not active (the input gate's cause).
    #[error("the stream is not active ({0:?})")]
    Paused(PauseCause),
    /// Over [`MAX_TEXT_BYTES`] (or too long for one message).
    #[error("the text is larger than {MAX_TEXT_BYTES} bytes")]
    TooLarge,
    /// Empty, or the clipboard holds no text.
    #[error("there is no text")]
    NoText,
    /// The text has a NUL character, which a Windows text clipboard cannot hold.
    #[error("the text has a NUL character")]
    InvalidText,
    /// Another application kept the clipboard open through every retry.
    #[error("the clipboard is in use by another application")]
    Busy,
    /// Any other failure of the clipboard.
    #[error("the clipboard is not available")]
    Unavailable,
}

impl ClipboardRefusal {
    /// The `reason` of `clipboard.refused`.
    pub fn code(&self) -> &'static str {
        match self {
            ClipboardRefusal::Disabled => "disabled",
            ClipboardRefusal::Paused(cause) => Refusal::Paused(*cause).code(),
            ClipboardRefusal::TooLarge => "too_large",
            ClipboardRefusal::NoText => "no_text",
            ClipboardRefusal::InvalidText => "invalid_text",
            ClipboardRefusal::Busy => "busy",
            ClipboardRefusal::Unavailable => "unavailable",
        }
    }
}

/// The text of a transfer is within the caps: not empty, at most [`MAX_TEXT_BYTES`], no NUL.
/// Returns its size in bytes.
pub fn check_text(text: &str) -> Result<usize, ClipboardRefusal> {
    if text.is_empty() {
        return Err(ClipboardRefusal::NoText);
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(ClipboardRefusal::TooLarge);
    }
    if text.contains('\0') {
        return Err(ClipboardRefusal::InvalidText);
    }
    Ok(text.len())
}

/// The policy: the owner's setting first, then the stream's activity. `Ok` means the
/// clipboard may be touched for `direction` now.
pub fn decide(
    mode: ClipboardMode,
    direction: Direction,
    gate: GateState,
) -> Result<(), ClipboardRefusal> {
    if !mode.allows(direction) {
        return Err(ClipboardRefusal::Disabled);
    }
    match gate {
        GateState::Active => Ok(()),
        GateState::Paused(cause) => Err(ClipboardRefusal::Paused(cause)),
    }
}

/// `text` with every line end as `\r\n`, the Windows clipboard's convention (lone `\n` and lone
/// `\r` included).
pub fn to_crlf(text: &str) -> Cow<'_, str> {
    let needs = text.char_indices().any(|(i, c)| match c {
        '\n' => !text[..i].ends_with('\r'),
        '\r' => !text[i + 1..].starts_with('\n'),
        _ => false,
    });
    if !needs {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

/// `text` with every line end as `\n` (what a phone expects).
pub fn to_lf(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// One clipboard request from the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `{"t":"clipboard.get"}`.
    Get,
    /// `{"t":"clipboard","s":...}`.
    Set(String),
}

impl Request {
    /// Decodes a `control` message if it is a clipboard request; `None` for everything else (a
    /// key, a ping, garbage, a clipboard message that is over [`MAX_MESSAGE_BYTES`]).
    pub fn parse(bytes: &[u8]) -> Option<Request> {
        #[derive(Deserialize)]
        #[serde(tag = "t")]
        enum Wire {
            #[serde(rename = "clipboard.get")]
            Get {},
            #[serde(rename = "clipboard")]
            Set { s: String },
        }
        if bytes.len() > MAX_MESSAGE_BYTES {
            return None;
        }
        match serde_json::from_slice::<Wire>(bytes).ok()? {
            Wire::Get {} => Some(Request::Get),
            Wire::Set { s } => Some(Request::Set(s)),
        }
    }

    /// The direction the request asks for.
    pub fn direction(&self) -> Direction {
        match self {
            Request::Get => Direction::ToPhone,
            Request::Set(_) => Direction::FromPhone,
        }
    }
}

/// A message from the host to the phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "t")]
pub enum Reply<'a> {
    /// The answer to `clipboard.get`.
    #[serde(rename = "clipboard")]
    Text {
        /// The text.
        s: &'a str,
    },
    /// A write went through.
    #[serde(rename = "clipboard.ack")]
    Ack {
        /// UTF-8 bytes stored.
        bytes: usize,
    },
    /// Nothing happened.
    #[serde(rename = "clipboard.refused")]
    Refused {
        /// The request's kind.
        op: Op,
        /// [`ClipboardRefusal::code`].
        reason: &'a str,
    },
}

impl Reply<'_> {
    /// The JSON text.
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The system clipboard, as the service sees it. Text only: `get_text` returns the text with
/// `\n` line ends, `set_text` takes the same. Implemented by `gander_win::clipboard` and, for
/// tests, by [`MemoryClipboard`]. Implementations may block for a short while (retries while
/// another application holds the clipboard) but never forever.
pub trait ClipboardBackend: Send + Sync + std::fmt::Debug {
    /// The clipboard's text, `None` when it holds none.
    fn get_text(&self) -> Result<Option<String>, BackendError>;
    /// Replaces the clipboard with `text`.
    fn set_text(&self, text: &str) -> Result<(), BackendError>;
}

/// Why the system clipboard could not be used. Never carries clipboard content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    /// Another application kept it open through every retry.
    #[error("the clipboard is in use by another application")]
    Busy,
    /// The text on it is more than [`MAX_TEXT_BYTES`] allow.
    #[error("the clipboard text is too large")]
    TooLarge,
    /// An OS failure (`GetLastError` code).
    #[error("the clipboard failed (error {0})")]
    Failed(u32),
}

impl From<BackendError> for ClipboardRefusal {
    fn from(e: BackendError) -> Self {
        match e {
            BackendError::Busy => ClipboardRefusal::Busy,
            BackendError::TooLarge => ClipboardRefusal::TooLarge,
            BackendError::Failed(_) => ClipboardRefusal::Unavailable,
        }
    }
}

/// What the host's audit log and notifications are told about one request. Direction and size
/// only: never the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardEvent {
    /// The stream session.
    pub session: SessionId,
    /// The phone.
    pub device: DeviceId,
    /// Which way.
    pub direction: Direction,
    /// UTF-8 bytes moved, or why nothing was.
    pub outcome: Result<usize, ClipboardRefusal>,
}

/// Receives one [`ClipboardEvent`] per request (the host: audit entry, toast). Called on the
/// stream's input thread: return quickly.
pub trait ClipboardObserver: Send + Sync + std::fmt::Debug {
    /// A request was served or refused.
    fn transfer(&self, event: &ClipboardEvent);
}

/// An observer that ignores everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoObserver;

impl ClipboardObserver for NoObserver {
    fn transfer(&self, _event: &ClipboardEvent) {}
}

/// An observer that keeps every event (tests and harnesses).
#[derive(Debug, Default)]
pub struct RecordingObserver {
    events: Mutex<Vec<ClipboardEvent>>,
}

impl RecordingObserver {
    /// Everything received so far.
    pub fn events(&self) -> Vec<ClipboardEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ClipboardObserver for RecordingObserver {
    fn transfer(&self, event: &ClipboardEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    }
}

/// An in-memory [`ClipboardBackend`] (tests and harnesses). `fail_next` makes the next calls
/// fail with the given error.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    text: Mutex<Option<String>>,
    failures: Mutex<Vec<BackendError>>,
    reads: std::sync::atomic::AtomicU32,
    writes: std::sync::atomic::AtomicU32,
}

impl MemoryClipboard {
    /// A clipboard holding `text` (none when `None`).
    pub fn with_text(text: Option<&str>) -> Self {
        let c = Self::default();
        *c.text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = text.map(str::to_owned);
        c
    }

    /// What is on it now.
    pub fn text(&self) -> Option<String> {
        self.text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Queues `error` for the next call (one per call, oldest first).
    pub fn fail_next(&self, error: BackendError) {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(error);
    }

    /// How many times `get_text` was called.
    pub fn reads(&self) -> u32 {
        self.reads.load(Ordering::Relaxed)
    }

    /// How many times `set_text` was called.
    pub fn writes(&self) -> u32 {
        self.writes.load(Ordering::Relaxed)
    }

    fn next_failure(&self) -> Option<BackendError> {
        let mut f = self
            .failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (!f.is_empty()).then(|| f.remove(0))
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn get_text(&self) -> Result<Option<String>, BackendError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if let Some(e) = self.next_failure() {
            return Err(e);
        }
        Ok(self.text())
    }

    fn set_text(&self, text: &str) -> Result<(), BackendError> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        if let Some(e) = self.next_failure() {
            return Err(e);
        }
        *self
            .text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(text.to_owned());
        Ok(())
    }
}

/// The clipboard bridge of a host: the live setting, the system clipboard and the observer. One
/// per host, shared by every session. Cheap to clone.
#[derive(Debug, Clone)]
pub struct ClipboardService {
    setting: ClipboardSetting,
    backend: Arc<dyn ClipboardBackend>,
    observer: Arc<dyn ClipboardObserver>,
}

impl ClipboardService {
    /// A service over `backend`, reading `setting` on every request.
    pub fn new(
        setting: ClipboardSetting,
        backend: Arc<dyn ClipboardBackend>,
        observer: Arc<dyn ClipboardObserver>,
    ) -> Self {
        Self {
            setting,
            backend,
            observer,
        }
    }

    /// The live setting.
    pub fn setting(&self) -> &ClipboardSetting {
        &self.setting
    }

    /// Serves one request of `device` in `session`, with the input gate as it is now, and
    /// returns the `control` message to send back. The order is fixed: the owner's setting, the
    /// stream's activity, the size caps, and only then the clipboard. The observer hears about
    /// every request, refused or not.
    pub fn serve(
        &self,
        session: &SessionId,
        device: &DeviceId,
        gate: GateState,
        request: &Request,
    ) -> String {
        let direction = request.direction();
        let (reply, outcome) = match self.run(direction, gate, request) {
            Ok((reply, bytes)) => (reply, Ok(bytes)),
            Err(refusal) => (
                Reply::Refused {
                    op: direction.op(),
                    reason: refusal.code(),
                }
                .to_text(),
                Err(refusal),
            ),
        };
        self.observer.transfer(&ClipboardEvent {
            session: session.clone(),
            device: device.clone(),
            direction,
            outcome,
        });
        reply
    }

    fn run(
        &self,
        direction: Direction,
        gate: GateState,
        request: &Request,
    ) -> Result<(String, usize), ClipboardRefusal> {
        decide(self.setting.get(), direction, gate)?;
        match request {
            Request::Get => {
                let text = self.backend.get_text()?.ok_or(ClipboardRefusal::NoText)?;
                let text = to_lf(&text);
                let bytes = check_text(&text)?;
                let reply = Reply::Text { s: &text }.to_text();
                // JSON escapes can make a text that fits longer than one message may be.
                if reply.len() > MAX_MESSAGE_BYTES {
                    return Err(ClipboardRefusal::TooLarge);
                }
                Ok((reply, bytes))
            }
            Request::Set(text) => {
                let text = to_lf(text);
                let bytes = check_text(&text)?;
                self.backend.set_text(&text)?;
                Ok((Reply::Ack { bytes }.to_text(), bytes))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PauseCause;

    fn ids() -> (SessionId, DeviceId) {
        (SessionId::from("ses_1"), DeviceId::from("dev_1"))
    }

    fn service(
        mode: ClipboardMode,
        text: Option<&str>,
    ) -> (
        ClipboardService,
        Arc<MemoryClipboard>,
        Arc<RecordingObserver>,
    ) {
        let backend = Arc::new(MemoryClipboard::with_text(text));
        let observer = Arc::new(RecordingObserver::default());
        let svc = ClipboardService::new(
            ClipboardSetting::new(mode),
            backend.clone(),
            observer.clone(),
        );
        (svc, backend, observer)
    }

    #[test]
    fn modes_spell_as_config_and_allow_by_direction() {
        for (mode, text) in [
            (ClipboardMode::Off, r#""off""#),
            (ClipboardMode::ToPhone, r#""to_phone""#),
            (ClipboardMode::FromPhone, r#""from_phone""#),
            (ClipboardMode::Both, r#""both""#),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), text);
            assert_eq!(serde_json::from_str::<ClipboardMode>(text).unwrap(), mode);
            assert_eq!(format!("\"{}\"", mode.as_str()), text);
        }
        assert!(serde_json::from_str::<ClipboardMode>(r#""everything""#).is_err());
        assert_eq!(ClipboardMode::default(), ClipboardMode::Off);
        let allows =
            |m: ClipboardMode| (m.allows(Direction::ToPhone), m.allows(Direction::FromPhone));
        assert_eq!(allows(ClipboardMode::Off), (false, false));
        assert_eq!(allows(ClipboardMode::ToPhone), (true, false));
        assert_eq!(allows(ClipboardMode::FromPhone), (false, true));
        assert_eq!(allows(ClipboardMode::Both), (true, true));
    }

    #[test]
    fn the_setting_is_shared_and_unknown_values_fail_closed() {
        let a = ClipboardSetting::new(ClipboardMode::Off);
        let b = a.clone();
        b.set(ClipboardMode::Both);
        assert_eq!(a.get(), ClipboardMode::Both);
        for m in ClipboardMode::ALL {
            a.set(m);
            assert_eq!(b.get(), m);
        }
        assert_eq!(ClipboardMode::from_u8(200), ClipboardMode::Off);
    }

    #[test]
    fn requests_decode_and_everything_else_is_not_ours() {
        assert_eq!(
            Request::parse(br#"{"t":"clipboard.get"}"#),
            Some(Request::Get)
        );
        assert_eq!(
            Request::parse(br#"{"t":"clipboard","s":"hi\nthere"}"#),
            Some(Request::Set("hi\nthere".into()))
        );
        assert_eq!(Request::parse(br#"{"t":"clipboard"}"#), None, "no text");
        assert_eq!(Request::parse(br#"{"t":"clipboard","s":5}"#), None);
        assert_eq!(
            Request::parse(br#"{"t":"key","code":"KeyA","down":true}"#),
            None
        );
        assert_eq!(Request::parse(br#"{"t":"ping","seq":1}"#), None);
        assert_eq!(Request::parse(b"not json"), None);
        let mut big = br#"{"t":"clipboard","s":""#.to_vec();
        big.resize(MAX_MESSAGE_BYTES + 1, b'a');
        big.extend_from_slice(br#""}"#);
        assert_eq!(Request::parse(&big), None, "over the message cap");
    }

    #[test]
    fn replies_have_the_documented_shapes() {
        assert_eq!(
            Reply::Text { s: "a\"b\n" }.to_text(),
            r#"{"t":"clipboard","s":"a\"b\n"}"#
        );
        assert_eq!(
            Reply::Ack { bytes: 12 }.to_text(),
            r#"{"t":"clipboard.ack","bytes":12}"#
        );
        assert_eq!(
            Reply::Refused {
                op: Op::Set,
                reason: "too_large"
            }
            .to_text(),
            r#"{"t":"clipboard.refused","op":"set","reason":"too_large"}"#
        );
    }

    #[test]
    fn text_caps() {
        assert_eq!(check_text("hello"), Ok(5));
        assert_eq!(check_text(""), Err(ClipboardRefusal::NoText));
        assert_eq!(check_text("a\0b"), Err(ClipboardRefusal::InvalidText));
        let exactly = "a".repeat(MAX_TEXT_BYTES);
        assert_eq!(check_text(&exactly), Ok(MAX_TEXT_BYTES));
        let over = "a".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(check_text(&over), Err(ClipboardRefusal::TooLarge));
        // The cap counts UTF-8 bytes, not characters.
        let wide = "\u{20AC}".repeat(MAX_TEXT_BYTES / 3 + 1); // 3 bytes each
        assert!(wide.chars().count() < MAX_TEXT_BYTES);
        assert_eq!(check_text(&wide), Err(ClipboardRefusal::TooLarge));
        let wide_ok = "\u{20AC}".repeat(MAX_TEXT_BYTES / 3);
        assert_eq!(check_text(&wide_ok), Ok(MAX_TEXT_BYTES / 3 * 3));
    }

    #[test]
    fn line_ends_convert_both_ways() {
        assert_eq!(to_crlf("a\nb"), "a\r\nb");
        assert_eq!(to_crlf("a\r\nb"), "a\r\nb");
        assert_eq!(to_crlf("a\rb"), "a\r\nb");
        assert_eq!(to_crlf("a\n\nb\r\n"), "a\r\n\r\nb\r\n");
        assert!(matches!(to_crlf("plain"), Cow::Borrowed(_)));
        assert!(matches!(to_crlf("a\r\nb"), Cow::Borrowed(_)));
        assert_eq!(to_lf("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert!(matches!(to_lf("plain\n"), Cow::Borrowed(_)));
        assert_eq!(to_lf(&to_crlf("x\ny\n")), "x\ny\n");
    }

    #[test]
    fn the_policy_checks_the_setting_before_the_gate() {
        let paused = GateState::Paused(PauseCause::Locked);
        assert_eq!(
            decide(ClipboardMode::Off, Direction::ToPhone, GateState::Active),
            Err(ClipboardRefusal::Disabled)
        );
        assert_eq!(
            decide(ClipboardMode::Off, Direction::ToPhone, paused),
            Err(ClipboardRefusal::Disabled),
            "a disabled bridge says nothing about the stream"
        );
        assert_eq!(
            decide(
                ClipboardMode::ToPhone,
                Direction::FromPhone,
                GateState::Active
            ),
            Err(ClipboardRefusal::Disabled)
        );
        assert_eq!(
            decide(ClipboardMode::Both, Direction::ToPhone, GateState::Active),
            Ok(())
        );
        for cause in [
            PauseCause::Locked,
            PauseCause::PhoneBackground,
            PauseCause::FocusLost,
            PauseCause::LocalUserActive,
        ] {
            assert_eq!(
                decide(
                    ClipboardMode::Both,
                    Direction::FromPhone,
                    GateState::Paused(cause)
                ),
                Err(ClipboardRefusal::Paused(cause))
            );
        }
    }

    #[test]
    fn refusal_codes_are_the_documented_strings() {
        let codes = [
            (ClipboardRefusal::Disabled, "disabled"),
            (ClipboardRefusal::Paused(PauseCause::Locked), "host_locked"),
            (
                ClipboardRefusal::Paused(PauseCause::PhoneBackground),
                "phone_background",
            ),
            (
                ClipboardRefusal::Paused(PauseCause::FocusLost),
                "focus_lost",
            ),
            (
                ClipboardRefusal::Paused(PauseCause::LocalUserActive),
                "local_user_active",
            ),
            (ClipboardRefusal::TooLarge, "too_large"),
            (ClipboardRefusal::NoText, "no_text"),
            (ClipboardRefusal::InvalidText, "invalid_text"),
            (ClipboardRefusal::Busy, "busy"),
            (ClipboardRefusal::Unavailable, "unavailable"),
        ];
        for (refusal, code) in codes {
            assert_eq!(refusal.code(), code);
        }
        assert_eq!(
            ClipboardRefusal::from(BackendError::Busy),
            ClipboardRefusal::Busy
        );
        assert_eq!(
            ClipboardRefusal::from(BackendError::Failed(5)),
            ClipboardRefusal::Unavailable
        );
    }

    #[test]
    fn set_then_get_round_trips_with_line_ends() {
        let (svc, backend, observer) = service(ClipboardMode::Both, None);
        let (s, d) = ids();
        let set = svc.serve(
            &s,
            &d,
            GateState::Active,
            &Request::Set("one\r\ntwo".into()),
        );
        assert_eq!(set, r#"{"t":"clipboard.ack","bytes":7}"#);
        assert_eq!(backend.text().as_deref(), Some("one\ntwo"));
        let got = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert_eq!(got, r#"{"t":"clipboard","s":"one\ntwo"}"#);
        assert_eq!(
            observer.events(),
            vec![
                ClipboardEvent {
                    session: s.clone(),
                    device: d.clone(),
                    direction: Direction::FromPhone,
                    outcome: Ok(7)
                },
                ClipboardEvent {
                    session: s,
                    device: d,
                    direction: Direction::ToPhone,
                    outcome: Ok(7)
                },
            ]
        );
    }

    #[test]
    fn a_refused_request_never_touches_the_clipboard() {
        let (s, d) = ids();
        // Off.
        let (svc, backend, observer) = service(ClipboardMode::Off, Some("secret"));
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert_eq!(
            reply,
            r#"{"t":"clipboard.refused","op":"get","reason":"disabled"}"#
        );
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Set("x".into()));
        assert_eq!(
            reply,
            r#"{"t":"clipboard.refused","op":"set","reason":"disabled"}"#
        );
        assert_eq!((backend.reads(), backend.writes()), (0, 0));
        assert_eq!(observer.events().len(), 2, "refusals are reported too");
        assert_eq!(
            observer.events()[0].outcome,
            Err(ClipboardRefusal::Disabled)
        );

        // One direction only.
        let (svc, backend, _) = service(ClipboardMode::ToPhone, Some("copy me"));
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Set("x".into()));
        assert!(reply.contains(r#""reason":"disabled""#), "{reply}");
        assert_eq!(backend.text().as_deref(), Some("copy me"));
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert!(
            reply.starts_with(r#"{"t":"clipboard","s":"copy me"#),
            "{reply}"
        );

        // Every way the stream can be inactive, including the lock.
        for cause in [
            PauseCause::Locked,
            PauseCause::PhoneBackground,
            PauseCause::FocusLost,
            PauseCause::LocalUserActive,
        ] {
            let (svc, backend, _) = service(ClipboardMode::Both, Some("secret"));
            let get = svc.serve(&s, &d, GateState::Paused(cause), &Request::Get);
            let set = svc.serve(&s, &d, GateState::Paused(cause), &Request::Set("x".into()));
            let code = ClipboardRefusal::Paused(cause).code();
            assert!(get.contains(&format!(r#""reason":"{code}""#)), "{get}");
            assert!(set.contains(&format!(r#""reason":"{code}""#)), "{set}");
            assert!(!get.contains("secret"));
            assert_eq!((backend.reads(), backend.writes()), (0, 0), "{cause:?}");
        }
    }

    #[test]
    fn the_setting_is_read_on_every_request() {
        let (svc, backend, _) = service(ClipboardMode::Both, Some("text"));
        let (s, d) = ids();
        assert!(
            svc.serve(&s, &d, GateState::Active, &Request::Get)
                .contains("text")
        );
        svc.setting().set(ClipboardMode::Off);
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert!(reply.contains("disabled"), "{reply}");
        assert_eq!(backend.reads(), 1);
    }

    #[test]
    fn size_caps_apply_in_both_directions_and_nothing_is_cut() {
        let (svc, backend, observer) = service(ClipboardMode::Both, None);
        let (s, d) = ids();
        let big = "a".repeat(MAX_TEXT_BYTES + 1);
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Set(big.clone()));
        assert_eq!(
            reply,
            r#"{"t":"clipboard.refused","op":"set","reason":"too_large"}"#
        );
        assert_eq!(backend.writes(), 0);

        // Exactly the cap goes through.
        let edge = "a".repeat(MAX_TEXT_BYTES);
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Set(edge.clone()));
        assert_eq!(
            reply,
            format!(r#"{{"t":"clipboard.ack","bytes":{MAX_TEXT_BYTES}}}"#)
        );
        assert_eq!(backend.text(), Some(edge));

        // A host clipboard over the cap is refused whole, not truncated.
        *backend.text.lock().unwrap() = Some(big);
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert_eq!(
            reply,
            r#"{"t":"clipboard.refused","op":"get","reason":"too_large"}"#
        );
        assert_eq!(
            observer.events().last().unwrap().outcome,
            Err(ClipboardRefusal::TooLarge)
        );

        // A text within the cap whose JSON escapes would not fit one message.
        *backend.text.lock().unwrap() = Some("\u{1}".repeat(MAX_TEXT_BYTES)); // 6 bytes each
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert!(
            reply.contains("too_large"),
            "{}",
            &reply[..60.min(reply.len())]
        );
    }

    #[test]
    fn empty_nul_and_missing_text_are_refused() {
        let (svc, backend, _) = service(ClipboardMode::Both, None);
        let (s, d) = ids();
        let nothing = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert!(nothing.contains(r#""reason":"no_text""#), "{nothing}");
        let empty = svc.serve(&s, &d, GateState::Active, &Request::Set(String::new()));
        assert!(empty.contains(r#""reason":"no_text""#), "{empty}");
        let nul = svc.serve(&s, &d, GateState::Active, &Request::Set("a\0b".into()));
        assert!(nul.contains(r#""reason":"invalid_text""#), "{nul}");
        assert_eq!(backend.writes(), 0);
    }

    #[test]
    fn backend_failures_become_refusals() {
        let (svc, backend, observer) = service(ClipboardMode::Both, Some("x"));
        let (s, d) = ids();
        backend.fail_next(BackendError::Busy);
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert!(reply.contains(r#""reason":"busy""#), "{reply}");
        backend.fail_next(BackendError::Failed(1418));
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Set("y".into()));
        assert!(reply.contains(r#""reason":"unavailable""#), "{reply}");
        backend.fail_next(BackendError::TooLarge);
        let reply = svc.serve(&s, &d, GateState::Active, &Request::Get);
        assert!(reply.contains(r#""reason":"too_large""#), "{reply}");
        assert_eq!(backend.text().as_deref(), Some("x"));
        assert_eq!(
            observer
                .events()
                .iter()
                .map(|e| e.outcome)
                .collect::<Vec<_>>(),
            vec![
                Err(ClipboardRefusal::Busy),
                Err(ClipboardRefusal::Unavailable),
                Err(ClipboardRefusal::TooLarge)
            ]
        );
    }

    #[test]
    fn events_and_replies_for_refusals_carry_no_content() {
        let (svc, _, observer) = service(ClipboardMode::Off, Some("top secret"));
        let (s, d) = ids();
        let reply = svc.serve(
            &s,
            &d,
            GateState::Active,
            &Request::Set("top secret".into()),
        );
        assert!(!reply.contains("secret"));
        assert!(!format!("{:?}", observer.events()).contains("secret"));
    }
}
