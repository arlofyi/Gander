// SPDX-License-Identifier: Apache-2.0
//! `POST /v1/pair`, `GET /v1/pair/{pairing_id}`, `POST /v1/token/rotate`.
//!
//! Status mapping (protocol spec "Pairing", pairing spec):
//!
//! | Situation | Response |
//! | --- | --- |
//! | window closed | 404 `pairing_closed` |
//! | 5 failures from this source in this window | 429 `pairing_rate_limited` |
//! | body not JSON / bad nonce / bad device fields | 400 `bad_request` (counts as a failure) |
//! | QR secret wrong | 403 `forbidden` (counts as a failure) |
//! | tagged Tailscale node, `allow_tagged_peers` off | 403 `forbidden` (counts as a failure) |
//! | another pairing pending | 409 `pairing_in_progress` |
//! | poll: pending / approved / denied | 202 `{state: pending}` / 200 token / 403 `{state: denied}` |
//! | poll: timed out, cancelled or already collected | 410 `pairing_expired` |
//! | poll: unknown id or another source address | 404 `not_found` |
//!
//! A denied poll returns `{"state":"denied"}`, not an error body: the protocol spec lists that
//! shape for 403 and the Android client handles it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use gander_core::config::TrustConfig;
use gander_core::error::ErrorCode;
use gander_core::net::tailscale::{WhoIsAnswer, pairing_admissible};
use gander_core::pairing::window::{BeginError, BeginRequest, Fresh, PollOutcome};
use gander_core::pairing::{NONCE_LEN, b64url, b64url_fixed};
use gander_core::protocol::PairingId;
use gander_core::protocol::host::TokenRotateResponse;
use gander_core::protocol::pairing::{
    PairApproved, PairPollStatus, PairRequest, PairStarted, PairingDevice, PairingState,
};

use super::registry::network_kind;
use super::{PairingEvent, PairingService, random_bytes};
use crate::audit::{AuditEvent, Outcome, kind};
use crate::server::{ApiError, Caller, ConnInfo};

/// Longest accepted device name, platform, model or version (characters).
pub const MAX_DEVICE_FIELD: usize = 64;

pub(super) fn router(svc: Arc<PairingService>) -> Router {
    Router::new()
        .route("/v1/pair", post(start))
        .route("/v1/pair/{pairing_id}", get(poll))
        .route("/v1/token/rotate", post(rotate))
        .with_state(svc)
}

