// SPDX-License-Identifier: Apache-2.0
//! The HTTPS listener: TLS 1.3, the trust adapters' listener plan, and the request gate.
//!
//! # Extension API for route packages (WP-13 pairing, WP-14 Browse/Fetch, WP-17 events, ...)
//!
//! Routes are plain axum [`Router`]s. A package builds its router (with its own state applied via
//! `Router::with_state`, so the result is `Router<()>`), the application merges them and hands the
//! result to [`Server::start`]:
//!
//! ```no_run
//! # async fn demo(identity: gander_host::server::HostIdentity) -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use axum::{Extension, Router, routing::get};
//! use gander_host::server::{Caller, ConnInfo, FixedPlan, Server, ServerOptions, hello};
//! # let plan = gander_core::net::plan::ListenerPlan::new(
//! #     gander_core::protocol::host::TrustAdapterKind::Generic, 0,
//! #     vec!["127.0.0.1".parse()?], vec![])?;
//! let routes = Router::new()
//!     .merge(hello::router(identity.host_id().clone(), Arc::default()))
//!     .route("/v1/me", get(|Extension(conn): Extension<ConnInfo>,
//!                            Extension(caller): Extension<Caller>| async move {
//!         format!("{:?} from {}", caller, conn.peer)
//!     }));
//! let options = ServerOptions::new(identity, Arc::new(FixedPlan(plan)));
//! let server = Server::start(options, routes).await?;
//! server.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! What every route can rely on (the gate in [`gate`] runs first, outermost to innermost):
//!
//! - `X-Gander-Protocol` and `X-Gander-Request` are added to every response;
//! - browser-marked requests (`Origin`, `Sec-Fetch-Mode`, `Cookie`) never arrive (403);
//! - the `Host` header names a served address, `localhost` on loopback, or a name registered with
//!   [`HostAllowList::set_names`] (400 `bad_host` otherwise);
//! - [`ServerOptions::authenticator`] ran and put a [`Caller`] in the extensions. **WP-13 replaces
//!   the default [`HelloOnly`] with the token authenticator**; it decides which endpoints are
//!   anonymous (`GET /v1/host`, `POST /v1/pair`, `GET /v1/pair/{id}`) and refuses everything else
//!   without a valid token. Network binding checks use [`ConnInfo::network`];
//! - the per-device concurrency limit (8) is enforced around the route and its response body;
//! - request bodies are capped at `MAX_REQUEST_BODY` (64 KiB). A route that accepts more adds its
//!   own `DefaultBodyLimit` layer;
//! - unknown paths get 404 `not_found`.
//!
//! Extractors available in handlers: `Extension<ConnInfo>` (peer, local socket, adapter, WhoIs
//! result, disconnect signal), `Extension<Caller>`, `Extension<RequestId>`, `Extension<ConnHandle>`
//! (closes this one connection; device revocation uses it, see [`closer`]). Errors are returned
//! as [`ApiError`] (the protocol's `{"error": {...}}` body). WebSocket handlers (WP-17) use
//! axum's `WebSocketUpgrade` (connections are served with upgrades enabled) and must stop when
//! [`ConnInfo::disconnect`] fires. Pairing/tray code uses [`Server::network`] for WhoIs, relay
//! state and Shields Up, [`Server::disconnect_all`] for the kill switch, and
//! [`selfcheck::connectivity_self_check`] for the firewall check.

pub mod closer;
pub mod error;
pub mod gate;
pub mod hello;
pub mod identity;
pub mod limiter;
pub mod listener;
pub mod network;
pub mod pause;
pub mod selfcheck;
pub mod tls;

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use gander_core::net::plan::ListenerPlan;
use gander_core::net::tailscale::TailscaleLocal;
use gander_core::protocol::HostId;
use gander_core::protocol::host::TrustAdapterKind;
use tokio::sync::{Notify, watch};
use tokio_rustls::TlsAcceptor;

