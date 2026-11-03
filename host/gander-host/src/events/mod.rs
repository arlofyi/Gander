// SPDX-License-Identifier: Apache-2.0
//! The events channel: `GET /v1/events`, a WebSocket that pushes what changes on the host
//! (WP-17; `docs/specs/2026-11-01-protocol-v1.md`, "Events").
//!
//! # What a client sees
//!
//! After the upgrade (bearer token in `Authorization`, like every other request) the host sends
//! `hello` and the current `host.state`, then pushes:
//!
//! | Message | When |
//! | --- | --- |
//! | `host.state { locked, attended, on_battery, stream_available }` | any of the four changes (see [`hoststate`]) |
//! | `share.changed { share, path }` | something changed in a directory this host is watching ([`watch`]), debounced 500 ms; only for shares the device may list |
//! | `shares.updated {}` | [`Events::shares_updated`] (the owner changed shares or permissions) |
//! | `device.revoked {}` | the device was revoked; the socket closes right after |
//! | `pong {}` | answer to the client's `ping {}` |
//! | `error { code, message, session? }` | a refused client message: every `stream.*` message gets `stream_unavailable` until Phase 2 |
//!
//! The server also sends a WebSocket ping every 20 s and drops the socket after two unanswered
//! ones. The kill switch (`Server::disconnect_all`) and revocation
//! ([`crate::server::ConnHandle`]) close it within milliseconds.
//!
//! # Wiring (application)
//!
//! ```no_run
//! # #[cfg(windows)]
//! # async fn demo(registry: std::sync::Arc<gander_host::pairing::DeviceRegistry>,
//! #               config: gander_core::config::HostConfig) -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use gander_host::events::{ConfigShares, Events, EventsOptions, system};
//!
//! let shares = ConfigShares::new(config);
//! let events = Events::new(EventsOptions {
//!     registry,
//!     shares: shares.clone(),
//!     host: system::host_state_monitor(&Default::default())?,
//!     backend: Arc::new(system::WindowsWatch::new(shares)?),
//!     config: Default::default(),
//! });
//! let routes = axum::Router::new().merge(gander_host::events::router(&events));
//! // WP-14: call `events.touch_armed(&share, dir).await` before enumerating a directory in /list.
//! // WP-18: call `events.shares_updated()` after a shares or permissions change.
//! # let _ = routes; Ok(()) }
//! ```
//!
//! # API by consumer
//!
//! - **WP-14 (`/list`)**: `events.touch_armed(&share, dir).await` BEFORE enumerating the
//!   directory (share id, share-relative directory; the first call waits a few milliseconds for
//!   the watch to open so the listing cannot miss a change, later calls return at once), or
//!   [`Events::touch`] (non-blocking) after it.
//! - **WP-18 (tray)**: [`Events::shares_updated`], [`Events::open_sockets`] (connected state).
//! - **Phase 2 (stream engine)**: [`Events::send_to`] pushes `stream.state` / `stream.offer` /
//!   `stream.ice` to every open socket of one device; implement [`StreamSignalling`] and install
//!   it with [`Events::set_stream_signalling`] to receive `stream.answer` / `ice` / `resize` /
//!   `pause` / `resume`. The lock state consumed by Stream is `gander_win::session::watch`.

pub mod hoststate;
mod socket;
#[cfg(windows)]
pub mod system;
pub mod watch;

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use gander_core::config::{HostConfig, ShareConfig};
use gander_core::error::ErrorCode;
use gander_core::protocol::events::{ClientEvent, HostStateEvent, ServerEvent};
use gander_core::protocol::files::Permission;
use gander_core::protocol::{DeviceId, SessionId, ShareId};
use tokio::sync::{Notify, broadcast};

pub use hoststate::{
    HostProbe, HostStateConfig, HostStateMonitor, ManualProbe, OverrideProbe,
    compute as compute_host_state,
};
pub use watch::{
    ChangeNotifier, ChangeSink, WatchBackend, WatchConfig, WatchEvent, WatchHandle, WatchRegistry,
};

