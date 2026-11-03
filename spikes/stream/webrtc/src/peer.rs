// SPDX-License-Identifier: Apache-2.0
//! The two WebRTC peers (webrtc-rs 0.21): the host sends the H.264 track and owns the `input`
//! data channel; the viewer receives, depacketises, decodes and sends input JSON.
//!
//! ICE: host candidates only, gathered on one explicit address (127.0.0.1, ephemeral port). No
//! ICE servers (no STUN/TURN traffic), mDNS disabled, UDP over IPv4 only. Congestion control:
//! TWCC feedback + Google Congestion Control + pacer on the host
//! (`configure_congestion_control`); the estimate is published to [`Metrics`] and the source
//! thread turns it into the encoder's `MeanBitRate`.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::{Bytes, BytesMut};
use rtc::ice::mdns::MulticastDnsMode;
use rtc::ice::network_type::NetworkType;
use rtc::interceptor::{
    Attribute, BandwidthEstimator, EstimatorStats, Gcc, Interceptor, Packet, PacketReport,
    Registry, Slot, StreamInfo, TaggedPacket,
};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::{
    CongestionFeedback, configure_congestion_control, configure_nack, configure_rtcp_reports,
    configure_twcc_receiver_only,
};
use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_H264, MediaEngine};
use rtc::peer_connection::configuration::setting_engine::{SettingEngine, SettingEngineBuilder};
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::packetizer::Depacketizer;
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RtpCodecKind,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::{TrackLocal, TrackLocalEvent};
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState,
};

use crate::au;
use crate::metrics::{Metrics, now_100ns};
use crate::relay::{self, Relay};
use crate::signal::{self, Signal};

/// One encoded access unit on its way to the track.
#[derive(Debug)]
pub struct EncodedFrame {
    /// Frame id (also in the SEI).
    pub id: u64,
    /// Capture time (QPC 100 ns).
    pub cap: i64,
    /// Annex-B access unit with the frame-id SEI.
    pub data: Vec<u8>,
}

/// H.264 `fmtp` line. Level 5.0 (`42e032`): 1080x2280 is 9724 macroblocks per frame, over
/// level 4.x's 8192 (WP-42 finding); constrained baseline, packetization mode 1.
pub const H264_FMTP: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e032";
const H264_PT: u8 = 102;

/// GCC settings (bps).
#[derive(Debug, Clone, Copy)]
pub struct GccConfig {
    /// Start estimate.
    pub initial: f64,
    /// Floor.
    pub min: f64,
    /// Ceiling.
    pub max: f64,
}

/// What the host saw, for the report.
#[derive(Debug, Default, Clone)]
pub struct HostReport {
    /// Candidate lines of the host's offer as generated (before any relay rewrite).
    pub offer_candidates: Vec<String>,
    /// Candidate lines of the viewer's answer as received (before any relay rewrite).
    pub answer_candidates: Vec<String>,
    /// Full offer SDP.
    pub offer_sdp: String,
    /// Full answer SDP.
    pub answer_sdp: String,
    /// Time from WebSocket accept to `Connected`, ms.
    pub connect_ms: f64,
}

/// Called for every JSON message on the `input` channel, after the echo has been sent: the
/// message and the host receive time (QPC, 100 ns). It runs on a blocking thread.
pub type InputHook = Arc<dyn Fn(&Value, i64) + Send + Sync>;

/// Extras for [`run_host`] beyond the loopback test of WP-43 (`Default` = the WP-43 behaviour).
#[derive(Default, Clone)]
pub struct HostOptions {
    /// Advertise this address in the offer's candidates instead of the bound one (the port is
    /// kept). The Android emulator reaches the host's loopback as 10.0.2.2, so the host still
    /// binds 127.0.0.1 only and tells the phone to send to 10.0.2.2.
    pub advertise_ip: Option<IpAddr>,
    /// Drop the viewer's candidates from its answer before applying it. The host then learns the
    /// viewer's address from the first STUN request it receives (peer-reflexive), which is the
    /// only path that exists when the viewer sits behind the emulator's NAT.
    pub strip_answer_candidates: bool,
    /// Receives the viewer's input messages (WP-44: injects taps into the window).
    pub on_input: Option<InputHook>,
}

