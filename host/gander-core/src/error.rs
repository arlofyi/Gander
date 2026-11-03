// SPDX-License-Identifier: Apache-2.0
//! Protocol error codes and the JSON error body.
//!
//! Every refusal the host sends carries one of these codes (protocol spec, "Error format"). The
//! `message` is English, human readable and never contains paths, tokens or file contents.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Machine-readable error code sent in `{"error": {"code": ...}}`.
///
/// Serialised in `snake_case`. A client that receives a code it does not know deserialises it as
/// [`ErrorCode::Unknown`] (compatibility rule: clients ignore what they do not understand) and
/// falls back to the HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// 400: malformed input.
    BadRequest,
    /// 400: the `Host` header is not one of the addresses the host serves.
    BadHost,
    /// 400: the path resolver rejected the path (every reason except not found).
    BadPath,
    /// 401: no bearer token.
    TokenMissing,
    /// 401: the token is not known.
    TokenInvalid,
    /// 401: the token was revoked; the client forgets the host after explaining why.
    TokenRevoked,
    /// 403: generic refusal.
    Forbidden,
    /// 403: the request carried `Origin`, `Sec-Fetch-Mode` or `Cookie`.
    BrowserRequest,
    /// 403: the private-network identity does not match the one recorded at pairing.
    NetworkIdentityMismatch,
    /// 403: the device lacks a share permission; `message` names it.
    PermissionDenied,
    /// 404: no such share for this device.
    ShareNotFound,
    /// 404: no such item (also hidden patterns and denied reparse points).
    NotFound,
    /// 404: pairing is not open on the host.
    PairingClosed,
    /// 409: another stream session is active.
    StreamBusy,
    /// 409: a pairing is already pending.
    PairingInProgress,
    /// 410: the pairing window or pending pairing expired.
    PairingExpired,
    /// 412: the host session is locked.
    HostLocked,
    /// 412: nobody at the host answered an approval prompt.
    HostUnattended,
    /// 412: the app for this file is not available or no window was found.
    AppUnavailable,
    /// 412: the file changed during a resumed download (`If-Range` mismatch).
    ///
    /// Used by the Fetch section of the protocol spec but missing from its error table.
    FileChanged,
    /// 413: request body or requested preview dimension over the limit.
    TooLarge,
    /// 415: no preview or thumbnail for this type, or a binary file asked for as text.
    UnsupportedType,
    /// 416: the `Range` header cannot be served (multipart or out of bounds).
    ///
    /// The protocol spec names the status (416) but no code; this code fills the gap.
    RangeNotSatisfiable,
    /// 422: stream refused by the launch policy; `message` says why in user terms.
    StreamRefused,
    /// 429: per-device concurrency limit hit; `Retry-After` is set.
    TooManyRequests,
    /// 429: too many failed pairing attempts in this window.
    PairingRateLimited,
    /// 500: internal error, never with details.
    Internal,
    /// 503: a thumbnail, preview or hash is being generated; `retry_after_ms` is set.
    Generating,
    /// 503: Stream mode is not available on this host (`capabilities.stream.enabled` is false;
    /// every `POST /stream` that passes the policy gets this until Phase 2).
    ///
    /// Named by the master plan (WP-17, WP-19) but missing from the protocol spec's error table.
    StreamUnavailable,
    /// A code this build does not know (sent by a newer host). Never sent by a host.
    #[serde(other)]
    Unknown,
}

impl ErrorCode {
    /// Every code a host may send, in declaration order (excludes [`ErrorCode::Unknown`]).
    pub const ALL: [ErrorCode; 29] = [
        ErrorCode::BadRequest,
        ErrorCode::BadHost,
        ErrorCode::BadPath,
        ErrorCode::TokenMissing,
        ErrorCode::TokenInvalid,
        ErrorCode::TokenRevoked,
        ErrorCode::Forbidden,
        ErrorCode::BrowserRequest,
        ErrorCode::NetworkIdentityMismatch,
        ErrorCode::PermissionDenied,
        ErrorCode::ShareNotFound,
        ErrorCode::NotFound,
        ErrorCode::PairingClosed,
        ErrorCode::StreamBusy,
        ErrorCode::PairingInProgress,
        ErrorCode::PairingExpired,
        ErrorCode::HostLocked,
        ErrorCode::HostUnattended,
        ErrorCode::AppUnavailable,
        ErrorCode::FileChanged,
        ErrorCode::TooLarge,
        ErrorCode::UnsupportedType,
        ErrorCode::RangeNotSatisfiable,
        ErrorCode::StreamRefused,
        ErrorCode::TooManyRequests,
        ErrorCode::PairingRateLimited,
        ErrorCode::Internal,
        ErrorCode::Generating,
        ErrorCode::StreamUnavailable,
    ];

