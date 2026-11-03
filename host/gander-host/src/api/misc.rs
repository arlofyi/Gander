// SPDX-License-Identifier: Apache-2.0
//! `GET /v1/me`, `POST /v1/stream` (policy check, then the WP-53 lifecycle, or
//! `stream_unavailable` without it) and `DELETE /v1/stream/{session}`.

use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use gander_core::error::ErrorCode;
use gander_core::policy::{
    AssociationResolver, StreamPolicy, evaluate_stream_request, handle_stream_request,
};
use gander_core::protocol::SessionId;
use gander_core::protocol::files::{EntryKind, Permission};
use gander_core::protocol::host::{
    Capabilities, DeviceSummary, FetchCapabilities, HashAlgo, Limits, MeResponse, NetworkIdentity,
    NetworkInfo, RelayState, SearchCapability, TrustAdapterKind, ZipCapability,
};
use gander_core::protocol::stream::{StreamCreated, StreamRequest};

use super::browse::source;
use super::{ApiState, Requester, bad_request, blocking};
use crate::audit::Outcome;
use crate::server::{ApiError, Caller, ConnInfo, TrustNetwork};

pub(super) async fn me(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
) -> Result<Json<MeResponse>, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let record = api
        .registry
        .get(&who.device)
        .ok_or_else(|| ApiError::new(ErrorCode::TokenInvalid, "This device is not paired."))?;
    let network = match &conn {
        Some(Extension(c)) => NetworkInfo {
            adapter: c.adapter,
            identity: c.network.identity().map(|p| NetworkIdentity {
                login: p.login.clone(),
                node: p.node_name.clone(),
            }),
            relayed: relayed(api.network().as_deref(), c.peer.ip()).await,
        },
        None => NetworkInfo {
            adapter: TrustAdapterKind::Generic,
            identity: None,
            relayed: None,
        },
    };
    let o = api.options;
    // The host-level summary: "indexed" when at least one share this device can list is searched
    // through the Windows Search index; each share's own `search` is in `/shares`. COM work
    // (cached for 30 s per share), so on the blocking pool.
    let search = {
        let api = Arc::clone(&api);
        let device = who.device.clone();
        blocking(move || {
            let config = api.config();
            let indexed = config
                .shares_for(&device)
                .filter(|s| s.allows(&device, Permission::List))
                .any(|s| api.share_search(s) == SearchCapability::Indexed);
            Ok(if indexed {
                SearchCapability::Indexed
            } else {
                SearchCapability::Substring
            })
        })
        .await?
    };
    let thumb_sizes = if api.thumbs.is_some() {
        gander_win::thumbs::THUMB_SIZES.to_vec()
    } else {
        Vec::new()
    };
    let preview_max_width = if api.previews.is_some() {
        gander_win::preview::PREVIEW_MAX_WIDTH
    } else {
        0
    };
    Ok(Json(MeResponse {
        device: DeviceSummary {
            id: record.id.clone(),
            name: record.name.clone(),
            created_at: record.created_at,
            last_seen_at: record.last_seen_at,
        },
        host: api.host.read().unwrap_or_else(|e| e.into_inner()).clone(),
        network,
        capabilities: Capabilities {
            list: true,
            search,
            recent: true,
            events: o.events,
            thumb_sizes,
            preview_max_width,
            preview_kinds: api.preview_kinds(),
            fetch: FetchCapabilities {
                range: true,
                hash: vec![HashAlgo::Blake3, HashAlgo::Sha256],
                zip: Some({
                    let zip = api.config().zip;
                    ZipCapability {
                        max_bytes: zip.max_bytes(),
                        max_entries: u64::from(zip.max_entries),
                    }
                }),
            },
            stream: match api.streams() {
                Some(streams) => streams.capabilities(),
                None => gander_core::policy::phase1_stream_capabilities(),
            },
            write: false,
            previews_unavailable: api.previews_unavailable(),
        },
        limits: Limits {
            list_max: o.list_max,
            search_max: o.search_max,
            concurrency: o.concurrency,
        },
    }))
}

/// `network.relayed` for a caller at `peer`: the path to the private-network peer that owns the
/// address (`direct`, `peer_relay`, `derp`), `None` for loopback, without a trust network, or
/// when tailscaled does not know the peer.
async fn relayed(network: Option<&TrustNetwork>, peer: IpAddr) -> Option<RelayState> {
    if peer.is_loopback() {
        return None;
    }
    network?.relay_state(peer).await
}

