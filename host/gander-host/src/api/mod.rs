// SPDX-License-Identifier: Apache-2.0
//! Browse, Fetch, thumbnails, previews and host facts (WP-14; protocol spec "Endpoints").
//!
//! # Route assembly
//!
//! [`host_routes`] is THE place where the host's routers are merged; the binary, the WP-1B e2e
//! harness and later packages all call it:
//!
//! ```no_run
//! # async fn demo(identity: gander_host::server::HostIdentity,
//! #               pairing: std::sync::Arc<gander_host::pairing::PairingService>,
//! #               api: std::sync::Arc<gander_host::api::ApiState>,
//! #               plan: std::sync::Arc<dyn gander_host::server::PlanSource>) -> anyhow::Result<()> {
//! use gander_host::api::host_routes;
//! use gander_host::server::{Server, ServerOptions};
//! let routes = host_routes(identity.host_id().clone(), &pairing, &api);
//! let mut options = ServerOptions::new(identity, plan);
//! options.authenticator = pairing.authenticator();
//! let server = Server::start(options, routes).await?;
//! # Ok(()) }
//! ```
//!
//! Served by [`ApiState::router`] (every route needs a device token; the gate has already
//! authenticated the caller):
//!
//! | route | permission | notes |
//! | --- | --- | --- |
//! | `GET /v1/me` | - | device, host (with `os_version`), network, capabilities, limits |
//! | `GET /v1/shares` | - | shares the device holds any permission on |
//! | `GET /v1/shares/{share}/list` | `list` | cursor, sort, order, kinds, ext, `ETag` / `If-None-Match` -> 304 |
//! | `GET /v1/shares/{share}/stat` | `list` | `meta=1`: header sniff, worker `Info` op with `preview` |
//! | `GET /v1/shares/{share}/search` | `list` | the Windows Search index when the folder is crawled (WP-66: `scope=name\|content`, `via`), else a bounded BFS (2 s); `partial`, resumable cursor |
//! | `GET /v1/recent` | `list` | per share: top 3 levels or 20,000 entries, cached 60 s |
//! | `GET/HEAD /v1/shares/{share}/file` | `fetch` | single `Range`, `If-Range`, `If-Match`, strong `ETag`, 412 / 416 |
//! | `GET /v1/shares/{share}/hash` | `fetch` | blake3 / sha256, cached per ETag, 503 while computing |
//! | `GET /v1/zip?share=&path=` | `fetch` | a folder as a streamed ZIP (WP-68): bounded walk, `413 too_large` before streaming, `dry_run=1` for the totals, no `Range` |
//! | `GET /v1/shares/{share}/thumb` | `preview` | WP-15 [`ThumbSource`] |
//! | `GET /v1/shares/{share}/preview/info`, `/preview`, `/preview/text` | `preview` | WP-16 [`PreviewSource`] |
//! | `GET /v1/shares/{share}/preview/tile` | `preview` | WP-67 deep-zoom tiles, [`PreviewSource::tile`] |
//! | `POST /v1/stream` | share visible | WP-19 policy, then the WP-53 lifecycle ([`ApiState::set_streams`]); `stream_unavailable` without it |
//! | `DELETE /v1/stream/{session}` | the session's device | `close_app` per `close_on_end`; 204, or `not_found` |
//!
//! `GET /v1/host` and the pairing endpoints (`POST /v1/pair`, `GET /v1/pair/{id}`,
//! `POST /v1/token/rotate`) come from [`crate::server::hello`] and
//! [`crate::pairing::PairingService::router`]; [`host_routes`] merges them, plus
//! `GET /v1/events` when the state was built [`ApiState::with_events`].
//!
//! # Events and configuration changes
//!
//! With [`ApiState::with_events`] the API and the events channel share one view of the shares:
//!
//! - `/list` arms the directory watch BEFORE it enumerates ([`Events::touch_armed`], "arm, then
//!   list"), so a change made while the listing is read still produces `share.changed`;
//! - [`ApiState::set_config`] also replaces the events' [`ConfigShares`] and sends
//!   `shares.updated`, so clients re-fetch `/shares`;
//! - `capabilities.events` in `/me` is `true`.
//!
//! [`ApiState::set_network`] (the server's [`TrustNetwork`], available once it started) lets
//! `/me` report `network.relayed` for the caller's peer.
//!
//! # Rules every handler follows
//!
//! - The `path` query value is percent-decoded exactly once ([`query::RawQuery`], `+` is a
//!   literal plus) and handed to the WP-11 resolver as bytes; nothing is ever joined onto a root.
//!   All I/O goes through the resolved handle.
//! - A share the device holds no permission on is `404 share_not_found` (its existence is not
//!   revealed); a missing permission is `403 permission_denied` naming the permission.
//! - Resolver rejections map to `bad_path` / `not_found` / `forbidden`
//!   ([`gander_core::path::ResolveError::error_code`]); a step-7 escape is also reported with
//!   [`AuditLog::resolver_bug`]. Refusals and every fetch, hash and stream request are recorded
//!   as [`kind::ACCESS`] events (`detail.action`, `detail.share`, `detail.path`); listings,
//!   thumbnails and previews are not audited (they would flood the Activity window).
//! - File-system work runs on the blocking pool.
//!
//! # List ETag and cursor contract
//!
//! The list `ETag` (weak, `W/"<32 hex>"`, also in the body as `etag`) covers the WHOLE listing
//! of the directory for the request's `sort`, `order`, `kinds` and `ext`, not just the page:
//! every enumerated entry (name, kind, size, mtime, ctime, attributes) and its per-device
//! decoration (`mime`, `thumb`, `preview`, `stream`), plus `truncated` and `unlistable`. `limit`
//! and `cursor` are not part of it, so a client that stored all pages can revalidate the first
//! page with `If-None-Match` and treat a 304 as "every stored page is current". A cursor encodes
//! the position (sort key and name) of the last entry returned, not an offset, so it stays valid
//! (and yields no duplicates or gaps) as long as the ETag is unchanged; after a change it still
//! continues after the same position.

