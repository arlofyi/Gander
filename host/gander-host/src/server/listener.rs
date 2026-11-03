// SPDX-License-Identifier: Apache-2.0
//! Binding, accepting and serving connections, and keeping the bound sockets in step with the
//! listener plan.
//!
//! - [`PlanSource`] yields the current [`ListenerPlan`]: [`SystemPlan`] in production (interfaces
//!   from the OS plus `[listener]` config), [`FixedPlan`] in tests.
//! - [`bind_exclusive`] binds one explicit address with `SO_EXCLUSIVEADDRUSE` (Windows) and
//!   refuses wildcard addresses outright, whatever the plan says.
//! - The supervisor re-plans when the OS reports an address change, every 30 s as a safety net,
//!   and on demand; it closes sockets whose address disappeared and binds new ones. Loopback stays
//!   bound throughout because it is in every plan.
//! - Each connection: TLS handshake (10 s limit), one WhoIs for the peer (tailscale adapter,
//!   non-loopback), then HTTP/1.1 with keep-alive and upgrades (WebSocket) over hyper, with
//!   [`ConnInfo`] inserted into every request.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use gander_core::config::ListenerConfig;
use gander_core::net::plan::{InterfaceInfo, ListenerPlan, plan_listener};
use gander_core::protocol::host::TrustAdapterKind;
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tower::ServiceExt as _;

use super::Shared;
use super::closer::ConnHandle;
use super::gate::{ConnInfo, DisconnectSignal, PeerNetwork};

/// TLS handshake limit.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Limit for receiving a request head (slow-loris guard).
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Safety-net re-plan interval.
pub const REPLAN_INTERVAL: Duration = Duration::from_secs(30);
/// Coalesces bursts of address-change notifications.
const REPLAN_DEBOUNCE: Duration = Duration::from_millis(300);

/// Produces the current listener plan. Called on a blocking thread.
pub trait PlanSource: Send + Sync + std::fmt::Debug + 'static {
    /// The plan for the machine as it is now.
    fn plan(&self) -> Result<ListenerPlan, String>;
}

/// The production plan: OS interfaces + `[listener]` config + loopback.
#[derive(Debug, Clone)]
pub struct SystemPlan {
    /// `[listener]` from `config.toml`.
    pub listener: ListenerConfig,
    /// Loopback addresses to serve (default `127.0.0.1` and `::1`).
    pub loopback: Vec<IpAddr>,
}

impl SystemPlan {
    /// Plan from config with the default loopback addresses.
    pub fn new(listener: ListenerConfig) -> Self {
        Self {
            listener,
            loopback: gander_core::net::plan::default_loopback(),
        }
    }
}

fn interfaces() -> Result<Vec<InterfaceInfo>, String> {
    #[cfg(windows)]
    {
        gander_win::net::interfaces::list_interfaces().map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        // Other hosts arrive later; until then only loopback is served there.
        Ok(Vec::new())
    }
}

impl PlanSource for SystemPlan {
    fn plan(&self) -> Result<ListenerPlan, String> {
        Ok(plan_listener(
            &self.listener,
            &interfaces()?,
            &self.loopback,
        ))
    }
}

/// A plan that never changes (tests).
#[derive(Debug, Clone)]
pub struct FixedPlan(pub ListenerPlan);

impl PlanSource for FixedPlan {
    fn plan(&self) -> Result<ListenerPlan, String> {
        Ok(self.0.clone())
    }
}

/// Binds one explicit address, exclusively. Wildcards are refused here as a second line of
/// defence behind the plan invariant.
pub fn bind_exclusive(addr: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    if addr.ip().is_unspecified() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to bind a wildcard address",
        ));
    }
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket as _;
        gander_win::net::socket::set_exclusive_address_use(socket.as_raw_socket())?;
    }
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    TcpListener::from_std(socket.into())
}

/// One bound socket.
#[derive(Debug)]
pub(crate) struct Bound {
    pub(crate) actual: SocketAddr,
    pub(crate) accept: AbortHandle,
}

/// Mutable listener state.
#[derive(Debug, Default)]
pub(crate) struct ListenerState {
    pub(crate) plan: Option<ListenerPlan>,
    /// Keyed by the planned address (port 0 in tests maps to an OS-chosen port).
    pub(crate) bound: BTreeMap<SocketAddr, Bound>,
}

