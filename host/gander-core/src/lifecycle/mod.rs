// SPDX-License-Identifier: Apache-2.0
//! The Stream session lifecycle (WP-53; `docs/specs/2026-11-01-stream-launch-policy.md`,
//! "Host-state preconditions" and "Session lifecycle"). Pure logic, no clock of its own: every
//! call takes `now`, so the tests drive time by hand.
//!
//! ```text
//! requested -> approving? -> launching -> window_found | pick -> streaming <-> paused -> ended
//! ```
//!
//! - [`preconditions`]: what `POST /v1/stream` answers before a session exists (`host_locked`,
//!   `stream_busy`, `host_unattended`) and which host prompt, if any, comes first.
//! - [`Machine`]: one session's state, the deadlines that end it (prompt, pick, negotiation,
//!   the 120 s resume grace) and the `stream.state` each step reports.
//! - [`close_action`]: `close_on_end` and `DELETE ?close_app=` at the end of a session.
//! - [`front_on_connect`]: whether the window is brought to the front when the phone connects.
//!
//! The engine (WP-50) reports its own `streaming` / `paused` changes to the phone; the machine
//! only follows them ([`Event::Engine`]) so it knows when the grace period runs. Everything the
//! engine does not report (approving, launching, pick, window_found, a lost peer connection, and
//! the final `ended`) is reported from the [`Step`]s returned here.

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use crate::config::{CloseOnEnd, IdlePromptDefault};
use crate::error::ErrorCode;
use crate::protocol::files::StreamDecision;
use crate::protocol::stream::{StreamReason, StreamState};

/// How long a session whose phone dropped off stays resumable (`stream.state paused network`).
pub const RESUME_GRACE: Duration = Duration::from_secs(120);
/// How long the owner has to answer "Allow this app for streaming?" (spec step 5).
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the "someone is using this computer" toast waits before its default applies.
pub const ATTENDED_PROMPT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the phone has to pick a window when discovery was ambiguous.
pub const PICK_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a found window may wait for the phone's WebRTC answer (and a resumed session for
/// its new peer connection) before the session ends with `timeout`.
pub const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Lifecycle timing. [`Timing::default`] is the spec's; tests shorten it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// [`RESUME_GRACE`].
    pub resume_grace: Duration,
    /// [`APPROVAL_TIMEOUT`].
    pub approval: Duration,
    /// [`ATTENDED_PROMPT_TIMEOUT`].
    pub attended_prompt: Duration,
    /// [`PICK_TIMEOUT`].
    pub pick: Duration,
    /// [`NEGOTIATION_TIMEOUT`].
    pub negotiation: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            resume_grace: RESUME_GRACE,
            approval: APPROVAL_TIMEOUT,
            attended_prompt: ATTENDED_PROMPT_TIMEOUT,
            pick: PICK_TIMEOUT,
            negotiation: NEGOTIATION_TIMEOUT,
        }
    }
}

// ------------------------------------------------------------------------------ preconditions

/// What the host knows when `POST /v1/stream` arrives (after the launch policy passed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFacts {
    /// The session is locked, disconnected or of unknown state (fail closed).
    pub locked: bool,
    /// Someone used the keyboard or mouse within `[stream] idle_prompt_seconds`.
    pub attended: bool,
    /// Another session is active (not ended), including one in its resume grace.
    pub busy: bool,
}

/// Which host prompt comes before the launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptKind {
    /// "Allow this app for streaming? Allow once / Always / Deny" (policy outcome `ask`).
    Approval,
    /// "Pixel 9 wants to stream Deck.pptx. This will move the mouse. Allow / Not now" (someone
    /// is using the computer).
    Attended,
}

/// The prompt a new session starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prompt {
    /// Which one.
    pub kind: PromptKind,
    /// How long it waits.
    pub timeout: Duration,
    /// What an unanswered prompt means.
    pub on_timeout: TimeoutAnswer,
}

/// The answer an unanswered prompt gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutAnswer {
    /// Go ahead (attended prompt, `idle_prompt_default = "allow"`).
    Allow,
    /// End the session with `host_unattended`.
    Unattended,
}

