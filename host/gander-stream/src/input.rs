// SPDX-License-Identifier: Apache-2.0
//! Input wiring (WP-52): the session's data channels -> [`gander_win::input::InputController`]
//! -> `SendInput`, and the controller's reports -> the session's state and the phone.
//!
//! ```no_run
//! # async fn demo(session: gander_stream::Session, hwnd: u64, pid: u32) -> std::io::Result<()> {
//! use gander_win::input::{InputOptions, InputTarget, query_lock_probe};
//! // WP-53, right after `StreamEngine::start_with` (the target comes from WP-51's WindowTarget):
//! let link = gander_stream::input::attach(
//!     &session,
//!     InputTarget::new(hwnd, pid),
//!     query_lock_probe(),
//!     InputOptions::default(),
//!     true, // bring the window to the front once the phone connects
//! )?;
//! // Keep `link` for the session's lifetime; it also stops by itself when the session ends.
//! # drop(link); Ok(()) }
//! ```
//!
//! # Channels: keys go on `control` (decision)
//!
//! `input` is unordered with `maxRetransmits: 0`: right for pointer moves, wrong for keys (a lost
//! key-up leaves a key held on the host, a reordered pair types the wrong thing). Keys and text
//! therefore travel on the ordered, reliable `control` channel, as the protocol spec already
//! lists them there; there is no sequencing layer. The injector refuses `key` and `text` that
//! arrive on `input` (`wrong_channel`). Pointer messages are accepted on both; a client that
//! wants press and release to be reliable sends `down`/`up` on `control` and only `move` on
//! `input`.
//!
//! # What the phone sees
//!
//! - Pauses: `stream.state paused` with `focus_lost` or `local_user_active` (and the mirrored
//!   `{"t":"state",...}` on `control`), through [`crate::Session::set_input_paused`]. Video keeps
//!   flowing during an input pause.
//! - One message not injected for any other reason: `{"t":"input.refused","kind":"down",
//!   "reason":"covered"}` on `control` ([`gander_core::input::Refusal::code`]; consecutive
//!   identical refusals once). Another application's pid or class is never sent.
//! - "Bring to front": the phone sends `{"t":"front"}` on `control`; the host runs the
//!   foreground procedure and answers `{"t":"front.result","acquired":true|false}`.
//!
//! # Bring to front when the phone connects
//!
//! With `front_on_connect` (the lifecycle decides it: `gander_core::lifecycle::front_on_connect`),
//! the worker runs the foreground procedure once, the first time the session is connected
//! (streaming or input-paused, not negotiating), before it lets the gate leave its client pause,
//! so a window that comes to the front never shows `focus_lost`. Refused while locked. It is not
//! repeated: afterwards only the phone's `front` runs it again. The outcome is logged and read
//! through [`InputLink::start_front`]; nothing is sent on `control` (the phone did not ask, and
//! `stream.state` already says whether input is paused for `focus_lost`).
//!
//! # Clipboard text (WP-56)
//!
//! With a [`ClipboardLink`] (see [`attach_with_clipboard`]) the worker also serves the phone's
//! `clipboard.get` and `clipboard` messages on `control`, through
//! [`gander_core::clipboard::ClipboardService`]: the owner's setting, then the same gate input
//! passes (lock probe included, fail closed), then the 64 KiB caps, then the Windows clipboard.
//! The answer (`clipboard`, `clipboard.ack` or `clipboard.refused`) goes back on `control`;
//! nothing about the text is ever logged. Without a link those messages are refused like any
//! other message the injector cannot handle (`input.refused`, reason `unsupported`), and on the
//! lossy `input` channel they are refused as `wrong_channel`.
//!
//! # Threads
//!
//! The engine calls the input handler on a runtime thread, which must not block; `SendInput`
//! takes about 5 ms and the foreground procedure up to a second. The handler only queues the
//! bytes; one worker thread owns the controller, handles messages in arrival order, polls the
//! gate every 100 ms in between, mirrors the session's own pauses (phone background, network)
//! into the gate, and exits when the session ends or the [`InputLink`] is dropped. Exiting
//! drops the controller, which releases any key or button still held.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use gander_core::clipboard::{ClipboardService, Request as ClipboardRequest};
use gander_core::input::{ChannelKind, GateState, InputSink, MessageKind, PauseCause, Refusal};
use gander_win::input::{InputController, InputOptions, InputTarget, LockProbe};

use crate::engine::{Channel, PauseReason, Session, SessionState};

/// How often the worker re-reads the gate when no message arrives.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The control message that asks for "Bring to front".
pub const FRONT: &str = "front";

/// Reports from the controller into the session.
struct SessionSink {
    session: Session,
}

