// SPDX-License-Identifier: Apache-2.0
//! [`LiveController`]: the real [`HostController`] over the running host.
//!
//! A thin adapter. Every action goes through the API that already enforces the rule:
//!
//! | UI action | Calls |
//! | --- | --- |
//! | Add device | `PairingService::open_window` / `snapshot` / `approve` / `deny` / `close_window` |
//! | Devices | `DeviceRegistry::list` / `revoke` (revocation closes the device's connections) |
//! | Shares | validate with the `config.toml` rules, `HostConfig::save` through the owner-only store, `ApiState::set_config`, the one path that also replaces the events channel's share view and sends `shares.updated` |
//! | Activity | `AuditLog::recent` |
//! | Network | `ServerHandle::plan` / `bound_addrs`, `selfcheck::connectivity_self_check`, `TrustNetwork::relay_state` |
//! | Kill switch | `StreamSessions::kill_all` + `PairingService::close_window` + `ServerHandle::disconnect_all`, audited |
//! | Stream (WP-53) | `StreamSessions::view` (tray, toast), `prompt` / `answer`, `stop_from_host` |
//! | Pause listener | [`ListenerPause`] (the plan serves loopback only) + disconnect + re-plan, with an auto-resume timer |
//!
//! Slow work (the firewall check, WhoIs, listing interfaces) runs on the tokio runtime; the UI
//! reads the last result. Nothing here logs a token, the QR payload or a pairing secret.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gander_core::clipboard::ClipboardMode;
use gander_core::config::{ConfigFile as _, HostConfig, ShareConfig};
use gander_core::pairing::qr::QrPayload;
use gander_core::protocol::host::TrustAdapterKind;
use gander_core::protocol::{DeviceId, PairingId};

use super::autostart::Autostart;
use super::controller::{
    Applied, ConnectedDevice, ControlError, ControlResult, DeviceRow, HostController, HostStatus,
    NetworkView, PairingView, PeerPath, SelfCheckView, SettingsView, StreamPromptAnswer,
    StreamPromptView, StreamSessionView, ThemeChoice,
};
use crate::audit::{AuditEvent, kind};
use crate::pairing::{PairingActionError, PairingService, SharedStore};
use crate::server::{ListenerPause, ServerHandle, selfcheck};

/// The thumbnail and preview caches, as Settings sees them.
pub trait CacheControl: Send + Sync + std::fmt::Debug + 'static {
    /// Bytes used by both caches.
    fn used_bytes(&self) -> u64;
    /// Deletes every entry; the caller reads [`CacheControl::used_bytes`] before to report what
    /// was freed.
    fn purge(&self) -> std::io::Result<()>;
}

/// The Windows render caches (`gander_win::thumbs::cache::ThumbCache`: thumbnails and previews).
#[cfg(windows)]
#[derive(Debug)]
pub struct ThumbCaches(pub Vec<Arc<gander_win::thumbs::cache::ThumbCache>>);

#[cfg(windows)]
impl CacheControl for ThumbCaches {
    fn used_bytes(&self) -> u64 {
        self.0.iter().map(|c| c.total_bytes()).sum()
    }

    fn purge(&self) -> std::io::Result<()> {
        for c in &self.0 {
            c.purge()?;
        }
        Ok(())
    }
}

/// Everything the live controller is built from. `main` fills it once the server is up.
pub struct LiveParts {
    /// The running server (kill switch, bound sockets, plan, Tailscale side).
    pub server: ServerHandle,
    /// Pairing window, approvals and the device registry.
    pub pairing: Arc<PairingService>,
    /// Where `config.toml` is saved (owner-only ACL on Windows).
    pub store: SharedStore,
    /// The config directory (`%APPDATA%\Gander`).
    pub config_dir: std::path::PathBuf,
    /// The configuration the host started with.
    pub config: HostConfig,
    /// The listener pause switch (the same one the plan source was wrapped with).
    pub pause: Arc<ListenerPause>,
    /// The Browse/Fetch/events API state. A share edit goes through `ApiState::set_config`, which
    /// also replaces the events channel's share view and sends `shares.updated` (the API is
    /// Windows only).
    #[cfg(windows)]
    pub api: Arc<crate::api::ApiState>,
    /// The render caches, if they opened.
    pub caches: Option<Arc<dyn CacheControl>>,
    /// The logon autostart switch.
    pub autostart: Arc<dyn Autostart>,
    /// The owner's live clipboard setting and the notices for the tray (shared with the stream
    /// sessions; see [`crate::clipboard`]).
    pub clipboard: crate::clipboard::HostClipboard,
    /// The tokio runtime (for background checks and the pause timer).
    pub runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for LiveParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveParts").finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct NetworkCache {
    interface: Option<String>,
    peers: Vec<PeerPath>,
    check: SelfCheckView,
}