/// Brings the bound sockets in line with a fresh plan.
pub(crate) async fn reconcile(shared: &Arc<Shared>) {
    let source = Arc::clone(&shared.plan_source);
    let plan = match tokio::task::spawn_blocking(move || source.plan()).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "listener plan failed; keeping current sockets");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "listener plan task failed");
            return;
        }
    };
    let want = plan.socket_addrs();
    let mut changed = false;
    {
        let mut state = shared.state.write().unwrap_or_else(|e| e.into_inner());
        let stale: Vec<SocketAddr> = state
            .bound
            .keys()
            .filter(|a| !want.contains(a))
            .copied()
            .collect();
        for addr in stale {
            if let Some(b) = state.bound.remove(&addr) {
                b.accept.abort();
                tracing::info!(addr = %b.actual, "listener closed (address no longer in plan)");
                changed = true;
            }
        }
        for addr in &want {
            if state.bound.contains_key(addr) {
                continue;
            }
            match bind_exclusive(*addr) {
                Ok(listener) => {
                    let actual = listener.local_addr().unwrap_or(*addr);
                    let task = tokio::spawn(accept_loop(listener, Arc::clone(shared)));
                    tracing::info!(addr = %actual, adapter = ?plan.adapter(), "listening");
                    state.bound.insert(
                        *addr,
                        Bound {
                            actual,
                            accept: task.abort_handle(),
                        },
                    );
                    changed = true;
                }
                Err(e) => {
                    tracing::warn!(addr = %addr, error = %e, "bind failed; retrying on next plan")
                }
            }
        }
        if state.plan.as_ref().map(ListenerPlan::adapter) != Some(plan.adapter()) {
            changed = true;
        }
        let served: Vec<IpAddr> = plan
            .loopback()
            .iter()
            .chain(plan.network())
            .copied()
            .collect();
        shared.hosts.set_addresses(served);
        state.plan = Some(plan);
    }
    if changed && let Some(n) = &shared.network {
        n.clear_cache();
    }
}

/// Closes every bound socket.
pub(crate) fn close_all(shared: &Shared) {
    let mut state = shared.state.write().unwrap_or_else(|e| e.into_inner());
    for (_, b) in std::mem::take(&mut state.bound) {
        b.accept.abort();
        tracing::info!(addr = %b.actual, "listener closed (shutdown)");
    }
}

/// Re-plans until shutdown: on OS address changes, on demand, and every `interval`.
pub(crate) async fn supervise(
    shared: Arc<Shared>,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            () = shared.replan.notified() => {
                tokio::time::sleep(REPLAN_DEBOUNCE).await;
            }
            () = tokio::time::sleep(interval) => {}
            _ = shutdown.changed() => break,
        }
        if *shutdown.borrow() {
            break;
        }
        reconcile(&shared).await;
    }
    close_all(&shared);
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        match listener.accept().await {
            Ok((tcp, peer)) => {
                tokio::spawn(serve_connection(tcp, peer, Arc::clone(&shared)));
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn serve_connection(tcp: TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
    let Ok(local) = tcp.local_addr() else {
        return;
    };
    let _ = tcp.set_nodelay(true);
    let mut kill = shared.kill.subscribe();
    let tls = match tokio::time::timeout(HANDSHAKE_TIMEOUT, shared.tls.accept(tcp)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::debug!(%peer, error = %e, "TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!(%peer, "TLS handshake timed out");
            return;
        }
    };
    let adapter = shared.adapter();
    let network = if peer.ip().is_loopback() {
        PeerNetwork::Loopback
    } else {
        match (&shared.network, adapter) {
            (Some(n), TrustAdapterKind::Tailscale) => n.identify(peer).await,
            _ => PeerNetwork::NoIdentity,
        }
    };
    let info = ConnInfo {
        peer,
        local,
        adapter,
        network,
        disconnect: DisconnectSignal(shared.kill.subscribe()),
    };
    let app: Router = shared.app.clone();
    // Per-connection close handle (device revocation closes this connection only).
    let handle = ConnHandle::new();
    let closer = handle.clone();
    let svc = tower::service_fn(move |mut req: hyper::Request<Incoming>| {
        req.extensions_mut().insert(info.clone());
        req.extensions_mut().insert(handle.clone());
        app.clone().oneshot(req.map(Body::new))
    });
    let conn = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .keep_alive(true)
        .serve_connection(TokioIo::new(tls), TowerToHyperService::new(svc))
        .with_upgrades();
    tokio::pin!(conn);
    tokio::select! {
        r = conn.as_mut() => {
            if let Err(e) = r {
                tracing::debug!(%peer, error = %e, "connection ended with error");
            }
        }
        _ = kill.changed() => {
            tracing::info!(%peer, "connection dropped by disconnect_all");
        }
        reason = closer.closed() => {
            tracing::info!(%peer, ?reason, "connection closed by its handle");
        }
    }
}
