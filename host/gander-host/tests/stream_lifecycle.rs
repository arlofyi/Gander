// SPDX-License-Identifier: Apache-2.0
//! WP-53 integration: the session lifecycle over the real stream engine (WebRTC on 127.0.0.1,
//! synthetic source, no capture) with a fake window platform (no app is launched). Covers the
//! simulated lock ending a session in every phase, the one-session rule, prompts, the pick flow,
//! the resume grace, close_on_end, keep-awake and the kill switch. The same paths against a
//! real `gander-testwin` window run in `cargo xtask e2e-host`.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gander_core::config::{AppCatalogue, CloseOnEnd, IdlePromptDefault, UdpPortRange};
use gander_core::error::ErrorCode;
use gander_core::lifecycle::Timing;
use gander_core::policy::{DecisionReason, HandlerInfo, PolicyDecision, Signature};
use gander_core::protocol::events::{ClientEvent, ServerEvent};
use gander_core::protocol::files::StreamDecision;
use gander_core::protocol::stream::{StreamReason, StreamState, Viewport};
use gander_core::protocol::{DeviceId, SessionId, ShareId};
use gander_host::audit::{AuditLog, kind};
use gander_host::events::StreamSignalling;
use gander_host::lifecycle::{
    Awake, FinishReport, FoundWindow, LaunchJob, LaunchOutcome, LaunchResult, LifecycleSignalling,
    NewSession, PickCandidate, PromptAnswer, PromptKind, SessionDeps, SessionsConfig,
    StreamSessions, WindowPlatform,
};
use gander_stream::testing::{CollectingSink, SyntheticSource, Viewer, ViewerOptions};
use gander_stream::{
    EndReason, EngineConfig, EngineDeps, FakeLock, FixedPower, Session, StaticAddrs, StreamEngine,
};
use tokio::sync::mpsc;

type Events = mpsc::UnboundedReceiver<(DeviceId, ServerEvent)>;

fn port_range() -> UdpPortRange {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let i = NEXT.fetch_add(1, Ordering::Relaxed) % 100;
    let start = 56_000 + i * 20;
    UdpPortRange::new(start, start + 19).expect("range")
}

fn device() -> DeviceId {
    DeviceId::from("dev_phone")
}

fn viewport() -> Viewport {
    Viewport {
        width: 1080,
        height: 2280,
        dpr: 2.625,
    }
}

/// A platform that launches nothing: it answers with a made-up window (or a pick) and records
/// what the end did.
#[derive(Debug, Default)]
struct FakePlatform {
    pick: AtomicBool,
    launched_by_us: AtomicBool,
    finished: Mutex<Vec<bool>>,
    /// `front_on_connect` of every `attach_input`, in order.
    fronts: Mutex<Vec<bool>>,
}

impl FakePlatform {
    fn window(&self, title: &str) -> FoundWindow {
        FoundWindow {
            hwnd: 0x1234,
            pid: 4242,
            title: title.to_owned(),
            launched_by_us: self.launched_by_us.load(Ordering::SeqCst),
            size: Some((640, 1352, 96)),
            native: None,
        }
    }
}

impl WindowPlatform for FakePlatform {
    fn launch(&self, _job: &LaunchJob) -> Result<LaunchResult, String> {
        let outcome = if self.pick.load(Ordering::SeqCst) {
            LaunchOutcome::Pick(vec![
                PickCandidate {
                    title: "one".into(),
                    native: Arc::new(()),
                },
                PickCandidate {
                    title: "two".into(),
                    native: Arc::new(()),
                },
            ])
        } else {
            LaunchOutcome::Found(self.window("fake window"))
        };
        Ok(LaunchResult {
            outcome,
            pid: 4242,
            close_on_end: CloseOnEnd::IfLaunchedByUs,
        })
    }

    fn pick(
        &self,
        _launched_pid: u32,
        candidate: &PickCandidate,
        _viewport: Viewport,
    ) -> Result<FoundWindow, String> {
        Ok(self.window(&candidate.title))
    }

