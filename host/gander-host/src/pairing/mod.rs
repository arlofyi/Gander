// SPDX-License-Identifier: Apache-2.0
//! Pairing, device tokens and the token authenticator (WP-13;
//! `docs/specs/2026-11-01-pairing-and-trust.md`).
//!
//! # Wiring (application / WP-18 / WP-1B)
//!
//! ```no_run
//! # async fn demo(identity: gander_host::server::HostIdentity,
//! #               store: gander_host::pairing::SharedStore,
//! #               plan: std::sync::Arc<dyn gander_host::server::PlanSource>) -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use axum::Router;
//! use gander_host::audit::AuditLog;
//! use gander_host::pairing::{DeviceRegistry, PairingService, host_details};
//! use gander_host::server::{Server, ServerOptions, hello};
//!
//! let audit = Arc::new(AuditLog::open("logs".as_ref())?);
//! let registry = Arc::new(DeviceRegistry::load(store, "cfg".as_ref(), Default::default(), audit.clone())?);
//! let details = host_details(identity.host_id().clone(), vec![], None);
//! let pairing = PairingService::new(&identity, registry.clone(), audit.clone(), details, 42637);
//! let routes = Router::new()
//!     .merge(hello::router(identity.host_id().clone(), pairing.pairing_open_flag()))
//!     .merge(pairing.router());
//! let mut options = ServerOptions::new(identity, plan);
//! options.authenticator = pairing.authenticator();
//! let server = Server::start(options, routes).await?;
//! # Ok(()) }
//! ```
//!
//! # API by consumer
//!
//! - **WP-14 (routes)**: every non-anonymous request reaching a route was authenticated; read
//!   `Extension(Caller::Device(id))`. Device facts: [`DeviceRegistry::get`]. Permissions are in
//!   `config.toml`: `HostConfig::shares_for(&id)`, `ShareConfig::permissions_for(&id)` /
//!   `allows(&id, Permission::..)`. Audit: `AuditLog::record(AuditEvent::new(kind::ACCESS)...)`
//!   and [`crate::audit::AuditLog::resolver_bug`] for `RejectReason::is_bug()`.
//!   `POST /v1/token/rotate` is served by [`PairingService::router`] (do not add it again).
//! - **WP-17 (`/events`)**: [`DeviceRegistry::subscribe`] yields [`DeviceEvent::Revoked`]; the
//!   WebSocket handler also takes `Extension<ConnHandle>` and ends when
//!   [`crate::server::ConnHandle::closed`] resolves (reason `Revoked`: send
//!   `device.revoked {}` then close). Revocation fires both within milliseconds.
//! - **WP-18 (tray)**: [`PairingService::open_window`] (returns the QR payload; render
//!   `to_uri()`, never log it), [`PairingService::close_window`], [`PairingService::snapshot`]
//!   (open, time left, failure count, pending device with SAS, flow and identity),
//!   [`PairingService::approve`] / [`PairingService::deny`], [`PairingService::subscribe`] for
//!   live updates; Devices window: [`DeviceRegistry::list`] and [`DeviceRegistry::revoke`]; kill
//!   switch: `Server::disconnect_all()` plus `close_window()`.
//! - **WP-1B / WP-22 (test hook)**: [`PairingService::enable_test_approve`], or the binary's
//!   `--test-approve` flag (cargo feature `test-hooks`), see [`PairingService::enable_test_approve`].

mod auth;
mod registry;
mod routes;

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use axum::Router;
use gander_core::pairing::qr::QrPayload;
use gander_core::pairing::window::{
    CloseReason, DecideError, PairingMachine, PendingView, Snapshot, TickReport,
};
use gander_core::pairing::{SECRET_LEN, b64url};
use gander_core::protocol::host::{HostDetails, HostOs};
use gander_core::protocol::{DeviceId, HostId, PairingId, PairingSecret};
use tokio::sync::broadcast;

pub use auth::{TokenAuthenticator, is_anonymous};
pub use registry::{
    DeviceEvent, DeviceRegistry, REVOKED_RETENTION, RegistryError, SEEN_PERSIST_INTERVAL,
    SharedStore,
};

use crate::audit::{AuditEvent, AuditLog, Outcome, kind};
use crate::server::{Authenticator, HostIdentity};

/// `N` bytes from the OS CSPRNG.
///
/// # Panics
///
/// If the OS random source fails, which leaves no safe way to continue.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    use ring::rand::{SecureRandom as _, SystemRandom};
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .expect("the OS random number generator failed");
    out
}

