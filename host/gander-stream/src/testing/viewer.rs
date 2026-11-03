// SPDX-License-Identifier: Apache-2.0
//! The loopback receiver (from the WP-43 spike): a webrtc-rs peer that answers the host's
//! offer, depacketises the H.264 track into access units and timestamps each one, sends PLI on
//! loss, and speaks the `control` channel (pings, pause/resume).
//!
//! It plays the phone in the engine's tests: frame cadence, keyframes, playout-delay, liveness.
//! An audio track (WP-55) is decoded with the same Opus implementation the host encodes with
//! (libopus through `opusic-c`) into [`AudioRx::pcm`], mono, for tone detection.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use rtc::interceptor::Registry;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::{
    configure_rtcp_reports, configure_twcc_receiver_only,
};
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::extension::playout_delay_extension::PlayoutDelayExtension;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc::shared::marshal::Unmarshal;
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceCandidateInit,
    RTCIceGatheringState, RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription,
};

use crate::control::{CONTROL_LABEL, ControlMessage};
use crate::h264;
use crate::peer::{PLAYOUT_DELAY_URI, media_engine, setting_engine};
use crate::sdp;
use crate::testing::relay::Relay;

/// How the viewer answers.
#[derive(Debug, Clone)]
pub struct ViewerOptions {
    /// Remove every candidate from the answer (the libwebrtc behaviour WP-44 saw: the host must
    /// learn the viewer's address peer-reflexively).
    pub strip_candidates: bool,
    /// Send the answer without candidates and return them separately for `stream.ice`.
    pub trickle: bool,
    /// Route media through this relay.
    pub relay: Option<Arc<Relay>>,
    /// Send `{"t":"ping"}` on the control channel at this interval.
    pub ping_interval: Option<Duration>,
    /// Send a PLI when an access unit is lost (at most every 300 ms).
    pub auto_pli: bool,
    /// The viewer's bind address (the phone side; 127.0.0.1 in tests).
    pub bind: SocketAddr,
    /// Keep every access unit's bytes in [`FrameRx::data`] (Annex B, for a decoder; WP-57).
    pub keep_data: bool,
}

impl Default for ViewerOptions {
    fn default() -> Self {
        Self {
            strip_candidates: false,
            trickle: false,
            relay: None,
            ping_interval: Some(Duration::from_secs(1)),
            auto_pli: true,
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            keep_data: false,
        }
    }
}

/// One complete access unit as received.
#[derive(Debug, Clone)]
pub struct FrameRx {
    /// When its last packet arrived.
    pub at: Instant,
    /// RTP timestamp.
    pub rtp_ts: u32,
    /// Bytes of the access unit.
    pub bytes: usize,
    /// Holds an IDR.
    pub idr: bool,
    /// Damaged (a packet missing).
    pub damaged: bool,
    /// Playout-delay extension (min, max) in 10 ms units, when present.
    pub playout: Option<(u16, u16)>,
    /// The access unit itself (Annex B), with [`ViewerOptions::keep_data`].
    pub data: Option<bytes::Bytes>,
}

/// What the viewer saw.
#[derive(Debug, Default)]
pub struct ViewerStats {
    /// Every access unit, in arrival order.
    pub frames: Mutex<Vec<FrameRx>>,
    /// PLIs sent.
    pub plis_sent: AtomicU64,
    /// RTP payload bytes received.
    pub rx_bytes: AtomicU64,
    /// Control-channel messages received (pongs, states).
    pub control_rx: Mutex<Vec<ControlMessage>>,
    /// Connection states, in order.
    pub states: Mutex<Vec<RTCPeerConnectionState>>,
    /// Every control-channel text received, raw (WP-52: `input.refused`, `front.result`).
    pub control_text_rx: Mutex<Vec<String>>,
    /// The `input` channel once opened (WP-52).
    pub input_dc: DcSlot,
    /// The audio track as received (WP-55).
    pub audio: Mutex<AudioRx>,
}

