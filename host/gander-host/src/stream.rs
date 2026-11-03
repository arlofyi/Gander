// SPDX-License-Identifier: Apache-2.0
//! Stream engine glue (WP-50): connects [`gander_stream::StreamEngine`] to the host.
//!
//! - [`EngineSignalling`]: the `/v1/events` [`StreamSignalling`] backed by the engine (replaces
//!   the Phase 1 `StreamUnavailable`): `stream.answer`, `stream.ice`, `stream.pause`,
//!   `stream.resume` and `stream.resize` reach the session; unknown sessions and other devices'
//!   sessions get `error { code: not_found }`.
//! - [`EventsSink`]: the engine's [`SignalSink`] over [`Events::send_to`] (`stream.offer`,
//!   `stream.ice`, `stream.state`, `error`).
//! - [`ServerAddrs`]: ICE binds the addresses the HTTPS listener serves (the listener plan:
//!   loopback plus the private-network address), never a wildcard.
//! - [`engine_config`]: engine settings from `config.toml` (`[firewall] udp_ports`).
//! - [`windows_engine`]: everything assembled with the Windows power and capture sources and the
//!   given lock source.
//! - [`OverrideLock`]: a lock source that is also locked while a test hook says so (the test
//!   host's `lock` / `unlock` stdin commands).
//!
//! Session creation (`POST /v1/stream`, `DELETE`, the one-session rule, approval, the
//! `host_locked` precondition) is WP-53's [`crate::lifecycle`], which installs its own
//! signalling ([`crate::lifecycle::LifecycleSignalling`]) in front of [`EngineSignalling`].
//! The engine runs with `owner_reports_end` ([`engine_config`]): the lifecycle reports every end.

use std::net::{IpAddr, SocketAddr};

use gander_core::config::HostConfig;
use gander_core::net::plan::ListenerPlan;
use gander_core::protocol::DeviceId;
use gander_core::protocol::events::{ClientEvent, ServerEvent};
use gander_stream::{BindAddrs, EngineConfig, LockSource, LockStatus, SignalSink, StreamEngine};
use tokio::sync::watch;

use crate::events::{Events, StreamSignalling};
use crate::server::ServerHandle;

/// `stream.*` client messages handled by the engine.
#[derive(Debug, Clone)]
pub struct EngineSignalling(pub StreamEngine);

impl StreamSignalling for EngineSignalling {
    fn on_client_event(&self, device: &DeviceId, event: ClientEvent) -> Option<ServerEvent> {
        self.0.handle_client_event(device, event)
    }
}

/// The engine's signalling output, over the events channel.
#[derive(Debug, Clone)]
pub struct EventsSink(pub Events);

impl SignalSink for EventsSink {
    fn send(&self, device: &DeviceId, event: ServerEvent) {
        self.0.send_to(device, event);
    }
}

/// ICE addresses from the running listener.
#[derive(Debug, Clone)]
pub struct ServerAddrs(pub ServerHandle);

impl BindAddrs for ServerAddrs {
    fn ice_ips(&self) -> Vec<IpAddr> {
        ice_ips(self.0.plan().as_ref(), &self.0.bound_addrs())
    }
}

