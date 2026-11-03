// SPDX-License-Identifier: Apache-2.0
//! The stream engine: one WebRTC session per streamed window.
//!
//! # API for WP-51/52/53/54
//!
//! ```no_run
//! # async fn demo(engine: gander_stream::StreamEngine, hwnd: isize,
//! #   device: gander_core::protocol::DeviceId) -> Result<(), gander_stream::EngineError> {
//! use gander_core::protocol::stream::Viewport;
//! use gander_stream::{EndReason, WindowTarget};
//!
//! let window = WindowTarget { hwnd, title: "Deck.pptx - PowerPoint".into() };   // WP-51 finds it
//! let viewport = Viewport { width: 1080, height: 2280, dpr: 2.625 };
//! let session = engine.start(window, viewport, device).await?; // sends stream.offer
//! // The phone's stream.answer / stream.ice / stream.pause / stream.resume reach the session
//! // through `engine.handle_client_event` (installed as the /v1/events StreamSignalling).
//! session.set_input_handler(std::sync::Arc::new(|_channel, _json: &[u8]| { /* WP-52 */ }));
//! session.request_keyframe();
//! let _stats = session.stats();
//! session.stop(EndReason::ClientClosed);          // WP-53: DELETE /stream, kill switch, ...
//! session.ended().await;
//! # Ok(()) }
//! ```
//!
//! # What a session does
//!
//! 1. Refuses at once when the session is locked ([`EngineError::HostLocked`]).
//! 2. Binds ICE on the host's addresses, one port each from `[firewall] udp_ports`.
//! 3. Starts the source (capture + encoder) paused, creates the offer, waits for host-candidate
//!    gathering (instant) and sends a complete `stream.offer` (candidates in the SDP). Candidates
//!    found later are trickled as `stream.ice`.
//! 4. Applies the answer (non-host candidates removed; an answer with NO candidates is fine: the
//!    host learns the phone's address from its first connectivity check, peer-reflexive), and
//!    adds trickled `stream.ice` candidates, buffering those that arrive before the answer.
//! 5. On `Connected`: unpauses the source with an IDR and reports `stream.state streaming`.
//! 6. While streaming, every 100 ms: encoder set point = estimate x 0.85, capped by the phone's
//!    `max_kbps` and, on battery, by the battery cap; liveness check on the `control` channel.
//! 7. Keyframes: on PLI/FIR (the interceptor delivers them), on `{"t":"keyframe"}`, on the first
//!    connection, on every reconnect (ICE back to `Connected`) and on resume.
//! 8. Pauses (no capture, no encode) on `stream.pause` / `{"t":"pause"}` (`phone_background`),
//!    on ICE `Disconnected` and on control-channel silence (`network`).
//! 9. Ends on [`Session::stop`], when the session locks (fail closed: a closed lock source counts
//!    as locked), when the window closes, when ICE fails or the peer connection closes
//!    ([`EndReason::NetworkLost`]), and on source errors. The end is reported as
//!    `stream.state ended` with a reason; a lock end also sends `error host_locked`. With
//!    [`EngineConfig::owner_reports_end`] (WP-53) the engine reports neither and its owner does.
//! 10. Audio (WP-55, with [`EngineDeps::audio`]): an Opus track next to the video, the window's
//!     process tree captured only while the session streams and the phone has not muted it
//!     (`{"t":"mute","on":true}`); `{"t":"audio","on":..,"reason":..}` on `control` reports each
//!     change ([`crate::audio`]). No audio source, or a capture that cannot start: no sound,
//!     never a system-wide fallback.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use gander_core::config::UdpPortRange;
use gander_core::error::ErrorCode;
use gander_core::protocol::events::{ClientEvent, ServerEvent, StreamStateEvent};
use gander_core::protocol::stream::{StreamReason, StreamState, Viewport};
use gander_core::protocol::{DeviceId, SessionId};
use rtc::media::Sample;
use rtc::rtp::extension::HeaderExtension;
use rtc::rtp::extension::playout_delay_extension::PlayoutDelayExtension;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionEventHandler, RTCIceCandidateInit, RTCIceGatheringState,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription,
};

use crate::audio::{AUDIO_SSRC, AudioControl, AudioPacket, AudioSource, OPUS_PT, PACKET_DURATION};
use crate::control::ControlMessage;
use crate::deps::{BindAddrs, LockSource, LockStatus, PowerSource, SignalSink};
use crate::error::EngineError;
use crate::estimator::{self, Published};
use crate::interceptor::is_keyframe_request;
use crate::peer::{self, H264_PTS, SSRC};
use crate::ports;
use crate::rate::RatePolicy;
use crate::sdp;
use crate::source::{EncodedFrame, SourceControl, SourceEnd, SourceInfo, SourceSpec, VideoSource};

pub use crate::source::WindowTarget;

/// Engine settings.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// ICE binds only inside this UDP range (`[firewall] udp_ports`).
    pub udp_ports: UdpPortRange,
    /// Frame rate cap.
    pub fps: u32,
    /// Keepalive: repeat the last picture after this long without new content. 450 ms, so a
    /// repeat leaves the host (encode and pacing included) within the 500 ms the plan requires.
    pub keepalive: Duration,
    /// Bandwidth estimator settings.
    pub estimator: estimator::Config,
    /// Set point policy (headroom, floor, ceiling, battery cap).
    pub rate: RatePolicy,
    /// Control-channel silence that pauses the stream (after the first ping).
    pub liveness_timeout: Duration,
    /// How long to wait for ICE gathering before sending the offer anyway.
    pub gather_timeout: Duration,
    /// How often the power source is read.
    pub power_poll: Duration,
    /// Insert the frame-id SEI into every access unit (latency probe; off in production).
    pub frame_id_sei: bool,
    /// The session's owner (WP-53's lifecycle) reports the end to the phone: when set, the
    /// engine sends neither `stream.state ended` nor the `error host_locked` that precedes a
    /// lock end. The owner reads [`Session::ended`] and reports the end itself, which lets it
    /// hold a session whose peer connection failed ([`EndReason::NetworkLost`]) for the resume
    /// grace instead of ending it. Off by default (the engine alone reports everything).
    pub owner_reports_end: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            udp_ports: UdpPortRange::DEFAULT,
            fps: 30,
            keepalive: Duration::from_millis(450),
            estimator: estimator::Config::default(),
            rate: RatePolicy::default(),
            liveness_timeout: Duration::from_secs(5),
            gather_timeout: Duration::from_secs(3),
            power_poll: Duration::from_secs(5),
            frame_id_sei: false,
            owner_reports_end: false,
        }
    }
}