    fn attach_input(
        &self,
        _session: &Session,
        _window: &FoundWindow,
        front_on_connect: bool,
    ) -> Option<Box<dyn Send>> {
        self.fronts.lock().unwrap().push(front_on_connect);
        None
    }

    fn is_only_window(&self, _window: &FoundWindow) -> bool {
        true
    }

    fn finish(&self, _window: &FoundWindow, close: bool) -> FinishReport {
        self.finished.lock().unwrap().push(close);
        FinishReport {
            closed: close,
            left_open_dialog: false,
        }
    }
}

/// Counts keep-awake guards alive.
#[derive(Debug, Default)]
struct CountingAwake(Arc<AtomicUsize>);

struct Guard(Arc<AtomicUsize>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Awake for CountingAwake {
    fn hold(&self) -> Option<Box<dyn Send>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(Box::new(Guard(Arc::clone(&self.0))))
    }
}

struct Harness {
    sessions: StreamSessions,
    events: Events,
    lock: Arc<FakeLock>,
    attended: Arc<AtomicBool>,
    platform: Arc<FakePlatform>,
    awake: Arc<AtomicUsize>,
    audit: Arc<AuditLog>,
}

fn harness_with(timing: Timing, idle_default: IdlePromptDefault) -> Harness {
    harness_full(timing, idle_default, None)
}

fn harness_full(
    timing: Timing,
    idle_default: IdlePromptDefault,
    clipboard: Option<gander_core::clipboard::ClipboardSetting>,
) -> Harness {
    let sink = Arc::new(CollectingSink::default());
    let events = sink.take_receiver();
    let lock = Arc::new(FakeLock::new(false));
    let engine = StreamEngine::new(
        EngineConfig {
            udp_ports: port_range(),
            owner_reports_end: true,
            ..EngineConfig::default()
        },
        EngineDeps {
            lock: lock.clone(),
            power: Arc::new(FixedPower::new(false)),
            addrs: Arc::new(StaticAddrs(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])),
            signal: sink.clone(),
            source: Arc::new(SyntheticSource::default()),
            audio: None,
        },
    );
    let attended = Arc::new(AtomicBool::new(false));
    let platform = Arc::new(FakePlatform::default());
    platform.launched_by_us.store(true, Ordering::SeqCst);
    let awake = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(AuditLog::memory());
    let a = Arc::clone(&attended);
    let sessions = StreamSessions::new(
        SessionsConfig {
            timing,
            idle_default,
            stop_wait: Duration::from_secs(3),
            clipboard,
        },
        SessionDeps {
            engine,
            platform: platform.clone(),
            lock: lock.clone(),
            attended: Arc::new(move || a.load(Ordering::SeqCst)),
            signal: sink,
            audit: Arc::clone(&audit),
            awake: Arc::new(CountingAwake(Arc::clone(&awake))),
            names: Arc::new(|_| "Pixel 9".to_owned()),
            approve_always: None,
        },
    );
    Harness {
        sessions,
        events,
        lock,
        attended,
        platform,
        awake,
        audit,
    }
}

fn harness() -> Harness {
    harness_with(Timing::default(), IdlePromptDefault::Allow)
}

fn decision(outcome: StreamDecision) -> PolicyDecision {
    let reason = match outcome {
        StreamDecision::Allowed => DecisionReason::CatalogueApproved,
        StreamDecision::Ask => DecisionReason::UnknownHandler,
        StreamDecision::Refused => DecisionReason::HardRefusedExtension,
    };
    PolicyDecision {
        outcome,
        reason,
        extension: Some("gtw".into()),
        handler: Some(HandlerInfo::new(
            PathBuf::from(r"C:\Apps\testwin.exe"),
            Signature::Unsigned,
        )),
        app: None,
        browser: None,
    }
}

fn request(outcome: StreamDecision) -> NewSession {
    NewSession {
        device: device(),
        source: None,
        decision: decision(outcome),
        catalogue: Arc::new(AppCatalogue::default()),
        file: PathBuf::from(r"C:\share\deck.gtw"),
        file_name: "deck.gtw".into(),
        share: ShareId::from("shr_a"),
        path: "deck.gtw".into(),
        viewport: viewport(),
        max_kbps: None,
    }
}