mod browse;
mod fetch;
mod folderzip;
mod hash;
pub mod indexed;
pub mod meta;
mod mime;
mod misc;
pub mod query;
mod render;

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::{delete, get, post};
use gander_core::config::{AppCatalogue, HostConfig, ShareConfig};
use gander_core::error::ErrorCode;
use gander_core::path::{ListedEntry, ResolveError};
use gander_core::policy::{
    AssocError, AssociationResolver, HandlerInfo, ListingPolicy, PolicyEnvironment, StreamPolicy,
};
use gander_core::protocol::files::{
    Entry, EntryKind, Permission, PreviewInfo, PreviewKind, StreamDecision,
};
use gander_core::protocol::host::{HostDetails, OfficePreview, PreviewKinds};
use gander_core::protocol::{DeviceId, HostId, ShareId};
use gander_win::path::{Resolved, ShareRoot};
use gander_win::preview::service::{
    InfoRequest, PageRequest, PageResponse, PreviewError, PreviewService, TextRequest,
    TextResponse, TileRequest, TileResponse,
};
use gander_win::thumbs::service::{
    ThumbError, ThumbRenderer, ThumbRequest, ThumbResponse, ThumbService,
};

use crate::audit::{AuditEvent, AuditLog, Outcome, kind};
use crate::events::{ConfigShares, Events};
use crate::pairing::{DeviceRegistry, PairingService};
use crate::server::{ApiError, Caller, TrustNetwork, hello};

pub use indexed::{IndexBackend, IndexFailure, IndexRun, IndexStatus, WindowsSearch};
pub use query::RawQuery;

/// Merges every host router: `GET /v1/host`, pairing (incl. `POST /v1/token/rotate`), the
/// Browse/Fetch/preview API and, when the state has [`ApiState::with_events`], `GET /v1/events`.
/// The binary and WP-1B's e2e harness use this.
pub fn host_routes(host_id: HostId, pairing: &Arc<PairingService>, api: &Arc<ApiState>) -> Router {
    let routes = Router::new()
        .merge(hello::router(host_id, pairing.pairing_open_flag()))
        .merge(pairing.router())
        .merge(api.router());
    match &api.events {
        Some(link) => routes.merge(crate::events::router(&link.events)),
        None => routes,
    }
}

