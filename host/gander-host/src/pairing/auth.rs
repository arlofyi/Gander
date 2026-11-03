// SPDX-License-Identifier: Apache-2.0
//! [`TokenAuthenticator`]: the production [`Authenticator`] (replaces WP-12's `HelloOnly`).
//!
//! Anonymous (no token looked at): `GET`/`HEAD /v1/host`, `POST /v1/pair`, `GET /v1/pair/{id}`.
//! Everything else, loopback included, needs `Authorization: Bearer <token>`:
//! 401 `token_missing` / `token_invalid` / `token_revoked`, 403 `network_identity_mismatch`.
//! On success the route sees `Extension(Caller::Device(id))`.

use std::sync::Arc;

use axum::http::request::Parts;
use axum::http::{Method, header};

use super::registry::DeviceRegistry;
use crate::server::{AuthFuture, Authenticator, Caller, ConnHandle, ConnInfo};

/// Bearer-token authenticator over a [`DeviceRegistry`].
#[derive(Debug, Clone)]
pub struct TokenAuthenticator {
    registry: Arc<DeviceRegistry>,
}

impl TokenAuthenticator {
    /// An authenticator over `registry`.
    pub fn new(registry: Arc<DeviceRegistry>) -> Self {
        Self { registry }
    }
}

/// Whether a request may proceed without a token.
pub fn is_anonymous(method: &Method, path: &str) -> bool {
    if (method == Method::GET || method == Method::HEAD) && path == "/v1/host" {
        return true;
    }
    if method == Method::POST && path == "/v1/pair" {
        return true;
    }
    if method == Method::GET
        && let Some(id) = path.strip_prefix("/v1/pair/")
    {
        return !id.is_empty() && !id.contains('/');
    }
    false
}

impl Authenticator for TokenAuthenticator {
    fn authenticate<'a>(&'a self, head: &'a Parts, conn: &'a ConnInfo) -> AuthFuture<'a> {
        Box::pin(async move {
            if is_anonymous(&head.method, head.uri.path()) {
                return Ok(Caller::Anonymous);
            }
            let authorization = head
                .headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            let handle = head.extensions.get::<ConnHandle>();
            self.registry
                .authenticate(authorization, conn, handle)
                .map(Caller::Device)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_endpoints() {
        assert!(is_anonymous(&Method::GET, "/v1/host"));
        assert!(is_anonymous(&Method::HEAD, "/v1/host"));
        assert!(is_anonymous(&Method::POST, "/v1/pair"));
        assert!(is_anonymous(&Method::GET, "/v1/pair/pair_abc"));
        assert!(!is_anonymous(&Method::GET, "/v1/pair/"));
        assert!(!is_anonymous(&Method::GET, "/v1/pair/a/b"));
        assert!(!is_anonymous(&Method::GET, "/v1/pair"));
        assert!(!is_anonymous(&Method::POST, "/v1/host"));
        assert!(!is_anonymous(&Method::DELETE, "/v1/pair/x"));
        assert!(!is_anonymous(&Method::GET, "/v1/me"));
        assert!(!is_anonymous(&Method::POST, "/v1/token/rotate"));
        assert!(!is_anonymous(&Method::GET, "/v1/events"));
    }
}