/// What arrived on the audio track (WP-55).
#[derive(Debug, Default, Clone)]
pub struct AudioRx {
    /// RTP packets received.
    pub packets: u64,
    /// Opus bytes received.
    pub bytes: u64,
    /// Packets that did not decode.
    pub decode_errors: u64,
    /// Sequence gaps (lost packets).
    pub lost: u64,
    /// Arrival time of every packet, in order.
    pub arrivals: Vec<Instant>,
    /// Decoded audio, mono (the two channels averaged), 48 kHz; at most 60 s are kept.
    pub pcm: Vec<f32>,
    /// The payload type of the first packet.
    pub payload_type: Option<u8>,
}

impl AudioRx {
    /// Samples of [`AudioRx::pcm`] kept at most.
    pub const MAX_PCM: usize = 48_000 * 60;
}

/// A data channel slot (WP-52 input tests).
#[derive(Default)]
pub struct DcSlot(Mutex<Option<Arc<dyn DataChannel>>>);

impl std::fmt::Debug for DcSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DcSlot")
    }
}

impl ViewerStats {
    /// A copy of what the audio track delivered so far.
    pub fn audio(&self) -> AudioRx {
        self.audio.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A copy of the frames so far.
    pub fn frames(&self) -> Vec<FrameRx> {
        self.frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Received payload rate over the `window` before now, bit/s.
    pub fn rate_bps(&self, window: Duration) -> f64 {
        let now = Instant::now();
        let bytes: usize = self
            .frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .rev()
            .take_while(|f| now.saturating_duration_since(f.at) <= window)
            .map(|f| f.bytes)
            .sum();
        bytes as f64 * 8.0 / window.as_secs_f64()
    }
}

enum Ev {
    Gathered,
    Candidate(String),
    Track(Arc<dyn TrackRemote>),
    Dc(Arc<dyn DataChannel>),
}

struct Handler {
    tx: mpsc::UnboundedSender<Ev>,
    stats: Arc<ViewerStats>,
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
            let _ = self.tx.send(Ev::Candidate(c));
        }
    }
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.tx.send(Ev::Gathered);
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        self.stats
            .states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(s);
    }
    async fn on_track(&self, t: Arc<dyn TrackRemote>) {
        let _ = self.tx.send(Ev::Track(t));
    }
    async fn on_data_channel(&self, d: Arc<dyn DataChannel>) {
        let _ = self.tx.send(Ev::Dc(d));
    }
}