/// Host facts for `PairApproved.host` and the QR code, from what the process knows.
///
/// `name` is `COMPUTERNAME` (`HOSTNAME` elsewhere); `os_version` comes from the platform layer
/// (`major.minor.build` on Windows, empty elsewhere until those hosts exist).
pub fn host_details(id: HostId, addresses: Vec<IpAddr>, magic_dns: Option<String>) -> HostDetails {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Gander host".to_owned());
    let os = if cfg!(windows) {
        HostOs::Windows
    } else if cfg!(target_os = "macos") {
        HostOs::Macos
    } else {
        HostOs::Linux
    };
    HostDetails {
        id,
        name,
        os,
        os_version: platform_os_version(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        addresses,
        magic_dns,
    }
}

#[cfg(windows)]
fn platform_os_version() -> String {
    gander_win::sysinfo::os_version()
}

#[cfg(not(windows))]
fn platform_os_version() -> String {
    String::new()
}

/// Live updates for the "Add a device" window (WP-18).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PairingEvent {
    /// The window opened.
    WindowOpened,
    /// The window closed.
    WindowClosed(CloseReason),
    /// A device asked to pair; show it with its SAS and Allow / Deny.
    Pending(Box<PendingView>),
    /// A `POST /v1/pair` was refused; `failures` is the window's total.
    AttemptRefused {
        /// Source address.
        source: IpAddr,
        /// Failed attempts in this window, all sources.
        failures: u32,
    },
    /// The pending pairing timed out.
    PendingExpired(PairingId),
    /// The owner (or the test hook) approved a device.
    Approved(DeviceId),
    /// The owner denied the pairing.
    Denied(PairingId),
}

/// Errors from approve / deny.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PairingActionError {
    /// No such pairing.
    #[error("no such pairing")]
    NotFound,
    /// It is no longer pending (timed out or already decided).
    #[error("the pairing is no longer pending")]
    NotPending,
    /// Saving the device failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

impl From<DecideError<RegistryError>> for PairingActionError {
    fn from(e: DecideError<RegistryError>) -> Self {
        match e {
            DecideError::NotFound => Self::NotFound,
            DecideError::NotPending => Self::NotPending,
            DecideError::Create(e) => Self::Registry(e),
        }
    }
}

/// Why the test hook was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TestHookError {
    /// A socket is bound to a non-loopback address (or nothing is bound).
    #[error("--test-approve is only allowed when every listener socket is on loopback")]
    NotLoopbackOnly,
}

#[derive(Debug, Clone)]
struct HostCard {
    details: HostDetails,
    port: u16,
}

/// The pairing window, its endpoints and the owner's decisions. Share as `Arc<PairingService>`.
#[derive(Debug)]
pub struct PairingService {
    machine: Mutex<PairingMachine>,
    registry: Arc<DeviceRegistry>,
    audit: Arc<AuditLog>,
    host_spki: Vec<u8>,
    card: RwLock<HostCard>,
    pairing_open: Arc<AtomicBool>,
    events: broadcast::Sender<PairingEvent>,
    test_approve: AtomicBool,
    timer_seq: AtomicU64,
    runtime: Option<tokio::runtime::Handle>,
    me: Weak<PairingService>,
}

impl PairingService {
    /// A service for the host `identity`. Call inside the tokio runtime (expiry timers run on it).
    pub fn new(
        identity: &HostIdentity,
        registry: Arc<DeviceRegistry>,
        audit: Arc<AuditLog>,
        details: HostDetails,
        port: u16,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(64);
        Arc::new_cyclic(|me| Self {
            machine: Mutex::new(PairingMachine::new()),
            registry,
            audit,
            host_spki: identity.spki_der().to_vec(),
            card: RwLock::new(HostCard { details, port }),
            pairing_open: Arc::new(AtomicBool::new(false)),
            events,
            test_approve: AtomicBool::new(false),
            timer_seq: AtomicU64::new(0),
            runtime: tokio::runtime::Handle::try_current().ok(),
            me: me.clone(),
        })
    }