impl fmt::Debug for HostOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostOptions")
            .field("advertise_ip", &self.advertise_ip)
            .field("strip_answer_candidates", &self.strip_answer_candidates)
            .field("on_input", &self.on_input.is_some())
            .finish()
    }
}

fn h264_codec(nack: bool) -> RTCRtpCodecParameters {
    let mut fb = vec![
        RTCPFeedback {
            typ: "nack".into(),
            parameter: "pli".into(),
        },
        RTCPFeedback {
            typ: "ccm".into(),
            parameter: "fir".into(),
        },
    ];
    if nack {
        fb.insert(
            0,
            RTCPFeedback {
                typ: "nack".into(),
                parameter: String::new(),
            },
        );
    }
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: H264_FMTP.to_owned(),
            rtcp_feedback: fb,
        },
        payload_type: H264_PT,
    }
}

fn settings() -> SettingEngine {
    SettingEngineBuilder::new()
        .with_multicast_dns_mode(MulticastDnsMode::Disabled)
        .with_network_types(vec![NetworkType::Udp4])
        .build()
}

/// Only this address is bound for ICE: loopback, ephemeral port. (A real host adds its tailnet
/// address here; never a wildcard.)
const ICE_ADDR: &str = "127.0.0.1:0";

/// Wraps GCC and publishes its numbers to [`Metrics`] after every update.
struct ReportingGcc {
    inner: Gcc,
    m: Arc<Metrics>,
}

impl ReportingGcc {
    fn publish(&self) {
        let s = self.inner.stats();
        self.m.gcc_target.set(self.inner.target_bitrate());
        self.m
            .gcc_delay
            .set(s.delay_based_bitrate.unwrap_or(f64::NAN));
        self.m
            .gcc_loss
            .set(s.loss_based_bitrate.unwrap_or(f64::NAN));
    }
}

impl BandwidthEstimator for ReportingGcc {
    fn on_reports(&mut self, now: Instant, reports: &[PacketReport]) {
        self.inner.on_reports(now, reports);
        self.publish();
    }
    fn target_bitrate(&self) -> f64 {
        self.inner.target_bitrate()
    }
    fn handle_timeout(&mut self, now: Instant) {
        self.inner.handle_timeout(now);
        self.publish();
    }
    fn poll_timeout(&self) -> Option<Instant> {
        self.inner.poll_timeout()
    }
    fn stats(&self) -> EstimatorStats {
        self.inner.stats()
    }
}

/// Marks inbound PLI/FIR for delivery to the application.
///
/// rtc 0.21 ends every inbound RTCP packet at the chain's terminus unless an interceptor attaches
/// `Attribute::DeliverToApplication` (measured: without this, `TrackLocal::poll` never yields a
/// PLI). Placed application-most (slot 14 000), after every interceptor that reads RTCP.
#[derive(Default)]
struct KeyframeRequests {
    reads: std::collections::VecDeque<TaggedPacket>,
    writes: std::collections::VecDeque<TaggedPacket>,
}

impl rtc::sansio::Protocol<TaggedPacket, TaggedPacket, ()> for KeyframeRequests {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = rtc::shared::error::Error;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(pkts) = &msg.message.packet
            && pkts.iter().any(|p| {
                let a = p.as_any();
                a.is::<PictureLossIndication>() || a.is::<FullIntraRequest>()
            })
        {
            msg.message.add(Attribute::DeliverToApplication);
        }
        self.reads.push_back(msg);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        self.reads.pop_front()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        self.writes.push_back(msg);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.writes.pop_front()
    }
}