/// A running loopback viewer.
pub struct Viewer {
    pc: Arc<dyn PeerConnection>,
    /// What it saw.
    pub stats: Arc<ViewerStats>,
    control: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    track: Arc<Mutex<Option<Arc<dyn TrackRemote>>>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pinging: Arc<std::sync::atomic::AtomicBool>,
    mid: Option<String>,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer").finish_non_exhaustive()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// The answer to hand to the engine, plus any candidates to trickle as `stream.ice`.
#[derive(Debug, Clone)]
pub struct Answer {
    /// SDP.
    pub sdp: String,
    /// Candidates to send separately (trickle mode).
    pub candidates: Vec<String>,
}

impl Viewer {
    /// Answers `offer` (as the host sent it).
    pub async fn answer(offer: &str, opts: ViewerOptions) -> anyhow::Result<(Viewer, Answer)> {
        let mut media = media_engine().map_err(|e| anyhow::anyhow!("{e}"))?;
        let registry = configure_twcc_receiver_only(Registry::new(), &mut media)?;
        let registry = configure_rtcp_reports(registry);
        let stats = Arc::new(ViewerStats::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let pc = PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::new().build())
            .with_media_engine(media)
            .with_setting_engine(setting_engine(&[opts.bind]))
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(Handler {
                tx,
                stats: Arc::clone(&stats),
            }))
            .with_udp_addrs(vec![opts.bind])
            .build()
            .await?;
        let pc: Arc<dyn PeerConnection> = Arc::new(pc);
        let offer = match &opts.relay {
            Some(r) => r
                .munge_offer(offer)
                .ok_or_else(|| anyhow::anyhow!("offer has no single candidate address"))?,
            None => offer.to_owned(),
        };
        pc.set_remote_description(RTCSessionDescription::offer(offer)?)
            .await?;
        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer).await?;
        let mut candidates = Vec::new();
        let mut pending = Vec::new();
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(Ev::Gathered) | None => break,
                    Some(Ev::Candidate(c)) => candidates.push(c),
                    Some(other) => pending.push(other),
                },
                () = &mut deadline => break,
            }
        }
        let local = pc
            .local_description()
            .await
            .ok_or_else(|| anyhow::anyhow!("no local description"))?;
        let mut sdp_text = local.sdp;
        if let Some(r) = &opts.relay {
            sdp_text = r
                .munge_answer(&sdp_text)
                .ok_or_else(|| anyhow::anyhow!("answer has no single candidate address"))?;
        }
        let mid = sdp::first_mid(&sdp_text);
        let extmap_id = playout_extmap(&sdp_text);
        let answer = if opts.strip_candidates {
            Answer {
                sdp: sdp::strip_candidates(&sdp_text),
                candidates: vec![],
            }
        } else if opts.trickle {
            let lines = sdp::candidate_lines(&sdp_text);
            Answer {
                sdp: sdp::strip_candidates(&sdp_text),
                candidates: if lines.is_empty() { candidates } else { lines },
            }
        } else {
            Answer {
                sdp: sdp_text,
                candidates: vec![],
            }
        };

        let control: Arc<Mutex<Option<Arc<dyn DataChannel>>>> = Arc::default();
        let track_slot: Arc<Mutex<Option<Arc<dyn TrackRemote>>>> = Arc::default();
        let pinging = Arc::new(std::sync::atomic::AtomicBool::new(
            opts.ping_interval.is_some(),
        ));
        let mut tasks = Vec::new();
        // Event pump: tracks and data channels.
        {
            let stats = Arc::clone(&stats);
            let control = Arc::clone(&control);
            let track_slot = Arc::clone(&track_slot);
            let auto_pli = opts.auto_pli;
            let keep_data = opts.keep_data;
            tasks.push(tokio::spawn(async move {
                let mut sub = Vec::new();
                for ev in pending {
                    on_event(
                        ev,
                        &stats,
                        &control,
                        &track_slot,
                        auto_pli,
                        keep_data,
                        extmap_id,
                        &mut sub,
                    );
                }
                while let Some(ev) = rx.recv().await {
                    on_event(
                        ev,
                        &stats,
                        &control,
                        &track_slot,
                        auto_pli,
                        keep_data,
                        extmap_id,
                        &mut sub,
                    );
                }
                for t in sub {
                    t.abort();
                }
            }));
        }
        if let Some(every) = opts.ping_interval {
            let control = Arc::clone(&control);
            let pinging = Arc::clone(&pinging);
            tasks.push(tokio::spawn(async move {
                let mut seq = 0u64;
                let mut tick = tokio::time::interval(every);
                loop {
                    tick.tick().await;
                    if !pinging.load(Ordering::Relaxed) {
                        continue;
                    }
                    let dc = control.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(dc) = dc {
                        seq += 1;
                        let _ = dc.send_text(&ControlMessage::Ping { seq }.to_text()).await;
                    }
                }
            }));
        }
        Ok((
            Viewer {
                pc,
                stats,
                control,
                track: track_slot,
                tasks,
                pinging,
                mid,
            },
            answer,
        ))
    }

    /// Adds a host candidate received as `stream.ice`.
    pub async fn add_host_candidate(&self, candidate: &str) -> anyhow::Result<()> {
        self.pc
            .add_ice_candidate(RTCIceCandidateInit {
                candidate: candidate.to_owned(),
                sdp_mid: self.mid.clone(),
                sdp_mline_index: Some(0),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Sends a control message (waits up to 5 s for the channel to open).
    pub async fn control_send(&self, msg: &ControlMessage) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let dc = self
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(dc) = dc {
                dc.send_text(&msg.to_text()).await?;
                return Ok(());
            }
            anyhow::ensure!(Instant::now() < deadline, "control channel never opened");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Sends raw text on `control` (WP-52 input messages; waits up to 5 s for the channel).
    pub async fn control_send_text(&self, text: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let dc = self
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(dc) = dc {
                dc.send_text(text).await?;
                return Ok(());
            }
            anyhow::ensure!(Instant::now() < deadline, "control channel never opened");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Sends raw text on `input` (WP-52; waits up to 5 s for the channel).
    pub async fn input_send_text(&self, text: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let dc = self
                .stats
                .input_dc
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(dc) = dc {
                dc.send_text(text).await?;
                return Ok(());
            }
            anyhow::ensure!(Instant::now() < deadline, "input channel never opened");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Mutes (`true`) or unmutes the window's sound (WP-55, `{"t":"mute"}`).
    pub async fn send_mute(&self, on: bool) -> anyhow::Result<()> {
        self.control_send(&ControlMessage::Mute { on }).await
    }

    /// Stops or restarts the periodic pings (a phone that went silent).
    pub fn set_pinging(&self, on: bool) {
        self.pinging.store(on, Ordering::Relaxed);
    }

    /// Sends one PLI now.
    pub async fn send_pli(&self) -> anyhow::Result<()> {
        let t = self.track.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let t = t.ok_or_else(|| anyhow::anyhow!("no track yet"))?;
        t.write_rtcp(vec![Box::new(PictureLossIndication {
            sender_ssrc: 0,
            media_ssrc: crate::peer::SSRC,
        })])
        .await?;
        self.stats.plis_sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Waits until the connection is `Connected` (or `timeout`).
    pub async fn wait_connected(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self
                .stats
                .states
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&RTCPeerConnectionState::Connected)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// Closes the connection.
    pub async fn close(&self) {
        let _ = self.pc.close().await;
    }
}

fn playout_extmap(sdp: &str) -> Option<u8> {
    sdp.lines().find_map(|l| {
        let rest = l.strip_prefix("a=extmap:")?;
        let (id, uri) = rest.split_once(' ')?;
        (uri.trim() == PLAYOUT_DELAY_URI)
            .then(|| id.split('/').next()?.parse().ok())
            .flatten()
    })
}

#[allow(clippy::too_many_arguments)]
fn on_event(
    ev: Ev,
    stats: &Arc<ViewerStats>,
    control: &Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    track_slot: &Arc<Mutex<Option<Arc<dyn TrackRemote>>>>,
    auto_pli: bool,
    keep_data: bool,
    extmap_id: Option<u8>,
    sub: &mut Vec<tokio::task::JoinHandle<()>>,
) {
    match ev {
        Ev::Track(t) => {
            let stats = Arc::clone(stats);
            let track_slot = Arc::clone(track_slot);
            sub.push(tokio::spawn(async move {
                if t.kind().await == RtpCodecKind::Audio {
                    read_audio(t, stats).await;
                    return;
                }
                *track_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&t));
                read_track(t, stats, auto_pli, keep_data, extmap_id).await;
            }));
        }
        Ev::Dc(d) => {
            let stats = Arc::clone(stats);
            let control = Arc::clone(control);
            sub.push(tokio::spawn(async move {
                let label = d.label().await.unwrap_or_default();
                let is_control = label == CONTROL_LABEL;
                if is_control {
                    *control.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&d));
                } else if label == crate::control::INPUT_LABEL {
                    *stats.input_dc.0.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Arc::clone(&d));
                }
                while let Some(ev) = d.poll().await {
                    if let DataChannelEvent::OnMessage(m) = &ev
                        && is_control
                    {
                        stats
                            .control_text_rx
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(String::from_utf8_lossy(&m.data).into_owned());
                    }
                    if let DataChannelEvent::OnMessage(m) = ev
                        && is_control
                        && let Some(c) = ControlMessage::parse(&m.data)
                    {
                        stats
                            .control_rx
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(c);
                    }
                }
            }));
        }
        Ev::Gathered | Ev::Candidate(_) => {}
    }
}