/// The engine's collaborators (see [`crate::deps`]).
#[derive(Debug, Clone)]
pub struct EngineDeps {
    /// Session lock state (fail closed).
    pub lock: Arc<dyn LockSource>,
    /// Power source (battery cap).
    pub power: Arc<dyn PowerSource>,
    /// Addresses ICE binds.
    pub addrs: Arc<dyn BindAddrs>,
    /// Signalling to the phone.
    pub signal: Arc<dyn SignalSink>,
    /// Capture + encode.
    pub source: Arc<dyn VideoSource>,
    /// The window's sound (WP-55): `None` streams no audio and offers no audio track.
    pub audio: Option<Arc<dyn AudioSource>>,
}

/// Why a session paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PauseReason {
    /// The phone said it is not showing the stream.
    PhoneBackground,
    /// ICE disconnected, or the phone went silent on the control channel.
    Network,
    /// Input paused (WP-52): the captured window is not the foreground window. Video keeps
    /// flowing; the phone offers "Bring to front".
    FocusLost,
    /// Input paused (WP-52): someone is using the host's own keyboard or mouse. Video keeps
    /// flowing.
    LocalUserActive,
}

impl PauseReason {
    /// The `stream.state` reason on the wire.
    pub fn wire(self) -> StreamReason {
        match self {
            PauseReason::PhoneBackground => StreamReason::PhoneBackground,
            PauseReason::Network => StreamReason::Network,
            PauseReason::FocusLost => StreamReason::FocusLost,
            PauseReason::LocalUserActive => StreamReason::LocalUserActive,
        }
    }

    /// The reason as the `control` channel's `state` message spells it.
    pub fn text(self) -> &'static str {
        match self {
            PauseReason::PhoneBackground => "phone_background",
            PauseReason::Network => "network",
            PauseReason::FocusLost => "focus_lost",
            PauseReason::LocalUserActive => "local_user_active",
        }
    }
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// The phone closed it (`DELETE /stream`).
    ClientClosed,
    /// The owner used the kill switch.
    HostKillSwitch,
    /// The session locked (or its state became unknown).
    HostLocked,
    /// The window was closed.
    WindowClosed,
    /// The device was revoked.
    Revoked,
    /// A timeout (grace period, approval).
    Timeout,
    /// The peer connection failed (ICE `Failed`) or closed by itself: the phone is gone, for
    /// now. With [`EngineConfig::owner_reports_end`] the owner keeps the session for the resume
    /// grace (WP-53) and re-offers on a new engine session with the same id; without it this is
    /// an `error` end like before.
    NetworkLost,
    /// An error (ICE failed, codec, source).
    Failed(String),
}

impl EndReason {
    /// The `stream.state` reason on the wire. There is no `host_locked` reason in the protocol;
    /// a lock end is sent as `error` plus an `error { code: host_locked }` event.
    pub fn wire(&self) -> StreamReason {
        match self {
            EndReason::ClientClosed => StreamReason::ClientClosed,
            EndReason::HostKillSwitch => StreamReason::HostKillSwitch,
            EndReason::WindowClosed => StreamReason::WindowClosed,
            EndReason::Revoked => StreamReason::Revoked,
            EndReason::Timeout => StreamReason::Timeout,
            EndReason::HostLocked | EndReason::NetworkLost | EndReason::Failed(_) => {
                StreamReason::Error
            }
        }
    }
}

/// A session's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Offer sent, not connected yet.
    Negotiating,
    /// Media flowing.
    Streaming,
    /// Paused; the first reason that applies.
    Paused(PauseReason),
    /// Ended.
    Ended(EndReason),
}

/// The data channel a message arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// `input` (unordered, lossy).
    Input,
    /// `control` (ordered, reliable).
    Control,
}

/// Receives the phone's input messages (WP-52): every `input` message and every `control`
/// message that is not a liveness/pause message. Runs on a runtime thread: return quickly.
pub type InputHandler = Arc<dyn Fn(Channel, &[u8]) + Send + Sync>;

/// Why an IDR was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyCause {
    Start,
    Pli,
    Control,
    Reconnect,
    Resume,
    Api,
}

/// Keyframe requests by cause.
#[derive(Debug, Default)]
struct KeyCounters {
    start: AtomicU64,
    pli: AtomicU64,
    control: AtomicU64,
    reconnect: AtomicU64,
    resume: AtomicU64,
    api: AtomicU64,
}

/// Keyframe requests by cause (snapshot).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyframeRequests {
    /// First connection.
    pub start: u64,
    /// PLI or FIR from the phone.
    pub pli: u64,
    /// `{"t":"keyframe"}` on the control channel.
    pub control: u64,
    /// ICE back to `Connected` after a disconnection.
    pub reconnect: u64,
    /// Resume after a pause.
    pub resume: u64,
    /// [`Session::request_keyframe`].
    pub api: u64,
}

#[derive(Debug, Default)]
struct Counters {
    frames_sent: AtomicU64,
    bytes_sent: AtomicU64,
    write_errors: AtomicU64,
    keys: KeyCounters,
    setpoint_bps: AtomicU64,
    ceiling_bps: AtomicU64,
    on_battery: std::sync::atomic::AtomicBool,
    remote_candidates: AtomicU64,
    trickled_out: AtomicU64,
    connects: AtomicU64,
    liveness_pauses: AtomicU64,
    audio_packets_sent: AtomicU64,
    audio_bytes_sent: AtomicU64,
}

/// A snapshot of a session's numbers.
#[derive(Debug, Clone)]
pub struct SessionStats {
    /// State now.
    pub state: SessionState,
    /// Access units written to the track.
    pub frames_sent: u64,
    /// Bytes of access units written.
    pub bytes_sent: u64,
    /// New pictures encoded.
    pub frames_new: u64,
    /// Keepalive repeats encoded.
    pub frames_keepalive: u64,
    /// IDRs produced.
    pub keyframes_out: u64,
    /// Keyframe requests by cause.
    pub keyframe_requests: KeyframeRequests,
    /// Estimator target, bit/s.
    pub estimate_bps: f64,
    /// Estimator's queueing-delay estimate.
    pub queue_delay: Duration,
    /// Delivered rate seen by the estimator, bit/s.
    pub delivered_bps: f64,
    /// Loss over the last second.
    pub loss: f64,
    /// Set point given to the source, bit/s.
    pub setpoint_bps: u64,
    /// Set point the encoder runs at, bit/s.
    pub applied_bps: u64,
    /// Ceiling in force (policy, phone, battery), bit/s.
    pub ceiling_bps: u64,
    /// The battery cap applies.
    pub on_battery: bool,
    /// Encoder re-creations.
    pub recreations: u64,
    /// ICE socket addresses.
    pub local_addrs: Vec<SocketAddr>,
    /// Source facts.
    pub source: SourceInfo,
    /// Times ICE reached `Connected`.
    pub connects: u64,
    /// Pauses caused by control-channel silence.
    pub liveness_pauses: u64,
    /// The window's sound now (WP-55).
    pub audio: AudioStatus,
    /// Opus packets written to the audio track.
    pub audio_packets_sent: u64,
    /// Bytes of Opus written.
    pub audio_bytes_sent: u64,
    /// Opus packets the audio source produced.
    pub audio_packets_encoded: u64,
    /// Frames (per channel) captured from the application.
    pub audio_frames_captured: u64,
    /// Times the capture stream started (each activation).
    pub audio_starts: u64,
}

