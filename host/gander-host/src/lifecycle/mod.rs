// SPDX-License-Identifier: Apache-2.0
//! Stream session lifecycle and host visibility (WP-53): what turns `POST /v1/stream` into a
//! running session and ends it again.
//!
//! [`StreamSessions`] owns the one session a host may run (v1: one per host) and drives it
//! through the pure state machine in [`gander_core::lifecycle`]:
//!
//! 1. **Preconditions** (`POST`): locked -> `412 host_locked`; another session (including one in
//!    its resume grace) -> `409 stream_busy`; an `ask` decision with nobody at the computer ->
//!    `412 host_unattended`. Otherwise `201` with the session id; the rest is reported over
//!    `/v1/events`.
//! 2. **Prompts** (`stream.state approving`): an `ask` decision shows the owner "Allow once /
//!    Always / Deny" for 60 s (unanswered: `host_unattended`); an allowed request while someone
//!    is using the computer shows "Allow / Not now" for 10 s (unanswered: `[stream]
//!    idle_prompt_default`). Both are host-side toasts read through [`StreamSessions::prompt`]
//!    and answered with [`StreamSessions::answer`]; "Always" writes the catalogue through
//!    [`SessionDeps::approve_always`].
//! 3. **Launch** (`launching`, then `window_found` or `pick`): [`WindowPlatform::launch`]
//!    (WP-51) on the blocking pool; a pick is answered by the phone's `stream.pick` within 30 s
//!    (else `app_unavailable`).
//! 4. **Streaming**: the engine (WP-50) starts with the session's id, input (WP-52) is attached
//!    to the same window, the computer is kept awake ([`Awake`]), the tray shows the session
//!    ([`StreamSessions::view`]) and the audit log records the start. When the phone connects,
//!    the window is brought to the front once (launch-policy step 7) if
//!    [`gander_core::lifecycle::front_on_connect`] allows it: never while locked, and with
//!    someone at the computer only after the owner's Allow on the host. A resume does the same
//!    once more; otherwise only the phone's `{"t":"front"}` does it.
//! 5. **Resume grace**: when the engine pauses for `network` or its peer connection is lost
//!    (`EndReason::NetworkLost`, reported by the host as `stream.state paused network`), the
//!    session is kept for 120 s; `POST /v1/stream { resume }` re-offers on a new peer connection
//!    without relaunching the app.
//! 6. **End**: `DELETE` (`client_closed`), kill switch, revocation, lock (fail closed: also
//!    during the prompt, the launch and the grace), window closed, timeouts. The engine stops,
//!    input is detached, keep-awake is cleared, the window placement is restored, `close_on_end`
//!    / `close_app` decide whether the app gets `WM_CLOSE`, the phone gets `stream.state ended`
//!    (preceded by an `error` that says why when the reason alone does not), the audit log
//!    records the end, and the slot frees.
//!
//! The engine runs with `EngineConfig::owner_reports_end`, so every `ended` the phone sees comes
//! from here, exactly once.

#[cfg(windows)]
pub mod windows;

use std::any::Any;
use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gander_core::clipboard::{ClipboardMode, ClipboardSetting};
use gander_core::config::{AppCatalogue, CloseOnEnd, IdlePromptDefault};
use gander_core::error::{ErrorBody, ErrorCode};
use gander_core::launch::plan::Authorisation;
use gander_core::lifecycle::{
    CloseAction, CloseFacts, EndCause, EngineReport, Event, FrontFacts, HostFacts, Machine, Step,
    Timing, close_action, front_on_connect, precondition_message, preconditions,
};
use gander_core::policy::PolicyDecision;
use gander_core::protocol::events::{ClientEvent, ServerEvent, StreamStateEvent, StreamWindow};
use gander_core::protocol::files::StreamDecision;
use gander_core::protocol::host::{StreamCapabilities, VideoCodec};
use gander_core::protocol::stream::{AppRef, StreamCreated, StreamState, Viewport};
use gander_core::protocol::{AppId, DeviceId, SessionId, ShareId};
use gander_stream::{
    EndReason, LockSource, LockStatus, Session, SessionState, SignalSink, StartRequest,
    StreamEngine, WindowTarget,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::audit::{AuditEvent, AuditLog, Outcome, kind};
use crate::events::StreamSignalling;

pub use gander_core::lifecycle::PromptKind;

/// `capabilities.stream` of a host with the lifecycle installed.
pub fn stream_capabilities() -> StreamCapabilities {
    StreamCapabilities {
        enabled: true,
        codecs: vec![VideoCodec::H264],
        max_sessions: 1,
        audio: false,
        input: true,
        fit_to_phone: true,
        // The owner's setting, filled in by `StreamSessions::capabilities`.
        clipboard: ClipboardMode::Off,
    }
}

// ------------------------------------------------------------------------------ platform

/// Opaque platform data carried with a window (Windows: the WP-51 `WindowTarget` with its
/// saved placement).
pub type Native = Arc<dyn Any + Send + Sync>;

/// A window a session captures.
#[derive(Clone)]
pub struct FoundWindow {
    /// Handle.
    pub hwnd: u64,
    /// Owning process.
    pub pid: u32,
    /// Title.
    pub title: String,
    /// The window belongs to the process tree Gander started for this session.
    pub launched_by_us: bool,
    /// Client size after fit-to-phone and the monitor dpi, when known.
    pub size: Option<(u32, u32, u32)>,
    /// Platform data.
    pub native: Option<Native>,
}

impl fmt::Debug for FoundWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FoundWindow")
            .field("hwnd", &self.hwnd)
            .field("pid", &self.pid)
            .field("launched_by_us", &self.launched_by_us)
            .finish_non_exhaustive()
    }
}

impl FoundWindow {
    fn wire(&self) -> Option<StreamWindow> {
        self.size.map(|(width, height, dpi)| StreamWindow {
            title: self.title.clone(),
            width,
            height,
            dpi,
        })
    }
}

/// One candidate of an ambiguous discovery.
#[derive(Clone)]
pub struct PickCandidate {
    /// Its title (sent to the phone).
    pub title: String,
    /// Platform data.
    pub native: Native,
}