/// Checks the host-state preconditions, in this order: locked (`412 host_locked`), another
/// session (`409 stream_busy`), an `ask` decision with nobody at the computer to answer it
/// (`412 host_unattended`). On success, returns the prompt the session starts with:
///
/// - `ask` + attended -> [`PromptKind::Approval`] (60 s; unanswered -> `host_unattended`);
/// - `allowed` + attended -> [`PromptKind::Attended`] (10 s; unanswered -> `idle_default`);
/// - `allowed` + nobody there -> no prompt (it is the owner's own device).
///
/// `outcome` must not be `refused` (the policy answered those already); a `refused` outcome is
/// treated as `stream_refused` all the same.
pub fn preconditions(
    facts: HostFacts,
    outcome: StreamDecision,
    idle_default: IdlePromptDefault,
    timing: &Timing,
) -> Result<Option<Prompt>, ErrorCode> {
    if facts.locked {
        return Err(ErrorCode::HostLocked);
    }
    if facts.busy {
        return Err(ErrorCode::StreamBusy);
    }
    match (outcome, facts.attended) {
        (StreamDecision::Refused, _) => Err(ErrorCode::StreamRefused),
        (StreamDecision::Ask, false) => Err(ErrorCode::HostUnattended),
        (StreamDecision::Ask, true) => Ok(Some(Prompt {
            kind: PromptKind::Approval,
            timeout: timing.approval,
            on_timeout: TimeoutAnswer::Unattended,
        })),
        (StreamDecision::Allowed, true) => Ok(Some(Prompt {
            kind: PromptKind::Attended,
            timeout: timing.attended_prompt,
            on_timeout: match idle_default {
                IdlePromptDefault::Allow => TimeoutAnswer::Allow,
                IdlePromptDefault::Deny => TimeoutAnswer::Unattended,
            },
        })),
        (StreamDecision::Allowed, false) => Ok(None),
    }
}

/// User-facing text for a precondition refusal (no paths, no host names).
pub fn precondition_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::HostLocked => {
            "The computer is locked. Unlock it to stream, or use Preview or Fetch."
        }
        ErrorCode::StreamBusy => "Another device is already streaming from this computer.",
        ErrorCode::HostUnattended => {
            "Nobody is at the computer to allow this app for streaming. Ask the owner to approve it there first."
        }
        ErrorCode::StreamRefused => "This file cannot be streamed.",
        _ => "Stream mode cannot start right now.",
    }
}

// ------------------------------------------------------------------------------ the machine

/// Why a session ended (the host side; [`EndCause::wire`] is what the phone sees).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndCause {
    /// `DELETE /v1/stream/{session}`.
    ClientClosed,
    /// The owner's kill switch (or a listener pause, which drops every session).
    KillSwitch,
    /// The app window closed on the host.
    WindowClosed,
    /// The device was revoked.
    Revoked,
    /// The session locked (fail closed).
    HostLocked,
    /// The owner answered Deny / Not now.
    Denied,
    /// Nobody answered the prompt (`host_unattended`).
    Unattended,
    /// The phone did not pick a window in time, or nothing appeared (`app_unavailable`).
    AppUnavailable,
    /// The resume grace or the negotiation ran out.
    Timeout,
    /// The launch, the engine or the source failed.
    Error,
}

impl EndCause {
    /// The `stream.state ended` reason.
    pub fn wire(self) -> StreamReason {
        match self {
            EndCause::ClientClosed => StreamReason::ClientClosed,
            EndCause::KillSwitch => StreamReason::HostKillSwitch,
            EndCause::WindowClosed => StreamReason::WindowClosed,
            EndCause::Revoked => StreamReason::Revoked,
            EndCause::Unattended | EndCause::AppUnavailable | EndCause::Timeout => {
                StreamReason::Timeout
            }
            EndCause::HostLocked | EndCause::Denied | EndCause::Error => StreamReason::Error,
        }
    }

    /// The `error` event sent before `stream.state ended`, if the reason alone does not say it.
    pub fn error(self) -> Option<(ErrorCode, &'static str)> {
        match self {
            EndCause::HostLocked => Some((
                ErrorCode::HostLocked,
                "The computer was locked; the stream ended.",
            )),
            EndCause::Denied => Some((
                ErrorCode::StreamRefused,
                "The person at the computer did not allow this stream.",
            )),
            EndCause::Unattended => Some((
                ErrorCode::HostUnattended,
                "Nobody at the computer answered the request.",
            )),
            EndCause::AppUnavailable => Some((
                ErrorCode::AppUnavailable,
                "The app did not show a window that could be streamed.",
            )),
            _ => None,
        }
    }