impl Interceptor for KeyframeRequests {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}

/// Event handler that forwards the few events the spike needs.
struct Events {
    gathered: mpsc::UnboundedSender<()>,
    state: mpsc::UnboundedSender<RTCPeerConnectionState>,
    track: Option<mpsc::UnboundedSender<Arc<dyn TrackRemote>>>,
    dc: Option<mpsc::UnboundedSender<Arc<dyn DataChannel>>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_gathering_state_change(&self, s: RTCIceGatheringState) {
        if s == RTCIceGatheringState::Complete {
            let _ = self.gathered.send(());
        }
    }
    async fn on_connection_state_change(&self, s: RTCPeerConnectionState) {
        let _ = self.state.send(s);
    }
    async fn on_track(&self, t: Arc<dyn TrackRemote>) {
        if let Some(tx) = &self.track {
            let _ = tx.send(t);
        }
    }
    async fn on_data_channel(&self, d: Arc<dyn DataChannel>) {
        if let Some(tx) = &self.dc {
            let _ = tx.send(d);
        }
    }
}

async fn wait_connected(rx: &mut mpsc::UnboundedReceiver<RTCPeerConnectionState>) -> Result<()> {
    let deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            s = rx.recv() => match s {
                Some(RTCPeerConnectionState::Connected) => return Ok(()),
                Some(RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) | None => {
                    bail!("peer connection failed")
                }
                Some(_) => {}
            },
            () = &mut deadline => bail!("not connected after 15 s"),
        }
    }
}