impl fmt::Debug for PickCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PickCandidate").finish_non_exhaustive()
    }
}

/// What a launch produced.
#[derive(Debug, Clone)]
pub enum LaunchOutcome {
    /// One window (fitted to the phone).
    Found(FoundWindow),
    /// The phone picks.
    Pick(Vec<PickCandidate>),
    /// Nothing appeared.
    TimedOut,
}

/// A launch.
#[derive(Debug, Clone)]
pub struct LaunchResult {
    /// What discovery found.
    pub outcome: LaunchOutcome,
    /// The process started (for "launched by us" after a pick).
    pub pid: u32,
    /// The catalogue's `close_on_end`.
    pub close_on_end: CloseOnEnd,
}

/// Everything a launch needs.
#[derive(Debug, Clone)]
pub struct LaunchJob {
    /// The policy decision.
    pub decision: PolicyDecision,
    /// The catalogue it was made with.
    pub catalogue: Arc<AppCatalogue>,
    /// The file's absolute path (from the resolver's handle).
    pub file: PathBuf,
    /// Policy alone, or the owner's "Allow once".
    pub authorisation: Authorisation,
    /// Fit the window to this.
    pub viewport: Viewport,
}

/// What the end of a session did to the window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FinishReport {
    /// The app received `WM_CLOSE` and its window went.
    pub closed: bool,
    /// The close was asked for but the app stayed open with a dialog ("save changes?").
    pub left_open_dialog: bool,
}

/// The window side of a session: WP-51 launch and placement, WP-52 input. All methods but
/// [`WindowPlatform::attach_input`] block and run on the blocking pool.
pub trait WindowPlatform: Send + Sync + fmt::Debug + 'static {
    /// Launches and discovers (blocks up to the discovery timeout).
    fn launch(&self, job: &LaunchJob) -> Result<LaunchResult, String>;
    /// The window for a picked candidate (checked to still exist), fitted to `viewport`.
    fn pick(
        &self,
        launched_pid: u32,
        candidate: &PickCandidate,
        viewport: Viewport,
    ) -> Result<FoundWindow, String>;
    /// Wires the session's data channels to input on `window`; the returned guard stops it.
    /// `front_on_connect`: bring the window to the front once when the phone connects (decided
    /// by [`gander_core::lifecycle::front_on_connect`]; the platform still refuses while locked).
    fn attach_input(
        &self,
        session: &Session,
        window: &FoundWindow,
        front_on_connect: bool,
    ) -> Option<Box<dyn Send>>;
    /// The window is the only visible top-level window of its process.
    fn is_only_window(&self, window: &FoundWindow) -> bool;
    /// Restores the window's placement, then sends `WM_CLOSE` when `close`.
    fn finish(&self, window: &FoundWindow, close: bool) -> FinishReport;
}

/// Keeps the computer awake while a session streams.
pub trait Awake: Send + Sync + fmt::Debug + 'static {
    /// Starts holding; dropping the guard clears it.
    fn hold(&self) -> Option<Box<dyn Send>>;
}

/// No keep-awake (tests, non-Windows).
#[derive(Debug, Default)]
pub struct NoAwake;

impl Awake for NoAwake {
    fn hold(&self) -> Option<Box<dyn Send>> {
        None
    }
}

// ------------------------------------------------------------------------------ views

/// A host prompt waiting for the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptView {
    /// Id for [`StreamSessions::answer`].
    pub id: u64,
    /// Approval (Allow once / Always / Deny) or attended (Allow / Not now).
    pub kind: PromptKind,
    /// The device asking.
    pub device_name: String,
    /// The file.
    pub file_name: String,
    /// The app (catalogue name, or the handler's file name).
    pub app_name: String,
    /// The handler's file name (`POWERPNT.EXE`).
    pub handler: String,
    /// The handler's signer, if signed.
    pub publisher: Option<String>,
    /// When the default answer applies.
    pub expires_at: Instant,
}

/// The owner's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptAnswer {
    /// Allow (attended prompt) or Allow once (approval).
    AllowOnce,
    /// Always: also approve the app (and extension) in the catalogue.
    Always,
    /// Deny / Not now.
    Deny,
}

/// The session as the tray shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// Session id.
    pub session: SessionId,
    /// The device's name.
    pub device_name: String,
    /// The file.
    pub file_name: String,
    /// The app.
    pub app_name: String,
    /// Protocol state now.
    pub state: StreamState,
    /// The window's sound is being sent now (WP-55): the being-viewed texts add "with sound".
    pub audio: bool,
}

impl SessionView {
    /// A window is open and captured (the tray's "streaming" state and the start toast).
    pub fn is_live(&self) -> bool {
        matches!(
            self.state,
            StreamState::WindowFound | StreamState::Streaming | StreamState::Paused
        )
    }
}

// ------------------------------------------------------------------------------ the manager

/// Settings.
#[derive(Debug, Clone)]
pub struct SessionsConfig {
    /// Timeouts and the resume grace.
    pub timing: Timing,
    /// Unanswered attended prompt (`[stream] idle_prompt_default`).
    pub idle_default: IdlePromptDefault,
    /// How long the end waits for an engine session to stop.
    pub stop_wait: Duration,
    /// The owner's live clipboard setting (WP-56), advertised as
    /// `capabilities.stream.clipboard`. `None`: this host has no clipboard bridge (nothing is
    /// advertised).
    pub clipboard: Option<ClipboardSetting>,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            timing: Timing::default(),
            idle_default: IdlePromptDefault::Allow,
            stop_wait: Duration::from_secs(3),
            clipboard: None,
        }
    }
}

/// Writes "Always" into the catalogue (and saves it): the decision and the extension.
pub type ApproveAlways = Arc<dyn Fn(&PolicyDecision) -> Result<(), String> + Send + Sync>;

/// Whether someone used the computer recently (`host.state attended`).
pub type AttendedProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// A device's display name.
pub type DeviceNames = Arc<dyn Fn(&DeviceId) -> String + Send + Sync>;