/// The addresses ICE binds: the plan's loopback and network addresses, or, without a plan, the
/// addresses actually bound. Unspecified addresses are dropped (they are never in a plan; this
/// is a second check), duplicates removed, order kept (loopback first).
pub fn ice_ips(plan: Option<&ListenerPlan>, bound: &[SocketAddr]) -> Vec<IpAddr> {
    let candidates: Vec<IpAddr> = match plan {
        Some(p) => p.loopback().iter().chain(p.network()).copied().collect(),
        None => bound.iter().map(SocketAddr::ip).collect(),
    };
    let mut out: Vec<IpAddr> = Vec::new();
    for ip in candidates {
        if !ip.is_unspecified() && !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// Engine settings from the host configuration. The lifecycle (WP-53) owns the end of every
/// session, so `owner_reports_end` is on.
pub fn engine_config(config: &HostConfig) -> EngineConfig {
    EngineConfig {
        udp_ports: config.firewall.udp_ports,
        owner_reports_end: true,
        ..EngineConfig::default()
    }
}

/// A lock source that is locked when `inner` is OR when the test hook says so. Fail closed: a
/// closed `inner` counts as locked from then on. Must be created inside a tokio runtime.
#[derive(Debug)]
pub struct OverrideLock {
    /// Kept so the inner source (and its watcher) lives as long as this one.
    _inner: std::sync::Arc<dyn LockSource>,
    tx: std::sync::Arc<watch::Sender<LockStatus>>,
    simulated: std::sync::Arc<watch::Sender<bool>>,
}

impl OverrideLock {
    /// Follows `inner`, plus the simulated flag (initially unlocked).
    pub fn new(inner: std::sync::Arc<dyn LockSource>) -> Self {
        let mut inner_rx = inner.subscribe();
        let first = *inner_rx.borrow();
        let tx = std::sync::Arc::new(watch::Sender::new(first));
        let simulated = std::sync::Arc::new(watch::Sender::new(false));
        let mut sim_rx = simulated.subscribe();
        let out = std::sync::Arc::clone(&tx);
        tokio::spawn(async move {
            let mut inner_closed = false;
            loop {
                let inner_locked = inner_closed || inner_rx.borrow().locked;
                let locked = inner_locked || *sim_rx.borrow();
                out.send_modify(|s| s.observe(locked));
                tokio::select! {
                    r = inner_rx.changed(), if !inner_closed => {
                        if r.is_err() {
                            inner_closed = true;
                        }
                    }
                    r = sim_rx.changed() => {
                        if r.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            _inner: inner,
            tx,
            simulated,
        }
    }

    /// Simulates a lock (`true`) or its end (`false`).
    pub fn set_simulated(&self, locked: bool) {
        self.simulated.send_replace(locked);
    }

    /// Whether the simulated lock is on.
    pub fn simulated(&self) -> bool {
        *self.simulated.borrow()
    }
}

impl LockSource for OverrideLock {
    fn subscribe(&self) -> watch::Receiver<LockStatus> {
        self.tx.subscribe()
    }
}

/// Installs the engine as the events channel's stream signalling.
pub fn install(events: &Events, engine: &StreamEngine) {
    events.set_stream_signalling(std::sync::Arc::new(EngineSignalling(engine.clone())));
}

/// The production engine on Windows: the session-lock guard `lock` (production:
/// `gander_stream::win::SessionLock` over `gander_win::session::watch`; the lifecycle uses the
/// same source), battery cap (`gander_win::session::power`), WGC + Media Foundation source, the
/// window's sound by process loopback unless `[stream] audio = false` (WP-55), ICE
/// on the listener's addresses inside `[firewall] udp_ports`, signalling over `events`. The
/// caller installs the signalling (the lifecycle's, or [`install`] for the engine alone).
#[cfg(windows)]
pub fn windows_engine(
    config: &HostConfig,
    events: &Events,
    server: ServerHandle,
    lock: std::sync::Arc<dyn LockSource>,
) -> StreamEngine {
    use std::sync::Arc;

    use gander_stream::EngineDeps;
    use gander_stream::win::{ProcessLoopback, SystemPower, WgcSource};

    StreamEngine::new(
        engine_config(config),
        EngineDeps {
            lock,
            power: Arc::new(SystemPower),
            addrs: Arc::new(ServerAddrs(server)),
            signal: Arc::new(EventsSink(events.clone())),
            source: Arc::new(WgcSource {
                prefer_luid: None,
                prefer_vendor: None,
                border_required: Some(config.stream.capture_border),
            }),
            // The window's own sound (WP-55), unless the owner turned it off.
            audio: config
                .stream
                .audio
                .then(|| Arc::new(ProcessLoopback) as Arc<dyn gander_stream::audio::AudioSource>),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gander_core::config::UdpPortRange;
    use gander_core::error::ErrorCode;
    use gander_core::protocol::SessionId;
    use gander_core::protocol::host::TrustAdapterKind;
    use gander_stream::testing::{CollectingSink, SyntheticSource};
    use gander_stream::{EngineDeps, FakeLock, FixedPower, StaticAddrs};

    use super::*;

    #[test]
    fn ice_addresses_come_from_the_plan_and_never_a_wildcard() {
        let plan = ListenerPlan::new(
            TrustAdapterKind::Generic,
            0,
            vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
            vec!["100.64.1.2".parse().unwrap()],
        )
        .unwrap();
        assert_eq!(
            ice_ips(Some(&plan), &[]),
            vec![
                "127.0.0.1".parse::<IpAddr>().unwrap(),
                "::1".parse().unwrap(),
                "100.64.1.2".parse().unwrap()
            ]
        );
        let bound: Vec<SocketAddr> = vec![
            "127.0.0.1:443".parse().unwrap(),
            "0.0.0.0:443".parse().unwrap(),
            "127.0.0.1:444".parse().unwrap(),
        ];
        assert_eq!(
            ice_ips(None, &bound),
            vec!["127.0.0.1".parse::<IpAddr>().unwrap()]
        );
    }

    #[tokio::test]
    async fn override_lock_is_locked_by_either_source() {
        async fn wait(rx: &watch::Receiver<LockStatus>, want: bool) -> LockStatus {
            let mut rx = rx.clone();
            let s = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                rx.wait_for(|s| s.locked == want),
            )
            .await
            .expect("state reached")
            .expect("sender alive");
            *s
        }
        let real = Arc::new(FakeLock::new(false));
        let lock = OverrideLock::new(real.clone());
        let rx = lock.subscribe();
        assert!(!wait(&rx, false).await.locked);
        lock.set_simulated(true);
        assert_eq!(wait(&rx, true).await.locks, 1);
        assert!(lock.simulated());
        lock.set_simulated(false);
        wait(&rx, false).await;
        real.set_locked(true);
        assert_eq!(wait(&rx, true).await.locks, 2);
        real.set_locked(false);
        wait(&rx, false).await;
    }

    #[test]
    fn engine_uses_the_firewall_port_range() {
        let mut config = HostConfig::default();
        config.firewall.udp_ports = UdpPortRange::new(49_200, 49_299).unwrap();
        assert_eq!(engine_config(&config).udp_ports, config.firewall.udp_ports);
        assert!(engine_config(&config).owner_reports_end);
    }

    #[tokio::test]
    async fn signalling_reaches_the_engine_instead_of_stream_unavailable() {
        let engine = StreamEngine::new(
            EngineConfig::default(),
            EngineDeps {
                lock: Arc::new(FakeLock::new(false)),
                power: Arc::new(FixedPower::new(false)),
                addrs: Arc::new(StaticAddrs(vec!["127.0.0.1".parse().unwrap()])),
                signal: Arc::new(CollectingSink::default()),
                source: Arc::new(SyntheticSource::default()),
                audio: None,
            },
        );
        let sig = EngineSignalling(engine);
        let device = DeviceId::from("dev_1");
        let reply = sig.on_client_event(
            &device,
            ClientEvent::StreamAnswer {
                session: SessionId::from("ses_unknown"),
                sdp: "v=0".into(),
            },
        );
        match reply {
            Some(ServerEvent::Error { code, session, .. }) => {
                assert_eq!(code, ErrorCode::NotFound);
                assert_eq!(session, Some(SessionId::from("ses_unknown")));
            }
            other => panic!("expected not_found, got {other:?}"),
        }
        assert!(sig.on_client_event(&device, ClientEvent::Ping {}).is_none());
    }
}