use crate::pairing::DeviceRegistry;
use crate::server::{ApiError, Caller, ConnHandle, ConnInfo};

/// Who may hear about changes in which share.
pub trait ShareAccess: Send + Sync + fmt::Debug + 'static {
    /// The device holds the `list` permission in the share (deny when the share is unknown).
    fn may_list(&self, device: &DeviceId, share: &ShareId) -> bool;
}

/// [`ShareAccess`] and share lookup over a [`HostConfig`] that can be replaced when
/// `config.toml` changes.
#[derive(Debug)]
pub struct ConfigShares {
    config: RwLock<HostConfig>,
}

impl ConfigShares {
    /// Shares and grants as in `config`.
    pub fn new(config: HostConfig) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(config),
        })
    }

    /// Replaces the configuration (call [`Events::shares_updated`] afterwards so clients re-fetch
    /// `/shares`).
    pub fn replace(&self, config: HostConfig) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// A copy of one share's configuration.
    pub fn share(&self, id: &ShareId) -> Option<ShareConfig> {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .share(id)
            .cloned()
    }
}

impl ShareAccess for ConfigShares {
    fn may_list(&self, device: &DeviceId, share: &ShareId) -> bool {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .share(share)
            .is_some_and(|s| s.allows(device, Permission::List))
    }
}

/// Handles `stream.*` messages from clients. Phase 1 installs [`StreamUnavailable`].
pub trait StreamSignalling: Send + Sync + fmt::Debug + 'static {
    /// Handles `event` (never `ping`) from `device`; the returned message, if any, is sent back
    /// on the same socket. Push anything else with [`Events::send_to`].
    fn on_client_event(&self, device: &DeviceId, event: ClientEvent) -> Option<ServerEvent>;
}

/// Phase 1: Stream mode does not exist yet, so every signalling message is refused with
/// `stream_unavailable`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamUnavailable;

/// The session a client stream message names.
fn session_of(event: &ClientEvent) -> Option<&SessionId> {
    match event {
        ClientEvent::StreamAnswer { session, .. }
        | ClientEvent::StreamIce { session, .. }
        | ClientEvent::StreamResize { session, .. }
        | ClientEvent::StreamPause { session }
        | ClientEvent::StreamResume { session }
        | ClientEvent::StreamPick { session, .. } => Some(session),
        ClientEvent::Ping {} => None,
    }
}

impl StreamSignalling for StreamUnavailable {
    fn on_client_event(&self, _device: &DeviceId, event: ClientEvent) -> Option<ServerEvent> {
        Some(ServerEvent::Error {
            code: ErrorCode::StreamUnavailable,
            message: "Stream mode is not available on this computer yet.".to_owned(),
            session: session_of(&event).cloned(),
        })
    }
}

/// Timing and limits of the channel.
#[derive(Debug, Clone)]
pub struct EventsConfig {
    /// WebSocket ping interval (20 s).
    pub heartbeat_interval: Duration,
    /// Unanswered pings before the socket is dropped (2).
    pub missed_pongs: u32,
    /// A send that takes longer than this drops the socket (a stuck client must not pile up
    /// events): 10 s.
    pub send_timeout: Duration,
    /// Largest client message accepted (64 KiB; an SDP answer is a few KiB).
    pub max_message_bytes: usize,
    /// Sockets per device; the oldest is closed when a newer one exceeds it (4). A phone that
    /// changes network leaves a half-dead socket behind until its heartbeat times out.
    pub max_sockets_per_device: usize,
    /// Broadcast buffer between the producers and the sockets (256). A socket that falls this
    /// far behind is closed (1013) and reconnects.
    pub hub_capacity: usize,
    /// Directory watching.
    pub watch: WatchConfig,
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(20),
            missed_pongs: 2,
            send_timeout: Duration::from_secs(10),
            max_message_bytes: 64 * 1024,
            max_sockets_per_device: 4,
            hub_capacity: 256,
            watch: WatchConfig::default(),
        }
    }
}