/// What [`StreamSessions`] is built from.
#[derive(Clone)]
pub struct SessionDeps {
    /// The engine, configured with `owner_reports_end`.
    pub engine: StreamEngine,
    /// Launch, placement, input.
    pub platform: Arc<dyn WindowPlatform>,
    /// The session lock (the engine's own source; fail closed).
    pub lock: Arc<dyn LockSource>,
    /// Someone at the computer.
    pub attended: AttendedProbe,
    /// Signalling to the phone (`stream.state`, `error`).
    pub signal: Arc<dyn SignalSink>,
    /// The audit log.
    pub audit: Arc<AuditLog>,
    /// Keep-awake.
    pub awake: Arc<dyn Awake>,
    /// Device names for the prompts and the tray.
    pub names: DeviceNames,
    /// "Always" (none: Always acts as Allow once).
    pub approve_always: Option<ApproveAlways>,
}

impl fmt::Debug for SessionDeps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionDeps").finish_non_exhaustive()
    }
}

/// A request that passed the launch policy.
#[derive(Debug, Clone)]
pub struct NewSession {
    /// The phone.
    pub device: DeviceId,
    /// Its address (audit).
    pub source: Option<IpAddr>,
    /// The policy decision (`allowed` or `ask`).
    pub decision: PolicyDecision,
    /// The catalogue the decision was made with.
    pub catalogue: Arc<AppCatalogue>,
    /// The file's absolute path.
    pub file: PathBuf,
    /// The file's name.
    pub file_name: String,
    /// The share.
    pub share: ShareId,
    /// Share-relative path (audit; the resume check).
    pub path: String,
    /// The phone's viewport.
    pub viewport: Viewport,
    /// `prefer.max_kbps`.
    pub max_kbps: Option<u32>,
}

enum Cmd {
    End {
        cause: EndCause,
        close_app: bool,
        done: oneshot::Sender<()>,
    },
    Resume {
        viewport: Viewport,
        max_kbps: Option<u32>,
        reply: oneshot::Sender<Result<StreamState, ErrorBody>>,
    },
    Pick {
        index: u32,
    },
}

struct Slot {
    id: SessionId,
    device: DeviceId,
    share: ShareId,
    path: String,
    app: AppRef,
    view: SessionView,
    cmd: mpsc::UnboundedSender<Cmd>,
}

struct PendingPrompt {
    view: PromptView,
    session: SessionId,
    tx: Option<oneshot::Sender<PromptAnswer>>,
}

struct Inner {
    cfg: SessionsConfig,
    deps: SessionDeps,
    slot: Mutex<Option<Slot>>,
    prompt: Mutex<Option<PendingPrompt>>,
    next_prompt: AtomicU64,
}

/// The session lifecycle (module documentation). Cheap to clone.
#[derive(Clone)]
pub struct StreamSessions {
    inner: Arc<Inner>,
}

impl fmt::Debug for StreamSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamSessions")
            .field("active", &self.view().map(|v| v.session))
            .finish_non_exhaustive()
    }
}

fn not_found() -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, "No such stream session.")
}

fn app_ref(decision: &PolicyDecision) -> AppRef {
    if let Some(app) = &decision.app {
        return app.clone();
    }
    let name = decision
        .handler
        .as_ref()
        .map_or_else(|| "app".to_owned(), |h| h.exe_name());
    AppRef {
        id: AppId::from(name.to_lowercase()),
        name,
    }
}

