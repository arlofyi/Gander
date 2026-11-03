// SPDX-License-Identifier: Apache-2.0
//! In-process link impairment (from the WP-43 spike): a two-socket UDP relay on 127.0.0.1
//! between the host and the viewer.
//!
//! ```text
//! host --> B [token bucket + drop-tail queue] A --> viewer      (media)
//! host <-- B <------------- unimpaired ------ A <-- viewer      (TWCC/RTCP, STUN)
//! ```
//!
//! Each side is told (by rewriting the candidate in the SDP it receives) that the other one
//! lives at a relay socket, so ICE treats the relay as the path. With a cap set, each media packet
//! departs at `max(now, previous departure) + size / cap`, and a packet whose queueing delay
//! would exceed `max_queue` is dropped (a router's drop-tail buffer). `blackhole` drops
//! everything in both directions (a dead link, for the reconnect test).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::sdp::rewrite_candidates;

/// Counters and the live settings.
#[derive(Debug, Default)]
pub struct RelayState {
    /// Cap in bit/s for host -> viewer; 0 = unlimited.
    pub cap_bps: AtomicU64,
    /// Drop-tail buffer depth, microseconds.
    pub max_queue_us: AtomicU64,
    /// Drop everything both ways.
    pub blackhole: AtomicBool,
    /// Bytes delivered host -> viewer.
    pub fwd_bytes: AtomicU64,
    /// Packets dropped by the queue limit or the blackhole.
    pub dropped: AtomicU64,
    /// Queueing delay of the last forwarded packet, microseconds.
    pub queue_us: AtomicU64,
}

/// A running relay.
#[derive(Debug)]
pub struct Relay {
    /// Socket the viewer is told is the host.
    pub a: SocketAddr,
    /// Socket the host is told is the viewer.
    pub b: SocketAddr,
    host_real: Arc<Mutex<Option<SocketAddr>>>,
    viewer_real: Arc<Mutex<Option<SocketAddr>>>,
    /// Live state.
    pub state: Arc<RelayState>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Relay {
    /// Binds both sockets on 127.0.0.1 (ephemeral ports: the relay is the test's network, not
    /// the host's) and starts forwarding.
    pub async fn start(max_queue: Duration) -> std::io::Result<Relay> {
        let sa = Arc::new(UdpSocket::bind(("127.0.0.1", 0)).await?);
        let sb = Arc::new(UdpSocket::bind(("127.0.0.1", 0)).await?);
        let state = Arc::new(RelayState::default());
        state
            .max_queue_us
            .store(max_queue.as_micros() as u64, Ordering::Relaxed);
        let host_real: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
        let viewer_real: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
        let mut tasks = Vec::new();

        // Viewer -> host: unimpaired (except the blackhole).
        {
            let (sa, sb) = (sa.clone(), sb.clone());
            let to = host_real.clone();
            let st = state.clone();
            tasks.push(tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                loop {
                    // Windows reports ICMP port-unreachable as a recv error on UDP; keep going.
                    let Ok((n, _)) = sa.recv_from(&mut buf).await else {
                        continue;
                    };
                    if st.blackhole.load(Ordering::Relaxed) {
                        continue;
                    }
                    let dest = *to.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(d) = dest {
                        let _ = sb.send_to(&buf[..n], d).await;
                    }
                }
            }));
        }
        // Host -> viewer: shaped.
        let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Instant, Vec<u8>)>();
        {
            let sb = sb.clone();
            let st = state.clone();
            tasks.push(tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                let mut last_depart = Instant::now();
                loop {
                    let Ok((n, _)) = sb.recv_from(&mut buf).await else {
                        continue;
                    };
                    if st.blackhole.load(Ordering::Relaxed) {
                        st.dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let now = Instant::now();
                    let cap = st.cap_bps.load(Ordering::Relaxed);
                    let max_queue = Duration::from_micros(st.max_queue_us.load(Ordering::Relaxed));
                    let start = now.max(last_depart);
                    let depart = if cap == 0 {
                        start
                    } else {
                        if start - now > max_queue {
                            st.dropped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        start + Duration::from_secs_f64((n as f64 + 28.0) * 8.0 / cap as f64)
                    };
                    last_depart = depart;
                    if tx.send((now, depart, buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }));
        }
        let sa2 = sa.clone();
        {
            let to = viewer_real.clone();
            let st = state.clone();
            tasks.push(tokio::spawn(async move {
                while let Some((arrived, depart, pkt)) = rx.recv().await {
                    if depart > Instant::now() {
                        tokio::time::sleep_until(depart).await;
                    }
                    st.queue_us.store(
                        depart.saturating_duration_since(arrived).as_micros() as u64,
                        Ordering::Relaxed,
                    );
                    let dest = *to.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(d) = dest
                        && sa2.send_to(&pkt, d).await.is_ok()
                    {
                        st.fwd_bytes.fetch_add(pkt.len() as u64, Ordering::Relaxed);
                    }
                }
            }));
        }
        Ok(Relay {
            a: sa.local_addr()?,
            b: sb.local_addr()?,
            host_real,
            viewer_real,
            state,
            tasks,
        })
    }

    /// Rewrites the host's (offer) candidates to relay A and remembers the real address.
    pub fn munge_offer(&self, sdp: &str) -> Option<String> {
        let (out, real) = rewrite_candidates(sdp, self.a)?;
        *self.host_real.lock().unwrap_or_else(|e| e.into_inner()) = Some(real);
        Some(out)
    }

    /// Rewrites the viewer's (answer) candidates to relay B and remembers the real address.
    pub fn munge_answer(&self, sdp: &str) -> Option<String> {
        let (out, real) = rewrite_candidates(sdp, self.b)?;
        *self.viewer_real.lock().unwrap_or_else(|e| e.into_inner()) = Some(real);
        Some(out)
    }

    /// Sets the cap (0 = unlimited).
    pub fn set_cap(&self, bps: u64) {
        self.state.cap_bps.store(bps, Ordering::Relaxed);
    }

    /// Cuts or restores the link both ways.
    pub fn set_blackhole(&self, on: bool) {
        self.state.blackhole.store(on, Ordering::Relaxed);
    }
}
