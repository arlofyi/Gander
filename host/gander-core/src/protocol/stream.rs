// SPDX-License-Identifier: Apache-2.0
//! Stream mode HTTP: `POST /v1/stream` and `DELETE /v1/stream/{session}`
//! (`docs/specs/2026-11-01-stream-launch-policy.md`). Signalling continues over `/v1/events`.

use serde::{Deserialize, Serialize};

use super::host::VideoCodec;
use super::ids::{AppId, SessionId, ShareId};

/// `POST /v1/stream` request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamRequest {
    /// Share of the file.
    pub share: ShareId,
    /// Share-relative path of the file.
    pub path: String,
    /// Catalogue app to use; `null` lets the host's policy choose.
    pub app: Option<AppId>,
    /// The phone's viewport, for fit-to-phone.
    pub viewport: Viewport,
    /// Encoding preferences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<StreamPreference>,
    /// Session to resume within the reconnection grace period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<SessionId>,
}

/// Phone viewport in physical pixels plus device pixel ratio.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Device pixel ratio.
    pub dpr: f64,
}

/// Encoding preferences for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPreference {
    /// Preferred codec.
    pub codec: VideoCodec,
    /// Bitrate ceiling in kbit/s (the host may cap lower, for example on battery).
    pub max_kbps: u32,
}

/// `POST /v1/stream` 201 response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamCreated {
    /// New session id.
    pub session: SessionId,
    /// Initial state, normally `requested`.
    pub state: StreamState,
    /// The app the host will use.
    pub app: AppRef,
    /// The host owner must approve first (`stream.state: approving` follows).
    pub needs_approval: bool,
}

/// Reference to a catalogue app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRef {
    /// Catalogue id.
    pub id: AppId,
    /// Display name.
    pub name: String,
}

/// Stream session state.
///
/// `pick` comes from the launch-policy spec (window ambiguity); the protocol spec's
/// `stream.state` list omits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamState {
    /// Accepted, nothing started yet.
    Requested,
    /// Waiting for the host owner.
    Approving,
    /// App launching.
    Launching,
    /// Several candidate windows or none; the user picks.
    Pick,
    /// Target window identified.
    WindowFound,
    /// Media flowing.
    Streaming,
    /// Temporarily paused; see the reason.
    Paused,
    /// Finished; see the reason.
    Ended,
    /// A state this build does not know.
    #[serde(other)]
    Unknown,
}

/// Why a session paused or ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamReason {
    /// Paused: the captured window lost the foreground.
    FocusLost,
    /// Paused: someone is using the host.
    LocalUserActive,
    /// Paused: the client asked (app in background).
    PhoneBackground,
    /// Paused: ICE disconnected; grace period running.
    Network,
    /// Ended: the client closed the session.
    ClientClosed,
    /// Ended: the host owner used the kill switch.
    HostKillSwitch,
    /// Ended: the app window was closed on the host.
    WindowClosed,
    /// Ended: the device was revoked.
    Revoked,
    /// Ended: a timeout (approval, pick, grace period).
    Timeout,
    /// Ended: an error.
    Error,
    /// A reason this build does not know.
    #[serde(other)]
    Unknown,
}

/// Query of `DELETE /v1/stream/{session}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamDeleteQuery {
    /// Close the app too; honoured only if the host launched it for this session and the window
    /// is the only one of that process.
    #[serde(default)]
    pub close_app: bool,
}