/// The window's sound, as the session reports it on `control` (WP-55).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStatus {
    /// Captured and sent now.
    pub on: bool,
    /// Why not: `muted`, `paused`, `unavailable` or `none`.
    pub reason: Option<&'static str>,
    /// Why the capture could not start (`unavailable`), for logs.
    pub detail: Option<String>,
}

enum Cmd {
    Answer(String),
    RemoteIce(String),
    Pause,
    Resume,
    InputPause(Option<PauseReason>),
    Keyframe(KeyCause),
    Control(ControlMessage),
    SendControl(String),
    Stop(EndReason),
}

enum PcEvent {
    State(RTCPeerConnectionState),
    Candidate(String),
    Gathered,
}

struct Handler {
    tx: mpsc::UnboundedSender<PcEvent>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(init) = event.candidate.to_json() {
            let c = init.candidate;
            let c = if c.starts_with("candidate:") {
                c
            } else {
                format!("candidate:{c}")
            };
            let _ = self.tx.send(PcEvent::Candidate(c));
        }
    }
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.tx.send(PcEvent::Gathered);
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        let _ = self.tx.send(PcEvent::State(s));
    }
}

struct Shared {
    id: SessionId,
    device: DeviceId,
    window: WindowTarget,
    cmd: mpsc::UnboundedSender<Cmd>,
    state: watch::Receiver<SessionState>,
    ctl: Arc<SourceControl>,
    info: SourceInfo,
    estimate: Arc<Published>,
    counters: Counters,
    local_addrs: Vec<SocketAddr>,
    input: RwLock<Option<InputHandler>>,
    /// The audio source's steering, when one started.
    audio_ctl: Option<Arc<AudioControl>>,
    /// What `control` last said about the sound.
    audio_status: Mutex<AudioStatus>,
}

/// A running (or ended) stream session. Cheap to clone.
#[derive(Clone)]
pub struct Session {
    shared: Arc<Shared>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.shared.id)
            .field("state", &*self.shared.state.borrow())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Session id (`ses_` + ULID).
    pub fn id(&self) -> &SessionId {
        &self.shared.id
    }

    /// The device it streams to.
    pub fn device(&self) -> &DeviceId {
        &self.shared.device
    }

    /// The captured window.
    pub fn window(&self) -> &WindowTarget {
        &self.shared.window
    }

    /// State now.
    pub fn state(&self) -> SessionState {
        self.shared.state.borrow().clone()
    }

    /// A receiver that follows the state.
    pub fn watch_state(&self) -> watch::Receiver<SessionState> {
        self.shared.state.clone()
    }

    /// Ends the session (idempotent).
    pub fn stop(&self, reason: EndReason) {
        let _ = self.shared.cmd.send(Cmd::Stop(reason));
    }

    /// Asks the encoder for an IDR.
    pub fn request_keyframe(&self) {
        let _ = self.shared.cmd.send(Cmd::Keyframe(KeyCause::Api));
    }

    /// Input paused (WP-52, `Some(FocusLost | LocalUserActive)`) or not (`None`): reported as
    /// `stream.state paused` when no stream-level pause (phone background, network) applies.
    /// Video is not stopped by an input pause.
    pub fn set_input_paused(&self, reason: Option<PauseReason>) {
        let _ = self.shared.cmd.send(Cmd::InputPause(reason));
    }

    /// Installs the input handler (WP-52).
    pub fn set_input_handler(&self, handler: InputHandler) {
        *self.shared.input.write().unwrap_or_else(|e| e.into_inner()) = Some(handler);
    }

    /// Sends a JSON text message on the `control` channel (clipboard replies, resize acks).
    pub fn send_control(&self, text: String) {
        let _ = self.shared.cmd.send(Cmd::SendControl(text));
    }

    /// The window's sound is being captured and sent now (WP-55; the host's "being viewed"
    /// text adds "with sound").
    pub fn audio_active(&self) -> bool {
        self.audio_status().on
    }

    /// The window's sound now (WP-55).
    pub fn audio_status(&self) -> AudioStatus {
        self.shared
            .audio_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Waits until the session has ended; returns why.
    pub async fn ended(&self) -> EndReason {
        let mut rx = self.shared.state.clone();
        loop {
            if let SessionState::Ended(r) = &*rx.borrow_and_update() {
                return r.clone();
            }
            if rx.changed().await.is_err() {
                if let SessionState::Ended(r) = &*rx.borrow() {
                    return r.clone();
                }
                return EndReason::Failed("session task gone".into());
            }
        }
    }

    /// The session's numbers now.
    pub fn stats(&self) -> SessionStats {
        let s = &self.shared;
        let c = &s.counters;
        let k = &c.keys;
        let st = &s.ctl.stats;
        let ld = |a: &AtomicU64| a.load(Ordering::Relaxed);
        SessionStats {
            state: self.state(),
            frames_sent: ld(&c.frames_sent),
            bytes_sent: ld(&c.bytes_sent),
            frames_new: ld(&st.frames_new),
            frames_keepalive: ld(&st.frames_keepalive),
            keyframes_out: ld(&st.keyframes_out),
            keyframe_requests: KeyframeRequests {
                start: ld(&k.start),
                pli: ld(&k.pli),
                control: ld(&k.control),
                reconnect: ld(&k.reconnect),
                resume: ld(&k.resume),
                api: ld(&k.api),
            },
            estimate_bps: s.estimate.target_bps(),
            queue_delay: s.estimate.queue_delay(),
            delivered_bps: s.estimate.delivered_bps(),
            loss: s.estimate.loss(),
            setpoint_bps: ld(&c.setpoint_bps),
            applied_bps: u64::from(st.applied_bps.load(Ordering::Relaxed)),
            ceiling_bps: ld(&c.ceiling_bps),
            on_battery: c.on_battery.load(Ordering::Relaxed),
            recreations: ld(&st.recreations),
            local_addrs: s.local_addrs.clone(),
            source: s.info.clone(),
            connects: ld(&c.connects),
            liveness_pauses: ld(&c.liveness_pauses),
            audio: self.audio_status(),
            audio_packets_sent: ld(&c.audio_packets_sent),
            audio_bytes_sent: ld(&c.audio_bytes_sent),
            audio_packets_encoded: s.audio_ctl.as_ref().map_or(0, |a| ld(&a.stats.packets_out)),
            audio_frames_captured: s
                .audio_ctl
                .as_ref()
                .map_or(0, |a| ld(&a.stats.frames_captured)),
            audio_starts: s.audio_ctl.as_ref().map_or(0, |a| ld(&a.stats.starts)),
        }
    }
}

