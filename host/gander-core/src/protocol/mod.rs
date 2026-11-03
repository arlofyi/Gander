// SPDX-License-Identifier: Apache-2.0
//! Protocol v1 data transfer objects (`docs/specs/2026-11-01-protocol-v1.md`).
//!
//! Every JSON body the host sends or accepts has a type here. `protocol/openapi.yaml` describes
//! the same shapes; the `protocol_examples_roundtrip` test deserialises every example in that
//! document into these types and serialises it back unchanged.
//!
//! Compatibility rules the types follow:
//!
//! - Unknown JSON fields are ignored on input (no `deny_unknown_fields` anywhere in this module).
//! - Unknown enum values that a newer host may add deserialise to an `Unknown` variant where the
//!   spec expects growth (error codes, events, stream reasons).
//! - Optional fields the spec shows as `null` are serialised as `null`; optional fields the spec
//!   says are "included only if" are omitted when absent.

pub mod events;
pub mod files;
pub mod host;
pub mod ids;
pub mod input;
pub mod pairing;
pub mod schema;
pub mod stream;

pub use ids::{
    AppId, BearerToken, DeviceId, HostId, PairingId, PairingSecret, SessionId, ShareId, Timestamp,
};

/// The protocol version this crate implements; paths are prefixed `/v1`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Path prefix of every endpoint.
pub const PATH_PREFIX: &str = "/v1";

/// Default TCP port ("GANDR" on a phone keypad).
pub const DEFAULT_PORT: u16 = 42637;

/// Maximum request body in bytes, except where an endpoint says otherwise.
pub const MAX_REQUEST_BODY: usize = 64 * 1024;

/// Maximum in-flight requests per device before `429 too_many_requests`.
pub const PER_DEVICE_CONCURRENCY: u32 = 8;

/// Hard cap for one `GET /preview/text` response, in bytes.
pub const MAX_TEXT_PREVIEW: u64 = 1024 * 1024;

/// Response and request header names used by the protocol.
pub mod headers {
    /// On every response: the protocol version, `1`.
    pub const PROTOCOL: &str = "X-Gander-Protocol";
    /// On every response: a ULID identifying the request in the host's local log.
    pub const REQUEST_ID: &str = "X-Gander-Request";
    /// On `GET /preview/text`: the detected source encoding.
    pub const TEXT_ENCODING: &str = "X-Gander-Text-Encoding";
    /// On `GET /preview/text`: the total size of the file in bytes.
    pub const TEXT_TOTAL: &str = "X-Gander-Text-Total";
    /// Request headers that mark a browser request; any of them -> 403 `browser_request`.
    pub const BROWSER_MARKERS: [&str; 3] = ["Origin", "Sec-Fetch-Mode", "Cookie"];
}