struct Inner {
    parts: LiveParts,
    config: Mutex<HostConfig>,
    network: Mutex<NetworkCache>,
    host_name: String,
    theme: Mutex<ThemeChoice>,
}

/// The file the theme choice is kept in, next to `config.toml`. Not part of the host config: it
/// only changes how the windows look.
const THEME_FILE: &str = "ui-theme.txt";

/// See the module docs. Cheap to clone.
#[derive(Clone)]
pub struct LiveController {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for LiveController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveController").finish_non_exhaustive()
    }
}

fn friendly(e: &PairingActionError) -> ControlError {
    match e {
        PairingActionError::NotFound | PairingActionError::NotPending => ControlError::new(
            "The request is no longer pending: it timed out or was already decided.",
        ),
        PairingActionError::Registry(e) => {
            ControlError::new(format!("Saving the device failed: {e}"))
        }
    }
}

impl LiveController {
    /// Builds the controller and starts the first connectivity check in the background.
    pub fn new(parts: LiveParts) -> Self {
        let host_name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "this computer".to_owned());
        let config = parts.config.clone();
        let parts_dir = parts.config_dir.clone();
        let me = Self {
            inner: Arc::new(Inner {
                parts,
                config: Mutex::new(config),
                network: Mutex::new(NetworkCache::default()),
                host_name,
                theme: Mutex::new(ThemeChoice::parse(
                    &std::fs::read_to_string(parts_dir.join(THEME_FILE)).unwrap_or_default(),
                )),
            }),
        };
        me.refresh_network();
        me
    }

    /// The owner-facing sentence for why previews are off, if they are.
    fn previews_problem(&self) -> Option<String> {
        #[cfg(windows)]
        {
            self.p()
                .api
                .previews_unavailable()
                .map(|why| why.describe().to_owned())
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn p(&self) -> &LiveParts {
        &self.inner.parts
    }

    fn config(&self) -> std::sync::MutexGuard<'_, HostConfig> {
        self.inner.config.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn network_cache(&self) -> std::sync::MutexGuard<'_, NetworkCache> {
        self.inner.network.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn audit(&self, event: AuditEvent) {
        self.p().pairing.audit().record(event);
    }

    /// Validates `candidate`, saves it, and makes it the current configuration everywhere.
    /// `shares_changed` also hands it to the API (and through it to the events channel).
    fn commit(&self, candidate: HostConfig, shares_changed: bool) -> ControlResult<()> {
        candidate
            .validate()
            .map_err(|e| ControlError::new(describe_config_error(&e)))?;
        candidate
            .save(&*self.p().store, &self.p().config_dir)
            .map_err(|e| ControlError::new(format!("config.toml could not be saved: {e}")))?;
        *self.config() = candidate.clone();
        #[cfg(windows)]
        if shares_changed {
            self.p().api.set_config(candidate);
        }
        #[cfg(not(windows))]
        let _ = shares_changed;
        Ok(())
    }

    /// The Stream lifecycle, when the host runs one.
    fn streams(&self) -> Option<crate::lifecycle::StreamSessions> {
        #[cfg(windows)]
        {
            self.p().api.streams()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn connected(&self) -> Vec<ConnectedDevice> {
        let registry = self.p().pairing.registry();
        registry
            .list()
            .into_iter()
            .filter(|d| d.revoked_at.is_none() && registry.open_connections(&d.id) > 0)
            .map(|d| ConnectedDevice {
                id: d.id,
                name: d.name,
            })
            .collect()
    }
}

fn describe_config_error(e: &gander_core::config::ConfigError) -> String {
    match e {
        gander_core::config::ConfigError::Invalid { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// Loopback first, then the rest, each group sorted.
fn sorted_bound(mut bound: Vec<SocketAddr>) -> Vec<SocketAddr> {
    bound.sort_by_key(|a| (!a.ip().is_loopback(), *a));
    bound
}

impl HostController for LiveController {
    fn status(&self) -> HostStatus {
        HostStatus {
            host_name: self.inner.host_name.clone(),
            paused_for: self.p().pause.remaining(),
            connected: self.connected(),
            stream: self
                .streams()
                .and_then(|s| s.view())
                .filter(|v| v.is_live())
                .map(|v| StreamSessionView {
                    device_name: v.device_name,
                    file_name: v.file_name,
                    app_name: v.app_name,
                    audio: v.audio,
                }),
        }
    }

    fn ui_gpu(&self) -> gander_core::config::UiGpu {
        self.config().ui.gpu
    }

    fn shares(&self) -> Vec<ShareConfig> {
        self.config().shares.clone()
    }

    fn apply_shares(&self, shares: Vec<ShareConfig>) -> ControlResult<()> {
        let current = self.config().clone();
        // A folder that is gone is refused when it is new or changed; one that existed before
        // (an unplugged drive) must not block other edits.
        for s in &shares {
            let unchanged = current.share(&s.id).is_some_and(|old| old.root == s.root);
            if !unchanged && !s.root.is_dir() {
                return Err(ControlError::new(format!(
                    "The folder {} does not exist.",
                    s.root.display()
                )));
            }
        }
        let count = shares.len();
        let mut candidate = current;
        candidate.shares = shares;
        self.commit(candidate, true)?;
        self.audit(AuditEvent::new(kind::SHARES_CHANGED).detail("shares", count));
        Ok(())
    }

    fn devices(&self) -> Vec<DeviceRow> {
        let registry = self.p().pairing.registry();
        registry
            .list()
            .iter()
            .map(|r| DeviceRow::from_record(r, registry.open_connections(&r.id) > 0))
            .collect()
    }

    fn revoke_device(&self, id: &DeviceId) -> ControlResult<()> {
        self.p()
            .pairing
            .registry()
            .revoke(id)
            .map(|_| ())
            .map_err(|e| ControlError::new(format!("Could not revoke the device: {e}")))
    }

    fn open_pairing_window(&self) -> QrPayload {
        self.p().pairing.open_window()
    }

    fn pairing_qr(&self) -> Option<QrPayload> {
        self.p().pairing.qr_payload()
    }

    fn pairing(&self) -> PairingView {
        self.p().pairing.snapshot()
    }

    fn close_pairing_window(&self) {
        self.p().pairing.close_window();
    }

    fn approve_pairing(&self, id: &PairingId) -> ControlResult<DeviceId> {
        self.p().pairing.approve(id).map_err(|e| friendly(&e))
    }

    fn deny_pairing(&self, id: &PairingId) -> ControlResult<()> {
        self.p().pairing.deny(id).map_err(|e| friendly(&e))
    }

    fn activity(&self, n: usize) -> Vec<AuditEvent> {
        self.p().pairing.audit().recent(n)
    }

    fn network(&self) -> NetworkView {
        let plan = self.p().server.plan();
        let cache = self.network_cache();
        NetworkView {
            adapter: plan
                .as_ref()
                .map_or(TrustAdapterKind::Generic, |p| p.adapter()),
            interface: cache.interface.clone(),
            port: plan
                .as_ref()
                .map_or_else(|| self.config().listener.port, |p| p.port()),
            bound: sorted_bound(self.p().server.bound_addrs()),
            paused_for: self.p().pause.remaining(),
            peers: cache.peers.clone(),
            self_check: cache.check.clone(),
        }
    }

    fn refresh_network(&self) {
        {
            let mut cache = self.network_cache();
            if cache.check.running {
                return;
            }
            cache.check.running = true;
        }
        let me = self.clone();
        self.p().runtime.spawn(async move {
            let network = me.p().server.network();
            let report = selfcheck::connectivity_self_check(network.as_deref()).await;

            let registry = me.p().pairing.registry();
            let mut peers = Vec::new();
            for d in registry.list() {
                if d.revoked_at.is_some() || registry.open_connections(&d.id) == 0 {
                    continue;
                }
                let Some(address) = d.last_address else {
                    continue;
                };
                let path = match &network {
                    Some(n) => n.relay_state(address).await,
                    None => None,
                };
                peers.push(PeerPath {
                    device_name: d.name,
                    address,
                    path,
                });
            }
            let bound = me.p().server.bound_addrs();
            let interface = tokio::task::spawn_blocking(move || interface_name(&bound))
                .await
                .ok()
                .flatten();

            let mut cache = me.network_cache();
            cache.interface = interface;
            cache.peers = peers;
            cache.check = SelfCheckView {
                running: false,
                done: true,
                blocking_rules: report.blocking_rules,
                shields_up: report.shields_up,
                errors: report.errors,
            };
        });
    }

    fn settings(&self) -> SettingsView {
        let config = self.config();
        SettingsView {
            port: config.listener.port,
            cache_used: self.p().caches.as_ref().map(|c| c.used_bytes()),
            cache_cap_mib: config.cache.cap_mib,
            autostart: self.p().autostart.status(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            theme: *self.inner.theme.lock().unwrap_or_else(|e| e.into_inner()),
            previews_problem: self.previews_problem(),
            clipboard: config.stream.clipboard,
        }
    }

    fn set_port(&self, port: u16) -> ControlResult<Applied> {
        let mut candidate = self.config().clone();
        candidate.listener.port = port;
        self.commit(candidate, false)?;
        self.audit(AuditEvent::new(kind::SETTINGS_CHANGED).detail("setting", "port"));
        Ok(Applied::AfterRestart)
    }

    fn set_cache_cap_mib(&self, mib: u32) -> ControlResult<Applied> {
        let mut candidate = self.config().clone();
        candidate.cache.cap_mib = mib;
        self.commit(candidate, false)?;
        self.audit(AuditEvent::new(kind::SETTINGS_CHANGED).detail("setting", "cache_cap_mib"));
        Ok(Applied::AfterRestart)
    }

    fn purge_cache(&self) -> ControlResult<u64> {
        let Some(caches) = &self.p().caches else {
            return Err(ControlError::new("The cache is not available."));
        };
        let before = caches.used_bytes();
        caches
            .purge()
            .map_err(|e| ControlError::new(format!("Could not empty the cache: {e}")))?;
        self.audit(AuditEvent::new(kind::CACHE_PURGED).detail("bytes", before));
        Ok(before.saturating_sub(caches.used_bytes()))
    }

    fn set_autostart(&self, enabled: bool) -> ControlResult<()> {
        self.p().autostart.set_enabled(enabled)?;
        self.audit(
            AuditEvent::new(kind::SETTINGS_CHANGED)
                .detail("setting", "autostart")
                .detail("enabled", enabled),
        );
        Ok(())
    }

    fn set_theme(&self, choice: ThemeChoice) -> ControlResult<()> {
        std::fs::create_dir_all(&self.p().config_dir)
            .and_then(|()| std::fs::write(self.p().config_dir.join(THEME_FILE), choice.as_str()))
            .map_err(|e| ControlError::new(format!("The theme could not be saved: {e}")))?;
        *self.inner.theme.lock().unwrap_or_else(|e| e.into_inner()) = choice;
        Ok(())
    }

    fn set_clipboard_mode(&self, mode: ClipboardMode) -> ControlResult<Applied> {
        let mut candidate = self.config().clone();
        candidate.stream.clipboard = mode;
        self.commit(candidate, false)?;
        // In force now: every running session reads this on its next clipboard request.
        self.p().clipboard.setting().set(mode);
        self.audit(
            AuditEvent::new(kind::SETTINGS_CHANGED)
                .detail("setting", "stream.clipboard")
                .detail("mode", mode.as_str()),
        );
        Ok(Applied::Now)
    }

    fn take_notices(&self) -> Vec<crate::clipboard::Notice> {
        self.p().clipboard.notices().take()
    }

    fn pause_listener(&self, duration: Duration) {
        let pause = Arc::clone(&self.p().pause);
        let epoch = pause.pause_for(duration);
        // A paused listener serves nobody: drop what is open and stop pairing.
        if let Some(s) = self.streams() {
            s.kill_all();
        }
        self.p().pairing.close_window();
        self.p().server.disconnect_all();
        self.p().server.replan_now();
        self.audit(AuditEvent::new(kind::LISTENER_PAUSED).detail("seconds", duration.as_secs()));
        let me = self.clone();
        self.p().runtime.spawn(async move {
            tokio::time::sleep(duration).await;
            if pause.resume_if_epoch(epoch) {
                me.p().server.replan_now();
                me.audit(AuditEvent::new(kind::LISTENER_RESUMED).detail("reason", "timer"));
            }
        });
    }

    fn resume_listener(&self) {
        if self.p().pause.resume() {
            self.p().server.replan_now();
            self.audit(AuditEvent::new(kind::LISTENER_RESUMED).detail("reason", "owner"));
        }
    }

    fn kill_switch(&self) {
        if let Some(s) = self.streams() {
            s.kill_all();
        }
        self.p().pairing.close_window();
        self.p().server.disconnect_all();
        self.audit(AuditEvent::new(kind::KILL_SWITCH));
    }

    fn stream_prompt(&self) -> Option<StreamPromptView> {
        self.streams().and_then(|s| s.prompt())
    }

    fn answer_stream_prompt(&self, id: u64, answer: StreamPromptAnswer) {
        if let Some(s) = self.streams() {
            s.answer(id, answer);
        }
    }

    fn stop_stream(&self) {
        if let Some(s) = self.streams() {
            s.stop_from_host();
        }
    }
}

/// The Windows adapter alias that owns the non-loopback bound addresses.
#[cfg(windows)]
fn interface_name(bound: &[SocketAddr]) -> Option<String> {
    let wanted: Vec<_> = bound
        .iter()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_loopback())
        .collect();
    if wanted.is_empty() {
        return None;
    }
    gander_win::net::interfaces::list_interfaces()
        .ok()?
        .into_iter()
        .find(|i| i.addresses.iter().any(|a| wanted.contains(a)))
        .map(|i| i.alias)
}

#[cfg(not(windows))]
fn interface_name(_bound: &[SocketAddr]) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::{Path, PathBuf};

    use gander_core::config::SecureFileStore;
    use gander_core::net::plan::ListenerPlan;
    use gander_core::pairing::window::{Flow, PendingView};
    use gander_core::protocol::files::{Permission, StreamDecision};
    use gander_core::protocol::pairing::PairingDevice;
    use gander_core::protocol::{DeviceId, ShareId};

    use super::*;
    use crate::audit::AuditLog;
    use crate::events::{
        ChangeNotifier, ConfigShares, Events, EventsOptions, HostStateConfig, HostStateMonitor,
        ManualProbe, WatchBackend, WatchHandle,
    };
    use crate::pairing::{DeviceRegistry, host_details};
    use crate::server::{FixedPlan, HostIdentity, PausablePlan, Server, ServerOptions, hello};
    use crate::ui::autostart::StubAutostart;

    #[derive(Debug, Default)]
    struct MemoryStore {
        files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    }

    impl SecureFileStore for MemoryStore {
        fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
            Ok(self.files.lock().unwrap().get(path).cloned())
        }

        fn write_owner_only(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
            self.files
                .lock()
                .unwrap()
                .insert(path.to_owned(), contents.to_vec());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct NoWatch;

    impl WatchBackend for NoWatch {
        fn watch(&self, _: &ShareId, _: &str, _: ChangeNotifier) -> io::Result<WatchHandle> {
            Ok(Box::new(()))
        }
    }

    #[derive(Debug)]
    struct FakeCaches {
        used: Mutex<u64>,
    }

    impl CacheControl for FakeCaches {
        fn used_bytes(&self) -> u64 {
            *self.used.lock().unwrap()
        }

        fn purge(&self) -> io::Result<()> {
            *self.used.lock().unwrap() = 0;
            Ok(())
        }
    }

    const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    /// Another loopback address (the whole 127/8 block is loopback), standing in for the private
    /// network address that a pause must close.
    const NET: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));

    struct Rig {
        ctrl: LiveController,
        server: Server,
        shares: Arc<ConfigShares>,
        store: Arc<MemoryStore>,
        audit: Arc<AuditLog>,
        caches: Arc<FakeCaches>,
        clipboard: crate::clipboard::HostClipboard,
        dir: PathBuf,
    }

    async fn rig() -> Rig {
        let dir = std::env::temp_dir().join(format!("gander-live-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(MemoryStore::default());
        let identity = HostIdentity::generate().unwrap();
        let audit = Arc::new(AuditLog::memory());
        let registry = Arc::new(
            DeviceRegistry::load(
                store.clone(),
                &dir.join("cfg"),
                Default::default(),
                Arc::clone(&audit),
            )
            .unwrap(),
        );
        let pairing = PairingService::new(
            &identity,
            Arc::clone(&registry),
            Arc::clone(&audit),
            host_details(identity.host_id().clone(), vec![LO], None),
            0,
        );
        let config = HostConfig::default();
        let shares = ConfigShares::new(config.clone());
        let monitor = HostStateMonitor::spawn(
            Arc::new(ManualProbe::default()),
            Arc::default(),
            HostStateConfig::default(),
        );
        let events = Events::new(EventsOptions {
            registry,
            shares: shares.clone(),
            host: monitor,
            backend: Arc::new(NoWatch),
            config: Default::default(),
        });
        // The same wiring as the binary: a share edit goes through ApiState::set_config, which also
        // replaces the events channel's share view.
        #[cfg(windows)]
        let api = Arc::new(
            crate::api::ApiState::new(
                config.clone(),
                Arc::clone(pairing.registry()),
                Arc::clone(&audit),
                host_details(identity.host_id().clone(), vec![LO], None),
            )
            .with_events(events, shares.clone()),
        );
        let routes = axum::Router::new()
            .merge(hello::router(
                identity.host_id().clone(),
                pairing.pairing_open_flag(),
            ))
            .merge(pairing.router());
        let plan = ListenerPlan::new(TrustAdapterKind::Generic, 0, vec![LO], vec![NET]).unwrap();
        let pause = ListenerPause::new();
        let source = PausablePlan::new(Arc::new(FixedPlan(plan)), Arc::clone(&pause));
        let mut options = ServerOptions::new(identity, Arc::new(source));
        options.watch_address_changes = false;
        options.authenticator = pairing.authenticator();
        let server = Server::start(options, routes).await.unwrap();
        let caches = Arc::new(FakeCaches {
            used: Mutex::new(5 * 1024 * 1024),
        });
        let clipboard = crate::clipboard::HostClipboard::new(ClipboardMode::Off);
        let ctrl = LiveController::new(LiveParts {
            server: server.handle(),
            pairing,
            store: store.clone(),
            config_dir: dir.join("cfg"),
            config,
            pause,
            #[cfg(windows)]
            api,
            caches: Some(caches.clone()),
            autostart: Arc::new(StubAutostart::new()),
            clipboard: clipboard.clone(),
            runtime: tokio::runtime::Handle::current(),
        });
        Rig {
            ctrl,
            server,
            shares,
            store,
            audit,
            caches,
            clipboard,
            dir,
        }
    }

    impl Rig {
        async fn finish(self) {
            self.server.shutdown().await;
            let _ = std::fs::remove_dir_all(&self.dir);
        }

        fn saved_config(&self) -> HostConfig {
            let bytes = self
                .store
                .read(&HostConfig::path_in(&self.dir.join("cfg")))
                .unwrap()
                .expect("config.toml was written");
            HostConfig::from_toml_str(std::str::from_utf8(&bytes).unwrap()).unwrap()
        }

        /// Pairs a device straight through the registry (no network).
        fn add_device(&self, name: &str) -> DeviceId {
            let view = PendingView {
                pairing_id: PairingId::from("pair_test"),
                device: PairingDevice {
                    name: name.into(),
                    platform: "android".into(),
                    model: name.into(),
                    app_version: "0.1.0".into(),
                },
                flow: Flow::Qr,
                identity: None,
                sas: gander_core::pairing::sas::derive_sas(
                    b"x",
                    &[0; gander_core::pairing::NONCE_LEN],
                    &[0; gander_core::pairing::NONCE_LEN],
                ),
                source: LO,
                expires_in: Duration::from_secs(60),
            };
            let (id, _token) = self
                .ctrl
                .p()
                .pairing
                .registry()
                .add_from_pairing(&view)
                .unwrap();
            id
        }
    }

    fn share_for(root: &Path, id: &str, device: &DeviceId) -> ShareConfig {
        ShareConfig {
            id: ShareId::from(id),
            name: "Docs".into(),
            root: root.to_path_buf(),
            show_hidden: false,
            show_path: false,
            hidden_patterns: vec!["**/.git".into()],
            stream_default: StreamDecision::Ask,
            grants: vec![gander_core::config::ShareGrant {
                device: device.clone(),
                permissions: vec![Permission::List, Permission::Fetch],
            }],
        }
    }

    #[tokio::test]
    async fn shares_are_validated_saved_and_pushed_to_the_events_channel() {
        let rig = rig().await;
        let device = rig.add_device("Pixel 9");
        let folder = rig.dir.join("shared");
        std::fs::create_dir_all(&folder).unwrap();

        rig.ctrl
            .apply_shares(vec![share_for(&folder, "shr_a", &device)])
            .unwrap();

        // Saved through the store, in a form that loads back.
        let saved = rig.saved_config();
        assert_eq!(saved.shares.len(), 1);
        assert_eq!(saved.shares[0].hidden_patterns, vec!["**/.git"]);
        // The events channel's share lookup sees it (so `share.changed` is filtered correctly).
        let seen = rig
            .shares
            .share(&ShareId::from("shr_a"))
            .expect("events saw the share");
        assert!(seen.allows(&device, Permission::Fetch));
        assert_eq!(rig.ctrl.shares().len(), 1);
        assert!(
            rig.audit
                .recent(10)
                .iter()
                .any(|e| e.event == kind::SHARES_CHANGED)
        );
        rig.finish().await;
    }

    #[tokio::test]
    async fn invalid_or_missing_folders_are_refused_and_nothing_changes() {
        let rig = rig().await;
        let device = rig.add_device("Pixel 9");
        let missing = rig.dir.join("not-there");
        let err = rig
            .ctrl
            .apply_shares(vec![share_for(&missing, "shr_a", &device)])
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");

        let folder = rig.dir.join("shared");
        std::fs::create_dir_all(&folder).unwrap();
        let mut bad = share_for(&folder, "shr_a", &device);
        bad.name = "  ".into();
        let err = rig.ctrl.apply_shares(vec![bad]).unwrap_err();
        assert!(err.to_string().contains("empty name"), "{err}");

        assert!(rig.ctrl.shares().is_empty());
        assert!(rig.shares.share(&ShareId::from("shr_a")).is_none());
        assert!(
            rig.store
                .read(&HostConfig::path_in(&rig.dir.join("cfg")))
                .unwrap()
                .is_none(),
            "nothing was written"
        );
        rig.finish().await;
    }

    #[tokio::test]
    async fn a_share_whose_folder_vanished_does_not_block_other_edits() {
        let rig = rig().await;
        let device = rig.add_device("Pixel 9");
        let folder = rig.dir.join("shared");
        std::fs::create_dir_all(&folder).unwrap();
        rig.ctrl
            .apply_shares(vec![share_for(&folder, "shr_a", &device)])
            .unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        let mut renamed = share_for(&folder, "shr_a", &device);
        renamed.name = "Renamed".into();
        rig.ctrl.apply_shares(vec![renamed]).unwrap();
        assert_eq!(rig.ctrl.shares()[0].name, "Renamed");
        rig.finish().await;
    }

    #[tokio::test]
    async fn revoking_marks_the_device_and_reports_it_revoked() {
        let rig = rig().await;
        let id = rig.add_device("Pixel 9");
        assert!(
            rig.ctrl
                .devices()
                .iter()
                .any(|d| d.id == id && d.revoked_at.is_none())
        );
        rig.ctrl.revoke_device(&id).unwrap();
        let row = rig.ctrl.devices().into_iter().find(|d| d.id == id).unwrap();
        assert!(row.revoked_at.is_some());
        assert!(!row.connected);
        assert!(rig.ctrl.revoke_device(&DeviceId::from("dev_nope")).is_err());
        assert!(
            rig.audit
                .recent(10)
                .iter()
                .any(|e| e.event == kind::DEVICE_REVOKED)
        );
        rig.finish().await;
    }

    #[tokio::test]
    async fn the_pairing_window_opens_closes_and_reports_a_qr_only_while_open() {
        let rig = rig().await;
        assert!(rig.ctrl.pairing_qr().is_none());
        assert!(!rig.ctrl.pairing().open);
        let qr = rig.ctrl.open_pairing_window();
        assert!(rig.ctrl.pairing().open);
        assert_eq!(
            rig.ctrl.pairing_qr().unwrap().secret.expose(),
            qr.secret.expose()
        );
        rig.ctrl.close_pairing_window();
        assert!(!rig.ctrl.pairing().open);
        assert!(rig.ctrl.pairing_qr().is_none());
        // Deciding a request that does not exist is a readable error, not a panic.
        let err = rig
            .ctrl
            .approve_pairing(&PairingId::from("nope"))
            .unwrap_err();
        assert!(err.to_string().contains("no longer pending"), "{err}");
        rig.finish().await;
    }

    #[tokio::test]
    async fn the_kill_switch_closes_pairing_and_is_audited() {
        let rig = rig().await;
        rig.ctrl.open_pairing_window();
        rig.ctrl.kill_switch();
        assert!(!rig.ctrl.pairing().open);
        assert!(
            rig.audit
                .recent(20)
                .iter()
                .any(|e| e.event == kind::KILL_SWITCH)
        );
        // The sockets stay bound: the kill switch drops sessions, it does not stop listening.
        assert_eq!(rig.ctrl.network().bound.len(), 2);
        rig.finish().await;
    }

    async fn wait_for_bound(rig: &Rig, want: usize) -> bool {
        for _ in 0..100 {
            if rig.ctrl.network().bound.len() == want {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        false
    }

    #[tokio::test]
    async fn pausing_closes_the_network_socket_and_resuming_brings_it_back() {
        let rig = rig().await;
        assert_eq!(
            rig.ctrl.network().bound.len(),
            2,
            "loopback and the network address"
        );
        rig.ctrl.open_pairing_window();

        rig.ctrl.pause_listener(Duration::from_secs(600));
        assert!(rig.ctrl.status().paused_for.is_some());
        assert!(
            !rig.ctrl.pairing().open,
            "pausing closes the pairing window"
        );
        assert!(wait_for_bound(&rig, 1).await, "only loopback stays bound");
        let net = rig.ctrl.network();
        assert!(net.bound[0].ip().is_loopback());
        assert!(net.paused_for.is_some());

        rig.ctrl.resume_listener();
        assert!(rig.ctrl.status().paused_for.is_none());
        assert!(wait_for_bound(&rig, 2).await, "the network socket is back");
        let events: Vec<String> = rig.audit.recent(50).into_iter().map(|e| e.event).collect();
        assert!(events.contains(&kind::LISTENER_PAUSED.to_owned()));
        assert!(events.contains(&kind::LISTENER_RESUMED.to_owned()));
        rig.finish().await;
    }

    #[tokio::test]
    async fn a_pause_ends_by_itself_after_its_time() {
        let rig = rig().await;
        rig.ctrl.pause_listener(Duration::from_millis(1500));
        assert!(wait_for_bound(&rig, 1).await);
        assert!(
            wait_for_bound(&rig, 2).await,
            "the timer resumed the listener"
        );
        assert!(rig.ctrl.status().paused_for.is_none());
        rig.finish().await;
    }

    #[tokio::test]
    async fn settings_are_saved_and_reported_as_needing_a_restart() {
        let rig = rig().await;
        assert_eq!(rig.ctrl.set_port(50123).unwrap(), Applied::AfterRestart);
        assert_eq!(rig.ctrl.settings().port, 50123);
        assert_eq!(rig.saved_config().listener.port, 50123);
        assert!(rig.ctrl.set_port(0).is_err(), "port 0 is invalid");
        assert_eq!(
            rig.ctrl.settings().port,
            50123,
            "a refused change changes nothing"
        );

        assert_eq!(
            rig.ctrl.set_cache_cap_mib(4096).unwrap(),
            Applied::AfterRestart
        );
        assert_eq!(rig.saved_config().cache.cap_mib, 4096);
        assert!(rig.ctrl.set_cache_cap_mib(1).is_err(), "below the floor");
        rig.finish().await;
    }

    #[tokio::test]
    async fn the_clipboard_mode_is_saved_audited_and_in_force_at_once() {
        let rig = rig().await;
        assert_eq!(rig.ctrl.settings().clipboard, ClipboardMode::Off);
        assert_eq!(rig.clipboard.setting().get(), ClipboardMode::Off);

        assert_eq!(
            rig.ctrl.set_clipboard_mode(ClipboardMode::Both).unwrap(),
            Applied::Now
        );
        // The running sessions read this value on every request.
        assert_eq!(rig.clipboard.setting().get(), ClipboardMode::Both);
        assert_eq!(rig.ctrl.settings().clipboard, ClipboardMode::Both);
        assert_eq!(rig.saved_config().stream.clipboard, ClipboardMode::Both);
        let changed = rig
            .audit
            .recent(10)
            .into_iter()
            .find(|e| {
                e.event == kind::SETTINGS_CHANGED && e.detail["setting"] == "stream.clipboard"
            })
            .expect("the change is audited");
        assert_eq!(changed.detail["mode"], "both");

        rig.ctrl.set_clipboard_mode(ClipboardMode::Off).unwrap();
        assert_eq!(rig.clipboard.setting().get(), ClipboardMode::Off);
        assert_eq!(rig.saved_config().stream.clipboard, ClipboardMode::Off);
        rig.finish().await;
    }

    #[tokio::test]
    async fn notices_from_the_clipboard_bridge_reach_the_tray_once() {
        let rig = rig().await;
        assert!(rig.ctrl.take_notices().is_empty());
        rig.clipboard.notices().push(crate::clipboard::Notice {
            title: "Pixel 9 copied text from this computer".into(),
            body: "5 bytes".into(),
        });
        let notices = rig.ctrl.take_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Pixel 9 copied text from this computer");
        assert!(rig.ctrl.take_notices().is_empty(), "taken once");
        rig.finish().await;
    }

    #[tokio::test]
    async fn purge_reports_what_it_freed_and_the_autostart_switch_is_the_stub() {
        let rig = rig().await;
        assert_eq!(rig.ctrl.settings().cache_used, Some(5 * 1024 * 1024));
        assert_eq!(rig.ctrl.purge_cache().unwrap(), 5 * 1024 * 1024);
        assert_eq!(rig.caches.used_bytes(), 0);
        assert!(
            rig.audit
                .recent(10)
                .iter()
                .any(|e| e.event == kind::CACHE_PURGED)
        );

        assert!(!rig.ctrl.settings().autostart.enabled);
        rig.ctrl.set_autostart(true).unwrap();
        let a = rig.ctrl.settings().autostart;
        assert!(a.enabled);
        assert!(a.note.is_some(), "the stub says it is not wired");
        rig.finish().await;
    }

    #[tokio::test]
    async fn network_and_activity_read_back_what_happened() {
        let rig = rig().await;
        rig.ctrl.open_pairing_window();
        assert!(
            rig.ctrl
                .activity(10)
                .iter()
                .any(|e| e.event == kind::PAIR_WINDOW_OPENED)
        );
        // The connectivity check finishes in the background.
        let mut done = false;
        for _ in 0..100 {
            if rig.ctrl.network().self_check.done {
                done = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert!(done, "the first self-check completed");
        let net = rig.ctrl.network();
        assert_eq!(net.adapter, TrustAdapterKind::Generic);
        assert!(
            net.bound.iter().all(|a| !a.ip().is_unspecified()),
            "never a wildcard"
        );
        assert!(!net.self_check.running);
        rig.finish().await;
    }

    #[tokio::test]
    async fn status_lists_nobody_connected_on_a_fresh_host() {
        let rig = rig().await;
        let s = rig.ctrl.status();
        assert!(s.connected.is_empty() && s.stream.is_none() && s.paused_for.is_none());
        assert!(!s.host_name.is_empty());
        rig.finish().await;
    }
}