/// What the sockets are told, besides `host.state` (which has its own channel).
#[derive(Debug, Clone)]
pub(crate) enum HubEvent {
    SharesUpdated,
    ShareChanged {
        share: ShareId,
        path: String,
    },
    Direct {
        device: DeviceId,
        event: Arc<ServerEvent>,
    },
}

/// One open socket of a device, for the per-device cap.
#[derive(Debug)]
struct Slot {
    id: u64,
    evict: Arc<Notify>,
}

pub(crate) struct Inner {
    pub(crate) registry: Arc<DeviceRegistry>,
    pub(crate) shares: Arc<dyn ShareAccess>,
    pub(crate) hub: broadcast::Sender<HubEvent>,
    pub(crate) host: HostStateMonitor,
    pub(crate) watches: Arc<WatchRegistry>,
    pub(crate) config: EventsConfig,
    pub(crate) streams: RwLock<Arc<dyn StreamSignalling>>,
    sockets: Mutex<HashMap<DeviceId, Vec<Slot>>>,
    next_slot: AtomicU64,
    open: AtomicUsize,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Events")
            .field("open_sockets", &self.open.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// An open socket's registration; dropping it frees the slot.
pub(crate) struct SocketRegistration {
    inner: Arc<Inner>,
    device: DeviceId,
    id: u64,
    /// Notified when a newer socket of the same device pushed this one out.
    pub(crate) evicted: Arc<Notify>,
}

impl Drop for SocketRegistration {
    fn drop(&mut self) {
        let mut sockets = self.inner.sockets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = sockets.get_mut(&self.device) {
            list.retain(|s| s.id != self.id);
            if list.is_empty() {
                sockets.remove(&self.device);
            }
        }
        self.inner.open.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Inner {
    pub(crate) fn register_socket(self: &Arc<Self>, device: &DeviceId) -> SocketRegistration {
        let id = self.next_slot.fetch_add(1, Ordering::Relaxed);
        let evict = Arc::new(Notify::new());
        {
            let mut sockets = self.sockets.lock().unwrap_or_else(|e| e.into_inner());
            let list = sockets.entry(device.clone()).or_default();
            let cap = self.config.max_sockets_per_device.max(1);
            while list.len() >= cap {
                // Oldest first: its `evicted` permit is stored even if it is not waiting yet.
                list.remove(0).evict.notify_one();
            }
            list.push(Slot {
                id,
                evict: Arc::clone(&evict),
            });
        }
        self.open.fetch_add(1, Ordering::Relaxed);
        SocketRegistration {
            inner: Arc::clone(self),
            device: device.clone(),
            id,
            evicted: evict,
        }
    }
}

/// Everything [`Events::new`] needs.
pub struct EventsOptions {
    /// Paired devices: revocation events and "is this device still active".
    pub registry: Arc<DeviceRegistry>,
    /// Who may hear about which share.
    pub shares: Arc<dyn ShareAccess>,
    /// Source of `host.state`.
    pub host: HostStateMonitor,
    /// How directories are watched (production: `system::WindowsWatch`).
    pub backend: Arc<dyn WatchBackend>,
    /// Timing and limits.
    pub config: EventsConfig,
}

impl fmt::Debug for EventsOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventsOptions").finish_non_exhaustive()
    }
}

/// The events service: producers push into it, [`router`] serves the sockets. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Events {
    pub(crate) inner: Arc<Inner>,
}

impl Events {
    /// Creates the service and starts its background tasks (host-state polling and the directory
    /// watch registry). Must be called inside a tokio runtime.
    pub fn new(options: EventsOptions) -> Self {
        let EventsOptions {
            registry,
            shares,
            host,
            backend,
            config,
        } = options;
        let (hub, _) = broadcast::channel(config.hub_capacity.max(1));
        let sink: ChangeSink = {
            let hub = hub.clone();
            Arc::new(move |share, dir| {
                // No receivers (nobody connected) is fine.
                let _ = hub.send(HubEvent::ShareChanged {
                    share: share.clone(),
                    path: dir.to_owned(),
                });
            })
        };
        let watches = WatchRegistry::spawn(backend, sink, config.watch.clone());
        Self {
            inner: Arc::new(Inner {
                registry,
                shares,
                hub,
                host,
                watches,
                config,
                streams: RwLock::new(Arc::new(StreamUnavailable)),
                sockets: Mutex::new(HashMap::new()),
                next_slot: AtomicU64::new(1),
                open: AtomicUsize::new(0),
            }),
        }
    }