/// Tunables (defaults from the protocol spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiOptions {
    /// `limits.list_max` (1000).
    pub list_max: u32,
    /// `limits.search_max` (500), also the cap of `/recent`'s `limit`.
    pub search_max: u32,
    /// `limits.concurrency` reported in `/me` (8, enforced by the gate).
    pub concurrency: u32,
    /// Host time one search request may spend (2 s).
    pub search_budget: Duration,
    /// How long a suspended search walk is kept for its cursor (5 min).
    pub search_session_ttl: Duration,
    /// How long a share's recent-files walk is reused (60 s).
    pub recent_ttl: Duration,
    /// Directory levels the recent walk covers (3: the root, its children, their children).
    pub recent_levels: usize,
    /// Entries the recent walk enumerates per share before it stops (20,000).
    pub recent_max_entries: usize,
    /// Files up to this size are hashed inside the request; larger ones in the background with
    /// `503 generating` meanwhile (16 MiB).
    pub hash_inline_max: u64,
    /// Background hash jobs running at once (2).
    pub hash_concurrency: usize,
    /// `capabilities.events`; [`ApiState::with_events`] sets it.
    pub events: bool,
    /// Folder zips streaming at once, host-wide (2); more get `429 too_many_requests`.
    pub zip_concurrency: usize,
    /// Wall-clock time the walk that measures a folder for a zip may take (30 s); over it is
    /// `413 too_large`. The size and entry limits are `[zip]` in `config.toml`.
    pub zip_walk_budget: Duration,
}

impl Default for ApiOptions {
    fn default() -> Self {
        Self {
            list_max: 1000,
            search_max: 500,
            concurrency: 8,
            search_budget: Duration::from_secs(2),
            search_session_ttl: Duration::from_secs(300),
            recent_ttl: Duration::from_secs(60),
            recent_levels: 3,
            recent_max_entries: 20_000,
            hash_inline_max: 16 * 1024 * 1024,
            hash_concurrency: 2,
            events: false,
            zip_concurrency: 2,
            zip_walk_budget: Duration::from_secs(30),
        }
    }
}

/// The thumbnail service as the API uses it ([`ThumbService`] implements it).
pub trait ThumbSource: Send + Sync + std::fmt::Debug {
    /// The listing's `thumb` prediction for a file extension.
    fn predicts_thumb(&self, ext: &str) -> bool;
    /// `GET /thumb` after resolution.
    fn thumb(&self, req: ThumbRequest) -> Result<ThumbResponse, ThumbError>;
}

impl<R: ThumbRenderer> ThumbSource for ThumbService<R> {
    fn predicts_thumb(&self, ext: &str) -> bool {
        ThumbService::predicts_thumb(self, ext)
    }
    fn thumb(&self, req: ThumbRequest) -> Result<ThumbResponse, ThumbError> {
        ThumbService::thumb(self, req)
    }
}

/// The preview service as the API uses it ([`PreviewService`] implements it).
pub trait PreviewSource: Send + Sync + std::fmt::Debug {
    /// The listing's `preview` field for a file extension.
    fn predict(&self, ext: &str) -> PreviewKind;
    /// `capabilities.preview_kinds`.
    fn preview_kinds(&self) -> PreviewKinds;
    /// `GET /preview/info` (blocks up to the service's info wait).
    fn info(&self, req: InfoRequest) -> Result<PreviewInfo, PreviewError>;
    /// `GET /preview` (blocks up to the service's page wait).
    fn page(&self, req: PageRequest) -> Result<PageResponse, PreviewError>;
    /// `GET /preview/text`.
    fn text(&self, req: TextRequest) -> Result<TextResponse, PreviewError>;
    /// `GET /preview/tile` (blocks up to the service's page wait).
    fn tile(&self, req: TileRequest) -> Result<TileResponse, PreviewError>;
}

impl<R: gander_win::preview::runner::JobRunner> PreviewSource for PreviewService<R> {
    fn predict(&self, ext: &str) -> PreviewKind {
        PreviewService::predict(self, ext)
    }
    fn preview_kinds(&self) -> PreviewKinds {
        PreviewService::preview_kinds(self)
    }
    fn info(&self, req: InfoRequest) -> Result<PreviewInfo, PreviewError> {
        PreviewService::info(self, req)
    }
    fn page(&self, req: PageRequest) -> Result<PageResponse, PreviewError> {
        PreviewService::page(self, req)
    }
    fn text(&self, req: TextRequest) -> Result<TextResponse, PreviewError> {
        PreviewService::text(self, req)
    }
    fn tile(&self, req: TileRequest) -> Result<TileResponse, PreviewError> {
        PreviewService::tile(self, req)
    }
}

/// Association lookups memoised for a minute: the production resolver reads Authenticode
/// signatures, which is far too slow to repeat for every listing.
struct CachedAssociations {
    inner: Arc<dyn AssociationResolver + Send + Sync>,
    ttl: Duration,
    memo: Mutex<HashMap<String, AssocMemo>>,
}