async fn read_track(
    track: Arc<dyn TrackRemote>,
    stats: Arc<ViewerStats>,
    auto_pli: bool,
    keep_data: bool,
    extmap_id: Option<u8>,
) {
    let mut depack = H264Packet::default();
    let mut au = BytesMut::new();
    let mut au_lost = false;
    let mut cur_ts: Option<u32> = None;
    let mut expected: Option<u16> = None;
    let mut last_pli: Option<Instant> = None;
    let mut playout = None;
    while let Some(ev) = track.poll().await {
        let TrackRemoteEvent::OnRtpPacket(p) = ev else {
            continue;
        };
        let seq = p.header.sequence_number;
        let ahead = expected.map_or(0, |e| seq.wrapping_sub(e));
        if ahead >= 0x8000 {
            continue; // late or duplicate
        }
        expected = Some(seq.wrapping_add(1));
        stats
            .rx_bytes
            .fetch_add(p.payload.len() as u64, Ordering::Relaxed);
        if cur_ts != Some(p.header.timestamp) {
            au.clear();
            au_lost = false;
            depack = H264Packet::default();
            cur_ts = Some(p.header.timestamp);
            playout = None;
        }
        if ahead != 0 {
            au_lost = true;
        }
        if let Some(id) = extmap_id
            && let Some(mut b) = p.header.get_extension(id)
            && let Ok(x) = PlayoutDelayExtension::unmarshal(&mut b)
        {
            playout = Some((x.min_delay, x.max_delay));
        }
        match depack.depacketize(&p.payload) {
            Ok(b) => au.extend_from_slice(&b),
            Err(_) => au_lost = true,
        }
        if !p.header.marker {
            continue;
        }
        let data = std::mem::take(&mut au);
        stats
            .frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(FrameRx {
                at: Instant::now(),
                rtp_ts: p.header.timestamp,
                bytes: data.len(),
                idr: h264::is_idr(&data),
                damaged: au_lost,
                playout,
                data: keep_data.then(|| data.freeze()),
            });
        cur_ts = None;
        if auto_pli && au_lost && last_pli.is_none_or(|l| l.elapsed() > Duration::from_millis(300))
        {
            last_pli = Some(Instant::now());
            if track
                .write_rtcp(vec![Box::new(PictureLossIndication {
                    sender_ssrc: 0,
                    media_ssrc: p.header.ssrc,
                })])
                .await
                .is_ok()
            {
                stats.plis_sent.fetch_add(1, Ordering::Relaxed);
            }
        }
        au_lost = false;
    }
}