/// The host: accepts one viewer on `listener`, offers the track and the `input` channel, then
/// sends every frame from `frames` until the channel closes or `m.stop` is set.
// One argument over clippy's limit since WP-44 added `opts`; a parameter struct would churn the
// WP-43 call site for no gain in a spike.
#[allow(clippy::too_many_arguments)]
pub async fn run_host(
    listener: TcpListener,
    mut frames: mpsc::Receiver<EncodedFrame>,
    m: Arc<Metrics>,
    relay: Option<Arc<Relay>>,
    gcc: GccConfig,
    nack: bool,
    opts: HostOptions,
    connected: tokio::sync::oneshot::Sender<HostReport>,
) -> Result<()> {
    let mut media = MediaEngine::default();
    let codec = h264_codec(nack);
    media.register_codec(codec.clone(), RtpCodecKind::Video)?;
    let estimator = ReportingGcc {
        inner: Gcc::new(gcc.initial, gcc.min, gcc.max),
        m: m.clone(),
    };
    m.gcc_target.set(gcc.initial);
    // Diagnostic switch: SPIKE_NO_CC=1 leaves congestion control out (fixed rate, no pacer).
    let registry = if std::env::var_os("SPIKE_NO_CC").is_some() {
        Registry::new()
    } else {
        configure_congestion_control(
            Registry::new(),
            estimator,
            CongestionFeedback::Twcc,
            &mut media,
        )?
    };
    let registry = configure_rtcp_reports(registry);
    let registry = if nack {
        configure_nack(registry, &mut media)
    } else {
        registry
    };
    let registry = registry.with(Slot::from(14_000), KeyframeRequests::default());

    let (g_tx, mut g_rx) = mpsc::unbounded_channel();
    let (s_tx, mut s_rx) = mpsc::unbounded_channel();
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(media)
        .with_setting_engine(settings())
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Events {
            gathered: g_tx,
            state: s_tx,
            track: None,
            dc: None,
        }))
        .with_udp_addrs(vec![ICE_ADDR.to_owned()])
        .build()
        .await?;

    let ssrc: u32 = 0x4741_4E44; // "GAND"
    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "gander".to_owned(),
            "window".to_owned(),
            "window".to_owned(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec: codec.rtp_codec.clone(),
                ..Default::default()
            }],
        ),
    )?);
    let sender = pc
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
        .await?;
    let dc = pc.create_data_channel("input", None).await?;

    // Signalling.
    let mut ch = signal::accept(&listener).await?;
    let t_accept = Instant::now();
    match ch.recv().await? {
        Signal::Hello { .. } => {}
        other => bail!("expected hello, got {other:?}"),
    }
    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;
    tokio::time::timeout(Duration::from_secs(5), g_rx.recv())
        .await
        .context("ICE gathering did not complete")?;
    let local = pc
        .local_description()
        .await
        .context("no local description")?;
    let mut report = HostReport {
        offer_candidates: relay::candidates(&local.sdp),
        offer_sdp: local.sdp.clone(),
        ..Default::default()
    };
    let offer_sdp = match (&relay, opts.advertise_ip) {
        (Some(r), _) => r.munge_offer(&local.sdp)?,
        (None, Some(ip)) => relay::rewrite_candidate_ip(&local.sdp, ip)?,
        (None, None) => local.sdp.clone(),
    };
    ch.send(&Signal::Offer { sdp: offer_sdp }).await?;
    let answer = match ch.recv().await? {
        Signal::Answer { sdp } => sdp,
        other => bail!("expected answer, got {other:?}"),
    };
    report.answer_candidates = relay::candidates(&answer);
    report.answer_sdp = answer.clone();
    let answer = match &relay {
        Some(r) => r.munge_answer(&answer)?,
        None if opts.strip_answer_candidates => relay::strip_candidates(&answer),
        None => answer,
    };
    pc.set_remote_description(RTCSessionDescription::answer(answer)?)
        .await?;
    wait_connected(&mut s_rx).await?;
    report.connect_ms = t_accept.elapsed().as_secs_f64() * 1000.0;
    let pt = sender
        .get_parameters()
        .await?
        .rtp_parameters
        .codecs
        .first()
        .map_or(H264_PT, |c| c.payload_type);

    // RTCP from the viewer: PLI / FIR -> IDR.
    {
        let track = track.clone();
        let m = m.clone();
        tokio::spawn(async move {
            // `poll` returns None until the track is bound (webrtc 0.21 binds it some time after
            // `Connected`), so retry instead of ending at the first None.
            while !m.stop.load(Ordering::Relaxed) {
                let Some(ev) = track.poll().await else {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                };
                if let TrackLocalEvent::OnRtcpPacket(pkts) = ev {
                    for p in pkts {
                        m.rtcp_kinds
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(format!("{:?}", p.header().packet_type));
                        let a = p.as_any();
                        if a.is::<PictureLossIndication>() || a.is::<FullIntraRequest>() {
                            m.plis_recv.fetch_add(1, Ordering::Relaxed);
                            m.force_key.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
        });
    }
    // Input channel: echo every JSON message back with the host receive time.
    {
        let dc = dc.clone();
        let m = m.clone();
        let hook = opts.on_input.clone();
        tokio::spawn(async move {
            while let Some(ev) = dc.poll().await {
                if let DataChannelEvent::OnMessage(msg) = ev {
                    let host_rx = now_100ns();
                    let Ok(mut v) = serde_json::from_slice::<Value>(&msg.data) else {
                        continue;
                    };
                    let original = v.clone();
                    let kind = v.get("t").and_then(Value::as_str).unwrap_or("").to_owned();
                    v["t"] = json!("echo");
                    v["of"] = json!(kind);
                    v["host_rx"] = json!(host_rx);
                    // The echo goes out first: the phone's round trip must not include the
                    // injection.
                    if dc.send_text(&v.to_string()).await.is_ok() {
                        m.dc_echoed.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Some(h) = &hook {
                        let h = h.clone();
                        tokio::task::spawn_blocking(move || h(&original, host_rx));
                    }
                }
            }
        });
    }
    let _ = connected.send(report);

    // Media.
    let mut prev_cap: Option<i64> = None;
    loop {
        let f = tokio::select! {
            f = frames.recv() => f,
            s = s_rx.recv() => match s {
                Some(RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) | None => None,
                _ => continue,
            },
        };
        let Some(f) = f else { break };
        let dur_100ns = prev_cap.map_or(333_333, |p| (f.cap - p).clamp(10_000, 2_000_000));
        prev_cap = Some(f.cap);
        let sample = Sample {
            data: Bytes::from(f.data),
            duration: Duration::from_nanos(dur_100ns as u64 * 100),
            ..Sample::new(Instant::now())
        };
        track.sample_writer(ssrc, pt).write_sample(&sample).await?;
        let t = now_100ns();
        m.with_frame(f.id, |r| r.sent = Some(t));
        if m.stop.load(Ordering::Relaxed) {
            break;
        }
    }
    let _ = ch.send(&Signal::Bye).await;
    let _ = pc.close().await;
    Ok(())
}

/// The viewer: connects to the host's signalling socket, answers, receives the track and feeds
/// clean access units to `decode`; sends a tap every 100 ms on the `input` channel.
pub async fn run_viewer(
    addr: std::net::SocketAddr,
    m: Arc<Metrics>,
    decode: std::sync::mpsc::Sender<(u64, Vec<u8>)>,
    nack: bool,
) -> Result<()> {
    let mut media = MediaEngine::default();
    media.register_codec(h264_codec(nack), RtpCodecKind::Video)?;
    // TWCC arrival reports (for the host's GCC) and sender/receiver reports; NACK only on request.
    let registry = configure_twcc_receiver_only(Registry::new(), &mut media)?;
    let registry = configure_rtcp_reports(registry);
    let registry = if nack {
        configure_nack(registry, &mut media)
    } else {
        registry
    };
    let (g_tx, mut g_rx) = mpsc::unbounded_channel();
    let (s_tx, mut s_rx) = mpsc::unbounded_channel();
    let (t_tx, mut t_rx) = mpsc::unbounded_channel();
    let (d_tx, mut d_rx) = mpsc::unbounded_channel();
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(media)
        .with_setting_engine(settings())
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Events {
            gathered: g_tx,
            state: s_tx,
            track: Some(t_tx),
            dc: Some(d_tx),
        }))
        .with_udp_addrs(vec![ICE_ADDR.to_owned()])
        .build()
        .await?;

    let mut ch = signal::connect(addr).await?;
    ch.send(&Signal::Hello {
        client: "spike-webrtc-viewer".into(),
        version: 1,
    })
    .await?;
    let offer = match ch.recv().await? {
        Signal::Offer { sdp } => sdp,
        other => bail!("expected offer, got {other:?}"),
    };
    pc.set_remote_description(RTCSessionDescription::offer(offer)?)
        .await?;
    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer).await?;
    tokio::time::timeout(Duration::from_secs(5), g_rx.recv())
        .await
        .context("ICE gathering did not complete")?;
    let local = pc
        .local_description()
        .await
        .context("no local description")?;
    ch.send(&Signal::Answer { sdp: local.sdp }).await?;
    wait_connected(&mut s_rx).await?;

    // Input channel.
    let dc_task = {
        let m = m.clone();
        tokio::spawn(async move {
            let Some(dc) = d_rx.recv().await else { return };
            let tapper = {
                let dc = dc.clone();
                tokio::spawn(async move {
                    let mut seq = 0u64;
                    let mut tick = tokio::time::interval(Duration::from_millis(100));
                    loop {
                        tick.tick().await;
                        seq += 1;
                        // Normalised to the streamed picture (0..1, origin top-left).
                        let x = 0.25 + 0.5 * ((seq % 10) as f64 / 10.0);
                        let msg = json!({"t":"tap","seq":seq,"x":x,"y":0.5,"ts":now_100ns()});
                        let _ = dc.send_text(&msg.to_string()).await;
                    }
                })
            };
            while let Some(ev) = dc.poll().await {
                if let DataChannelEvent::OnMessage(msg) = ev {
                    let now = now_100ns();
                    if let Ok(v) = serde_json::from_slice::<Value>(&msg.data)
                        && v.get("t").and_then(Value::as_str) == Some("echo")
                        && let Some(ts) = v.get("ts").and_then(Value::as_i64)
                    {
                        m.dc_rtt_ms
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push((now - ts) as f64 / 10_000.0);
                    }
                }
            }
            tapper.abort();
        })
    };

    // Video.
    let track = tokio::time::timeout(Duration::from_secs(10), t_rx.recv())
        .await
        .context("no track")?
        .context("no track")?;
    let mut media_ssrc;
    let mut depack = H264Packet::default();
    let mut au = BytesMut::new();
    let mut au_lost = false;
    let mut cur_ts: Option<u32> = None;
    let mut expected: Option<u16> = None;
    let mut broken = true; // wait for the first IDR
    let mut last_pli: Option<Instant> = None;
    loop {
        let ev = tokio::select! {
            ev = track.poll() => ev,
            s = s_rx.recv() => match s {
                Some(RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) | None => None,
                _ => continue,
            },
        };
        let Some(ev) = ev else { break };
        let TrackRemoteEvent::OnRtpPacket(p) = ev else {
            continue;
        };
        media_ssrc = p.header.ssrc;
        let seq = p.header.sequence_number;
        let ahead = expected.map_or(0, |e| seq.wrapping_sub(e));
        if ahead >= 0x8000 {
            // Older than expected: a NACK retransmission or a duplicate. This receiver does not
            // repair access units, so late packets are counted and ignored.
            m.rx_late.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let gap = ahead != 0;
        if gap {
            m.rx_pkts_lost
                .fetch_add(u64::from(ahead), Ordering::Relaxed);
        }
        expected = Some(seq.wrapping_add(1));
        m.rx_bytes
            .fetch_add(p.payload.len() as u64, Ordering::Relaxed);
        if cur_ts != Some(p.header.timestamp) {
            if !au.is_empty() {
                // The previous access unit never saw its marker packet.
                m.rx_aus_lost.fetch_add(1, Ordering::Relaxed);
                broken = true;
            }
            au.clear();
            au_lost = false;
            depack = H264Packet::default();
            cur_ts = Some(p.header.timestamp);
        }
        if gap {
            au_lost = true;
        }
        match depack.depacketize(&p.payload) {
            Ok(b) => au.extend_from_slice(&b),
            Err(_) => au_lost = true,
        }
        if !p.header.marker {
            continue;
        }
        let t = now_100ns();
        let data = std::mem::take(&mut au).to_vec();
        let mut want_pli = false;
        if au_lost {
            m.rx_aus_lost.fetch_add(1, Ordering::Relaxed);
            broken = true;
            want_pli = true;
        } else if broken && !au::is_idr(&data) {
            m.rx_aus_skipped.fetch_add(1, Ordering::Relaxed);
            want_pli = true;
        } else {
            broken = false;
            let id = au::parse_sei(&data).map(|(id, _)| id);
            if let Some(id) = id {
                m.with_frame(id, |f| f.recv = Some(t));
                let _ = decode.send((id, data));
            }
        }
        au_lost = false;
        cur_ts = None;
        if want_pli && last_pli.is_none_or(|l| l.elapsed() > Duration::from_millis(300)) {
            last_pli = Some(Instant::now());
            m.plis_sent.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = track
                .write_rtcp(vec![Box::new(PictureLossIndication {
                    sender_ssrc: 0,
                    media_ssrc,
                })])
                .await
            {
                m.rtcp_kinds
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("viewer PLI write error: {e}"));
            }
        }
    }
    dc_task.abort();
    let _ = pc.close().await;
    Ok(())
}