    /// Snake-case name for the audit log.
    pub fn as_str(self) -> &'static str {
        match self {
            EndCause::ClientClosed => "client_closed",
            EndCause::KillSwitch => "host_kill_switch",
            EndCause::WindowClosed => "window_closed",
            EndCause::Revoked => "revoked",
            EndCause::HostLocked => "host_locked",
            EndCause::Denied => "denied",
            EndCause::Unattended => "host_unattended",
            EndCause::AppUnavailable => "app_unavailable",
            EndCause::Timeout => "timeout",
            EndCause::Error => "error",
        }
    }
}

/// What the stream engine reports about its peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineReport {
    /// Offer sent, not connected yet.
    Negotiating,
    /// Media flowing.
    Streaming,
    /// Paused for this reason (`network` starts the resume grace).
    Paused(StreamReason),
    /// The peer connection failed or closed; the session is kept for the resume grace.
    NetworkLost,
}

/// What happens to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A host prompt was shown.
    PromptShown,
    /// The prompt was answered with Allow / Allow once / Always.
    Allowed,
    /// The prompt was answered with Deny / Not now.
    Denied,
    /// The app is being started.
    Launching,
    /// Discovery found the window.
    WindowFound,
    /// Discovery was ambiguous; the phone picks among these titles.
    Pick(Vec<String>),
    /// The phone picked a window (it was checked to still exist).
    Picked,
    /// The engine's state changed.
    Engine(EngineReport),
    /// `POST /v1/stream` with `resume`: a new peer connection on the same window.
    Resumed,
    /// Ends the session (DELETE, kill switch, revocation, lock, window closed, errors).
    End(EndCause),
}

/// The protocol state of a session plus the bookkeeping for its deadlines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    state: StreamState,
    reason: Option<StreamReason>,
    end: Option<EndCause>,
    prompt: Option<Prompt>,
    deadline: Option<Instant>,
    /// The engine session is gone (peer connection lost); only a resume brings media back.
    engine_lost: bool,
    timing: Timing,
}

/// The result of one event: what the phone must be told, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// State after the event.
    pub state: StreamState,
    /// Its reason (`paused`, `ended`).
    pub reason: Option<StreamReason>,
    /// Candidate titles (`pick`).
    pub candidates: Vec<String>,
    /// Report it as `stream.state` (the engine already reported `streaming` / `paused` changes
    /// it made itself, so those are `false`).
    pub report: bool,
    /// The session just ended with this cause.
    pub ended: Option<EndCause>,
}

/// An event the current state does not accept (for example a pick while streaming).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{event} is not accepted in state {state:?}")]
pub struct Rejected {
    /// The state.
    pub state: StreamState,
    /// The event, by name.
    pub event: &'static str,
}

impl Machine {
    /// A new session in `requested`. With `prompt`, the first event must be
    /// [`Event::PromptShown`].
    pub fn new(prompt: Option<Prompt>, timing: Timing) -> Self {
        Self {
            state: StreamState::Requested,
            reason: None,
            end: None,
            prompt,
            deadline: None,
            engine_lost: false,
            timing,
        }
    }

    /// State now.
    pub fn state(&self) -> StreamState {
        self.state
    }

    /// Reason now (`paused`, `ended`).
    pub fn reason(&self) -> Option<StreamReason> {
        self.reason
    }

    /// The prompt the session started with.
    pub fn prompt(&self) -> Option<Prompt> {
        self.prompt
    }

    /// Why it ended, once ended.
    pub fn end_cause(&self) -> Option<EndCause> {
        self.end
    }

    /// Ended.
    pub fn is_ended(&self) -> bool {
        self.end.is_some()
    }

    /// When the current state times out by itself, if it does.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// In the resume grace: paused for `network` (peer connection disconnected or lost).
    pub fn in_grace(&self) -> bool {
        self.state == StreamState::Paused && self.reason == Some(StreamReason::Network)
    }

    /// The engine session is gone and only a resume brings media back.
    pub fn engine_lost(&self) -> bool {
        self.engine_lost
    }

    /// A window exists for this session (a resume can re-offer on it).
    pub fn has_window(&self) -> bool {
        matches!(
            self.state,
            StreamState::WindowFound | StreamState::Streaming | StreamState::Paused
        ) && !self.is_ended()
    }

