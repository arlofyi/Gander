// SPDX-License-Identifier: Apache-2.0
//! `GET /v1/host` (no auth): `{ "protocol": [1], "pairing_open": false, "host_id": "..." }`.
//!
//! Deliberately minimal. `pairing_open` is a shared flag the pairing window (WP-13) flips.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::routing::get;
use axum::{Json, Router};
use gander_core::protocol::host::HostHello;
use gander_core::protocol::{HostId, PROTOCOL_VERSION};

/// Router with `GET /v1/host`.
pub fn router(host_id: HostId, pairing_open: Arc<AtomicBool>) -> Router {
    Router::new().route(
        "/v1/host",
        get(move || {
            let hello = HostHello {
                protocol: vec![PROTOCOL_VERSION],
                pairing_open: pairing_open.load(Ordering::SeqCst),
                host_id: host_id.clone(),
            };
            async move { Json(hello) }
        }),
    )
}