    /// The router with `GET /v1/events`; same as [`router`].
    pub fn router(&self) -> Router {
        router(self)
    }

    /// The registry of recently listed directories.
    pub fn watch_registry(&self) -> Arc<WatchRegistry> {
        Arc::clone(&self.inner.watches)
    }

    /// Shorthand for `watch_registry().touch(share, dir)`: `dir` of `share` was just listed.
    pub fn touch(&self, share: &ShareId, dir: &str) {
        self.inner.watches.touch(share, dir);
    }

    /// Shorthand for `watch_registry().touch_armed(share, dir)`: arm the watch, then list.
    pub async fn touch_armed(&self, share: &ShareId, dir: &str) {
        self.inner.watches.touch_armed(share, dir).await;
    }

    /// Tells every open socket that shares or permissions changed (`shares.updated`); clients
    /// re-fetch `/shares`.
    pub fn shares_updated(&self) {
        let _ = self.inner.hub.send(HubEvent::SharesUpdated);
    }

    /// Pushes `event` to every open socket of `device` (Phase 2: `stream.state`, `stream.offer`,
    /// `stream.ice`). Dropped silently when the device has no socket.
    pub fn send_to(&self, device: &DeviceId, event: ServerEvent) {
        let _ = self.inner.hub.send(HubEvent::Direct {
            device: device.clone(),
            event: Arc::new(event),
        });
    }

    /// Installs the handler for client `stream.*` messages (default: [`StreamUnavailable`]).
    pub fn set_stream_signalling(&self, handler: Arc<dyn StreamSignalling>) {
        *self
            .inner
            .streams
            .write()
            .unwrap_or_else(|e| e.into_inner()) = handler;
    }

    /// The `host.state` clients currently see.
    pub fn host_state(&self) -> HostStateEvent {
        self.inner.host.current()
    }

    /// The host-state monitor (re-read now with [`HostStateMonitor::refresh_now`]).
    pub fn host_monitor(&self) -> &HostStateMonitor {
        &self.inner.host
    }

    /// Open `/events` sockets across all devices (the tray's "connected" state).
    pub fn open_sockets(&self) -> usize {
        self.inner.open.load(Ordering::Relaxed)
    }
}

/// The router serving `GET /v1/events`. Merge it into the application's routes.
pub fn router(events: &Events) -> Router {
    Router::new()
        .route("/v1/events", get(upgrade))
        .with_state(events.clone())
}

async fn upgrade(
    State(events): State<Events>,
    Extension(conn): Extension<ConnInfo>,
    Extension(caller): Extension<Caller>,
    Extension(handle): Extension<ConnHandle>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Caller::Device(device) = caller else {
        return ApiError::new(
            ErrorCode::TokenMissing,
            "This request needs a device token.",
        )
        .into_response();
    };
    let Ok(ws) = ws else {
        return ApiError::new(
            ErrorCode::BadRequest,
            "Expected a WebSocket upgrade request.",
        )
        .into_response();
    };
    let max = events.inner.config.max_message_bytes;
    let inner = Arc::clone(&events.inner);
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| socket::serve(socket, inner, device, handle, conn.disconnect))
}