impl StreamSessions {
    /// The lifecycle over `deps`.
    pub fn new(cfg: SessionsConfig, deps: SessionDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                deps,
                slot: Mutex::new(None),
                prompt: Mutex::new(None),
                next_prompt: AtomicU64::new(1),
            }),
        }
    }

    /// The engine.
    pub fn engine(&self) -> &StreamEngine {
        &self.inner.deps.engine
    }

    /// `capabilities.stream` as it is now: [`stream_capabilities`] plus the owner's current
    /// clipboard setting and whether sessions carry sound (WP-55).
    pub fn capabilities(&self) -> StreamCapabilities {
        StreamCapabilities {
            audio: self.inner.deps.engine.has_audio(),
            clipboard: self
                .inner
                .cfg
                .clipboard
                .as_ref()
                .map_or(ClipboardMode::Off, ClipboardSetting::get),
            ..stream_capabilities()
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Slot>> {
        self.inner.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn locked_now(&self) -> bool {
        self.inner.locked_now()
    }

    /// Starts a session (`POST /v1/stream`): preconditions, then `201` and a task that drives
    /// the rest. Must be called inside a tokio runtime.
    pub fn start(&self, req: NewSession) -> Result<StreamCreated, ErrorBody> {
        let inner = &self.inner;
        let app = app_ref(&req.decision);
        let mut slot = self.slot();
        let facts = HostFacts {
            locked: self.locked_now(),
            attended: (inner.deps.attended)(),
            busy: slot.is_some(),
        };
        let prompt = preconditions(
            facts,
            req.decision.outcome,
            inner.cfg.idle_default,
            &inner.cfg.timing,
        )
        .map_err(|code| {
            self.audit_refused(&req, code);
            ErrorBody::new(code, precondition_message(code))
        })?;
        let id = SessionId::from(format!("ses_{}", ulid::Ulid::new()));
        let (cmd, cmd_rx) = mpsc::unbounded_channel();
        let device_name = (inner.deps.names)(&req.device);
        *slot = Some(Slot {
            id: id.clone(),
            device: req.device.clone(),
            share: req.share.clone(),
            path: req.path.clone(),
            app: app.clone(),
            view: SessionView {
                session: id.clone(),
                device_name: device_name.clone(),
                file_name: req.file_name.clone(),
                app_name: app.name.clone(),
                state: StreamState::Requested,
                audio: false,
            },
            cmd,
        });
        drop(slot);
        let needs_approval = req.decision.outcome == StreamDecision::Ask;
        let lock_rx = inner.deps.lock.subscribe();
        let lock_base = *lock_rx.borrow();
        let task = Task {
            inner: Arc::clone(inner),
            id: id.clone(),
            device_name,
            app: app.clone(),
            machine: Machine::new(prompt, inner.cfg.timing),
            cmd_rx,
            lock_rx,
            lock_base,
            close_app: false,
            done: Vec::new(),
            req,
            started: Instant::now(),
            streamed_since: None,
            owner_allowed: false,
        };
        tokio::spawn(task.run());
        Ok(StreamCreated {
            session: id,
            state: StreamState::Requested,
            app,
            needs_approval,
        })
    }

    fn audit_refused(&self, req: &NewSession, code: ErrorCode) {
        let mut ev = AuditEvent::new(kind::STREAM_REFUSED)
            .device(&req.device)
            .outcome(Outcome::Refused)
            .detail("share", req.share.as_str())
            .detail("path", req.path.as_str())
            .detail("reason", code.as_str());
        if let Some(s) = req.source {
            ev = ev.source(s);
        }
        self.inner.deps.audit.record(ev);
    }

    /// `POST /v1/stream { resume }`: a new peer connection on the same window. The request
    /// must name the same share and path, from the same device, while the session has a
    /// window.
    pub async fn resume(
        &self,
        device: &DeviceId,
        session: &SessionId,
        share: &ShareId,
        path: &str,
        viewport: Viewport,
        max_kbps: Option<u32>,
    ) -> Result<StreamCreated, ErrorBody> {
        let (tx, app) = {
            let slot = self.slot();
            let Some(s) = slot
                .as_ref()
                .filter(|s| &s.id == session && &s.device == device)
            else {
                return Err(not_found());
            };
            if &s.share != share || s.path != path {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    "A resumed session must name the same file.",
                ));
            }
            (s.cmd.clone(), s.app.clone())
        };
        if self.locked_now() {
            return Err(ErrorBody::new(
                ErrorCode::HostLocked,
                precondition_message(ErrorCode::HostLocked),
            ));
        }
        let (reply, rx) = oneshot::channel();
        tx.send(Cmd::Resume {
            viewport,
            max_kbps,
            reply,
        })
        .map_err(|_| not_found())?;
        let state = rx.await.map_err(|_| not_found())??;
        Ok(StreamCreated {
            session: session.clone(),
            state,
            app,
            needs_approval: false,
        })
    }

    /// `DELETE /v1/stream/{session}`: ends the device's session and waits (a few seconds at
    /// most) for the cleanup, so the window is restored or closed when the answer arrives.
    pub async fn end(
        &self,
        device: &DeviceId,
        session: &SessionId,
        close_app: bool,
    ) -> Result<(), ErrorBody> {
        let tx = {
            let slot = self.slot();
            match slot
                .as_ref()
                .filter(|s| &s.id == session && &s.device == device)
            {
                Some(s) => s.cmd.clone(),
                None => return Err(not_found()),
            }
        };
        let (done, rx) = oneshot::channel();
        let _ = tx.send(Cmd::End {
            cause: EndCause::ClientClosed,
            close_app,
            done,
        });
        let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
        Ok(())
    }

    fn end_all(&self, filter: impl Fn(&Slot) -> bool, cause: EndCause) {
        let slot = self.slot();
        if let Some(s) = slot.as_ref().filter(|s| filter(s)) {
            let (done, _) = oneshot::channel();
            let _ = s.cmd.send(Cmd::End {
                cause,
                close_app: false,
                done,
            });
        }
    }

    /// The kill switch (and a listener pause): ends every session now.
    pub fn kill_all(&self) {
        self.end_all(|_| true, EndCause::KillSwitch);
        // Belt and braces: an engine session the lifecycle does not know about.
        self.inner.deps.engine.stop_all(EndReason::HostKillSwitch);
    }

    /// Host shutdown: ends the session (kill switch) and waits up to 5 s for its cleanup, so
    /// the window is restored and an app Gander started is closed before the host exits.
    pub async fn shutdown(&self) {
        self.kill_all();
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.active().is_some() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Revocation: ends the device's session.
    pub fn stop_device(&self, device: &DeviceId) {
        self.end_all(|s| &s.device == device, EndCause::Revoked);
        self.inner
            .deps
            .engine
            .stop_device(device, EndReason::Revoked);
    }

    /// Ends the device's session when the registry revokes it. Must be called inside a tokio
    /// runtime.
    pub fn watch_revocations(&self, registry: &crate::pairing::DeviceRegistry) {
        let mut rx = registry.subscribe();
        let me = self.clone();
        tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match rx.recv().await {
                    Ok(crate::pairing::DeviceEvent::Revoked { device_id }) => {
                        me.stop_device(&device_id);
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    /// The session, for the tray (`None` when there is none).
    pub fn view(&self) -> Option<SessionView> {
        let mut view = self.slot().as_ref().map(|s| s.view.clone())?;
        view.audio = self
            .inner
            .deps
            .engine
            .session(&view.session)
            .is_some_and(|s| s.audio_active());
        Some(view)
    }

    /// The id of the active session, if any.
    pub fn active(&self) -> Option<SessionId> {
        self.slot().as_ref().map(|s| s.id.clone())
    }

    /// The prompt waiting for the owner, if any.
    pub fn prompt(&self) -> Option<PromptView> {
        self.inner
            .prompt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| p.view.clone())
    }

    /// Answers the prompt `id`; false when it is no longer waiting.
    pub fn answer(&self, id: u64, answer: PromptAnswer) -> bool {
        let mut p = self.inner.prompt.lock().unwrap_or_else(|e| e.into_inner());
        match p.as_mut().filter(|p| p.view.id == id) {
            Some(pending) => pending.tx.take().is_some_and(|tx| tx.send(answer).is_ok()),
            None => false,
        }
    }

    /// Ends the session the toast's Stop button names (the owner, at the host).
    pub fn stop_from_host(&self) {
        self.end_all(|_| true, EndCause::KillSwitch);
    }

    /// `stream.pick` from the phone.
    fn pick(&self, device: &DeviceId, session: &SessionId, index: u32) -> Option<ServerEvent> {
        let slot = self.slot();
        match slot
            .as_ref()
            .filter(|s| &s.id == session && &s.device == device)
        {
            Some(s) => {
                let _ = s.cmd.send(Cmd::Pick { index });
                None
            }
            None => Some(ServerEvent::Error {
                code: ErrorCode::NotFound,
                message: "No such stream session.".to_owned(),
                session: Some(session.clone()),
            }),
        }
    }

    fn knows(&self, device: &DeviceId, session: &SessionId) -> bool {
        self.slot()
            .as_ref()
            .is_some_and(|s| &s.id == session && &s.device == device)
    }
}

impl Inner {
    fn locked_now(&self) -> bool {
        let rx = self.deps.lock.subscribe();
        let s = *rx.borrow();
        // A lock source whose sender is gone counts as locked (fail closed).
        s.locked || rx.has_changed().is_err()
    }

    fn set_state(&self, id: &SessionId, state: StreamState) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = slot.as_mut().filter(|s| &s.id == id) {
            s.view.state = state;
        }
    }

    fn release(&self, id: &SessionId) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|s| &s.id == id) {
            *slot = None;
        }
        let mut p = self.prompt.lock().unwrap_or_else(|e| e.into_inner());
        if p.as_ref().is_some_and(|p| &p.session == id) {
            *p = None;
        }
    }
}