/// The next event matching `f` within `timeout`.
async fn next<T>(
    events: &mut Events,
    timeout: Duration,
    mut f: impl FnMut(&ServerEvent) -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .ok()??;
        if let Some(t) = f(&ev.1) {
            return Some(t);
        }
    }
}

async fn state(events: &mut Events, id: &SessionId, want: StreamState) -> Option<StreamReason> {
    next(events, Duration::from_secs(10), |e| match e {
        ServerEvent::StreamState(s) if &s.session == id && s.state == want => Some(s.reason),
        _ => None,
    })
    .await
    .unwrap_or_else(|| panic!("no stream.state {want:?}"))
}

async fn error_code(events: &mut Events, id: &SessionId) -> ErrorCode {
    next(events, Duration::from_secs(10), |e| match e {
        ServerEvent::Error { code, session, .. } if session.as_ref() == Some(id) => Some(*code),
        _ => None,
    })
    .await
    .expect("an error event")
}

async fn offer(events: &mut Events, id: &SessionId) -> String {
    next(events, Duration::from_secs(10), |e| match e {
        ServerEvent::StreamOffer { session, sdp } if session == id => Some(sdp.clone()),
        _ => None,
    })
    .await
    .expect("stream.offer")
}

async fn until_free(h: &Harness) {
    for _ in 0..200 {
        if h.sessions.active().is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the session slot was not freed");
}

/// The `front_on_connect` flags once `n` engine sessions had input attached.
async fn fronts(h: &Harness, n: usize) -> Vec<bool> {
    for _ in 0..400 {
        let f = h.platform.fronts.lock().unwrap().clone();
        if f.len() >= n {
            return f;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("input was not attached {n} time(s)");
}

/// Answers the offer with a loopback viewer and waits for `streaming`.
async fn connect(h: &mut Harness, id: &SessionId) -> Viewer {
    let sdp = offer(&mut h.events, id).await;
    let (viewer, answer) = Viewer::answer(&sdp, ViewerOptions::default())
        .await
        .expect("viewer");
    let sig = LifecycleSignalling(h.sessions.clone());
    assert!(
        sig.on_client_event(
            &device(),
            ClientEvent::StreamAnswer {
                session: id.clone(),
                sdp: answer.sdp,
            },
        )
        .is_none()
    );
    for c in answer.candidates {
        sig.on_client_event(
            &device(),
            ClientEvent::StreamIce {
                session: id.clone(),
                candidate: c,
            },
        );
    }
    state(&mut h.events, id, StreamState::Streaming).await;
    viewer
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_advertised_clipboard_capability_follows_the_owners_setting() {
    use gander_core::clipboard::{ClipboardMode, ClipboardSetting};

    // A host without the bridge advertises nothing, and the field is not even sent.
    let none = harness();
    assert_eq!(none.sessions.capabilities().clipboard, ClipboardMode::Off);
    let json = serde_json::to_value(none.sessions.capabilities()).unwrap();
    assert!(json.get("clipboard").is_none(), "{json}");

    let setting = ClipboardSetting::new(ClipboardMode::ToPhone);
    let h = harness_full(
        Timing::default(),
        IdlePromptDefault::Allow,
        Some(setting.clone()),
    );
    assert_eq!(h.sessions.capabilities().clipboard, ClipboardMode::ToPhone);
    let json = serde_json::to_value(h.sessions.capabilities()).unwrap();
    assert_eq!(json["clipboard"], "to_phone");
    assert_eq!(
        json["input"], true,
        "the rest of the capabilities are unchanged"
    );
    // The owner changes it in Settings: the next /v1/me says so.
    setting.set(ClipboardMode::Both);
    assert_eq!(h.sessions.capabilities().clipboard, ClipboardMode::Both);
    setting.set(ClipboardMode::Off);
    assert_eq!(h.sessions.capabilities().clipboard, ClipboardMode::Off);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preconditions_refuse_before_a_session_exists() {
    let h = harness();
    h.lock.set_locked(true);
    let e = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap_err();
    assert_eq!(e.error.code, ErrorCode::HostLocked);
    h.lock.set_locked(false);
    let e = h.sessions.start(request(StreamDecision::Ask)).unwrap_err();
    assert_eq!(e.error.code, ErrorCode::HostUnattended);
    assert!(h.sessions.active().is_none());
    let refused = h
        .audit
        .recent(10)
        .iter()
        .filter(|e| e.event == kind::STREAM_REFUSED)
        .count();
    assert_eq!(refused, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_session_then_delete_closes_what_we_launched() {
    let mut h = harness();
    let created = h.sessions.start(request(StreamDecision::Allowed)).unwrap();
    assert_eq!(created.state, StreamState::Requested);
    assert!(!created.needs_approval);
    let id = created.session;
    state(&mut h.events, &id, StreamState::Launching).await;
    state(&mut h.events, &id, StreamState::WindowFound).await;
    offer(&mut h.events, &id).await;
    assert!(h.sessions.view().unwrap().is_live());

    // One session per host.
    let busy = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap_err();
    assert_eq!(busy.error.code, ErrorCode::StreamBusy);
    // Another device cannot end it.
    assert_eq!(
        h.sessions
            .end(&DeviceId::from("dev_other"), &id, false)
            .await
            .unwrap_err()
            .error
            .code,
        ErrorCode::NotFound
    );

    h.sessions.end(&device(), &id, false).await.unwrap();
    let reason = state(&mut h.events, &id, StreamState::Ended).await;
    assert_eq!(reason, Some(StreamReason::ClientClosed));
    assert!(h.sessions.active().is_none(), "freed when DELETE answers");
    assert_eq!(*h.platform.finished.lock().unwrap(), vec![true]);
    assert!(h.sessions.engine().sessions().is_empty());
    let ended = h
        .audit
        .recent(20)
        .into_iter()
        .find(|e| e.event == kind::STREAM_ENDED)
        .expect("audit end");
    assert_eq!(ended.detail["reason"], "client_closed");
    assert_eq!(ended.detail["app_closed"], true);
    // Free again.
    let again = h.sessions.start(request(StreamDecision::Allowed)).unwrap();
    h.sessions
        .end(&device(), &again.session, false)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simulated_lock_ends_a_streaming_session_and_releases_keep_awake() {
    let mut h = harness();
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    let _viewer = connect(&mut h, &id).await;
    assert_eq!(
        h.awake.load(Ordering::SeqCst),
        1,
        "kept awake while streaming"
    );
    // The tray follows the engine's state.
    let mut tries = 0;
    while h.sessions.view().unwrap().state != StreamState::Streaming && tries < 100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tries += 1;
    }
    assert_eq!(h.sessions.view().unwrap().state, StreamState::Streaming);
    let started = h
        .audit
        .recent(20)
        .into_iter()
        .find(|e| e.event == kind::STREAM_STARTED)
        .expect("audit start");
    assert_eq!(started.detail["decision"], "catalogue_approved");

    h.lock.set_locked(true);
    assert_eq!(error_code(&mut h.events, &id).await, ErrorCode::HostLocked);
    assert_eq!(
        state(&mut h.events, &id, StreamState::Ended).await,
        Some(StreamReason::Error)
    );
    until_free(&h).await;
    assert_eq!(h.awake.load(Ordering::SeqCst), 0, "keep-awake cleared");
    // Still locked: no new session.
    let e = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap_err();
    assert_eq!(e.error.code, ErrorCode::HostLocked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simulated_lock_ends_a_session_waiting_on_a_prompt() {
    let mut h = harness();
    h.attended.store(true, Ordering::SeqCst);
    let id = h
        .sessions
        .start(request(StreamDecision::Ask))
        .unwrap()
        .session;
    state(&mut h.events, &id, StreamState::Approving).await;
    let p = h.sessions.prompt().expect("prompt shown");
    assert_eq!(p.kind, PromptKind::Approval);
    h.lock.set_locked(true);
    assert_eq!(error_code(&mut h.events, &id).await, ErrorCode::HostLocked);
    state(&mut h.events, &id, StreamState::Ended).await;
    until_free(&h).await;
    assert!(
        h.sessions.prompt().is_none(),
        "the prompt goes with the session"
    );
    assert!(!h.sessions.answer(p.id, PromptAnswer::AllowOnce));
    assert!(
        h.platform.finished.lock().unwrap().is_empty(),
        "nothing launched"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompts_allow_deny_and_default() {
    let mut h = harness();
    h.attended.store(true, Ordering::SeqCst);
    // Attended + allowed: "Not now" ends it with stream_refused.
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    state(&mut h.events, &id, StreamState::Approving).await;
    let p = h.sessions.prompt().unwrap();
    assert_eq!(p.kind, PromptKind::Attended);
    assert_eq!(p.device_name, "Pixel 9");
    assert!(h.sessions.answer(p.id, PromptAnswer::Deny));
    assert_eq!(
        error_code(&mut h.events, &id).await,
        ErrorCode::StreamRefused
    );
    state(&mut h.events, &id, StreamState::Ended).await;
    until_free(&h).await;

    // Ask + attended: Allow once launches.
    let created = h.sessions.start(request(StreamDecision::Ask)).unwrap();
    assert!(created.needs_approval);
    let id = created.session;
    state(&mut h.events, &id, StreamState::Approving).await;
    let p = h.sessions.prompt().unwrap();
    assert!(h.sessions.answer(p.id, PromptAnswer::AllowOnce));
    state(&mut h.events, &id, StreamState::WindowFound).await;
    h.sessions.end(&device(), &id, false).await.unwrap();
    until_free(&h).await;

    // Unanswered prompts: attended default allow launches; approval ends host_unattended.
    let mut h = harness_with(
        Timing {
            attended_prompt: Duration::from_millis(200),
            approval: Duration::from_millis(200),
            ..Timing::default()
        },
        IdlePromptDefault::Allow,
    );
    h.attended.store(true, Ordering::SeqCst);
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    state(&mut h.events, &id, StreamState::WindowFound).await;
    h.sessions.end(&device(), &id, false).await.unwrap();
    until_free(&h).await;
    let id = h
        .sessions
        .start(request(StreamDecision::Ask))
        .unwrap()
        .session;
    assert_eq!(
        error_code(&mut h.events, &id).await,
        ErrorCode::HostUnattended
    );
    assert_eq!(
        state(&mut h.events, &id, StreamState::Ended).await,
        Some(StreamReason::Timeout)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pick_by_index_over_the_events_channel() {
    let mut h = harness();
    h.platform.pick.store(true, Ordering::SeqCst);
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    let candidates = next(&mut h.events, Duration::from_secs(10), |e| match e {
        ServerEvent::StreamState(s) if s.session == id && s.state == StreamState::Pick => {
            Some(s.candidates.clone())
        }
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(candidates, vec!["one".to_owned(), "two".to_owned()]);
    let sig = LifecycleSignalling(h.sessions.clone());
    // Messages for a session with no engine yet are ignored, not refused.
    assert!(
        sig.on_client_event(
            &device(),
            ClientEvent::StreamPause {
                session: id.clone()
            }
        )
        .is_none()
    );
    // Another device's pick is refused.
    assert!(matches!(
        sig.on_client_event(
            &DeviceId::from("dev_other"),
            ClientEvent::StreamPick {
                session: id.clone(),
                index: 0
            }
        ),
        Some(ServerEvent::Error {
            code: ErrorCode::NotFound,
            ..
        })
    ));
    assert!(
        sig.on_client_event(
            &device(),
            ClientEvent::StreamPick {
                session: id.clone(),
                index: 1
            }
        )
        .is_none()
    );
    let window = next(&mut h.events, Duration::from_secs(10), |e| match e {
        ServerEvent::StreamState(s) if s.session == id && s.state == StreamState::WindowFound => {
            s.window.clone()
        }
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(window.title, "two");
    h.sessions.kill_all();
    assert_eq!(
        state(&mut h.events, &id, StreamState::Ended).await,
        Some(StreamReason::HostKillSwitch)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_peer_is_held_for_the_grace_then_resumed_or_timed_out() {
    let mut h = harness_with(
        Timing {
            resume_grace: Duration::from_millis(1500),
            ..Timing::default()
        },
        IdlePromptDefault::Allow,
    );
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    offer(&mut h.events, &id).await;
    // The peer connection fails (what ICE Failed does inside the engine).
    let engine_session = h.sessions.engine().session(&id).expect("engine session");
    engine_session.stop(EndReason::NetworkLost);
    assert_eq!(
        state(&mut h.events, &id, StreamState::Paused).await,
        Some(StreamReason::Network)
    );
    assert!(h.sessions.active().is_some(), "held for the grace");

    // Resume within the grace: same session id, a new offer, the window again.
    let resumed = h
        .sessions
        .resume(
            &device(),
            &id,
            &ShareId::from("shr_a"),
            "deck.gtw",
            viewport(),
            None,
        )
        .await
        .expect("resume");
    assert_eq!(resumed.session, id);
    assert_eq!(resumed.state, StreamState::WindowFound);
    state(&mut h.events, &id, StreamState::WindowFound).await;
    offer(&mut h.events, &id).await;
    // Nobody at the computer: brought to the front at the start and once more on the resume.
    assert_eq!(fronts(&h, 2).await, vec![true, true]);
    // A resume naming another file is refused.
    let e = h
        .sessions
        .resume(
            &device(),
            &id,
            &ShareId::from("shr_a"),
            "other.gtw",
            viewport(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.error.code, ErrorCode::BadRequest);

    // Lost again, and nobody comes back: timeout after the grace.
    h.sessions
        .engine()
        .session(&id)
        .expect("engine session")
        .stop(EndReason::NetworkLost);
    state(&mut h.events, &id, StreamState::Paused).await;
    assert_eq!(
        state(&mut h.events, &id, StreamState::Ended).await,
        Some(StreamReason::Timeout)
    );
    until_free(&h).await;
    let e = h
        .sessions
        .resume(
            &device(),
            &id,
            &ShareId::from("shr_a"),
            "deck.gtw",
            viewport(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.error.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_and_close_on_end_for_an_app_we_did_not_start() {
    let mut h = harness();
    h.platform.launched_by_us.store(false, Ordering::SeqCst);
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    offer(&mut h.events, &id).await;
    h.sessions.stop_device(&device());
    assert_eq!(
        state(&mut h.events, &id, StreamState::Ended).await,
        Some(StreamReason::Revoked)
    );
    until_free(&h).await;
    assert_eq!(
        *h.platform.finished.lock().unwrap(),
        vec![false],
        "left open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn front_on_connect_only_unattended_or_after_the_owners_allow() {
    // Nobody at the computer, no prompt: brought to the front.
    let mut h = harness();
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    offer(&mut h.events, &id).await;
    assert_eq!(fronts(&h, 1).await, vec![true]);
    h.sessions.end(&device(), &id, false).await.unwrap();
    until_free(&h).await;

    // Someone there and the owner pressed Allow on the host: brought to the front.
    h.attended.store(true, Ordering::SeqCst);
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    state(&mut h.events, &id, StreamState::Approving).await;
    let p = h.sessions.prompt().unwrap();
    assert!(h.sessions.answer(p.id, PromptAnswer::AllowOnce));
    offer(&mut h.events, &id).await;
    assert_eq!(fronts(&h, 2).await, vec![true, true]);
    h.sessions.end(&device(), &id, false).await.unwrap();
    until_free(&h).await;

    // Someone there but nobody answered (the idle default let it through): left alone; the
    // phone's "Bring to front" is still available.
    let mut h = harness_with(
        Timing {
            attended_prompt: Duration::from_millis(200),
            ..Timing::default()
        },
        IdlePromptDefault::Allow,
    );
    h.attended.store(true, Ordering::SeqCst);
    let id = h
        .sessions
        .start(request(StreamDecision::Allowed))
        .unwrap()
        .session;
    offer(&mut h.events, &id).await;
    assert_eq!(fronts(&h, 1).await, vec![false]);
    h.sessions.end(&device(), &id, false).await.unwrap();
    until_free(&h).await;
}