/// Everything to start a session.
#[derive(Debug, Clone)]
pub struct StartRequest {
    /// The phone.
    pub device: DeviceId,
    /// The window (WP-51).
    pub window: WindowTarget,
    /// The phone's viewport.
    pub viewport: Viewport,
    /// The phone's `prefer.max_kbps`.
    pub max_kbps: Option<u32>,
    /// Use this id (WP-53 creates it for `POST /stream`); `None` makes a new one.
    pub session: Option<SessionId>,
}

struct Inner {
    cfg: EngineConfig,
    deps: EngineDeps,
    sessions: Mutex<HashMap<SessionId, Session>>,
}

/// The engine. Cheap to clone.
#[derive(Clone)]
pub struct StreamEngine {
    inner: Arc<Inner>,
}

impl fmt::Debug for StreamEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamEngine")
            .field("sessions", &self.sessions().len())
            .finish_non_exhaustive()
    }
}

impl StreamEngine {
    /// A new engine.
    pub fn new(cfg: EngineConfig, deps: EngineDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                deps,
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The settings.
    pub fn config(&self) -> &EngineConfig {
        &self.inner.cfg
    }

    /// Sessions carry the window's sound (WP-55): the engine has an audio source.
    pub fn has_audio(&self) -> bool {
        self.inner.deps.audio.is_some()
    }

    /// Starts streaming `window` to `device` (see the module docs).
    pub async fn start(
        &self,
        window: WindowTarget,
        viewport: Viewport,
        device: DeviceId,
    ) -> Result<Session, EngineError> {
        self.start_with(StartRequest {
            device,
            window,
            viewport,
            max_kbps: None,
            session: None,
        })
        .await
    }

    /// [`StreamEngine::start`] with every option.
    pub async fn start_with(&self, req: StartRequest) -> Result<Session, EngineError> {
        let inner = &self.inner;
        let cfg = &inner.cfg;
        let lock_rx = inner.deps.lock.subscribe();
        let lock_base = *lock_rx.borrow();
        if lock_base.locked || lock_rx.has_changed().is_err() {
            return Err(EngineError::HostLocked);
        }
        let ips = inner.deps.addrs.ice_ips();

        // Peer + offer. A port probed free can be taken before webrtc-rs binds it; retry then.
        let mut built = None;
        for _ in 0..3 {
            let addrs = ports::pick_udp_addrs(&ips, cfg.udp_ports)?;
            let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
            let hp = peer::build(
                &addrs,
                cfg.estimator,
                Arc::new(Handler { tx: ev_tx }),
                inner.deps.audio.is_some(),
            )
            .await?;
            let offer = hp.pc.create_offer(None).await?;
            hp.pc.set_local_description(offer).await?;
            let deadline = tokio::time::sleep(cfg.gather_timeout);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    ev = ev_rx.recv() => match ev {
                        Some(PcEvent::Gathered) | None => break,
                        Some(_) => {}
                    },
                    () = &mut deadline => break,
                }
            }
            let local = hp
                .pc
                .local_description()
                .await
                .ok_or_else(|| EngineError::WebRtc("no local description".into()))?;
            if ports::candidates_within(&local.sdp, &addrs) {
                built = Some((hp, addrs, local.sdp, ev_rx));
                break;
            }
            tracing::warn!("ICE did not bind the probed ports; retrying");
            let _ = hp.pc.close().await;
        }
        let Some((hp, addrs, offer_sdp, ev_rx)) = built else {
            return Err(EngineError::PortsExhausted {
                range: cfg.udp_ports,
                ip: ips.first().copied().ok_or(EngineError::NoBindAddress)?,
            });
        };

        // Source, paused until connected.
        let initial = cfg.rate.setpoint(
            cfg.estimator.initial_bps,
            inner.deps.power.on_battery(),
            req.max_kbps.map(|k| k.saturating_mul(1000)),
        );
        let ctl = Arc::new(SourceControl::new(initial));
        let (ftx, frx) = mpsc::channel::<EncodedFrame>(8);
        let spec = SourceSpec {
            window: req.window.clone(),
            viewport: (req.viewport.width, req.viewport.height),
            fps: cfg.fps,
            initial_bps: initial,
            keepalive: cfg.keepalive,
            frame_id_sei: cfg.frame_id_sei,
        };
        let source = Arc::clone(&inner.deps.source);
        let ctl2 = Arc::clone(&ctl);
        let started = tokio::task::spawn_blocking(move || source.start(&spec, ctl2, ftx))
            .await
            .map_err(|e| EngineError::Source(e.to_string()))
            .and_then(|r| r);
        let info = match started {
            Ok(i) => i,
            Err(e) => {
                let _ = hp.pc.close().await;
                return Err(e);
            }
        };
        if lock_rx.borrow().locks != lock_base.locks || lock_rx.borrow().locked {
            ctl.stop.store(true, Ordering::Release);
            let _ = hp.pc.close().await;
            return Err(EngineError::HostLocked);
        }

        // The window's sound (WP-55), inactive until the session streams. A capture that cannot
        // start leaves the session without sound; there is no other capture to fall back to.
        let (atx, arx) = mpsc::channel::<AudioPacket>(16);
        let (audio_ctl, audio_unavailable) = match &inner.deps.audio {
            Some(src) => {
                let actl = Arc::new(AudioControl::new());
                let (src, actl2, window) = (Arc::clone(src), Arc::clone(&actl), req.window.clone());
                let r = tokio::task::spawn_blocking(move || src.start(&window, actl2, atx))
                    .await
                    .map_err(|e| crate::audio::AudioUnavailable(e.to_string()))
                    .and_then(|r| r);
                match r {
                    Ok(info) => {
                        tracing::info!(pid = info.process_id, "audio: {}", info.description);
                        (Some(actl), None)
                    }
                    Err(e) => {
                        tracing::warn!("audio unavailable for this session: {e}");
                        (None, Some(e.0))
                    }
                }
            }
            None => (None, None),
        };

