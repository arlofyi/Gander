// SPDX-License-Identifier: Apache-2.0
//! Transport-independent entry points: the `stream` field of listing entries and the
//! `POST /v1/stream` handler body. Wired into HTTP by WP-14 (listings) and WP-50 (stream).

use std::collections::HashMap;

use super::{PolicyDecision, PolicyInput, StreamPolicy, rules};
use crate::clipboard::ClipboardMode;
use crate::config::{HostConfig, ShareConfig};
use crate::error::{ErrorBody, ErrorCode};
use crate::protocol::DeviceId;
use crate::protocol::files::{Permission, StreamDecision};
use crate::protocol::host::StreamCapabilities;
use crate::protocol::stream::{StreamCreated, StreamRequest};

/// `capabilities.stream` until Phase 2 ships: Stream mode is not available.
pub fn phase1_stream_capabilities() -> StreamCapabilities {
    StreamCapabilities {
        enabled: false,
        codecs: Vec::new(),
        max_sessions: 0,
        audio: false,
        input: false,
        fit_to_phone: false,
        clipboard: ClipboardMode::Off,
    }
}

/// The `stream` field for every file of one listing (one device, one share), memoised by
/// extension so a large directory costs one association lookup per type.
///
/// Directories are not streamable: callers set `refused` for them without asking this type.
#[derive(Debug)]
pub struct ListingPolicy<'a> {
    policy: StreamPolicy<'a>,
    stream_permitted: bool,
    share_default: StreamDecision,
    cache: HashMap<Option<String>, StreamDecision>,
}

impl<'a> ListingPolicy<'a> {
    /// Policy view for `device` browsing `share`.
    pub fn new(policy: StreamPolicy<'a>, share: &ShareConfig, device: &DeviceId) -> Self {
        Self {
            policy,
            stream_permitted: share.allows(device, Permission::Stream),
            share_default: share.stream_default,
            cache: HashMap::new(),
        }
    }

    /// `allowed | refused | ask` for a file, without prompting.
    pub fn decision(&mut self, file_name: &str) -> StreamDecision {
        let key = rules::final_extension(file_name);
        if let Some(cached) = self.cache.get(&key) {
            return *cached;
        }
        let outcome = self
            .policy
            .evaluate(&PolicyInput {
                file_name,
                stream_permitted: self.stream_permitted,
                share_default: self.share_default,
                requested_app: None,
            })
            .outcome;
        self.cache.insert(key, outcome);
        outcome
    }
}

/// `POST /v1/stream`, minus HTTP. Phase 1 behaviour: validate, run the policy, and answer
/// `stream_unavailable` (503) when the policy did not refuse, because Stream mode
/// (`capabilities.stream.enabled = false`) does not exist yet. A refusal is returned as the
/// policy's own error (`permission_denied`, `stream_refused`, `app_unavailable`).
///
/// The HTTP layer (WP-14 for routing and auth, WP-50 for the Phase 2 session start) wires this in.
/// Before calling it the caller must have authenticated `device` and resolved `request.path`
/// inside the share with the path resolver (answering `bad_path` / `not_found` itself), and
/// passes the resolved file's name as `file_name`. Ok is never returned in Phase 1; WP-50
/// replaces the final `stream_unavailable` with the session start (owner prompt on `ask`).
pub fn handle_stream_request(
    policy: &StreamPolicy<'_>,
    config: &HostConfig,
    device: &DeviceId,
    request: &StreamRequest,
    file_name: &str,
) -> Result<StreamCreated, ErrorBody> {
    evaluate_stream_request(policy, config, device, request, file_name)?;
    Err(ErrorBody::new(
        ErrorCode::StreamUnavailable,
        "Stream mode is not available on this computer yet.",
    ))
}

/// The validation part of [`handle_stream_request`]: share visibility, permission, viewport
/// sanity and the policy. Returns the decision when it is `allowed` or `ask`. Phase 2 (WP-53)
/// calls this and hands the decision to the session lifecycle instead of answering
/// `stream_unavailable`.
pub fn evaluate_stream_request(
    policy: &StreamPolicy<'_>,
    config: &HostConfig,
    device: &DeviceId,
    request: &StreamRequest,
    file_name: &str,
) -> Result<PolicyDecision, ErrorBody> {
    let share = config
        .share(&request.share)
        .filter(|s| !s.permissions_for(device).is_empty())
        .ok_or_else(|| ErrorBody::new(ErrorCode::ShareNotFound, "No such share."))?;
    let vp = request.viewport;
    if vp.width == 0 || vp.height == 0 || !vp.dpr.is_finite() || vp.dpr <= 0.0 {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            "The viewport must have a positive size and pixel ratio.",
        ));
    }
    let decision = policy.evaluate(&PolicyInput {
        file_name,
        stream_permitted: share.allows(device, Permission::Stream),
        share_default: share.stream_default,
        requested_app: request.app.as_ref(),
    });
    match decision.error_body() {
        Some(err) => Err(err),
        None => Ok(decision),
    }
}