    /// Whether `POST /v1/stream { resume }` is accepted now: a window exists and the session
    /// has not ended. (A phone may resume while the host still believes it is streaming: its
    /// network changed before the host noticed.)
    pub fn can_resume(&self) -> bool {
        self.has_window()
    }

    fn step(&self, report: bool, candidates: Vec<String>) -> Step {
        Step {
            state: self.state,
            reason: self.reason,
            candidates,
            report,
            ended: self.end,
        }
    }

    fn set(&mut self, state: StreamState, reason: Option<StreamReason>, deadline: Option<Instant>) {
        self.state = state;
        self.reason = reason;
        self.deadline = deadline;
    }

    fn reject(&self, event: &'static str) -> Rejected {
        Rejected {
            state: self.state,
            event,
        }
    }

    fn finish(&mut self, cause: EndCause) -> Step {
        self.end = Some(cause);
        self.set(StreamState::Ended, Some(cause.wire()), None);
        self.step(true, Vec::new())
    }

    /// Applies `event` at `now`.
    pub fn apply(&mut self, event: Event, now: Instant) -> Result<Step, Rejected> {
        if self.is_ended() {
            return Err(self.reject("any event"));
        }
        use StreamState as S;
        match event {
            Event::End(cause) => Ok(self.finish(cause)),
            Event::PromptShown => {
                let Some(p) = self.prompt.filter(|_| self.state == S::Requested) else {
                    return Err(self.reject("prompt_shown"));
                };
                self.set(S::Approving, None, Some(now + p.timeout));
                Ok(self.step(true, Vec::new()))
            }
            Event::Allowed | Event::Denied if self.state != S::Approving => {
                Err(self.reject("prompt_answer"))
            }
            Event::Allowed => {
                // Launching follows at once; the answer itself is not a state.
                self.deadline = None;
                Ok(self.step(false, Vec::new()))
            }
            Event::Denied => Ok(self.finish(EndCause::Denied)),
            Event::Launching => match self.state {
                S::Requested if self.prompt.is_none() => {
                    self.set(S::Launching, None, None);
                    Ok(self.step(true, Vec::new()))
                }
                S::Approving if self.deadline.is_none() => {
                    self.set(S::Launching, None, None);
                    Ok(self.step(true, Vec::new()))
                }
                _ => Err(self.reject("launching")),
            },
            Event::WindowFound => match self.state {
                S::Launching | S::Pick => {
                    self.set(S::WindowFound, None, Some(now + self.timing.negotiation));
                    Ok(self.step(true, Vec::new()))
                }
                _ => Err(self.reject("window_found")),
            },
            Event::Pick(titles) => match self.state {
                S::Launching => {
                    self.set(S::Pick, None, Some(now + self.timing.pick));
                    Ok(self.step(true, titles))
                }
                _ => Err(self.reject("pick")),
            },
            Event::Picked => match self.state {
                S::Pick => {
                    self.set(S::WindowFound, None, Some(now + self.timing.negotiation));
                    Ok(self.step(true, Vec::new()))
                }
                _ => Err(self.reject("picked")),
            },
            Event::Resumed => {
                if !self.can_resume() {
                    return Err(self.reject("resume"));
                }
                self.engine_lost = false;
                self.set(S::WindowFound, None, Some(now + self.timing.negotiation));
                Ok(self.step(true, Vec::new()))
            }
            Event::Engine(report) => {
                if !self.has_window() {
                    return Err(self.reject("engine"));
                }
                Ok(self.on_engine(report, now))
            }
        }
    }

    fn on_engine(&mut self, report: EngineReport, now: Instant) -> Step {
        use StreamState as S;
        let was_grace = self.in_grace();
        let grace_until = if was_grace { self.deadline } else { None };
        match report {
            // A lost peer connection outranks anything the old engine session says afterwards.
            _ if self.engine_lost => self.step(false, Vec::new()),
            EngineReport::Negotiating => self.step(false, Vec::new()),
            EngineReport::Streaming => {
                self.set(S::Streaming, None, None);
                self.step(false, Vec::new())
            }
            EngineReport::Paused(StreamReason::Network) => {
                // The grace period starts at the first network pause and is not extended by
                // more of them.
                let until = grace_until.unwrap_or(now + self.timing.resume_grace);
                self.set(S::Paused, Some(StreamReason::Network), Some(until));
                self.step(false, Vec::new())
            }
            EngineReport::Paused(reason) => {
                self.set(S::Paused, Some(reason), None);
                self.step(false, Vec::new())
            }
            EngineReport::NetworkLost => {
                let until = grace_until.unwrap_or(now + self.timing.resume_grace);
                self.engine_lost = true;
                self.set(S::Paused, Some(StreamReason::Network), Some(until));
                // The engine session ended without telling the phone; the host says it.
                self.step(true, Vec::new())
            }
        }
    }

