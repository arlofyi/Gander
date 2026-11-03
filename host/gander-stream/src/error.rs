// SPDX-License-Identifier: Apache-2.0
//! Engine errors.

use std::net::IpAddr;

use gander_core::config::UdpPortRange;
use gander_core::error::ErrorCode;

/// Why the engine refused or failed an operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The session is locked (or its state is unknown, which counts as locked).
    #[error("the host session is locked")]
    HostLocked,
    /// No address to bind ICE on (the listener plan is empty).
    #[error("no address to bind host candidates on")]
    NoBindAddress,
    /// Refused a wildcard address.
    #[error("refusing to bind the unspecified address {0}")]
    Wildcard(IpAddr),
    /// Every port of the configured UDP range is in use on this address.
    #[error("every UDP port in {range} is in use on {ip}")]
    PortsExhausted {
        /// The configured range.
        range: UdpPortRange,
        /// The address that ran out.
        ip: IpAddr,
    },
    /// The window could not be captured or encoded.
    #[error("video source: {0}")]
    Source(String),
    /// WebRTC setup or negotiation failed.
    #[error("webrtc: {0}")]
    WebRtc(String),
    /// The phone's answer has no codec the host can send.
    #[error("no common video codec (the host sends H.264)")]
    CodecUnsupported,
    /// No such session (or it belongs to another device).
    #[error("no such stream session")]
    UnknownSession,
}

impl EngineError {
    /// The protocol error code for an `error` event or an HTTP answer.
    pub fn code(&self) -> ErrorCode {
        match self {
            EngineError::HostLocked => ErrorCode::HostLocked,
            EngineError::UnknownSession => ErrorCode::NotFound,
            EngineError::CodecUnsupported => ErrorCode::StreamRefused,
            EngineError::Source(_) => ErrorCode::AppUnavailable,
            EngineError::NoBindAddress
            | EngineError::Wildcard(_)
            | EngineError::PortsExhausted { .. }
            | EngineError::WebRtc(_) => ErrorCode::StreamUnavailable,
        }
    }
}

impl From<webrtc::error::Error> for EngineError {
    fn from(e: webrtc::error::Error) -> Self {
        EngineError::WebRtc(e.to_string())
    }
}