        let id = req
            .session
            .clone()
            .unwrap_or_else(|| SessionId::from(format!("ses_{}", ulid::Ulid::new())));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (state_tx, state_rx) = watch::channel(SessionState::Negotiating);
        let shared = Arc::new(Shared {
            id: id.clone(),
            device: req.device.clone(),
            window: req.window.clone(),
            cmd: cmd_tx,
            state: state_rx,
            ctl: Arc::clone(&ctl),
            info,
            estimate: Arc::clone(&hp.estimate),
            counters: Counters::default(),
            local_addrs: addrs,
            input: RwLock::new(None),
            audio_status: Mutex::new(AudioStatus {
                on: false,
                reason: Some(if audio_ctl.is_some() {
                    "paused"
                } else if audio_unavailable.is_some() {
                    "unavailable"
                } else {
                    "none"
                }),
                detail: audio_unavailable,
            }),
            audio_ctl,
        });
        let session = Session {
            shared: Arc::clone(&shared),
        };
        inner
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), session.clone());

        let offered: Vec<String> = sdp::candidate_lines(&offer_sdp);
        let mid = sdp::first_mid(&offer_sdp);
        inner.deps.signal.send(
            &req.device,
            ServerEvent::StreamOffer {
                session: id.clone(),
                sdp: offer_sdp,
            },
        );
        let task = SessionTask {
            engine: Arc::clone(inner),
            shared,
            peer: hp,
            state: state_tx,
            cmd_rx,
            ev_rx,
            frames: frx,
            audio_rx: Some(arx),
            lock_rx,
            lock_base,
            offered,
            mid,
            client_max_bps: req.max_kbps.map(|k| k.saturating_mul(1000)),
        };
        tokio::spawn(task.run());
        Ok(session)
    }

    /// A session by id.
    pub fn session(&self, id: &SessionId) -> Option<Session> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Every running session.
    pub fn sessions(&self) -> Vec<Session> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Ends every session (kill switch).
    pub fn stop_all(&self, reason: EndReason) {
        for s in self.sessions() {
            s.stop(reason.clone());
        }
    }

    /// Ends every session of one device (revocation).
    pub fn stop_device(&self, device: &DeviceId, reason: EndReason) {
        for s in self.sessions().into_iter().filter(|s| s.device() == device) {
            s.stop(reason.clone());
        }
    }

    /// Handles a client `stream.*` message from `/v1/events`; the returned event, if any, goes
    /// back on the same socket (the `StreamSignalling` contract of WP-17).
    pub fn handle_client_event(
        &self,
        device: &DeviceId,
        event: ClientEvent,
    ) -> Option<ServerEvent> {
        let (session, cmd) = match event {
            ClientEvent::Ping {} => return None,
            ClientEvent::StreamAnswer { session, sdp } => (session, Cmd::Answer(sdp)),
            ClientEvent::StreamIce { session, candidate } => (session, Cmd::RemoteIce(candidate)),
            ClientEvent::StreamPause { session } => (session, Cmd::Pause),
            ClientEvent::StreamResume { session } => (session, Cmd::Resume),
            // The window pick belongs to the session's owner (WP-53), before the engine starts.
            ClientEvent::StreamPick { session, .. } => {
                return match self.session(&session) {
                    Some(s) if s.device() == device => None,
                    _ => Some(unknown_session(session)),
                };
            }
            // Fit-to-phone belongs to WP-51; the encoder follows the window's size on its own.
            ClientEvent::StreamResize { session, .. } => {
                return match self.session(&session) {
                    Some(s) if s.device() == device => None,
                    _ => Some(unknown_session(session)),
                };
            }
        };
        match self.session(&session) {
            Some(s) if s.device() == device => {
                let _ = s.shared.cmd.send(cmd);
                None
            }
            _ => Some(unknown_session(session)),
        }
    }
}

fn unknown_session(session: SessionId) -> ServerEvent {
    ServerEvent::Error {
        code: ErrorCode::NotFound,
        message: "No such stream session.".to_owned(),
        session: Some(session),
    }
}

struct SessionTask {
    engine: Arc<Inner>,
    shared: Arc<Shared>,
    peer: peer::HostPeer,
    state: watch::Sender<SessionState>,
    cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    ev_rx: mpsc::UnboundedReceiver<PcEvent>,
    frames: mpsc::Receiver<EncodedFrame>,
    audio_rx: Option<mpsc::Receiver<AudioPacket>>,
    lock_rx: watch::Receiver<LockStatus>,
    lock_base: LockStatus,
    offered: Vec<String>,
    mid: Option<String>,
    client_max_bps: Option<u32>,
}

