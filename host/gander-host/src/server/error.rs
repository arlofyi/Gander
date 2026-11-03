// SPDX-License-Identifier: Apache-2.0
//! [`ApiError`]: a protocol error as an axum response (`{"error": {...}}` with the code's status).

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use gander_core::error::{ErrorBody, ErrorCode};

/// A protocol error. Handlers return `Result<T, ApiError>`; middleware returns it directly.
///
/// The message is English, has no paths, tokens or file contents (protocol spec, "Error format").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError(pub ErrorBody);

impl ApiError {
    /// An error with `retry_after_ms = 0`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(ErrorBody::new(code, message))
    }

    /// An error asking the client to retry; also sets `Retry-After` (seconds, rounded up).
    pub fn retry_after(code: ErrorCode, message: impl Into<String>, retry_after_ms: u64) -> Self {
        Self(ErrorBody::retry_after(code, message, retry_after_ms))
    }

    /// The code.
    pub fn code(&self) -> ErrorCode {
        self.0.error.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let retry_ms = self.0.error.retry_after_ms;
        let mut resp = (status, Json(self.0)).into_response();
        if retry_ms > 0 {
            let secs = retry_ms.div_ceil(1000).max(1);
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_retry_after() {
        let r =
            ApiError::retry_after(ErrorCode::TooManyRequests, "slow down", 1500).into_response();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()[header::RETRY_AFTER], "2");
        let r = ApiError::new(ErrorCode::BadHost, "x").into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert!(r.headers().get(header::RETRY_AFTER).is_none());
    }
}
