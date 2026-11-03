// SPDX-License-Identifier: Apache-2.0
//! Async face of the Tailscale trust adapter: WhoIs with the 60 s cache, relay classification
//! from a short-lived `status` cache, and Shields Up.
//!
//! Every backend call is blocking (pipe or CLI) and runs on the blocking pool with a timeout, so a
//! stuck tailscaled can delay one connection's identity lookup but never the accept loop.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gander_core::net::tailscale::{TailscaleLocal, TailscaleStatus, WhoIsAnswer, WhoIsCache};
use gander_core::protocol::host::RelayState;

use super::gate::PeerNetwork;

/// Longest a WhoIs may delay a new connection.
pub const WHOIS_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a `status` answer is reused for relay classification.
pub const STATUS_TTL: Duration = Duration::from_secs(10);

/// The Tailscale side of the trust adapter.
#[derive(Debug)]
pub struct TrustNetwork {
    backend: Arc<dyn TailscaleLocal>,
    cache: Arc<WhoIsCache>,
    status: Mutex<Option<(Instant, TailscaleStatus)>>,
}

impl TrustNetwork {
    /// Wraps a backend (production: `gander_win::net::tailscale::system_tailscale()`).
    pub fn new(backend: Arc<dyn TailscaleLocal>) -> Self {
        Self {
            backend,
            cache: Arc::new(WhoIsCache::default()),
            status: Mutex::new(None),
        }
    }

    /// The identity of a connection's peer. Loopback peers are never looked up.
    pub async fn identify(&self, peer: SocketAddr) -> PeerNetwork {
        if peer.ip().is_loopback() {
            return PeerNetwork::Loopback;
        }
        let backend = Arc::clone(&self.backend);
        let cache = Arc::clone(&self.cache);
        let job =
            tokio::task::spawn_blocking(move || cache.lookup(&*backend, peer, Instant::now()));
        match tokio::time::timeout(WHOIS_TIMEOUT, job).await {
            Ok(Ok(Ok(WhoIsAnswer::Found(p)))) => PeerNetwork::Identified(p),
            Ok(Ok(Ok(WhoIsAnswer::NotFound))) => PeerNetwork::NoIdentity,
            Ok(Ok(Err(e))) => {
                tracing::warn!(error = %e, "whois failed");
                PeerNetwork::Unavailable
            }
            Ok(Err(_)) | Err(_) => {
                tracing::warn!("whois timed out");
                PeerNetwork::Unavailable
            }
        }
    }

    /// `status`, cached for [`STATUS_TTL`].
    pub async fn status(&self) -> Option<TailscaleStatus> {
        if let Some((at, s)) = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && at.elapsed() < STATUS_TTL
        {
            return Some(s.clone());
        }
        let backend = Arc::clone(&self.backend);
        let job = tokio::task::spawn_blocking(move || backend.status());
        match tokio::time::timeout(WHOIS_TIMEOUT * 2, job).await {
            Ok(Ok(Ok(s))) => {
                *self.status.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((Instant::now(), s.clone()));
                Some(s)
            }
            _ => None,
        }
    }

    /// Path to the peer owning `addr` (`/v1/me.network.relayed`); `None` when unknown.
    pub async fn relay_state(&self, addr: IpAddr) -> Option<RelayState> {
        self.status().await?.relay_state_for(addr)
    }

    /// Whether Shields Up blocks inbound tailnet connections; `None` when unknown.
    pub async fn shields_up(&self) -> Option<bool> {
        let backend = Arc::clone(&self.backend);
        let job = tokio::task::spawn_blocking(move || backend.shields_up());
        match tokio::time::timeout(WHOIS_TIMEOUT * 2, job).await {
            Ok(Ok(Ok(v))) => Some(v),
            _ => self
                .status()
                .await
                .map(|s| s.health_mentions_shields_up())
                .filter(|v| *v),
        }
    }

    /// Drops cached WhoIs answers (after a network change).
    pub fn clear_cache(&self) {
        self.cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gander_core::net::tailscale::{FakeTailscale, PeerIdentity, PeerStatus};

    fn peer() -> PeerIdentity {
        PeerIdentity {
            login: "alice@example.com".into(),
            node_id: "n1".into(),
            node_name: "pixel-9".into(),
            tags: vec![],
            shared_in: false,
        }
    }

    #[tokio::test]
    async fn identify_semantics_and_cache() {
        let fake = Arc::new(FakeTailscale::new().with_peer("100.64.0.2".parse().unwrap(), peer()));
        let net = TrustNetwork::new(fake.clone());
        assert_eq!(
            net.identify("127.0.0.1:5".parse().unwrap()).await,
            PeerNetwork::Loopback
        );
        assert_eq!(fake.whois_calls(), 0, "loopback is never looked up");
        assert_eq!(
            net.identify("100.64.0.2:5".parse().unwrap()).await,
            PeerNetwork::Identified(peer())
        );
        net.identify("100.64.0.2:6".parse().unwrap()).await;
        assert_eq!(fake.whois_calls(), 1, "second connection served from cache");
        assert_eq!(
            net.identify("192.168.1.9:5".parse().unwrap()).await,
            PeerNetwork::NoIdentity
        );
        let down = TrustNetwork::new(Arc::new(FakeTailscale::unavailable()));
        assert_eq!(
            down.identify("100.64.0.2:5".parse().unwrap()).await,
            PeerNetwork::Unavailable
        );
        assert_eq!(down.shields_up().await, None);
    }

    #[tokio::test]
    async fn relay_and_shields() {
        let fake = FakeTailscale::new()
            .with_shields_up(true)
            .with_status(TailscaleStatus {
                peers: vec![PeerStatus {
                    addresses: vec!["100.64.0.2".parse().unwrap()],
                    relay: "fra".into(),
                    online: true,
                    active: true,
                    ..PeerStatus::default()
                }],
                ..TailscaleStatus::default()
            });
        let net = TrustNetwork::new(Arc::new(fake));
        assert_eq!(
            net.relay_state("100.64.0.2".parse().unwrap()).await,
            Some(RelayState::Derp)
        );
        assert_eq!(net.shields_up().await, Some(true));
    }
}