/// Mutable per-session state inside the task.
struct Live {
    connected: bool,
    ever_connected: bool,
    remote_set: bool,
    pending_ice: Vec<String>,
    phone_paused: bool,
    ice_down: bool,
    silent: bool,
    last_control: Option<Instant>,
    pt: u8,
    last_captured: Option<Instant>,
    on_battery: bool,
    last_power: Option<Instant>,
    reported: Option<SessionState>,
    /// Input-only pause (WP-52); does not stop video.
    input_paused: Option<PauseReason>,
    /// The phone muted the sound (WP-55).
    muted: bool,
    /// What `control` last said about the sound.
    audio_reported: Option<(bool, Option<&'static str>)>,
}

impl SessionTask {
    async fn run(mut self) {
        let shared = Arc::clone(&self.shared);
        let mut aux: Vec<JoinHandle<()>> = vec![
            spawn_rtcp_reader(Arc::clone(&self.peer.track), shared.cmd.clone()),
            spawn_dc_reader(
                Arc::clone(&self.peer.control),
                Channel::Control,
                Arc::clone(&shared),
            ),
            spawn_dc_reader(
                Arc::clone(&self.peer.input),
                Channel::Input,
                Arc::clone(&shared),
            ),
        ];
        if let Some(audio) = &self.peer.audio {
            aux.push(spawn_rtcp_drain(Arc::clone(audio), shared.cmd.clone()));
        }
        let mut live = Live {
            connected: false,
            ever_connected: false,
            remote_set: false,
            pending_ice: Vec::new(),
            phone_paused: false,
            ice_down: false,
            silent: false,
            last_control: None,
            pt: H264_PTS[0],
            last_captured: None,
            on_battery: self.engine.deps.power.on_battery(),
            last_power: Some(Instant::now()),
            reported: None,
            input_paused: None,
            muted: false,
            audio_reported: None,
        };
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let reason = loop {
            tokio::select! {
                biased;
                changed = self.lock_rx.changed() => {
                    // A closed lock source is an unknown state: fail closed. A lock that was
                    // undone before this task looked still counts (`locks` moved).
                    let now = *self.lock_rx.borrow();
                    if changed.is_err() || now.locked || now.locks != self.lock_base.locks {
                        break EndReason::HostLocked;
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break EndReason::Failed("engine dropped".into()) };
                    if let Some(end) = self.on_cmd(cmd, &mut live).await {
                        break end;
                    }
                }
                ev = self.ev_rx.recv() => {
                    let Some(ev) = ev else { break EndReason::Failed("peer connection gone".into()) };
                    if let Some(end) = self.on_pc_event(ev, &mut live) {
                        break end;
                    }
                }
                frame = self.frames.recv() => {
                    match frame {
                        Some(f) => self.send_frame(f, &mut live).await,
                        None => break match self.shared.ctl.end() {
                            Some(SourceEnd::WindowClosed) => EndReason::WindowClosed,
                            Some(SourceEnd::Error(e)) => EndReason::Failed(e),
                            None => EndReason::Failed("source ended".into()),
                        },
                    }
                }
                pkt = recv_audio(&mut self.audio_rx) => match pkt {
                    Some(p) => self.send_audio(p, &live).await,
                    None => {
                        // The capture thread ended (device gone): no sound from now on.
                        self.audio_rx = None;
                        if let Some(a) = &self.shared.audio_ctl {
                            a.stop();
                        }
                        self.update_audio(&mut live, true);
                    }
                },
                _ = tick.tick() => self.on_tick(&mut live),
            }
        };
        self.finish(reason, aux).await;
    }

    fn set_paused_state(&mut self, live: &mut Live) {
        let paused = !live.connected || live.phone_paused || live.ice_down || live.silent;
        let was = self.shared.ctl.paused.swap(paused, Ordering::AcqRel);
        if was && !paused {
            // Back to streaming: the phone needs an IDR to start (again).
            let cause = if !live.ever_connected {
                KeyCause::Start
            } else if self.shared.counters.connects.load(Ordering::Relaxed) > 1
                && live.reported == Some(SessionState::Paused(PauseReason::Network))
            {
                KeyCause::Reconnect
            } else {
                KeyCause::Resume
            };
            self.keyframe(cause);
            live.ever_connected = true;
        }
        let state = if !live.ever_connected {
            SessionState::Negotiating
        } else if live.phone_paused {
            SessionState::Paused(PauseReason::PhoneBackground)
        } else if live.ice_down || live.silent || !live.connected {
            SessionState::Paused(PauseReason::Network)
        } else if let Some(r) = live.input_paused {
            SessionState::Paused(r)
        } else {
            SessionState::Streaming
        };
        if live.reported.as_ref() != Some(&state) && state != SessionState::Negotiating {
            let (wire, reason, text) = match state {
                SessionState::Streaming => (StreamState::Streaming, None, ("streaming", None)),
                SessionState::Paused(r) => (
                    StreamState::Paused,
                    Some(r.wire()),
                    ("paused", Some(r.text())),
                ),
                _ => (
                    StreamState::Paused,
                    Some(StreamReason::Network),
                    ("paused", Some("network")),
                ),
            };
            self.engine.deps.signal.send(
                &self.shared.device,
                ServerEvent::StreamState(StreamStateEvent {
                    session: self.shared.id.clone(),
                    state: wire,
                    reason,
                    window: None,
                    candidates: vec![],
                }),
            );
            let msg = ControlMessage::State {
                state: text.0.to_owned(),
                reason: text.1.map(str::to_owned),
            };
            let control = Arc::clone(&self.peer.control);
            tokio::spawn(async move {
                let _ = control.send_text(&msg.to_text()).await;
            });
            live.reported = Some(state.clone());
            self.state.send_replace(state);
        }
        self.update_audio(live, false);
    }

    /// Opens or closes the audio gate and reports a change on `control` (WP-55). The gate is
    /// open only while the session streams (connected, the phone looking, `control` alive) and
    /// the phone has not muted it. `ended`: the capture is gone for good.
    fn update_audio(&mut self, live: &mut Live, ended: bool) {
        let streaming = live.ever_connected
            && live.connected
            && !live.phone_paused
            && !live.ice_down
            && !live.silent;
        let ctl = self
            .shared
            .audio_ctl
            .as_ref()
            .filter(|c| !ended && !c.is_stopped());
        let (on, reason): (bool, Option<&'static str>) = if ctl.is_none() {
            let why = if self.engine.deps.audio.is_none() {
                "none"
            } else {
                "unavailable"
            };
            (false, Some(why))
        } else if live.muted {
            (false, Some("muted"))
        } else if !streaming {
            (false, Some("paused"))
        } else {
            (true, None)
        };
        if let Some(c) = ctl {
            c.set_active(on);
        }
        {
            let mut st = self
                .shared
                .audio_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            st.on = on;
            st.reason = reason;
            if ended && st.detail.is_none() && self.engine.deps.audio.is_some() {
                st.detail = Some("the capture ended".to_owned());
            }
        }
        // The phone hears about it from the first `streaming` on.
        if !live.ever_connected || live.audio_reported == Some((on, reason)) {
            return;
        }
        live.audio_reported = Some((on, reason));
        let msg = ControlMessage::Audio {
            on,
            reason: reason.map(str::to_owned),
        };
        let control = Arc::clone(&self.peer.control);
        tokio::spawn(async move {
            let _ = control.send_text(&msg.to_text()).await;
        });
    }