/// One memoised association lookup and when it was made.
type AssocMemo = (Instant, Result<Option<HandlerInfo>, AssocError>);

impl AssociationResolver for CachedAssociations {
    fn resolve(&self, ext: &str) -> Result<Option<HandlerInfo>, AssocError> {
        if let Some((at, v)) = self.memo.lock().unwrap_or_else(|e| e.into_inner()).get(ext)
            && at.elapsed() < self.ttl
        {
            return v.clone();
        }
        let v = self.inner.resolve(ext);
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if memo.len() > 4096 {
            memo.clear();
        }
        memo.insert(ext.to_owned(), (Instant::now(), v.clone()));
        v
    }

    fn inspect(&self, exe: &std::path::Path) -> Result<HandlerInfo, AssocError> {
        self.inner.inspect(exe)
    }
}

/// What a share root was opened with; a config change that alters it reopens the root.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RootKey {
    root: PathBuf,
    show_hidden: bool,
    hidden_patterns: Vec<String>,
}

impl RootKey {
    fn of(share: &ShareConfig) -> Self {
        Self {
            root: share.root.clone(),
            show_hidden: share.show_hidden,
            hidden_patterns: share.hidden_patterns.clone(),
        }
    }
}

/// Who is asking, for permission checks and the audit log.
#[derive(Debug, Clone)]
pub struct Requester {
    /// The authenticated device.
    pub device: DeviceId,
    /// The connection's source address, when known.
    pub source: Option<IpAddr>,
}

impl Requester {
    /// From the gate's extensions. `Caller::Anonymous` never reaches these routes with the
    /// token authenticator; it is refused as `token_missing` all the same.
    pub fn new(caller: &Caller, source: Option<IpAddr>) -> Result<Self, ApiError> {
        match caller {
            Caller::Device(id) => Ok(Self {
                device: id.clone(),
                source,
            }),
            Caller::Anonymous => Err(ApiError::new(
                ErrorCode::TokenMissing,
                "This request needs a device token.",
            )),
        }
    }
}

/// An item resolved for a request: the share, its root and the open handle.
#[derive(Debug)]
pub struct Opened {
    /// The share's configuration (a snapshot).
    pub share: ShareConfig,
    /// The opened share root.
    pub root: Arc<ShareRoot>,
    /// The resolved item.
    pub resolved: Resolved,
}

/// The events channel as the API drives it.
#[derive(Debug, Clone)]
struct EventsLink {
    events: Events,
    shares: Arc<ConfigShares>,
}

/// State behind the API routes. Build it once, share it as `Arc<ApiState>`.
pub struct ApiState {
    config: RwLock<Arc<HostConfig>>,
    catalogue: RwLock<Arc<AppCatalogue>>,
    roots: Mutex<HashMap<ShareId, (RootKey, Arc<ShareRoot>)>>,
    registry: Arc<DeviceRegistry>,
    audit: Arc<AuditLog>,
    host: RwLock<HostDetails>,
    associations: Arc<CachedAssociations>,
    environment: PolicyEnvironment,
    thumbs: Option<Arc<dyn ThumbSource>>,
    previews: Option<Arc<dyn PreviewSource>>,
    previews_unavailable: Option<gander_core::protocol::host::PreviewsUnavailable>,
    options: ApiOptions,
    hashes: hash::HashCache,
    zip_slots: Arc<tokio::sync::Semaphore>,
    recent: Mutex<HashMap<ShareId, Arc<browse::RecentWalk>>>,
    searches: Mutex<HashMap<String, browse::SearchSession>>,
    /// The Windows Search index (WP-66); `None` turns the index off and every search walks.
    index: Option<Arc<dyn IndexBackend>>,
    /// Per share: the root key, when the answer was found, and whether the root is indexed.
    index_status: Mutex<HashMap<ShareId, (RootKey, Instant, bool)>>,
    events: Option<EventsLink>,
    network: RwLock<Option<Arc<TrustNetwork>>>,
    streams: RwLock<Option<crate::lifecycle::StreamSessions>>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("options", &self.options)
            .field("thumbs", &self.thumbs.is_some())
            .field("previews", &self.previews.is_some())
            .finish_non_exhaustive()
    }
}