pub use closer::{CloseReason, ConnHandle, WeakConnHandle};
pub use error::ApiError;
pub use gate::{
    AuthFuture, Authenticator, Caller, ConnInfo, DisconnectSignal, GateState, HelloOnly,
    HostAllowList, PeerNetwork, RequestId,
};
pub use identity::HostIdentity;
pub use limiter::{ConcurrencyLimiter, LimitKey};
pub use listener::{FixedPlan, PlanSource, SystemPlan};
pub use network::TrustNetwork;
pub use pause::{ListenerPause, PausablePlan};

/// How to run the server.
#[derive(Debug)]
pub struct ServerOptions {
    /// Host key and certificate.
    pub identity: HostIdentity,
    /// Where the listener plan comes from.
    pub plan_source: Arc<dyn PlanSource>,
    /// The local tailscaled (production: pipe then CLI). `None`: no WhoIs.
    pub tailscale: Option<Arc<dyn TailscaleLocal>>,
    /// Who is calling; default [`HelloOnly`].
    pub authenticator: Arc<dyn Authenticator>,
    /// Safety-net re-plan interval (30 s).
    pub replan_interval: Duration,
    /// Re-plan on OS address-change notifications (Windows). Off in tests with a fixed plan.
    pub watch_address_changes: bool,
}

impl ServerOptions {
    /// Defaults: no tailscale backend, [`HelloOnly`], 30 s re-plan, OS notifications on.
    pub fn new(identity: HostIdentity, plan_source: Arc<dyn PlanSource>) -> Self {
        Self {
            identity,
            plan_source,
            tailscale: None,
            authenticator: Arc::new(HelloOnly),
            replan_interval: listener::REPLAN_INTERVAL,
            watch_address_changes: true,
        }
    }
}

/// Errors starting the server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StartError {
    /// The TLS configuration was refused.
    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),
    /// Not a single socket could be bound (loopback should always work).
    #[error("no listener socket could be bound")]
    NothingBound,
}

/// State shared by the supervisor, the accept loops and the connections.
pub(crate) struct Shared {
    pub(crate) tls: TlsAcceptor,
    pub(crate) app: Router,
    pub(crate) hosts: Arc<HostAllowList>,
    pub(crate) network: Option<Arc<TrustNetwork>>,
    pub(crate) plan_source: Arc<dyn PlanSource>,
    pub(crate) state: RwLock<listener::ListenerState>,
    pub(crate) replan: Notify,
    pub(crate) kill: watch::Sender<u64>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn adapter(&self) -> TrustAdapterKind {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .plan
            .as_ref()
            .map_or(TrustAdapterKind::Generic, ListenerPlan::adapter)
    }

    pub(crate) fn bound_addrs(&self) -> Vec<SocketAddr> {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .bound
            .values()
            .map(|b| b.actual)
            .collect()
    }

    pub(crate) fn plan(&self) -> Option<ListenerPlan> {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .plan
            .clone()
    }

    pub(crate) fn disconnect_all(&self) {
        self.kill.send_modify(|g| *g = g.wrapping_add(1));
    }
}

/// A running server. Dropping it without [`Server::shutdown`] leaves the tasks running until the
/// runtime ends; call `shutdown` to close every socket and connection.
#[derive(Debug)]
pub struct Server {
    shared: Arc<Shared>,
    host_id: HostId,
    shutdown: watch::Sender<bool>,
    supervisor: tokio::task::JoinHandle<()>,
    #[cfg(windows)]
    _watcher: Option<gander_win::net::interfaces::AddressChangeWatcher>,
}