/// `POST /v1/stream`: share visibility, path resolution, the WP-19 policy, then the WP-53
/// lifecycle (preconditions, `201`, the session task). Without the lifecycle (Phase 1, or a host
/// whose engine did not start) a request the policy does not refuse gets
/// `503 stream_unavailable`. `resume` re-offers an existing session after the same checks.
pub(super) async fn stream(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    body: Bytes,
) -> Result<(StatusCode, Json<StreamCreated>), ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let request: StreamRequest = serde_json::from_slice(&body)
        .map_err(|_| bad_request("The body is not a valid stream request."))?;
    let streams = api.streams();
    let device = who.device.clone();
    let req = request.clone();
    let api2 = Arc::clone(&api);
    let has_streams = streams.is_some();
    let checked = blocking(move || {
        let api = api2;
        let request = req;
        let share = api.share_for(&who, &request.share, None, "stream")?;
        let root = api.root_of(&share)?;
        let resolved = api.resolve_in(&who, &share.id, &root, request.path.as_bytes(), "stream")?;
        if resolved.kind() != EntryKind::File {
            return Err(ApiError::new(
                ErrorCode::BadPath,
                "Only files can be opened in Stream mode.",
            ));
        }
        let config = api.config();
        let catalogue = api.catalogue();
        let resolver: &dyn AssociationResolver = &*api.associations;
        let policy = StreamPolicy::new(&catalogue, resolver, &api.environment);
        let path = resolved.path().to_protocol_string();
        let mut ev = api.accessed(&who, &share.id, &path, "stream");
        if !has_streams {
            let result =
                handle_stream_request(&policy, &config, &who.device, &request, resolved.name());
            if let Err(body) = &result {
                ev = ev
                    .outcome(if body.error.code == ErrorCode::StreamUnavailable {
                        Outcome::Ok
                    } else {
                        Outcome::Refused
                    })
                    .detail("result", body.error.code.as_str());
            }
            api.audit.record(ev);
            return result.map(|_| None).map_err(ApiError);
        }
        let decision =
            evaluate_stream_request(&policy, &config, &who.device, &request, resolved.name());
        let decision = match decision {
            Ok(d) => d,
            Err(body) => {
                api.audit.record(
                    ev.outcome(Outcome::Refused)
                        .detail("result", body.error.code.as_str()),
                );
                return Err(ApiError(body));
            }
        };
        let file = resolved.final_path().map_err(|e| {
            tracing::warn!(error = %e, "stream: the file's path could not be read");
            ApiError::new(ErrorCode::NotFound, "No such file or folder.")
        })?;
        api.audit
            .record(ev.detail("decision", decision.reason.as_str()));
        Ok(Some(crate::lifecycle::NewSession {
            device: who.device.clone(),
            source: who.source,
            decision,
            catalogue,
            file,
            file_name: resolved.name().to_owned(),
            share: share.id.clone(),
            path,
            viewport: request.viewport,
            max_kbps: request.prefer.map(|p| p.max_kbps),
        }))
    })
    .await?;
    let (Some(streams), Some(new)) = (streams, checked) else {
        // Unreachable: without the lifecycle the closure answered already.
        return Err(ApiError::new(
            ErrorCode::StreamUnavailable,
            "Stream mode is not available on this computer yet.",
        ));
    };
    let created = match &request.resume {
        Some(session) => {
            streams
                .resume(
                    &device,
                    session,
                    &new.share,
                    &new.path,
                    new.viewport,
                    new.max_kbps,
                )
                .await
        }
        None => streams.start(new),
    };
    created
        .map(|c| (StatusCode::CREATED, Json(c)))
        .map_err(ApiError)
}

/// `DELETE /v1/stream/{session}?close_app=`: ends the caller's own session (`404 not_found`
/// for any other id), honouring `close_app` per the catalogue's `close_on_end`.
pub(super) async fn stream_end(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(session): Path<String>,
    uri: axum::http::Uri,
) -> Result<StatusCode, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let query = super::RawQuery::parse(uri.query())?;
    let close_app = match query.str("close_app")? {
        None | Some("false" | "0") => false,
        Some("true" | "1") => true,
        Some(_) => return Err(bad_request("close_app must be true or false.")),
    };
    let Some(streams) = api.streams() else {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            "No such stream session.",
        ));
    };
    streams
        .end(&who.device, &SessionId::from(session), close_app)
        .await
        .map_err(ApiError)?;
    Ok(StatusCode::NO_CONTENT)
}