impl ApiState {
    /// State with the production association lookup (`AssocQueryStringW`), `%SystemRoot%` from
    /// the OS, an empty app catalogue, no thumbnail or preview service and default options.
    pub fn new(
        config: HostConfig,
        registry: Arc<DeviceRegistry>,
        audit: Arc<AuditLog>,
        host: HostDetails,
    ) -> Self {
        let options = ApiOptions::default();
        Self {
            config: RwLock::new(Arc::new(config)),
            catalogue: RwLock::new(Arc::new(AppCatalogue::default())),
            roots: Mutex::new(HashMap::new()),
            registry,
            audit,
            host: RwLock::new(host),
            associations: Arc::new(CachedAssociations {
                inner: Arc::new(gander_win::assoc::WindowsAssociations),
                ttl: Duration::from_secs(60),
                memo: Mutex::new(HashMap::new()),
            }),
            environment: gander_win::assoc::policy_environment(),
            thumbs: None,
            previews: None,
            previews_unavailable: None,
            hashes: hash::HashCache::new(options.hash_concurrency),
            zip_slots: Arc::new(tokio::sync::Semaphore::new(options.zip_concurrency)),
            options,
            recent: Mutex::new(HashMap::new()),
            searches: Mutex::new(HashMap::new()),
            index: Some(Arc::new(WindowsSearch)),
            index_status: Mutex::new(HashMap::new()),
            events: None,
            network: RwLock::new(None),
            streams: RwLock::new(None),
        }
    }

    /// Replaces the options (`events` stays `true` once [`ApiState::with_events`] was applied).
    pub fn with_options(mut self, options: ApiOptions) -> Self {
        self.hashes = hash::HashCache::new(options.hash_concurrency);
        self.zip_slots = Arc::new(tokio::sync::Semaphore::new(options.zip_concurrency));
        let events = self.events.is_some();
        self.options = options;
        self.options.events |= events;
        self
    }

    /// Connects the events channel (WP-17): `/list` arms directory watches before it enumerates,
    /// [`ApiState::set_config`] replaces `shares` (the [`ConfigShares`] the events were built
    /// with) and sends `shares.updated`, [`host_routes`] serves `GET /v1/events`, and `/me`
    /// reports `capabilities.events = true`.
    pub fn with_events(mut self, events: Events, shares: Arc<ConfigShares>) -> Self {
        self.events = Some(EventsLink { events, shares });
        self.options.events = true;
        self
    }

    /// The events channel, if connected.
    pub fn events(&self) -> Option<&Events> {
        self.events.as_ref().map(|l| &l.events)
    }

    /// The private-network side of the trust adapter (`Server::network()`, available once the
    /// server started): `/me` asks it for the caller's `network.relayed`.
    pub fn set_network(&self, network: Option<Arc<TrustNetwork>>) {
        *self.network.write().unwrap_or_else(|e| e.into_inner()) = network;
    }