impl Server {
    /// Binds the first plan and starts serving `routes` behind the gate.
    pub async fn start(options: ServerOptions, routes: Router) -> Result<Self, StartError> {
        let tls = TlsAcceptor::from(tls::server_config(&options.identity)?);
        let hosts = Arc::new(HostAllowList::default());
        let gate = GateState::new(Arc::clone(&hosts), options.authenticator);
        let app = gate::apply_gate(routes, gate);
        let network = options.tailscale.map(|b| Arc::new(TrustNetwork::new(b)));
        let (kill, _) = watch::channel(0u64);
        let shared = Arc::new(Shared {
            tls,
            app,
            hosts,
            network,
            plan_source: options.plan_source,
            state: RwLock::new(listener::ListenerState::default()),
            replan: Notify::new(),
            kill,
        });
        listener::reconcile(&shared).await;
        if shared
            .state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .bound
            .is_empty()
        {
            return Err(StartError::NothingBound);
        }

        #[cfg(windows)]
        let watcher = if options.watch_address_changes {
            let s = Arc::clone(&shared);
            match gander_win::net::interfaces::AddressChangeWatcher::new(move || {
                s.replan.notify_one();
            }) {
                Ok(w) => Some(w),
                Err(e) => {
                    tracing::warn!(error = %e, "address-change notifications unavailable; relying on the poll");
                    None
                }
            }
        } else {
            None
        };

        let (shutdown, shutdown_rx) = watch::channel(false);
        let supervisor = tokio::spawn(listener::supervise(
            Arc::clone(&shared),
            options.replan_interval,
            shutdown_rx,
        ));
        Ok(Self {
            shared,
            host_id: options.identity.host_id().clone(),
            shutdown,
            supervisor,
            #[cfg(windows)]
            _watcher: watcher,
        })
    }

    /// The host id clients pin.
    pub fn host_id(&self) -> &HostId {
        &self.host_id
    }

    /// A cheap clonable handle on the running server for the tray UI: everything that does not
    /// consume the server (see [`ServerHandle`]).
    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// The sockets currently bound (actual addresses, ports resolved).
    pub fn bound_addrs(&self) -> Vec<SocketAddr> {
        self.shared.bound_addrs()
    }

    /// The plan in force.
    pub fn plan(&self) -> Option<ListenerPlan> {
        self.shared.plan()
    }

    /// Re-plans now (after a config change).
    pub fn replan_now(&self) {
        self.shared.replan.notify_one();
    }

    /// Re-plans and waits until the new plan is applied.
    pub async fn replan(&self) {
        listener::reconcile(&self.shared).await;
    }

    /// The `Host` allow list (add the MagicDNS name here if clients use it).
    pub fn host_allow_list(&self) -> Arc<HostAllowList> {
        Arc::clone(&self.shared.hosts)
    }

    /// The Tailscale side of the trust adapter, if configured.
    pub fn network(&self) -> Option<Arc<TrustNetwork>> {
        self.shared.network.clone()
    }

    /// Kill switch: drops every open connection now (the sockets stay bound).
    pub fn disconnect_all(&self) {
        self.shared.disconnect_all();
    }

    /// Closes every socket and connection and stops the supervisor.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        listener::close_all(&self.shared);
        self.disconnect_all();
        let _ = self.supervisor.await;
    }
}

/// What the tray UI (WP-18) needs from a running [`Server`]: clonable, `Send + Sync`, and usable
/// without owning the server, so the application keeps the `Server` for its final
/// [`Server::shutdown`]. Obtain it with [`Server::handle`].
#[derive(Debug, Clone)]
pub struct ServerHandle {
    shared: Arc<Shared>,
}

impl ServerHandle {
    /// The sockets currently bound (actual addresses, ports resolved).
    pub fn bound_addrs(&self) -> Vec<SocketAddr> {
        self.shared.bound_addrs()
    }

    /// The plan in force.
    pub fn plan(&self) -> Option<ListenerPlan> {
        self.shared.plan()
    }

    /// Re-plans soon (after a pause starts or ends, or a config change).
    pub fn replan_now(&self) {
        self.shared.replan.notify_one();
    }

    /// The Tailscale side of the trust adapter, if configured.
    pub fn network(&self) -> Option<Arc<TrustNetwork>> {
        self.shared.network.clone()
    }

    /// Kill switch: drops every open connection now (the sockets stay bound).
    pub fn disconnect_all(&self) {
        self.shared.disconnect_all();
    }
}