fn no_store(mut resp: Response) -> Response {
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn field_ok(s: &str) -> bool {
    let n = s.chars().count();
    (1..=MAX_DEVICE_FIELD).contains(&n) && !s.chars().any(char::is_control)
}

fn device_ok(d: &PairingDevice) -> bool {
    field_ok(&d.name) && field_ok(&d.platform) && field_ok(&d.model) && field_ok(&d.app_version)
}

impl PairingService {
    fn refuse(&self, source: IpAddr, reason: &str, err: ApiError) -> Response {
        let failures = self.lock().snapshot(Instant::now()).failures;
        self.audit.record(
            AuditEvent::new(kind::PAIR_ATTEMPT)
                .source(source)
                .outcome(Outcome::Refused)
                .detail("reason", reason)
                .detail("failures", failures),
        );
        self.emit(PairingEvent::AttemptRefused { source, failures });
        err.into_response()
    }

    fn fail(&self, source: IpAddr, reason: &str, code: ErrorCode, message: &str) -> Response {
        self.lock().record_failure(Instant::now(), source);
        self.refuse(source, reason, ApiError::new(code, message))
    }

    fn trust(&self) -> TrustConfig {
        self.registry.trust()
    }
}

fn begin_error(e: BeginError) -> ApiError {
    let message = match e {
        BeginError::Closed => "Pairing is not open on this computer.",
        BeginError::RateLimited => "Too many failed pairing attempts. Re-open \"Add a device\".",
        BeginError::BadSecret => "This pairing code is not valid.",
        BeginError::InProgress => "Another device is waiting for approval.",
    };
    ApiError::new(e.error_code(), message)
}

async fn start(
    State(svc): State<Arc<PairingService>>,
    Extension(conn): Extension<ConnInfo>,
    body: Bytes,
) -> Response {
    let source = conn.peer.ip();
    let now = Instant::now();
    if svc.test_approve_enabled() && source.is_loopback() && !svc.lock().is_open(now) {
        svc.open_window();
    }
    // Bind first: an `if let` scrutinee's guard would live through the block (deadlock).
    let admitted = svc.lock().admit(now, source);
    if let Err(e) = admitted {
        return svc.refuse(source, e.as_str(), begin_error(e));
    }
    let Ok(request) = serde_json::from_slice::<PairRequest>(&body) else {
        return svc.fail(
            source,
            "bad_body",
            ErrorCode::BadRequest,
            "Malformed pairing request.",
        );
    };
    let Some(device_nonce) = b64url_fixed::<NONCE_LEN>(&request.nonce) else {
        return svc.fail(
            source,
            "bad_nonce",
            ErrorCode::BadRequest,
            "Malformed pairing request.",
        );
    };
    if !device_ok(&request.device) {
        return svc.fail(
            source,
            "bad_device",
            ErrorCode::BadRequest,
            "Malformed pairing request.",
        );
    }
    let identity = conn.network.identity().cloned();
    if let Some(p) = &identity
        && pairing_admissible(&WhoIsAnswer::Found(p.clone()), &svc.trust()).is_err()
    {
        return svc.fail(
            source,
            "tagged_peer",
            ErrorCode::Forbidden,
            "Tagged Tailscale devices may not pair with this computer.",
        );
    }
    let fresh = Fresh {
        pairing_id: PairingId::from(format!("pair_{}", b64url(&random_bytes::<32>()))),
        host_nonce: random_bytes::<NONCE_LEN>(),
    };
    let begun = svc.lock().begin(
        now,
        BeginRequest {
            source,
            secret: request.secret.as_ref().map(|s| s.expose()),
            device: request.device,
            device_nonce,
            identity,
        },
        fresh,
        &svc.host_spki,
    );
    let started = match begun {
        Ok(s) => s,
        Err(e) => return svc.refuse(source, e.as_str(), begin_error(e)),
    };
    let view = svc.lock().pending(now);
    svc.audit.record(
        AuditEvent::new(kind::PAIR_ATTEMPT)
            .source(source)
            .detail("flow", started.flow.as_str())
            .detail("network", network_kind(&conn.network))
            .detail(
                "name",
                view.as_ref()
                    .map(|v| v.device.name.clone())
                    .unwrap_or_default(),
            ),
    );
    if let Some(v) = view {
        svc.emit(PairingEvent::Pending(Box::new(v)));
    }
    svc.schedule_tick(started.expires_in);
    if svc.test_approve_enabled()
        && source.is_loopback()
        && let Err(e) = svc.approve_inner(&started.pairing_id, kind::TEST_APPROVE)
    {
        tracing::warn!(error = %e, "test hook approval failed");
    }
    let expires_at = time::OffsetDateTime::now_utc()
        + time::Duration::try_from(started.expires_in).unwrap_or(time::Duration::ZERO);
    no_store(
        Json(PairStarted {
            pairing_id: started.pairing_id,
            sas: started.sas.to_string(),
            host_nonce: b64url(&started.host_nonce),
            expires_at,
        })
        .into_response(),
    )
}

async fn poll(
    State(svc): State<Arc<PairingService>>,
    Extension(conn): Extension<ConnInfo>,
    Path(pairing_id): Path<String>,
) -> Response {
    svc.tick();
    let source = conn.peer.ip();
    let outcome = svc
        .lock()
        .poll(Instant::now(), &PairingId::from(pairing_id), source);
    match outcome {
        PollOutcome::Pending => (
            StatusCode::ACCEPTED,
            Json(PairPollStatus {
                state: PairingState::Pending,
            }),
        )
            .into_response(),
        PollOutcome::Denied => (
            StatusCode::FORBIDDEN,
            Json(PairPollStatus {
                state: PairingState::Denied,
            }),
        )
            .into_response(),
        PollOutcome::Approved { device_id, token } => {
            svc.audit.record(
                AuditEvent::new(kind::PAIR_COLLECTED)
                    .device(&device_id)
                    .source(source),
            );
            no_store(
                Json(PairApproved {
                    token,
                    device_id,
                    host: svc.card().details,
                })
                .into_response(),
            )
        }
        PollOutcome::Expired => ApiError::new(
            ErrorCode::PairingExpired,
            "This pairing request has expired.",
        )
        .into_response(),
        PollOutcome::NotFound => {
            ApiError::new(ErrorCode::NotFound, "No such pairing request.").into_response()
        }
    }
}

async fn rotate(
    State(svc): State<Arc<PairingService>>,
    Extension(caller): Extension<Caller>,
) -> Response {
    let Caller::Device(id) = caller else {
        return ApiError::new(
            ErrorCode::TokenMissing,
            "This request needs a device token.",
        )
        .into_response();
    };
    match svc.registry.rotate(&id) {
        Ok(token) => no_store(Json(TokenRotateResponse { token }).into_response()),
        Err(super::RegistryError::Revoked) => ApiError::new(
            ErrorCode::TokenRevoked,
            "This device was removed on the computer.",
        )
        .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "token rotation failed");
            ApiError::new(ErrorCode::Internal, "Internal error.").into_response()
        }
    }
}