    fn network(&self) -> Option<Arc<TrustNetwork>> {
        self.network
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Installs the Stream session lifecycle (WP-53): `POST /v1/stream` starts sessions,
    /// `DELETE /v1/stream/{session}` ends them, `/me` reports `capabilities.stream.enabled`.
    /// Set once the server runs (the engine binds ICE on the listener's addresses).
    pub fn set_streams(&self, streams: Option<crate::lifecycle::StreamSessions>) {
        *self.streams.write().unwrap_or_else(|e| e.into_inner()) = streams;
    }

    /// The Stream session lifecycle, if installed.
    pub fn streams(&self) -> Option<crate::lifecycle::StreamSessions> {
        self.streams
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The device registry.
    pub fn registry(&self) -> Arc<DeviceRegistry> {
        Arc::clone(&self.registry)
    }

    /// The app catalogue in force (a snapshot).
    pub fn current_catalogue(&self) -> Arc<AppCatalogue> {
        self.catalogue()
    }

    /// The policy environment.
    pub fn environment(&self) -> &PolicyEnvironment {
        &self.environment
    }

    /// Replaces the association lookup (tests use `StaticAssociations`).
    pub fn with_associations(
        mut self,
        resolver: Arc<dyn AssociationResolver + Send + Sync>,
    ) -> Self {
        self.associations = Arc::new(CachedAssociations {
            inner: resolver,
            ttl: Duration::from_secs(60),
            memo: Mutex::new(HashMap::new()),
        });
        self
    }

    /// Replaces the Windows Search backend (tests use a fake); `None` turns the index off, so
    /// every share offers the bounded walk only.
    pub fn with_search_index(mut self, index: Option<Arc<dyn IndexBackend>>) -> Self {
        self.index = index;
        self
    }

    /// Replaces the policy environment.
    pub fn with_environment(mut self, environment: PolicyEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Sets the app catalogue (`apps.toml`).
    pub fn with_catalogue(self, catalogue: AppCatalogue) -> Self {
        self.set_catalogue(catalogue);
        self
    }

    /// Serves `/thumb` and the listing's `thumb` prediction from `thumbs`.
    pub fn with_thumbs(mut self, thumbs: Arc<dyn ThumbSource>) -> Self {
        self.thumbs = Some(thumbs);
        self
    }

    /// Serves `/preview*`, the listing's `preview` field and `stat?meta=1` (worker `Info`) from
    /// `previews`.
    pub fn with_previews(mut self, previews: Arc<dyn PreviewSource>) -> Self {
        self.previews = Some(previews);
        self
    }

    /// Records why previews and thumbnails are off (`/me` `capabilities.previews_unavailable`,
    /// the tray's Settings window).
    pub fn with_previews_unavailable(
        mut self,
        why: gander_core::protocol::host::PreviewsUnavailable,
    ) -> Self {
        self.previews_unavailable = Some(why);
        self
    }

    /// Why previews and thumbnails are off, if they are.
    pub fn previews_unavailable(&self) -> Option<gander_core::protocol::host::PreviewsUnavailable> {
        self.previews_unavailable
    }

    /// Applies a new `config.toml` (tray, WP-18): share roots are reopened lazily and the
    /// recent-files cache is dropped. With events connected, the events' share view is replaced
    /// too and every open socket is sent `shares.updated`.
    pub fn set_config(&self, config: HostConfig) {
        if let Some(link) = &self.events {
            link.shares.replace(config.clone());
        }
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(config);
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.searches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.index_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if let Some(link) = &self.events {
            link.events.shares_updated();
        }
    }

    /// The configuration in force (a snapshot).
    pub fn current_config(&self) -> Arc<HostConfig> {
        self.config()
    }

    /// Applies a new app catalogue.
    pub fn set_catalogue(&self, catalogue: AppCatalogue) {
        *self.catalogue.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(catalogue);
    }

    /// Updates the host facts `/me` reports (addresses after the listener bound).
    pub fn set_host_details(&self, host: HostDetails) {
        *self.host.write().unwrap_or_else(|e| e.into_inner()) = host;
    }

    /// The options in force.
    pub fn options(&self) -> &ApiOptions {
        &self.options
    }

    fn config(&self) -> Arc<HostConfig> {
        Arc::clone(&self.config.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn catalogue(&self) -> Arc<AppCatalogue> {
        Arc::clone(&self.catalogue.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// The router with every API route (state applied).
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/v1/me", get(misc::me))
            .route("/v1/shares", get(browse::shares))
            .route("/v1/shares/{share}/list", get(browse::list))
            .route("/v1/shares/{share}/stat", get(browse::stat))
            .route("/v1/shares/{share}/search", get(browse::search))
            .route("/v1/recent", get(browse::recent))
            .route("/v1/shares/{share}/file", get(fetch::file))
            .route("/v1/shares/{share}/hash", get(fetch::hash))
            .route("/v1/zip", get(folderzip::zip))
            .route("/v1/shares/{share}/thumb", get(render::thumb))
            .route("/v1/shares/{share}/preview/info", get(render::preview_info))
            .route("/v1/shares/{share}/preview", get(render::preview_page))
            .route("/v1/shares/{share}/preview/text", get(render::preview_text))
            .route("/v1/shares/{share}/preview/tile", get(render::preview_tile))
            .route("/v1/stream", post(misc::stream))
            .route("/v1/stream/{session}", delete(misc::stream_end))
            .with_state(Arc::clone(self))
    }

    // -----------------------------------------------------------------------------------------
    // Access checks (blocking)

    /// The share as `who` may see it: `share_not_found` without any permission, then
    /// `permission_denied` when `need` is missing.
    pub fn share_for(
        &self,
        who: &Requester,
        share: &ShareId,
        need: Option<Permission>,
        action: &str,
    ) -> Result<ShareConfig, ApiError> {
        let config = self.config();
        let Some(s) = config
            .share(share)
            .filter(|s| !s.permissions_for(&who.device).is_empty())
        else {
            return Err(ApiError::new(ErrorCode::ShareNotFound, "No such share."));
        };
        if let Some(p) = need
            && !s.allows(&who.device, p)
        {
            let name = permission_name(p);
            self.refused(
                who,
                share,
                None,
                action,
                &format!("permission_denied:{name}"),
            );
            return Err(ApiError::new(
                ErrorCode::PermissionDenied,
                format!("This device does not have the \"{name}\" permission for this share."),
            ));
        }
        Ok(s.clone())
    }

    fn root_of(&self, share: &ShareConfig) -> Result<Arc<ShareRoot>, ApiError> {
        let key = RootKey::of(share);
        let mut roots = self.roots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, root)) = roots.get(&share.id)
            && *k == key
        {
            return Ok(Arc::clone(root));
        }
        match ShareRoot::from_config(share) {
            Ok(root) => {
                let root = Arc::new(root);
                roots.insert(share.id.clone(), (key, Arc::clone(&root)));
                Ok(root)
            }
            Err(e) => {
                tracing::warn!(share = %share.id, error = %e, "share root cannot be opened");
                roots.remove(&share.id);
                Err(ApiError::new(
                    ErrorCode::ShareNotFound,
                    "The shared folder is not available on the computer right now.",
                ))
            }
        }
    }

    /// Checks access and resolves `path` (bytes, percent-decoded once) inside the share. Blocking:
    /// call from the blocking pool.
    pub fn open(
        &self,
        who: &Requester,
        share: &ShareId,
        need: Permission,
        path: &[u8],
        action: &str,
    ) -> Result<Opened, ApiError> {
        let share_cfg = self.share_for(who, share, Some(need), action)?;
        let root = self.root_of(&share_cfg)?;
        let resolved = self.resolve_in(who, &share_cfg.id, &root, path, action)?;
        Ok(Opened {
            share: share_cfg,
            root,
            resolved,
        })
    }

    /// Resolves `path` in an opened root, mapping and auditing rejections.
    fn resolve_in(
        &self,
        who: &Requester,
        share: &ShareId,
        root: &ShareRoot,
        path: &[u8],
        action: &str,
    ) -> Result<Resolved, ApiError> {
        root.resolve(path)
            .map_err(|e| self.resolve_error(who, share, path, action, e))
    }

    fn resolve_error(
        &self,
        who: &Requester,
        share: &ShareId,
        path: &[u8],
        action: &str,
        e: ResolveError,
    ) -> ApiError {
        let shown = String::from_utf8_lossy(path);
        match e.reason() {
            Some(reason) if reason.is_bug() => {
                self.audit
                    .resolver_bug(&who.device, who.source, share, &shown, reason.as_str());
            }
            // A plain "does not exist" is not worth an audit line; every policy refusal
            // (including denied reparse points, which are also sent as `not_found`) is.
            Some(reason) if reason != gander_core::path::RejectReason::NotFound => {
                self.refused(who, share, Some(&shown), action, reason.as_str());
            }
            Some(_) => {}
            None => tracing::warn!(share = %share, error = %e, "path resolution failed"),
        }
        let code = e.error_code();
        let message = match code {
            ErrorCode::NotFound => "No such file or folder.",
            ErrorCode::Forbidden => "The computer refused access to this item.",
            ErrorCode::BadPath => "This path is not allowed.",
            _ => "Internal error.",
        };
        ApiError::new(code, message)
    }

    fn refused(
        &self,
        who: &Requester,
        share: &ShareId,
        path: Option<&str>,
        action: &str,
        reason: &str,
    ) {
        let mut ev = AuditEvent::new(kind::ACCESS)
            .device(&who.device)
            .outcome(Outcome::Refused)
            .detail("action", action)
            .detail("share", share.as_str())
            .detail("reason", reason);
        if let Some(p) = path {
            ev = ev.detail("path", p);
        }
        if let Some(s) = who.source {
            ev = ev.source(s);
        }
        self.audit.record(ev);
    }

    /// Records a successful access (fetch, hash, stream request).
    fn accessed(&self, who: &Requester, share: &ShareId, path: &str, action: &str) -> AuditEvent {
        let mut ev = AuditEvent::new(kind::ACCESS)
            .device(&who.device)
            .detail("action", action)
            .detail("share", share.as_str())
            .detail("path", path);
        if let Some(s) = who.source {
            ev = ev.source(s);
        }
        ev
    }

    // -----------------------------------------------------------------------------------------
    // Decoration

    /// Runs `f` with an entry decorator for `who` in `share`.
    fn with_decorator<T>(
        &self,
        who: &Requester,
        share: &ShareConfig,
        f: impl FnOnce(&mut Decorator<'_>) -> T,
    ) -> T {
        let catalogue = self.catalogue();
        let resolver: &dyn AssociationResolver = &*self.associations;
        let policy = StreamPolicy::new(&catalogue, resolver, &self.environment);
        let mut d = Decorator {
            policy: ListingPolicy::new(policy, share, &who.device),
            thumbs: self.thumbs.as_deref(),
            previews: self.previews.as_deref(),
            can_preview: share.allows(&who.device, Permission::Preview),
            memo: HashMap::new(),
        };
        f(&mut d)
    }

    fn preview_kinds(&self) -> PreviewKinds {
        self.previews
            .as_ref()
            .map_or(NO_PREVIEWS, |p| p.preview_kinds())
    }
}

const NO_PREVIEWS: PreviewKinds = PreviewKinds {
    image: false,
    pdf: false,
    text: false,
    office: OfficePreview::None,
    video_poster: false,
    raw: false,
};

fn permission_name(p: Permission) -> &'static str {
    match p {
        Permission::List => "list",
        Permission::Preview => "preview",
        Permission::Fetch => "fetch",
        Permission::Stream => "stream",
    }
}

/// Per-file decoration that depends only on the extension (and the device's permissions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExtDeco {
    mime: Option<&'static str>,
    thumb: bool,
    preview: PreviewKind,
}

/// The decoration of one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Deco {
    mime: Option<&'static str>,
    thumb: bool,
    preview: PreviewKind,
    stream: StreamDecision,
}

/// Fills `mime`, `thumb`, `preview` and `stream` for one device and share.
pub(crate) struct Decorator<'a> {
    policy: ListingPolicy<'a>,
    thumbs: Option<&'a dyn ThumbSource>,
    previews: Option<&'a dyn PreviewSource>,
    can_preview: bool,
    memo: HashMap<Option<String>, ExtDeco>,
}

impl Decorator<'_> {
    pub(crate) fn deco(&mut self, e: &ListedEntry) -> Deco {
        if e.kind == EntryKind::Dir {
            return Deco {
                mime: None,
                thumb: false,
                preview: PreviewKind::None,
                stream: StreamDecision::Refused,
            };
        }
        let ext = e.ext();
        let ext_deco = match self.memo.get(&ext) {
            Some(d) => *d,
            None => {
                let e_str = ext.as_deref().unwrap_or("");
                let d = ExtDeco {
                    mime: ext.as_deref().and_then(mime::for_ext),
                    thumb: self.can_preview
                        && !e_str.is_empty()
                        && self.thumbs.is_some_and(|t| t.predicts_thumb(e_str)),
                    preview: match self.previews {
                        Some(p) if self.can_preview && !e_str.is_empty() => p.predict(e_str),
                        _ => PreviewKind::None,
                    },
                };
                self.memo.insert(ext, d);
                d
            }
        };
        if e.attrs.link {
            // A denied reparse point is shown greyed and is never openable.
            return Deco {
                mime: ext_deco.mime,
                thumb: false,
                preview: PreviewKind::None,
                stream: StreamDecision::Refused,
            };
        }
        Deco {
            mime: ext_deco.mime,
            thumb: ext_deco.thumb,
            preview: ext_deco.preview,
            stream: self.policy.decision(&e.name),
        }
    }

