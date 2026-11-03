// SPDX-License-Identifier: Apache-2.0
//! The host's WebRTC peer: media engine, interceptor chain, ICE settings, the video track and
//! the two data channels.
//!
//! - **Codecs**: H.264 only, offered twice in preference order: constrained baseline level 5.0
//!   (`42e032`, needed above 8192 macroblocks) then level 3.1 (`42e01f`), both with
//!   `level-asymmetry-allowed=1` and `packetization-mode=1`; feedback `nack pli`, `ccm fir`,
//!   `transport-cc`. No generic NACK (WP-43: retransmissions compete with media on a congested
//!   link; the viewer asks for an IDR instead). HEVC is not offered: the track's packetizer is fixed
//!   when the track is created, before the answer names a codec (open question in the report).
//! - **Header extensions**: transport-wide CC (for the estimator) and playout-delay, which every
//!   video packet carries with min = max = 0 (render as soon as decoded; libwebrtc honours it).
//! - **Interceptors**: TWCC sender + pacer + [`crate::estimator::DelayEstimator`]
//!   (`configure_congestion_control`), sender/receiver reports, and
//!   [`crate::interceptor::KeyframeRequests`].
//! - **ICE**: host candidates only, on the exact `ip:port`s given (see [`crate::ports`]); no ICE
//!   servers, mDNS disabled, UDP only.
//! - **Audio** (WP-55, when the engine has an audio source): Opus 48 kHz stereo, payload type
//!   111, `minptime=10;useinbandfec=1`, a `sendonly` audio transceiver after the video one, in the
//!   same BUNDLE (one transport). The Android viewer (libwebrtc) answers it `recvonly` and plays it.
//! - **Transceiver**: `sendonly` video; data channels `input` (unordered, 0 retransmits) and
//!   `control` (ordered, reliable).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use rtc::ice::mdns::MulticastDnsMode;
use rtc::ice::network_type::NetworkType;
use rtc::interceptor::{Registry, Slot};
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::{
    CongestionFeedback, configure_congestion_control, configure_rtcp_reports,
};
use rtc::peer_connection::configuration::media_engine::{
    MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine,
};
use rtc::peer_connection::configuration::setting_engine::{
    SctpMaxMessageSize, SettingEngine, SettingEngineBuilder,
};
use rtc::rtp_transceiver::rtp_sender::{
    RTCPFeedback, RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters,
    RTCRtpEncodingParameters, RTCRtpHeaderExtensionCapability, RtpCodecKind,
};
use rtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use webrtc::data_channel::DataChannel;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::peer_connection::{PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler};

use crate::audio::{AUDIO_SSRC, OPUS_PT};
use crate::control::{CONTROL_LABEL, INPUT_LABEL};
use crate::error::EngineError;
use crate::estimator::{self, DelayEstimator, Published};
use crate::h264;
use crate::interceptor::{KeyframeRequests, SLOT};

/// The URI of the playout-delay header extension.
pub const PLAYOUT_DELAY_URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay";

/// Payload types of the offered H.264 entries.
pub const H264_PTS: [u8; 2] = [102, 103];

/// The track's SSRC ("GAND").
pub const SSRC: u32 = 0x4741_4E44;

fn h264(pt: u8, profile_level_id: &str) -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: h264::fmtp(profile_level_id),
            rtcp_feedback: vec![
                RTCPFeedback {
                    typ: "nack".into(),
                    parameter: "pli".into(),
                },
                RTCPFeedback {
                    typ: "ccm".into(),
                    parameter: "fir".into(),
                },
            ],
        },
        payload_type: pt,
    }
}

/// The codecs the host offers, in preference order.
pub fn offered_codecs() -> Vec<RTCRtpCodecParameters> {
    vec![
        h264(H264_PTS[0], h264::PROFILE_LEVEL_5_0),
        h264(H264_PTS[1], h264::PROFILE_LEVEL_3_1),
    ]
}

/// The Opus entry of the audio track (WP-55): 48 kHz, two channels (always 2 in SDP), FEC.
pub fn opus_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: OPUS_PT,
    }
}

/// Media engine with the H.264 entries, Opus and the playout-delay extension.
pub fn media_engine() -> Result<MediaEngine, EngineError> {
    let mut media = MediaEngine::default();
    for c in offered_codecs() {
        media
            .register_codec(c, RtpCodecKind::Video)
            .map_err(|e| EngineError::WebRtc(e.to_string()))?;
    }
    media
        .register_codec(opus_codec(), RtpCodecKind::Audio)
        .map_err(|e| EngineError::WebRtc(e.to_string()))?;
    media
        .register_header_extension(
            RTCRtpHeaderExtensionCapability {
                uri: PLAYOUT_DELAY_URI.to_owned(),
            },
            RtpCodecKind::Video,
            None,
        )
        .map_err(|e| EngineError::WebRtc(e.to_string()))?;
    Ok(media)
}