    /// Ends the session if its deadline passed: prompt (its timeout answer), pick
    /// (`app_unavailable`), negotiation or grace (`timeout`). An attended prompt whose default
    /// is "allow" is not an end: it returns [`Event::Allowed`]'s step and the caller launches.
    pub fn tick(&mut self, now: Instant) -> Option<Step> {
        let deadline = self.deadline?;
        if now < deadline || self.is_ended() {
            return None;
        }
        use StreamState as S;
        Some(match self.state {
            S::Approving => match self.prompt.map(|p| p.on_timeout) {
                Some(TimeoutAnswer::Allow) => {
                    self.deadline = None;
                    self.step(false, Vec::new())
                }
                _ => self.finish(EndCause::Unattended),
            },
            S::Pick => self.finish(EndCause::AppUnavailable),
            _ => self.finish(EndCause::Timeout),
        })
    }
}

// ------------------------------------------------------------------------------ close_on_end

/// What to do with the app when a session ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseAction {
    /// Leave it open.
    Leave,
    /// Send `WM_CLOSE` (never `TerminateProcess`).
    Close,
}

/// The facts [`close_action`] needs about the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseFacts {
    /// The catalogue's `close_on_end`.
    pub policy: CloseOnEnd,
    /// Why the session ended.
    pub cause: EndCause,
    /// `DELETE ?close_app=true`.
    pub close_app: bool,
    /// The window belongs to the process tree Gander started for this session.
    pub launched_by_us: bool,
    /// The window is the only visible top-level window of its process.
    pub only_window: bool,
}

/// `close_on_end` (launch-policy spec) and `close_app` (protocol spec):
///
/// - never close an app Gander did not start for this session, or one with other windows open
///   (the protocol's condition for `close_app`, and the spec's for `if_launched_by_us`);
/// - nothing to close when the window closed itself;
/// - `never`: leave it;
/// - `if_launched_by_us`: close at any end;
/// - `ask`: the phone decides at the end, so only `DELETE ?close_app=true` closes.
pub fn close_action(f: CloseFacts) -> CloseAction {
    if !f.launched_by_us || !f.only_window || f.cause == EndCause::WindowClosed {
        return CloseAction::Leave;
    }
    match f.policy {
        CloseOnEnd::Never => CloseAction::Leave,
        CloseOnEnd::IfLaunchedByUs => CloseAction::Close,
        CloseOnEnd::Ask if f.cause == EndCause::ClientClosed && f.close_app => CloseAction::Close,
        CloseOnEnd::Ask => CloseAction::Leave,
    }
}

// ------------------------------------------------------------------------------ foreground

/// The facts [`front_on_connect`] needs, read when the engine session starts (first start and
/// every resume).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrontFacts {
    /// The session is locked, disconnected or of unknown state (fail closed).
    pub locked: bool,
    /// Someone used the keyboard or mouse recently (`host.state attended`).
    pub attended: bool,
    /// The owner explicitly allowed this session at the host (answered the approval or the
    /// attended prompt with Allow / Always; an unanswered prompt does not count).
    pub owner_allowed: bool,
}

/// Whether the host brings the streamed window to the front once when the phone connects
/// (launch-policy spec, "Launch and window discovery" step 7). Done once per engine session (the
/// first start and each resume); after that only the phone's explicit `{"t":"front"}` does it.
///
/// - Never while locked: nothing may take the foreground behind the lock screen.
/// - Nobody at the computer: yes; it is the owner's own device and the window is useless for
///   input until it is in front.
/// - Someone at the computer: only when the owner allowed this session on the host's prompt,
///   so the window never jumps in front of a local user who did not agree to it. The phone can
///   still ask with "Bring to front".
pub fn front_on_connect(f: FrontFacts) -> bool {
    !f.locked && (!f.attended || f.owner_allowed)
}