    pub(crate) fn entry(&mut self, e: &ListedEntry) -> Entry {
        let d = self.deco(e);
        let mut out = e.to_entry();
        out.mime = d.mime.map(str::to_owned);
        out.thumb = d.thumb;
        out.preview = d.preview;
        out.stream = d.stream;
        out
    }
}

/// Runs blocking work on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err(internal()))
}

fn internal() -> ApiError {
    ApiError::new(ErrorCode::Internal, "Internal error.")
}

fn bad_request(message: &str) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, message)
}

/// Whether an `If-None-Match` value matches `etag` (weak comparison, lists and `*`).
fn none_match(header: &str, etag: &str) -> bool {
    let strip = |s: &str| s.trim().trim_start_matches("W/").to_owned();
    let ours = strip(etag);
    header
        .split(',')
        .map(str::trim)
        .any(|t| t == "*" || strip(t) == ours)
}

#[cfg(test)]
mod tests {
    #[test]
    fn if_none_match_forms() {
        let e = "W/\"abc\"";
        assert!(super::none_match("W/\"abc\"", e));
        assert!(super::none_match("\"abc\"", e));
        assert!(super::none_match("\"x\", W/\"abc\"", e));
        assert!(super::none_match("*", e));
        assert!(!super::none_match("\"abd\"", e));
    }
}
