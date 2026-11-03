// SPDX-License-Identifier: Apache-2.0
//! Pairing: `POST /v1/pair` and `GET /v1/pair/{pairing_id}`
//! (`docs/specs/2026-11-01-pairing-and-trust.md`).

use serde::{Deserialize, Serialize};

use super::host::HostDetails;
use super::ids::{BearerToken, DeviceId, PairingId, PairingSecret, Timestamp};

/// `POST /v1/pair` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    /// The QR secret (flow A); absent in the manual flow B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<PairingSecret>,
    /// What the device says about itself (shown on the host's approval dialog).
    pub device: PairingDevice,
    /// 16 random bytes, base64url; input to the SAS code.
    pub nonce: String,
}

/// Self-description of a pairing device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingDevice {
    /// User-visible name, for example "Pixel 9".
    pub name: String,
    /// Platform, for example "android".
    pub platform: String,
    /// Device model.
    pub model: String,
    /// Gander client version.
    pub app_version: String,
}

/// `POST /v1/pair` 200 response: a pending pairing awaiting the owner's click.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStarted {
    /// Id to poll with `GET /v1/pair/{id}`.
    pub pairing_id: PairingId,
    /// Short authentication string, 6 digits grouped 3+3 (`"482 913"`).
    pub sas: String,
    /// Host nonce, base64url; input to the SAS code.
    pub host_nonce: String,
    /// When this pending pairing expires.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: Timestamp,
}

/// State of a pending pairing as reported by `GET /v1/pair/{id}` (202 or 403).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    /// Waiting for the owner (HTTP 202).
    Pending,
    /// The owner clicked Deny (HTTP 403).
    Denied,
}

/// `GET /v1/pair/{id}` body for 202 (pending) and 403 (denied).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairPollStatus {
    /// Current state.
    pub state: PairingState,
}

/// `GET /v1/pair/{id}` 200 body: approved. Returned exactly once; the token exists in plaintext
/// only here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairApproved {
    /// The device's bearer token.
    pub token: BearerToken,
    /// The new device id.
    pub device_id: DeviceId,
    /// The host the device is now paired with.
    pub host: HostDetails,
}
