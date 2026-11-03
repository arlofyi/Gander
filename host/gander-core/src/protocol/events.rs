// SPDX-License-Identifier: Apache-2.0
//! WebSocket messages on `GET /v1/events`: JSON text frames `{"type": ..., ...}`.

use serde::{Deserialize, Serialize};

use super::ids::{SessionId, ShareId, Timestamp};
use super::stream::{StreamReason, StreamState};
use crate::error::ErrorCode;

/// A message from the host to the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    /// First message after the upgrade.
    #[serde(rename = "hello")]
    Hello {
        /// Protocol version.
        protocol: u32,
        /// Host clock.
        #[serde(with = "time::serde::rfc3339")]
        host_time: Timestamp,
    },
    /// Something under `path` in `share` changed (debounced 500 ms).
    #[serde(rename = "share.changed")]
    ShareChanged {
        /// Share id.
        share: ShareId,
        /// Share-relative directory that changed.
        path: String,
    },
    /// The owner added or removed a share or changed permissions; re-fetch `/shares`.
    #[serde(rename = "shares.updated")]
    SharesUpdated {},
    /// This device was revoked; the socket closes next.
    #[serde(rename = "device.revoked")]
    DeviceRevoked {},
    /// Host state relevant to Stream mode.
    #[serde(rename = "host.state")]
    HostState(HostStateEvent),
    /// Stream session progress.
    #[serde(rename = "stream.state")]
    StreamState(StreamStateEvent),
    /// WebRTC offer from the host (the host is the offerer).
    #[serde(rename = "stream.offer")]
    StreamOffer {
        /// Session id.
        session: SessionId,
        /// SDP offer.
        sdp: String,
    },
    /// ICE candidate from the host (host candidates only).
    #[serde(rename = "stream.ice")]
    StreamIce {
        /// Session id.
        session: SessionId,
        /// Candidate line.
        candidate: String,
    },
    /// Answer to a client `ping`.
    #[serde(rename = "pong")]
    Pong {},
    /// A client message was refused (WP-17). The socket stays open.
    ///
    /// Not in the protocol spec's event list, which has no way to refuse a stream signalling
    /// message: Phase 1 answers every `stream.*` client message with `code: stream_unavailable`.
    /// Clients that do not know the type ignore it (compatibility rule).
    #[serde(rename = "error")]
    Error {
        /// Why, using the HTTP error codes.
        code: ErrorCode,
        /// English, human readable, no paths.
        message: String,
        /// The stream session the refused message named, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
    },
    /// An event type this build does not know; clients ignore it.
    #[serde(other)]
    Unknown,
}

/// `host.state` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostStateEvent {
    /// The session is locked.
    pub locked: bool,
    /// Someone is actively using the computer.
    pub attended: bool,
    /// Running on battery.
    pub on_battery: bool,
    /// A stream could start now.
    pub stream_available: bool,
}

/// `stream.state` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamStateEvent {
    /// Session id.
    pub session: SessionId,
    /// New state.
    pub state: StreamState,
    /// Why (for `paused` and `ended`); `null` otherwise.
    pub reason: Option<StreamReason>,
    /// The captured window, once found.
    pub window: Option<StreamWindow>,
    /// Candidate window titles when `state` is `pick`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

/// The captured window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamWindow {
    /// Window title.
    pub title: String,
    /// Client width in physical pixels.
    pub width: u32,
    /// Client height in physical pixels.
    pub height: u32,
    /// Monitor DPI.
    pub dpi: u32,
}

/// A message from the client to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientEvent {
    /// WebRTC answer.
    #[serde(rename = "stream.answer")]
    StreamAnswer {
        /// Session id.
        session: SessionId,
        /// SDP answer.
        sdp: String,
    },
    /// ICE candidate from the client.
    #[serde(rename = "stream.ice")]
    StreamIce {
        /// Session id.
        session: SessionId,
        /// Candidate line.
        candidate: String,
    },
    /// Fit-to-phone resize request.
    #[serde(rename = "stream.resize")]
    StreamResize {
        /// Session id.
        session: SessionId,
        /// New viewport width in physical pixels.
        width: u32,
        /// New viewport height in physical pixels.
        height: u32,
    },
    /// Pause (for example, the app went to the background).
    #[serde(rename = "stream.pause")]
    StreamPause {
        /// Session id.
        session: SessionId,
    },
    /// Resume after a pause.
    #[serde(rename = "stream.resume")]
    StreamResume {
        /// Session id.
        session: SessionId,
    },
    /// The phone's choice after `stream.state pick` (launch-policy spec, "Launch and window
    /// discovery" step 4): `index` into that event's `candidates`.
    #[serde(rename = "stream.pick")]
    StreamPick {
        /// Session id.
        session: SessionId,
        /// Zero-based index into the `candidates` of the `pick` state.
        index: u32,
    },
    /// Keep-alive; the host answers `pong`.
    #[serde(rename = "ping")]
    Ping {},
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_server_event_is_ignored() {
        let ev: ServerEvent =
            serde_json::from_str(r#"{"type":"future.thing","x":1}"#).expect("tolerated");
        assert_eq!(ev, ServerEvent::Unknown);
    }

    #[test]
    fn error_event_wire_form() {
        let ev = ServerEvent::Error {
            code: ErrorCode::StreamUnavailable,
            message: "Stream mode is not available on this computer yet.".into(),
            session: Some(SessionId::from("ses_1")),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "error",
                "code": "stream_unavailable",
                "message": "Stream mode is not available on this computer yet.",
                "session": "ses_1"
            })
        );
        assert_eq!(serde_json::from_value::<ServerEvent>(json).unwrap(), ev);
        let bare = ServerEvent::Error {
            code: ErrorCode::BadRequest,
            message: "x".into(),
            session: None,
        };
        assert!(
            serde_json::to_value(&bare)
                .unwrap()
                .get("session")
                .is_none(),
            "an absent session is omitted"
        );
    }

    #[test]
    fn empty_events_have_only_a_type() {
        let json = serde_json::to_string(&ServerEvent::SharesUpdated {}).unwrap();
        assert_eq!(json, r#"{"type":"shares.updated"}"#);
    }
}