    /// The flag `GET /v1/host` reports as `pairing_open` (pass to `hello::router`).
    pub fn pairing_open_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.pairing_open)
    }

    /// The token authenticator for `ServerOptions::authenticator`.
    pub fn authenticator(&self) -> Arc<dyn Authenticator> {
        Arc::new(TokenAuthenticator::new(Arc::clone(&self.registry)))
    }

    /// The audit log this service writes to (the tray's Activity window reads it).
    pub fn audit(&self) -> &Arc<AuditLog> {
        &self.audit
    }

    /// The device registry.
    pub fn registry(&self) -> &Arc<DeviceRegistry> {
        &self.registry
    }

    /// `POST /v1/pair`, `GET /v1/pair/{id}`, `POST /v1/token/rotate`.
    pub fn router(self: &Arc<Self>) -> Router {
        routes::router(Arc::clone(self))
    }

    /// Updates the addresses, name and port shown in the QR code and the approved response.
    pub fn set_host_details(&self, details: HostDetails, port: u16) {
        *self.card.write().unwrap_or_else(|e| e.into_inner()) = HostCard { details, port };
    }

    fn card(&self) -> HostCard {
        self.card.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Subscribes to window updates.
    pub fn subscribe(&self) -> broadcast::Receiver<PairingEvent> {
        self.events.subscribe()
    }

    fn emit(&self, e: PairingEvent) {
        let _ = self.events.send(e);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PairingMachine> {
        self.machine.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens (or re-opens) the "Add a device" window for 5 minutes with a fresh secret and
    /// returns the QR payload. Render `payload.to_uri()` as a QR code; never log or display it as
    /// text.
    pub fn open_window(&self) -> QrPayload {
        let secret = random_bytes::<SECRET_LEN>();
        let now = Instant::now();
        let generation = self.lock().open(now, secret);
        self.pairing_open.store(true, Ordering::SeqCst);
        self.audit
            .record(AuditEvent::new(kind::PAIR_WINDOW_OPENED).detail("generation", generation));
        self.emit(PairingEvent::WindowOpened);
        self.schedule_tick(gander_core::pairing::window::WINDOW_TTL);
        self.qr_for(secret)
    }

    fn qr_for(&self, secret: [u8; SECRET_LEN]) -> QrPayload {
        let card = self.card();
        QrPayload {
            host_id: card.details.id.clone(),
            addresses: card.details.addresses.clone(),
            port: card.port,
            host_name: card.details.name.clone(),
            secret: PairingSecret::new(b64url(&secret)),
            magic_dns: card.details.magic_dns.clone(),
        }
    }

    /// The QR payload of the open window, if any.
    pub fn qr_payload(&self) -> Option<QrPayload> {
        let secret = self.lock().secret(Instant::now())?;
        Some(self.qr_for(secret))
    }

    /// The owner closed the dialog (also the kill switch). A pending pairing is cancelled.
    pub fn close_window(&self) {
        let was_open = {
            let mut m = self.lock();
            let open = m.is_open(Instant::now());
            m.close(Instant::now(), CloseReason::Owner);
            open
        };
        self.pairing_open.store(false, Ordering::SeqCst);
        if was_open {
            self.window_closed(CloseReason::Owner);
        }
    }

    fn window_closed(&self, reason: CloseReason) {
        self.audit
            .record(AuditEvent::new(kind::PAIR_WINDOW_CLOSED).detail("reason", reason.as_str()));
        self.emit(PairingEvent::WindowClosed(reason));
    }

    /// The window and pending pairing (for the dialog).
    pub fn snapshot(&self) -> Snapshot {
        self.tick();
        self.lock().snapshot(Instant::now())
    }

    /// Owner clicked Allow: creates and saves the device record and its token (identity captured
    /// from the WhoIs of the pairing request), closes the window. The device collects the token
    /// with its next poll.
    pub fn approve(&self, id: &PairingId) -> Result<DeviceId, PairingActionError> {
        self.approve_inner(id, kind::PAIR_APPROVED)
    }

    fn approve_inner(&self, id: &PairingId, event: &str) -> Result<DeviceId, PairingActionError> {
        let mut captured = None;
        let result = self.lock().approve(Instant::now(), id, |view| {
            captured = Some(view.clone());
            self.registry.add_from_pairing(view)
        });
        let device_id = result?;
        self.pairing_open.store(false, Ordering::SeqCst);
        let mut ev = AuditEvent::new(event).device(&device_id);
        if let Some(v) = &captured {
            ev = ev
                .source(v.source)
                .detail("flow", v.flow.as_str())
                .detail("name", v.device.name.as_str())
                .detail("platform", v.device.platform.as_str());
            ev = match &v.identity {
                Some(p) => ev
                    .detail("login", p.login.as_str())
                    .detail("node", p.node_name.as_str())
                    .detail("shared_in", p.shared_in),
                None => ev.detail("login", serde_json::Value::Null),
            };
        }
        self.audit.record(ev);
        self.emit(PairingEvent::Approved(device_id.clone()));
        self.window_closed(CloseReason::Approved);
        self.schedule_tick(gander_core::pairing::window::COLLECT_TTL);
        Ok(device_id)
    }

    /// Owner clicked Deny: the device's poll gets 403 `{state: denied}`; the window closes.
    pub fn deny(&self, id: &PairingId) -> Result<(), PairingActionError> {
        let source = self
            .lock()
            .pending(Instant::now())
            .filter(|p| &p.pairing_id == id)
            .map(|p| p.source);
        self.lock().deny(Instant::now(), id).map_err(|e| match e {
            DecideError::NotFound => PairingActionError::NotFound,
            _ => PairingActionError::NotPending,
        })?;
        self.pairing_open.store(false, Ordering::SeqCst);
        let mut ev = AuditEvent::new(kind::PAIR_DENIED);
        if let Some(s) = source {
            ev = ev.source(s);
        }
        self.audit.record(ev);
        self.emit(PairingEvent::Denied(id.clone()));
        self.window_closed(CloseReason::Denied);
        Ok(())
    }

    /// Applies time-based transitions (window and pending expiry, abandoned approvals).
    pub fn tick(&self) {
        let report: TickReport = self.lock().tick(Instant::now());
        let open = self.lock().is_open(Instant::now());
        self.pairing_open.store(open, Ordering::SeqCst);
        if report.window_expired {
            self.window_closed(CloseReason::Expired);
        }
        for id in report.pending_expired {
            self.audit.record(AuditEvent::new(kind::PAIR_EXPIRED));
            self.emit(PairingEvent::PendingExpired(id));
        }
        for id in report.abandoned {
            match self.registry.remove(&id) {
                Ok(()) => self.audit.record(
                    AuditEvent::new(kind::PAIR_ABANDONED)
                        .device(&id)
                        .detail("reason", "token never collected"),
                ),
                Err(e) => {
                    tracing::warn!(error = %e, "removing an abandoned device failed");
                    self.audit.record(
                        AuditEvent::new(kind::PAIR_ABANDONED)
                            .device(&id)
                            .outcome(Outcome::Error),
                    );
                }
            }
        }
    }

    /// Runs [`PairingService::tick`] after `after` (plus a little slack), on the runtime.
    fn schedule_tick(&self, after: Duration) {
        let Some(rt) = &self.runtime else {
            return;
        };
        let me = self.me.clone();
        self.timer_seq.fetch_add(1, Ordering::Relaxed);
        rt.spawn(async move {
            tokio::time::sleep(after + Duration::from_millis(50)).await;
            if let Some(svc) = me.upgrade() {
                svc.tick();
            }
        });
    }

    /// **Integration-test hook** (`--test-approve`): every pairing request from a loopback source
    /// is approved at once, and the window re-opens itself whenever a request arrives while it is
    /// closed, so a harness (WP-1B `cargo xtask e2e-host`, WP-22 instrumented tests via the
    /// emulator's `10.0.2.2`, which arrives on the host's 127.0.0.1) can pair without a UI using
    /// flow B (no secret) and the normal `POST /v1/pair` + `GET /v1/pair/{id}` calls.
    ///
    /// Refused unless `bound` is non-empty and every socket is on loopback. Requests from a
    /// non-loopback source are never auto-approved even when enabled. The binary accepts the flag
    /// only when built with the cargo feature `test-hooks` and then binds loopback only.
    pub fn enable_test_approve(&self, bound: &[SocketAddr]) -> Result<(), TestHookError> {
        if bound.is_empty() || bound.iter().any(|a| !a.ip().is_loopback()) {
            return Err(TestHookError::NotLoopbackOnly);
        }
        tracing::warn!("TEST HOOK ENABLED: loopback pairing requests are approved without a UI");
        self.test_approve.store(true, Ordering::SeqCst);
        self.open_window();
        Ok(())
    }

    /// Whether the test hook is on.
    pub fn test_approve_enabled(&self) -> bool {
        self.test_approve.load(Ordering::SeqCst)
    }
}