impl InputSink for SessionSink {
    fn state_changed(&self, state: GateState) {
        let reason = match state {
            GateState::Paused(PauseCause::FocusLost) => Some(PauseReason::FocusLost),
            GateState::Paused(PauseCause::LocalUserActive) => Some(PauseReason::LocalUserActive),
            // Lock: the engine's lock guard ends the session. Phone background: the engine
            // already reports it.
            GateState::Active | GateState::Paused(_) => None,
        };
        self.session.set_input_paused(reason);
    }

    fn refused(&self, kind: MessageKind, refusal: &Refusal) {
        tracing::debug!(kind = kind.wire(), reason = %refusal, "input not injected");
        if matches!(refusal, Refusal::Paused(_)) {
            return; // `stream.state` already says why.
        }
        self.session
            .send_control(refused_text(kind, refusal.code()));
    }
}

/// `{"t":"input.refused","kind":..,"reason":..}`.
pub fn refused_text(kind: MessageKind, reason: &str) -> String {
    serde_json::json!({ "t": "input.refused", "kind": kind.wire(), "reason": reason }).to_string()
}

/// `{"t":"front.result","acquired":..}`.
pub fn front_result_text(acquired: bool) -> String {
    serde_json::json!({ "t": "front.result", "acquired": acquired }).to_string()
}

/// `true` when `bytes` is the `{"t":"front"}` request.
pub fn is_front_request(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.get("t").and_then(|t| t.as_str()).map(|t| t == FRONT))
        .unwrap_or(false)
}

enum Job {
    Message(Channel, Vec<u8>),
    Stop,
}

/// What the one foreground attempt at connect did ([`InputLink::start_front`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFront {
    /// The window is in front (it already was, or a step of the procedure worked).
    Acquired,
    /// The procedure ran and every step was refused (foreground lock), or the window is gone.
    NotAcquired,
    /// Not attempted: the session is locked.
    Locked,
}

/// Where the clipboard bridge reads the stream's activity: tests and harnesses replace the real
/// input gate with a fixed answer.
pub type GateProbe = Arc<dyn Fn() -> GateState + Send + Sync>;

/// The clipboard bridge of a session (module documentation).
#[derive(Clone)]
pub struct ClipboardLink {
    service: ClipboardService,
    gate: Option<GateProbe>,
}

impl std::fmt::Debug for ClipboardLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardLink")
            .field("service", &self.service)
            .field("gate_probe", &self.gate.is_some())
            .finish()
    }
}

impl ClipboardLink {
    /// A link that asks the session's input gate (the lock probe, the phone's pauses, focus, the
    /// local user) before every request.
    pub fn new(service: ClipboardService) -> Self {
        Self {
            service,
            gate: None,
        }
    }

    /// Replaces the input gate with `probe`. For tests and harnesses only: production code never
    /// calls it, so the clipboard is always gated like a click.
    #[must_use]
    pub fn with_gate_probe(mut self, probe: GateProbe) -> Self {
        self.gate = Some(probe);
        self
    }
}

/// The running link; dropping it stops the worker (and releases held keys and buttons).
#[derive(Debug)]
pub struct InputLink {
    tx: mpsc::Sender<Job>,
    thread: Option<JoinHandle<()>>,
    start_front: Arc<Mutex<Option<StartFront>>>,
}