/// The largest data-channel message the session negotiates: 256 KiB, the SCTP ceiling (the
/// default is 64 KiB). A clipboard text of 64 KiB (WP-56) is longer than that once JSON has
/// escaped it. Every other message is a few hundred bytes.
pub const MAX_DATA_CHANNEL_MESSAGE: u32 = 256 * 1024;

/// ICE settings: no mDNS, UDP of the families present in `addrs`; data-channel messages up to
/// [`MAX_DATA_CHANNEL_MESSAGE`].
pub fn setting_engine(addrs: &[SocketAddr]) -> SettingEngine {
    let mut types = Vec::new();
    if addrs.iter().any(SocketAddr::is_ipv4) {
        types.push(NetworkType::Udp4);
    }
    if addrs.iter().any(SocketAddr::is_ipv6) {
        types.push(NetworkType::Udp6);
    }
    SettingEngineBuilder::new()
        .with_multicast_dns_mode(MulticastDnsMode::Disabled)
        .with_network_types(types)
        .with_sctp_max_message_size(SctpMaxMessageSize::Bounded(MAX_DATA_CHANNEL_MESSAGE))
        .build()
}

/// The host side of one session, freshly built.
pub struct HostPeer {
    /// The connection.
    pub pc: Arc<dyn PeerConnection>,
    /// The video track.
    pub track: Arc<TrackLocalStaticSample>,
    /// The audio track (WP-55), when the session offers audio.
    pub audio: Option<Arc<TrackLocalStaticSample>>,
    /// `input` channel.
    pub input: Arc<dyn DataChannel>,
    /// `control` channel.
    pub control: Arc<dyn DataChannel>,
    /// The estimator's published numbers.
    pub estimate: Arc<Published>,
}

impl std::fmt::Debug for HostPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostPeer").finish_non_exhaustive()
    }
}

/// Builds the host peer bound to exactly `addrs`; with `audio`, an Opus track too.
pub async fn build(
    addrs: &[SocketAddr],
    est: estimator::Config,
    handler: Arc<dyn PeerConnectionEventHandler>,
    audio: bool,
) -> Result<HostPeer, EngineError> {
    let mut media = media_engine()?;
    let estimator = DelayEstimator::new(est);
    let estimate = estimator.published();
    let registry = configure_congestion_control(
        Registry::new(),
        estimator,
        CongestionFeedback::Twcc,
        &mut media,
    )
    .map_err(|e| EngineError::WebRtc(e.to_string()))?;
    let registry = configure_rtcp_reports(registry);
    let registry = registry.with(Slot::from(SLOT), KeyframeRequests::default());

    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(media)
        .with_setting_engine(setting_engine(addrs))
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_udp_addrs(addrs.to_vec())
        .build()
        .await?;
    let pc: Arc<dyn PeerConnection> = Arc::new(pc);

    let codec = offered_codecs().remove(0);
    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "gander".to_owned(),
            "window".to_owned(),
            "window".to_owned(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(SSRC),
                    ..Default::default()
                },
                codec: codec.rtp_codec.clone(),
                ..Default::default()
            }],
        ),
    )?);
    pc.add_transceiver_from_track(
        Arc::clone(&track) as Arc<dyn TrackLocal>,
        Some(RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Sendonly,
            streams: vec![],
            send_encodings: vec![],
        }),
    )
    .await?;
    let audio = if audio {
        let track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "gander".to_owned(),
                "sound".to_owned(),
                "sound".to_owned(),
                RtpCodecKind::Audio,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(AUDIO_SSRC),
                        ..Default::default()
                    },
                    codec: opus_codec().rtp_codec,
                    ..Default::default()
                }],
            ),
        )?);
        pc.add_transceiver_from_track(
            Arc::clone(&track) as Arc<dyn TrackLocal>,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Sendonly,
                streams: vec![],
                send_encodings: vec![],
            }),
        )
        .await?;
        Some(track)
    } else {
        None
    };
    let input = pc
        .create_data_channel(
            INPUT_LABEL,
            Some(rtc::data_channel::RTCDataChannelInit {
                ordered: false,
                max_retransmits: Some(0),
                ..Default::default()
            }),
        )
        .await?;
    let control = pc.create_data_channel(CONTROL_LABEL, None).await?;
    Ok(HostPeer {
        pc,
        track,
        audio,
        input,
        control,
        estimate,
    })
}