    /// The HTTP status this code is sent with.
    pub const fn http_status(self) -> u16 {
        match self {
            ErrorCode::BadRequest | ErrorCode::BadHost | ErrorCode::BadPath => 400,
            ErrorCode::TokenMissing | ErrorCode::TokenInvalid | ErrorCode::TokenRevoked => 401,
            ErrorCode::Forbidden
            | ErrorCode::BrowserRequest
            | ErrorCode::NetworkIdentityMismatch
            | ErrorCode::PermissionDenied => 403,
            ErrorCode::ShareNotFound | ErrorCode::NotFound | ErrorCode::PairingClosed => 404,
            ErrorCode::StreamBusy | ErrorCode::PairingInProgress => 409,
            ErrorCode::PairingExpired => 410,
            ErrorCode::HostLocked
            | ErrorCode::HostUnattended
            | ErrorCode::AppUnavailable
            | ErrorCode::FileChanged => 412,
            ErrorCode::TooLarge => 413,
            ErrorCode::UnsupportedType => 415,
            ErrorCode::RangeNotSatisfiable => 416,
            ErrorCode::StreamRefused => 422,
            ErrorCode::TooManyRequests | ErrorCode::PairingRateLimited => 429,
            ErrorCode::Internal | ErrorCode::Unknown => 500,
            ErrorCode::Generating | ErrorCode::StreamUnavailable => 503,
        }
    }

    /// The wire string, for example `"share_not_found"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::BadHost => "bad_host",
            ErrorCode::BadPath => "bad_path",
            ErrorCode::TokenMissing => "token_missing",
            ErrorCode::TokenInvalid => "token_invalid",
            ErrorCode::TokenRevoked => "token_revoked",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::BrowserRequest => "browser_request",
            ErrorCode::NetworkIdentityMismatch => "network_identity_mismatch",
            ErrorCode::PermissionDenied => "permission_denied",
            ErrorCode::ShareNotFound => "share_not_found",
            ErrorCode::NotFound => "not_found",
            ErrorCode::PairingClosed => "pairing_closed",
            ErrorCode::StreamBusy => "stream_busy",
            ErrorCode::PairingInProgress => "pairing_in_progress",
            ErrorCode::PairingExpired => "pairing_expired",
            ErrorCode::HostLocked => "host_locked",
            ErrorCode::HostUnattended => "host_unattended",
            ErrorCode::AppUnavailable => "app_unavailable",
            ErrorCode::FileChanged => "file_changed",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::UnsupportedType => "unsupported_type",
            ErrorCode::RangeNotSatisfiable => "range_not_satisfiable",
            ErrorCode::StreamRefused => "stream_refused",
            ErrorCode::TooManyRequests => "too_many_requests",
            ErrorCode::PairingRateLimited => "pairing_rate_limited",
            ErrorCode::Internal => "internal",
            ErrorCode::Generating => "generating",
            ErrorCode::StreamUnavailable => "stream_unavailable",
            ErrorCode::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The JSON body of every error response: `{"error": {...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The error itself.
    pub error: ErrorDetail,
}

/// The inner object of [`ErrorBody`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Machine-readable code.
    pub code: ErrorCode,
    /// Human readable, English, never contains paths, tokens or file contents.
    pub message: String,
    /// Milliseconds the client should wait before retrying; 0 when not applicable.
    #[serde(default)]
    pub retry_after_ms: u64,
}

impl ErrorBody {
    /// Builds an error body with `retry_after_ms = 0`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ErrorDetail {
                code,
                message: message.into(),
                retry_after_ms: 0,
            },
        }
    }

    /// Builds an error body that asks the client to retry after `retry_after_ms`.
    pub fn retry_after(code: ErrorCode, message: impl Into<String>, retry_after_ms: u64) -> Self {
        Self {
            error: ErrorDetail {
                code,
                message: message.into(),
                retry_after_ms,
            },
        }
    }

    /// The HTTP status for this body.
    pub fn http_status(&self) -> u16 {
        self.error.code.http_status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_serde() {
        for code in ErrorCode::ALL {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json, serde_json::Value::String(code.as_str().to_owned()));
            let back: ErrorCode = serde_json::from_value(json).unwrap();
            assert_eq!(back, code);
        }
    }

    #[test]
    fn unknown_code_is_tolerated() {
        let code: ErrorCode = serde_json::from_str("\"from_the_future\"").unwrap();
        assert_eq!(code, ErrorCode::Unknown);
        assert_eq!(code.http_status(), 500);
    }

    #[test]
    fn statuses_follow_the_spec_table() {
        assert_eq!(ErrorCode::BadPath.http_status(), 400);
        assert_eq!(ErrorCode::TokenRevoked.http_status(), 401);
        assert_eq!(ErrorCode::BrowserRequest.http_status(), 403);
        assert_eq!(ErrorCode::PairingClosed.http_status(), 404);
        assert_eq!(ErrorCode::StreamBusy.http_status(), 409);
        assert_eq!(ErrorCode::PairingExpired.http_status(), 410);
        assert_eq!(ErrorCode::HostUnattended.http_status(), 412);
        assert_eq!(ErrorCode::TooLarge.http_status(), 413);
        assert_eq!(ErrorCode::UnsupportedType.http_status(), 415);
        assert_eq!(ErrorCode::StreamRefused.http_status(), 422);
        assert_eq!(ErrorCode::PairingRateLimited.http_status(), 429);
        assert_eq!(ErrorCode::Internal.http_status(), 500);
        assert_eq!(ErrorCode::Generating.http_status(), 503);
        assert_eq!(ErrorCode::StreamUnavailable.http_status(), 503);
    }
}