impl InputLink {
    /// The outcome of the foreground attempt at connect; `None` while it has not run (not
    /// connected yet, or the link was attached without `front_on_connect`).
    pub fn start_front(&self) -> Option<StartFront> {
        *self.start_front.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for InputLink {
    fn drop(&mut self) {
        let _ = self.tx.send(Job::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The phone is connected: the first moment the start acquisition may run.
fn connected(state: &SessionState) -> bool {
    matches!(
        state,
        SessionState::Streaming
            | SessionState::Paused(PauseReason::FocusLost | PauseReason::LocalUserActive)
    )
}

fn client_paused(state: &SessionState) -> bool {
    matches!(
        state,
        SessionState::Paused(PauseReason::PhoneBackground | PauseReason::Network)
            | SessionState::Negotiating
    )
}

/// Installs the session's input handler and starts the worker (module documentation).
/// `front_on_connect`: run the foreground procedure once when the phone first connects.
pub fn attach(
    session: &Session,
    target: InputTarget,
    lock: LockProbe,
    options: InputOptions,
    front_on_connect: bool,
) -> std::io::Result<InputLink> {
    attach_with_clipboard(session, target, lock, options, None, front_on_connect)
}

/// [`attach`] that also serves the clipboard bridge when `clipboard` is given.
pub fn attach_with_clipboard(
    session: &Session,
    target: InputTarget,
    lock: LockProbe,
    options: InputOptions,
    clipboard: Option<ClipboardLink>,
    front_on_connect: bool,
) -> std::io::Result<InputLink> {
    let (tx, rx) = mpsc::channel::<Job>();
    let worker_session = session.clone();
    let start_front = Arc::new(Mutex::new(None));
    let worker_front = Arc::clone(&start_front);
    let thread = std::thread::Builder::new()
        .name("gander-input".to_owned())
        .spawn(move || {
            let session = worker_session;
            let sink = Arc::new(SessionSink {
                session: session.clone(),
            });
            let mut controller = InputController::new(target, sink, lock, options);
            let mut mirrored = None;
            let mut pending_front = front_on_connect;
            loop {
                let state = session.state();
                if matches!(state, SessionState::Ended(_)) {
                    break;
                }
                if pending_front && connected(&state) {
                    pending_front = false;
                    let outcome = front_at_connect(&mut controller);
                    *worker_front.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
                }
                let paused = client_paused(&state);
                if mirrored != Some(paused) {
                    controller.set_client_paused(paused);
                    mirrored = Some(paused);
                }
                match rx.recv_timeout(POLL_INTERVAL) {
                    Ok(Job::Message(Channel::Control, bytes)) if is_front_request(&bytes) => {
                        let acquired = controller.bring_to_front().is_ok_and(|r| r.acquired);
                        session.send_control(front_result_text(acquired));
                    }
                    Ok(Job::Message(channel, bytes)) => {
                        let request = match (&clipboard, channel) {
                            (Some(_), Channel::Control) => ClipboardRequest::parse(&bytes),
                            _ => None,
                        };
                        if let (Some(link), Some(request)) = (&clipboard, request) {
                            // The same gate a click passes; the lock probe is part of it.
                            let gate = match &link.gate {
                                Some(probe) => probe(),
                                None => controller.poll(),
                            };
                            let reply =
                                link.service
                                    .serve(session.id(), session.device(), gate, &request);
                            // Direction only, never the text.
                            tracing::debug!(
                                direction = request.direction().as_str(),
                                "clipboard request served"
                            );
                            session.send_control(reply);
                            continue;
                        }
                        let kind = match channel {
                            Channel::Input => ChannelKind::Input,
                            Channel::Control => ChannelKind::Control,
                        };
                        let _ = controller.handle_bytes(kind, &bytes);
                    }
                    Ok(Job::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        controller.poll();
                    }
                }
            }
            drop(controller);
        })?;
    let handler_tx = tx.clone();
    session.set_input_handler(Arc::new(move |channel, bytes: &[u8]| {
        let _ = handler_tx.send(Job::Message(channel, bytes.to_vec()));
    }));
    Ok(InputLink {
        tx,
        thread: Some(thread),
        start_front,
    })
}

/// The one foreground attempt at connect; logs the outcome (never another app's details).
fn front_at_connect(controller: &mut InputController) -> StartFront {
    match controller.bring_to_front() {
        Ok(report) if report.acquired => {
            tracing::info!(method = ?report.method(), "streamed window brought to the front");
            StartFront::Acquired
        }
        Ok(report) => {
            tracing::info!(
                attempts = report.attempts.len(),
                "streamed window not brought to the front (foreground refused); the phone can ask"
            );
            StartFront::NotAcquired
        }
        Err(Refusal::Paused(PauseCause::Locked)) => {
            tracing::info!("session locked: the streamed window is not brought to the front");
            StartFront::Locked
        }
        Err(e) => {
            tracing::info!(reason = %e, "streamed window not brought to the front");
            StartFront::NotAcquired
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_texts() {
        assert_eq!(
            refused_text(MessageKind::Down, "covered"),
            r#"{"kind":"down","reason":"covered","t":"input.refused"}"#
        );
        assert_eq!(
            front_result_text(true),
            r#"{"acquired":true,"t":"front.result"}"#
        );
        assert!(is_front_request(br#"{"t":"front"}"#));
        assert!(!is_front_request(br#"{"t":"move","x":0,"y":0}"#));
        assert!(!is_front_request(b"garbage"));
    }

    #[test]
    fn engine_pauses_mirror_into_the_gate() {
        assert!(client_paused(&SessionState::Negotiating));
        assert!(client_paused(&SessionState::Paused(PauseReason::Network)));
        assert!(!client_paused(&SessionState::Paused(
            PauseReason::FocusLost
        )));
        assert!(!client_paused(&SessionState::Streaming));
    }

    #[test]
    fn start_front_waits_for_a_connected_phone() {
        assert!(!connected(&SessionState::Negotiating));
        assert!(!connected(&SessionState::Paused(PauseReason::Network)));
        assert!(!connected(&SessionState::Paused(
            PauseReason::PhoneBackground
        )));
        assert!(connected(&SessionState::Streaming));
        assert!(connected(&SessionState::Paused(PauseReason::FocusLost)));
    }
}