/// Decodes the Opus track into [`AudioRx::pcm`].
async fn read_audio(track: Arc<dyn TrackRemote>, stats: Arc<ViewerStats>) {
    use crate::audio::{CHANNELS, FRAME};
    let Ok(mut dec) =
        opusic_c::Decoder::new(opusic_c::Channels::Stereo, opusic_c::SampleRate::Hz48000)
    else {
        return;
    };
    // Up to 120 ms per packet.
    let mut out = vec![0.0f32; FRAME * 6 * CHANNELS];
    let mut expected: Option<u16> = None;
    while let Some(ev) = track.poll().await {
        let TrackRemoteEvent::OnRtpPacket(p) = ev else {
            continue;
        };
        let seq = p.header.sequence_number;
        let ahead = expected.map_or(0, |e| seq.wrapping_sub(e));
        if ahead >= 0x8000 {
            continue;
        }
        expected = Some(seq.wrapping_add(1));
        let decoded = dec.decode_float_to_slice(&p.payload, &mut out, false);
        let mut a = stats.audio.lock().unwrap_or_else(|e| e.into_inner());
        a.packets += 1;
        a.bytes += p.payload.len() as u64;
        a.lost += u64::from(ahead);
        a.arrivals.push(Instant::now());
        a.payload_type.get_or_insert(p.header.payload_type);
        match decoded {
            Ok(n) => {
                if a.pcm.len() + n <= AudioRx::MAX_PCM {
                    a.pcm.extend(
                        out[..n * CHANNELS]
                            .chunks_exact(CHANNELS)
                            .map(|s| (s[0] + s[1]) / 2.0),
                    );
                }
            }
            Err(_) => a.decode_errors += 1,
        }
    }
}

/// Largest gap between consecutive received access units in `frames` after `from`.
pub fn max_gap(frames: &[FrameRx], from: Instant) -> Option<Duration> {
    let sel: Vec<&FrameRx> = frames.iter().filter(|f| f.at >= from).collect();
    sel.windows(2)
        .map(|w| w[1].at.saturating_duration_since(w[0].at))
        .max()
}