    async fn send_audio(&self, p: AudioPacket, live: &Live) {
        let Some(track) = &self.peer.audio else {
            return;
        };
        if !live.connected
            || !self
                .shared
                .audio_ctl
                .as_ref()
                .is_some_and(|c| c.is_active())
        {
            return;
        }
        let len = p.data.len() as u64;
        let sample = Sample {
            data: p.data,
            duration: PACKET_DURATION,
            ..Sample::new(p.captured)
        };
        let c = &self.shared.counters;
        match track
            .sample_writer(AUDIO_SSRC, OPUS_PT)
            .write_sample(&sample)
            .await
        {
            Ok(()) => {
                c.audio_packets_sent.fetch_add(1, Ordering::Relaxed);
                c.audio_bytes_sent.fetch_add(len, Ordering::Relaxed);
            }
            Err(_) => {
                c.write_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn keyframe(&self, cause: KeyCause) {
        let k = &self.shared.counters.keys;
        let c = match cause {
            KeyCause::Start => &k.start,
            KeyCause::Pli => &k.pli,
            KeyCause::Control => &k.control,
            KeyCause::Reconnect => &k.reconnect,
            KeyCause::Resume => &k.resume,
            KeyCause::Api => &k.api,
        };
        c.fetch_add(1, Ordering::Relaxed);
        self.shared.ctl.request_keyframe();
    }

    async fn on_cmd(&mut self, cmd: Cmd, live: &mut Live) -> Option<EndReason> {
        match cmd {
            Cmd::Stop(r) => return Some(r),
            Cmd::Answer(answer) => {
                if live.remote_set {
                    self.error(ErrorCode::BadRequest, "The session already has an answer.");
                    return None;
                }
                let answer = sdp::keep_host_candidates(&answer);
                if !sdp::video_codecs(&answer)
                    .iter()
                    .any(|(_, n)| n.eq_ignore_ascii_case("H264"))
                {
                    self.error(
                        ErrorCode::StreamRefused,
                        "The phone offered no H.264 decoder; this computer streams H.264.",
                    );
                    return Some(EndReason::Failed(EngineError::CodecUnsupported.to_string()));
                }
                let desc = match RTCSessionDescription::answer(answer) {
                    Ok(d) => d,
                    Err(e) => {
                        self.error(ErrorCode::BadRequest, "The answer is not valid SDP.");
                        return Some(EndReason::Failed(e.to_string()));
                    }
                };
                if let Err(e) = self.peer.pc.set_remote_description(desc).await {
                    self.error(ErrorCode::BadRequest, "The answer was refused.");
                    return Some(EndReason::Failed(e.to_string()));
                }
                live.remote_set = true;
                live.pt = negotiated_pt(&self.peer.pc).await;
                for c in std::mem::take(&mut live.pending_ice) {
                    self.add_remote_ice(c).await;
                }
            }
            Cmd::RemoteIce(c) => {
                let trimmed = c.trim();
                if trimmed.is_empty() {
                    return None; // end-of-candidates
                }
                match sdp::parse_candidate(trimmed) {
                    Some(p) if p.typ == "host" => {}
                    _ => {
                        tracing::debug!("dropping a non-host or unreadable remote candidate");
                        return None;
                    }
                }
                if live.remote_set {
                    self.add_remote_ice(trimmed.to_owned()).await;
                } else if live.pending_ice.len() < 32 {
                    live.pending_ice.push(trimmed.to_owned());
                }
            }
            Cmd::Pause => {
                live.phone_paused = true;
                self.set_paused_state(live);
            }
            Cmd::InputPause(reason) => {
                live.input_paused = reason
                    .filter(|r| matches!(r, PauseReason::FocusLost | PauseReason::LocalUserActive));
                self.set_paused_state(live);
            }
            Cmd::Resume => {
                live.phone_paused = false;
                self.set_paused_state(live);
            }
            Cmd::Keyframe(cause) => self.keyframe(cause),
            Cmd::Control(msg) => {
                // The phone's first message proves `control` is open: (re)send the sound's
                // state, which may have gone out before the channel was (WP-55).
                if live.last_control.is_none() {
                    live.audio_reported = None;
                }
                live.last_control = Some(Instant::now());
                self.update_audio(live, false);
                if live.silent {
                    live.silent = false;
                    self.set_paused_state(live);
                }
                match msg {
                    ControlMessage::Ping { seq } => {
                        let control = Arc::clone(&self.peer.control);
                        tokio::spawn(async move {
                            let _ = control
                                .send_text(&ControlMessage::Pong { seq }.to_text())
                                .await;
                        });
                    }
                    ControlMessage::Pause => {
                        live.phone_paused = true;
                        self.set_paused_state(live);
                    }
                    ControlMessage::Resume => {
                        live.phone_paused = false;
                        self.set_paused_state(live);
                    }
                    ControlMessage::Keyframe => self.keyframe(KeyCause::Control),
                    ControlMessage::Mute { on } => {
                        live.muted = on;
                        self.update_audio(live, false);
                    }
                    ControlMessage::Pong { .. }
                    | ControlMessage::State { .. }
                    | ControlMessage::Audio { .. } => {}
                }
            }
            Cmd::SendControl(text) => {
                let control = Arc::clone(&self.peer.control);
                tokio::spawn(async move {
                    let _ = control.send_text(&text).await;
                });
            }
        }
        None
    }

    async fn add_remote_ice(&self, c: String) {
        let candidate = c.strip_prefix("a=").unwrap_or(&c).to_owned();
        let init = RTCIceCandidateInit {
            candidate,
            sdp_mid: self.mid.clone(),
            sdp_mline_index: Some(0),
            ..Default::default()
        };
        match self.peer.pc.add_ice_candidate(init).await {
            Ok(()) => {
                self.shared
                    .counters
                    .remote_candidates
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => tracing::debug!("add_ice_candidate: {e}"),
        }
    }

    fn on_pc_event(&mut self, ev: PcEvent, live: &mut Live) -> Option<EndReason> {
        match ev {
            PcEvent::Gathered => {}
            PcEvent::Candidate(c) => {
                if !self.offered.contains(&c) {
                    self.offered.push(c.clone());
                    self.shared
                        .counters
                        .trickled_out
                        .fetch_add(1, Ordering::Relaxed);
                    self.engine.deps.signal.send(
                        &self.shared.device,
                        ServerEvent::StreamIce {
                            session: self.shared.id.clone(),
                            candidate: c,
                        },
                    );
                }
            }
            PcEvent::State(s) => match s {
                RTCPeerConnectionState::Connected => {
                    self.shared
                        .counters
                        .connects
                        .fetch_add(1, Ordering::Relaxed);
                    live.connected = true;
                    live.ice_down = false;
                    self.set_paused_state(live);
                }
                RTCPeerConnectionState::Disconnected => {
                    live.ice_down = true;
                    self.set_paused_state(live);
                }
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                    return Some(EndReason::NetworkLost);
                }
                _ => {}
            },
        }
        None
    }

    async fn send_frame(&mut self, f: EncodedFrame, live: &mut Live) {
        if !live.connected || self.shared.ctl.is_paused() {
            return;
        }
        let fps = self.engine.cfg.fps.max(1);
        let duration = live
            .last_captured
            .map(|p| f.captured.saturating_duration_since(p))
            .filter(|d| !d.is_zero())
            .unwrap_or(Duration::from_secs(1) / fps)
            .min(Duration::from_secs(2));
        live.last_captured = Some(f.captured);
        let len = f.data.len() as u64;
        let sample = Sample {
            data: Bytes::clone(&f.data),
            duration,
            ..Sample::new(Instant::now())
        };
        let r = self
            .peer
            .track
            .sample_writer(SSRC, live.pt)
            .with_extension(HeaderExtension::PlayoutDelay(PlayoutDelayExtension {
                min_delay: 0,
                max_delay: 0,
            }))
            .write_sample(&sample)
            .await;
        let c = &self.shared.counters;
        match r {
            Ok(()) => {
                c.frames_sent.fetch_add(1, Ordering::Relaxed);
                c.bytes_sent.fetch_add(len, Ordering::Relaxed);
            }
            Err(_) => {
                c.write_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn on_tick(&mut self, live: &mut Live) {
        let cfg = &self.engine.cfg;
        let now = Instant::now();
        if live
            .last_power
            .is_none_or(|t| now.saturating_duration_since(t) >= cfg.power_poll)
        {
            live.on_battery = self.engine.deps.power.on_battery();
            live.last_power = Some(now);
        }
        let c = &self.shared.counters;
        let set = cfg.rate.setpoint(
            self.shared.estimate.target_bps(),
            live.on_battery,
            self.client_max_bps,
        );
        self.shared.ctl.target_bps.store(set, Ordering::Release);
        c.setpoint_bps.store(u64::from(set), Ordering::Relaxed);
        c.ceiling_bps.store(
            u64::from(cfg.rate.ceiling(live.on_battery, self.client_max_bps)),
            Ordering::Relaxed,
        );
        c.on_battery.store(live.on_battery, Ordering::Relaxed);
        if !live.silent
            && live
                .last_control
                .is_some_and(|t| now.saturating_duration_since(t) > cfg.liveness_timeout)
        {
            live.silent = true;
            c.liveness_pauses.fetch_add(1, Ordering::Relaxed);
            self.set_paused_state(live);
        }
    }

    fn error(&self, code: ErrorCode, message: &str) {
        self.engine.deps.signal.send(
            &self.shared.device,
            ServerEvent::Error {
                code,
                message: message.to_owned(),
                session: Some(self.shared.id.clone()),
            },
        );
    }

    async fn finish(self, reason: EndReason, aux: Vec<JoinHandle<()>>) {
        let ctl = &self.shared.ctl;
        ctl.paused.store(true, Ordering::Release);
        ctl.stop.store(true, Ordering::Release);
        if let Some(a) = &self.shared.audio_ctl {
            a.stop();
        }
        {
            let mut st = self
                .shared
                .audio_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            st.on = false;
            st.reason = Some("paused");
        }
        for h in aux {
            h.abort();
        }
        // Tell the phone first: the lock end must be visible even if closing stalls. With an
        // owner (WP-53), the owner tells it.
        if !self.engine.cfg.owner_reports_end {
            if reason == EndReason::HostLocked {
                self.error(
                    ErrorCode::HostLocked,
                    "The computer was locked; the stream ended.",
                );
            }
            self.engine.deps.signal.send(
                &self.shared.device,
                ServerEvent::StreamState(StreamStateEvent {
                    session: self.shared.id.clone(),
                    state: StreamState::Ended,
                    reason: Some(reason.wire()),
                    window: None,
                    candidates: vec![],
                }),
            );
        }
        // Out of the map before `Ended` is visible, and only if the entry is still this session:
        // an owner that restarts a session with the same id (WP-53 resume) as soon as it sees
        // `Ended` must not lose the new entry.
        {
            let mut sessions = self
                .engine
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if sessions
                .get(&self.shared.id)
                .is_some_and(|s| Arc::ptr_eq(&s.shared, &self.shared))
            {
                sessions.remove(&self.shared.id);
            }
        }
        self.state.send_replace(SessionState::Ended(reason));
        let pc = Arc::clone(&self.peer.pc);
        drop(self);
        let _ = tokio::time::timeout(Duration::from_secs(2), pc.close()).await;
    }
}

/// The payload type the answer picked for H.264 (the video sender's first H.264 codec; the
/// audio sender, WP-55, is skipped).
async fn negotiated_pt(pc: &Arc<dyn PeerConnection>) -> u8 {
    for s in pc.get_senders().await {
        if let Ok(p) = s.get_parameters().await
            && let Some(c) = p
                .rtp_parameters
                .codecs
                .iter()
                .find(|c| c.rtp_codec.mime_type.eq_ignore_ascii_case("video/H264"))
        {
            return c.payload_type;
        }
    }
    H264_PTS[0]
}

/// The next audio packet; never resolves without a receiver.
async fn recv_audio(rx: &mut Option<mpsc::Receiver<AudioPacket>>) -> Option<AudioPacket> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Reads (and drops) the audio track's RTCP so its event queue never grows.
fn spawn_rtcp_drain(
    track: Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>,
    cmd: mpsc::UnboundedSender<Cmd>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if track.poll().await.is_none() {
                if cmd.is_closed() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    })
}

fn spawn_rtcp_reader(
    track: Arc<webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample>,
    cmd: mpsc::UnboundedSender<Cmd>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // `poll` returns None until the track is bound (some time after Connected).
            let Some(ev) = track.poll().await else {
                if cmd.is_closed() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            };
            let TrackLocalEvent::OnRtcpPacket(pkts) = ev else {
                continue;
            };
            if pkts.iter().any(|p| is_keyframe_request(p.as_ref()))
                && cmd.send(Cmd::Keyframe(KeyCause::Pli)).is_err()
            {
                return;
            }
        }
    })
}

fn spawn_dc_reader(
    dc: Arc<dyn DataChannel>,
    which: Channel,
    shared: Arc<Shared>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(ev) = dc.poll().await {
            let DataChannelEvent::OnMessage(msg) = ev else {
                continue;
            };
            if which == Channel::Control
                && let Some(c) = ControlMessage::parse(&msg.data)
            {
                if shared.cmd.send(Cmd::Control(c)).is_err() {
                    return;
                }
                continue;
            }
            if which == Channel::Control {
                // Any control traffic proves liveness.
                let _ = shared
                    .cmd
                    .send(Cmd::Control(ControlMessage::Pong { seq: 0 }));
            }
            let handler = shared
                .input
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(h) = handler {
                h(which, &msg.data);
            }
        }
    })
}