/// `/v1/events` signalling with the lifecycle in front of the engine: `stream.pick` goes to the
/// lifecycle; messages for a session that exists but has no engine session yet (approving,
/// launching, pick, grace) are ignored; everything else goes to the engine (which answers
/// `not_found` for unknown sessions).
#[derive(Debug, Clone)]
pub struct LifecycleSignalling(pub StreamSessions);

impl StreamSignalling for LifecycleSignalling {
    fn on_client_event(&self, device: &DeviceId, event: ClientEvent) -> Option<ServerEvent> {
        let session = match &event {
            ClientEvent::StreamPick { session, index } => {
                return self.0.pick(device, session, *index);
            }
            ClientEvent::Ping {} => return None,
            ClientEvent::StreamAnswer { session, .. }
            | ClientEvent::StreamIce { session, .. }
            | ClientEvent::StreamResize { session, .. }
            | ClientEvent::StreamPause { session }
            | ClientEvent::StreamResume { session } => session.clone(),
        };
        let engine = self.0.engine();
        if engine.session(&session).is_none() && self.0.knows(device, &session) {
            return None;
        }
        engine.handle_client_event(device, event)
    }
}

// ------------------------------------------------------------------------------ the task

enum Interrupt {
    End(EndCause),
    Deadline,
    Resume {
        viewport: Viewport,
        max_kbps: Option<u32>,
        reply: oneshot::Sender<Result<StreamState, ErrorBody>>,
    },
    Pick(u32),
}

/// The engine session plus what hangs off it.
struct Live {
    session: Session,
    input: Option<Box<dyn Send>>,
}

struct Task {
    inner: Arc<Inner>,
    id: SessionId,
    device_name: String,
    app: AppRef,
    machine: Machine,
    cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    lock_rx: watch::Receiver<LockStatus>,
    lock_base: LockStatus,
    close_app: bool,
    done: Vec<oneshot::Sender<()>>,
    req: NewSession,
    started: Instant,
    streamed_since: Option<Instant>,
    /// The owner answered the host prompt with Allow / Always (not a timeout default).
    owner_allowed: bool,
}

/// How the drive part ended.
struct Drive {
    cause: EndCause,
    window: Option<FoundWindow>,
    close_on_end: CloseOnEnd,
    live: Option<Live>,
    awake: Option<Box<dyn Send>>,
}

fn engine_report(s: &SessionState) -> Option<EngineReport> {
    match s {
        SessionState::Negotiating => Some(EngineReport::Negotiating),
        SessionState::Streaming => Some(EngineReport::Streaming),
        SessionState::Paused(r) => Some(EngineReport::Paused(r.wire())),
        SessionState::Ended(_) => None,
    }
}

fn engine_end(cause: EndCause) -> EndReason {
    match cause {
        EndCause::ClientClosed => EndReason::ClientClosed,
        EndCause::KillSwitch => EndReason::HostKillSwitch,
        EndCause::WindowClosed => EndReason::WindowClosed,
        EndCause::Revoked => EndReason::Revoked,
        EndCause::HostLocked => EndReason::HostLocked,
        EndCause::Timeout | EndCause::Unattended | EndCause::AppUnavailable => EndReason::Timeout,
        EndCause::Denied | EndCause::Error => EndReason::Failed("ended".into()),
    }
}

async fn drop_blocking<T: Send + 'static>(value: T) {
    let _ = tokio::task::spawn_blocking(move || drop(value)).await;
}

impl Task {
    fn send(&self, event: ServerEvent) {
        self.inner.deps.signal.send(&self.req.device, event);
    }

    fn report(&self, step: &Step, window: Option<StreamWindow>) {
        self.inner.set_state(&self.id, step.state);
        if !step.report {
            return;
        }
        self.send(ServerEvent::StreamState(StreamStateEvent {
            session: self.id.clone(),
            state: step.state,
            reason: step.reason,
            window,
            candidates: step.candidates.clone(),
        }));
    }

    fn apply(&mut self, event: Event) -> Option<Step> {
        match self.machine.apply(event, Instant::now()) {
            Ok(step) => Some(step),
            Err(e) => {
                tracing::debug!(session = %self.id, "lifecycle: {e}");
                None
            }
        }
    }

    fn apply_report(&mut self, event: Event, window: Option<StreamWindow>) {
        if let Some(step) = self.apply(event) {
            self.report(&step, window);
        }
    }

    /// The next thing that is not the work in progress: a command, a lock, a deadline.
    async fn interrupt(&mut self) -> Interrupt {
        loop {
            let deadline = self.machine.deadline().map(tokio::time::Instant::from_std);
            let sleep = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                changed = self.lock_rx.changed() => {
                    let now = *self.lock_rx.borrow();
                    if changed.is_err() || now.locked || now.locks != self.lock_base.locks {
                        return Interrupt::End(EndCause::HostLocked);
                    }
                }
                cmd = self.cmd_rx.recv() => match cmd {
                    None => return Interrupt::End(EndCause::Error),
                    Some(Cmd::End { cause, close_app, done }) => {
                        self.close_app |= close_app;
                        self.done.push(done);
                        return Interrupt::End(cause);
                    }
                    Some(Cmd::Resume { viewport, max_kbps, reply }) => {
                        return Interrupt::Resume { viewport, max_kbps, reply };
                    }
                    Some(Cmd::Pick { index }) => return Interrupt::Pick(index),
                },
                () = sleep => return Interrupt::Deadline,
            }
        }
    }

    fn not_resumable(reply: oneshot::Sender<Result<StreamState, ErrorBody>>) {
        let _ = reply.send(Err(ErrorBody::new(
            ErrorCode::NotFound,
            "This session cannot be resumed now.",
        )));
    }

    async fn run(mut self) {
        let out = self.drive().await;
        self.finish(out).await;
    }

    fn outcome(cause: EndCause) -> Drive {
        Drive {
            cause,
            window: None,
            close_on_end: CloseOnEnd::Never,
            live: None,
            awake: None,
        }
    }

    /// Prompt, launch, stream; returns why and with what it ended.
    async fn drive(&mut self) -> Drive {
        let mut authorisation = Authorisation::Policy;
        if let Some(prompt) = self.machine.prompt() {
            // Reported once the prompt is up, so a client that sees `approving` finds it.
            let shown = self.apply(Event::PromptShown);
            match self.ask(prompt.kind, shown).await {
                Ok(auth) => authorisation = auth,
                Err(cause) => return Self::outcome(cause),
            }
        }

        // Launch.
        self.apply_report(Event::Launching, None);
        let job = LaunchJob {
            decision: self.req.decision.clone(),
            catalogue: Arc::clone(&self.req.catalogue),
            file: self.req.file.clone(),
            authorisation,
            viewport: self.req.viewport,
        };
        let platform = Arc::clone(&self.inner.deps.platform);
        let mut launch = tokio::task::spawn_blocking(move || platform.launch(&job));
        let mut pending_end: Option<EndCause> = None;
        let launched = loop {
            tokio::select! {
                r = &mut launch => break r,
                i = self.interrupt(), if pending_end.is_none() => match i {
                    Interrupt::End(c) => pending_end = Some(c),
                    Interrupt::Resume { reply, .. } => Self::not_resumable(reply),
                    Interrupt::Deadline | Interrupt::Pick(_) => {}
                },
            }
        };
        let launched = match launched {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                tracing::warn!(session = %self.id, error = %e, "stream launch failed");
                return Self::outcome(pending_end.unwrap_or(EndCause::AppUnavailable));
            }
            Err(_) => return Self::outcome(pending_end.unwrap_or(EndCause::Error)),
        };
        let close_on_end = launched.close_on_end;
        let mut window = match launched.outcome {
            LaunchOutcome::Found(w) => w,
            LaunchOutcome::TimedOut => {
                return Self::outcome(pending_end.unwrap_or(EndCause::AppUnavailable));
            }
            LaunchOutcome::Pick(candidates) => {
                if let Some(c) = pending_end {
                    // Leave the app open: nothing was chosen.
                    return Self::outcome(c);
                }
                match self.pick(launched.pid, candidates).await {
                    Ok(w) => w,
                    Err(c) => return Self::outcome(c),
                }
            }
        };
        let mut out = Drive {
            cause: EndCause::Error,
            window: None,
            close_on_end,
            live: None,
            awake: None,
        };
        if let Some(c) = pending_end {
            out.cause = c;
            out.window = Some(window);
            return out;
        }
        if self.machine.state() == StreamState::Launching {
            let wire = window.wire();
            self.apply_report(Event::WindowFound, wire);
        }
        window.title.truncate(512);
        out.window = Some(window.clone());

        // Stream.
        let live = match self
            .start_engine(&window, self.req.viewport, self.req.max_kbps)
            .await
        {
            Ok(l) => l,
            Err(c) => {
                out.cause = c;
                return out;
            }
        };
        out.awake = self.inner.deps.awake.hold();
        self.audit_started(&window);
        out.live = Some(live);
        out.cause = self.stream(&mut out, &window).await;
        out
    }

    /// The prompt; `Ok` with the launch authorisation, or the end cause.
    async fn ask(
        &mut self,
        kind: PromptKind,
        shown: Option<Step>,
    ) -> Result<Authorisation, EndCause> {
        let (tx, mut rx) = oneshot::channel();
        let id = self.inner.next_prompt.fetch_add(1, Ordering::Relaxed);
        let handler = self.req.decision.handler.as_ref();
        let view = PromptView {
            id,
            kind,
            device_name: self.device_name.clone(),
            file_name: self.req.file_name.clone(),
            app_name: self.app.name.clone(),
            handler: handler.map(|h| h.exe_name()).unwrap_or_default(),
            publisher: handler.and_then(|h| h.signature.publisher().map(str::to_owned)),
            expires_at: self.machine.deadline().unwrap_or_else(Instant::now),
        };
        *self.inner.prompt.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingPrompt {
            view,
            session: self.id.clone(),
            tx: Some(tx),
        });
        if let Some(step) = shown {
            self.report(&step, None);
        }
        let auth_on_allow = match kind {
            PromptKind::Approval => Authorisation::OwnerApprovedOnce,
            PromptKind::Attended => Authorisation::Policy,
        };
        let result = loop {
            tokio::select! {
                answer = &mut rx => {
                    let answer = answer.unwrap_or(PromptAnswer::Deny);
                    self.audit_prompt(kind, Some(answer));
                    match answer {
                        PromptAnswer::Deny => break Err(EndCause::Denied),
                        PromptAnswer::Always | PromptAnswer::AllowOnce => {
                            if answer == PromptAnswer::Always && kind == PromptKind::Approval {
                                self.approve_always().await;
                            }
                            self.apply(Event::Allowed);
                            self.owner_allowed = true;
                            break Ok(auth_on_allow);
                        }
                    }
                }
                i = self.interrupt() => match i {
                    Interrupt::End(c) => break Err(c),
                    Interrupt::Deadline => match self.machine.tick(Instant::now()) {
                        Some(step) if step.ended.is_some() => {
                            self.audit_prompt(kind, None);
                            break Err(step.ended.unwrap_or(EndCause::Unattended));
                        }
                        Some(_) => {
                            self.audit_prompt(kind, None);
                            break Ok(auth_on_allow);
                        }
                        None => {}
                    },
                    Interrupt::Resume { reply, .. } => Self::not_resumable(reply),
                    Interrupt::Pick(_) => {}
                },
            }
        };
        let mut p = self.inner.prompt.lock().unwrap_or_else(|e| e.into_inner());
        if p.as_ref().is_some_and(|p| p.view.id == id) {
            *p = None;
        }
        result
    }

    async fn approve_always(&self) {
        let Some(approve) = self.inner.deps.approve_always.clone() else {
            return;
        };
        let decision = self.req.decision.clone();
        match tokio::task::spawn_blocking(move || approve(&decision)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "the catalogue could not be updated"),
            Err(_) => {}
        }
    }

    /// `pick` until the phone chooses, the 30 s run out, or the session ends.
    async fn pick(
        &mut self,
        launched_pid: u32,
        candidates: Vec<PickCandidate>,
    ) -> Result<FoundWindow, EndCause> {
        let titles: Vec<String> = candidates.iter().map(|c| c.title.clone()).collect();
        self.apply_report(Event::Pick(titles), None);
        loop {
            match self.interrupt().await {
                Interrupt::End(c) => return Err(c),
                Interrupt::Deadline => {
                    if let Some(step) = self.machine.tick(Instant::now())
                        && let Some(c) = step.ended
                    {
                        return Err(c);
                    }
                }
                Interrupt::Resume { reply, .. } => Self::not_resumable(reply),
                Interrupt::Pick(index) => {
                    let Some(c) = candidates.get(index as usize).cloned() else {
                        self.send(ServerEvent::Error {
                            code: ErrorCode::BadRequest,
                            message: "No such window in the list.".to_owned(),
                            session: Some(self.id.clone()),
                        });
                        continue;
                    };
                    let platform = Arc::clone(&self.inner.deps.platform);
                    let viewport = self.req.viewport;
                    let picked = tokio::task::spawn_blocking(move || {
                        platform.pick(launched_pid, &c, viewport)
                    })
                    .await;
                    match picked {
                        Ok(Ok(w)) => {
                            let wire = w.wire();
                            self.apply_report(Event::Picked, wire);
                            return Ok(w);
                        }
                        _ => {
                            self.send(ServerEvent::Error {
                                code: ErrorCode::AppUnavailable,
                                message: "That window is no longer open.".to_owned(),
                                session: Some(self.id.clone()),
                            });
                        }
                    }
                }
            }
        }
    }

    async fn start_engine(
        &self,
        window: &FoundWindow,
        viewport: Viewport,
        max_kbps: Option<u32>,
    ) -> Result<Live, EndCause> {
        let engine = &self.inner.deps.engine;
        let started = engine
            .start_with(StartRequest {
                device: self.req.device.clone(),
                window: WindowTarget {
                    hwnd: window.hwnd as isize,
                    title: window.title.clone(),
                },
                viewport,
                max_kbps,
                session: Some(self.id.clone()),
            })
            .await;
        let session = match started {
            Ok(s) => s,
            Err(gander_stream::EngineError::HostLocked) => return Err(EndCause::HostLocked),
            Err(e) => {
                tracing::warn!(session = %self.id, error = %e, "the stream engine did not start");
                return Err(EndCause::Error);
            }
        };
        let front = front_on_connect(FrontFacts {
            locked: self.inner.locked_now(),
            attended: (self.inner.deps.attended)(),
            owner_allowed: self.owner_allowed,
        });
        let input = self
            .inner
            .deps
            .platform
            .attach_input(&session, window, front);
        Ok(Live { session, input })
    }

    async fn stop_engine(&self, live: &mut Option<Live>, reason: EndReason) {
        if let Some(l) = live.as_ref() {
            l.session.stop(reason);
            let _ = tokio::time::timeout(self.inner.cfg.stop_wait, l.session.ended()).await;
        }
        if let Some(l) = live.take() {
            drop_blocking(l.input).await;
        }
    }

    /// The streaming phase: follows the engine, holds the grace, serves resume.
    async fn stream(&mut self, out: &mut Drive, window: &FoundWindow) -> EndCause {
        loop {
            let (mut state_rx, session) = match &out.live {
                Some(l) => (Some(l.session.watch_state()), Some(l.session.clone())),
                None => (None, None),
            };
            let ended = async {
                match &session {
                    Some(s) => s.ended().await,
                    None => std::future::pending().await,
                }
            };
            let changed = async {
                match state_rx.as_mut() {
                    Some(rx) => {
                        if rx.changed().await.is_err() {
                            // The engine session is gone; `ended` reports why.
                            std::future::pending::<()>().await;
                        }
                        rx.borrow().clone()
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                reason = ended => {
                    if let Some(l) = out.live.take() {
                        drop_blocking(l.input).await;
                    }
                    match reason {
                        EndReason::NetworkLost => {
                            self.apply_report(Event::Engine(EngineReport::NetworkLost), None);
                        }
                        EndReason::HostLocked => return EndCause::HostLocked,
                        EndReason::WindowClosed => return EndCause::WindowClosed,
                        EndReason::Revoked => return EndCause::Revoked,
                        EndReason::HostKillSwitch => return EndCause::KillSwitch,
                        EndReason::ClientClosed => return EndCause::ClientClosed,
                        EndReason::Timeout => return EndCause::Timeout,
                        EndReason::Failed(e) => {
                            tracing::warn!(session = %self.id, error = %e, "stream session failed");
                            return EndCause::Error;
                        }
                    }
                }
                state = changed => {
                    if let Some(r) = engine_report(&state) {
                        if r == EngineReport::Streaming && self.streamed_since.is_none() {
                            self.streamed_since = Some(Instant::now());
                        }
                        self.apply(Event::Engine(r));
                        self.inner.set_state(&self.id, self.machine.state());
                    }
                }
                i = self.interrupt() => match i {
                    Interrupt::End(c) => {
                        self.stop_engine(&mut out.live, engine_end(c)).await;
                        return c;
                    }
                    Interrupt::Deadline => {
                        if let Some(step) = self.machine.tick(Instant::now())
                            && let Some(c) = step.ended
                        {
                            self.stop_engine(&mut out.live, EndReason::Timeout).await;
                            return c;
                        }
                    }
                    Interrupt::Pick(_) => {}
                    Interrupt::Resume { viewport, max_kbps, reply } => {
                        if !self.machine.can_resume() {
                            Self::not_resumable(reply);
                            continue;
                        }
                        // The old peer connection goes quietly; the same window is offered again.
                        self.stop_engine(&mut out.live, EndReason::NetworkLost).await;
                        // `window_found` first, then the engine's `stream.offer` (the order of
                        // the first start).
                        let wire = window.wire();
                        self.apply_report(Event::Resumed, wire);
                        match self.start_engine(window, viewport, max_kbps).await {
                            Ok(l) => {
                                out.live = Some(l);
                                self.audit_resumed();
                                let _ = reply.send(Ok(self.machine.state()));
                            }
                            Err(c) => {
                                let code = if c == EndCause::HostLocked {
                                    ErrorCode::HostLocked
                                } else {
                                    ErrorCode::Internal
                                };
                                let _ = reply.send(Err(ErrorBody::new(
                                    code,
                                    "The session could not be resumed.",
                                )));
                                return c;
                            }
                        }
                    }
                },
            }
        }
    }

    /// Stop, restore, close per policy, tell the phone, audit, free the slot.
    async fn finish(mut self, mut out: Drive) {
        let cause = out.cause;
        self.stop_engine(&mut out.live, engine_end(cause)).await;
        if let Some(a) = out.awake.take() {
            drop_blocking(a).await;
        }
        let mut report = FinishReport::default();
        let mut close = CloseAction::Leave;
        if let Some(w) = out.window.clone() {
            let platform = Arc::clone(&self.inner.deps.platform);
            let facts = CloseFacts {
                policy: out.close_on_end,
                cause,
                close_app: self.close_app,
                launched_by_us: w.launched_by_us,
                only_window: false,
            };
            let r = tokio::task::spawn_blocking(move || {
                let only = cause != EndCause::WindowClosed && platform.is_only_window(&w);
                let action = close_action(CloseFacts {
                    only_window: only,
                    ..facts
                });
                let report = if cause == EndCause::WindowClosed {
                    FinishReport::default()
                } else {
                    platform.finish(&w, action == CloseAction::Close)
                };
                (action, report)
            })
            .await;
            if let Ok((a, r)) = r {
                close = a;
                report = r;
            }
        }
        if let Some((code, message)) = cause.error() {
            self.send(ServerEvent::Error {
                code,
                message: message.to_owned(),
                session: Some(self.id.clone()),
            });
        }
        if report.left_open_dialog {
            // There is no protocol field for this; an error event the phone can show (open
            // question for the protocol: a `left_open` reason or field).
            self.send(ServerEvent::Error {
                code: ErrorCode::AppUnavailable,
                message: "The app was left open: it has unsaved changes.".to_owned(),
                session: Some(self.id.clone()),
            });
        }
        // A deadline (`tick`) may have ended the machine already; report that end.
        let step = if self.machine.is_ended() {
            Some(Step {
                state: StreamState::Ended,
                reason: self.machine.reason(),
                candidates: Vec::new(),
                report: true,
                ended: self.machine.end_cause(),
            })
        } else {
            self.apply(Event::End(cause))
        };
        if let Some(step) = step {
            self.report(&step, None);
        }
        self.audit_ended(cause, close, report);
        self.inner.release(&self.id);
        for d in self.done.drain(..) {
            let _ = d.send(());
        }
    }

    fn base_event(&self, event: &str) -> AuditEvent {
        let mut ev = AuditEvent::new(event)
            .device(&self.req.device)
            .detail("session", self.id.as_str())
            .detail("share", self.req.share.as_str())
            .detail("path", self.req.path.as_str());
        if let Some(s) = self.req.source {
            ev = ev.source(s);
        }
        ev
    }

    fn audit_prompt(&self, kind: PromptKind, answer: Option<PromptAnswer>) {
        let ev = self
            .base_event(kind::STREAM_PROMPT)
            .detail(
                "prompt",
                match kind {
                    PromptKind::Approval => "approval",
                    PromptKind::Attended => "attended",
                },
            )
            .detail(
                "answer",
                match answer {
                    Some(PromptAnswer::AllowOnce) => "allow_once",
                    Some(PromptAnswer::Always) => "always",
                    Some(PromptAnswer::Deny) => "deny",
                    None => "no_answer",
                },
            );
        self.inner.deps.audit.record(ev);
    }

    fn audit_started(&self, window: &FoundWindow) {
        let d = &self.req.decision;
        let ev = self
            .base_event(kind::STREAM_STARTED)
            .detail("app", self.app.id.as_str())
            .detail(
                "handler",
                d.handler.as_ref().map(|h| h.exe_name()).unwrap_or_default(),
            )
            .detail("decision", d.reason.as_str())
            .detail(
                "approval",
                match self.machine.prompt().map(|p| p.kind) {
                    Some(PromptKind::Approval) => "approved_by_owner",
                    Some(PromptKind::Attended) => "attended_prompt",
                    None => "policy",
                },
            )
            .detail("launched_by_us", window.launched_by_us);
        self.inner.deps.audit.record(ev);
    }

    fn audit_resumed(&self) {
        self.inner
            .deps
            .audit
            .record(self.base_event(kind::STREAM_RESUMED));
    }

    fn audit_ended(&self, cause: EndCause, close: CloseAction, report: FinishReport) {
        let outcome = match cause {
            EndCause::Error => Outcome::Error,
            EndCause::Denied | EndCause::Unattended => Outcome::Refused,
            _ => Outcome::Ok,
        };
        let ev = self
            .base_event(kind::STREAM_ENDED)
            .outcome(outcome)
            .detail("reason", cause.as_str())
            .detail("seconds", self.started.elapsed().as_secs())
            .detail(
                "streamed_seconds",
                self.streamed_since.map_or(0, |t| t.elapsed().as_secs()),
            )
            .detail("app_closed", close == CloseAction::Close && report.closed)
            .detail("left_open_unsaved", report.left_open_dialog);
        self.inner.deps.audit.record(ev);
    }
}
