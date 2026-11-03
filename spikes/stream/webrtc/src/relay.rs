// SPDX-License-Identifier: Apache-2.0
//! In-process link impairment: a two-socket UDP relay on 127.0.0.1 between the two peers.
//!
//! Substitute for the plan's `clumsy` / Windows QoS policy (neither installed, and machine QoS
//! policy must not change). Each peer is told (by rewriting the candidate in the SDP it receives)
//! that the other one lives at a relay socket, so every packet crosses the relay:
//!
//! ```text
//! sender S --> B [token bucket + drop-tail queue] A --> receiver R     (media, sender reports)
//! sender S <-- B <------------- unimpaired ------ A <-- receiver R     (TWCC/RTCP, STUN)
//! ```
//!
//! The sender -> receiver direction is shaped: when a cap is set, each packet departs at
//! `max(now, previous departure) + size / cap`; a packet whose queueing delay would exceed
//! `max_queue` is dropped (a router's drop-tail buffer). With no cap packets pass straight
//! through. Both peers see consistent addresses, so ICE treats the relay as the path.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Counters and the live cap.
#[derive(Debug, Default)]
pub struct RelayState {
    /// Cap in bits per second for sender -> receiver; 0 = unlimited.
    pub cap_bps: AtomicU64,
    /// Bytes delivered sender -> receiver.
    pub fwd_bytes: AtomicU64,
    /// Packets dropped by the queue limit.
    pub dropped: AtomicU64,
    /// Bytes delivered receiver -> sender.
    pub back_bytes: AtomicU64,
}

/// A running relay.
#[derive(Debug)]
pub struct Relay {
    /// Socket the receiver is told is the sender.
    pub a: SocketAddr,
    /// Socket the sender is told is the receiver.
    pub b: SocketAddr,
    sender_real: Arc<Mutex<Option<SocketAddr>>>,
    receiver_real: Arc<Mutex<Option<SocketAddr>>>,
    /// Live state.
    pub state: Arc<RelayState>,
}

impl Relay {
    /// Binds both sockets on 127.0.0.1 and starts forwarding.
    pub async fn start(max_queue: Duration) -> Result<Relay> {
        let sa = Arc::new(UdpSocket::bind(("127.0.0.1", 0)).await.context("relay A")?);
        let sb = Arc::new(UdpSocket::bind(("127.0.0.1", 0)).await.context("relay B")?);
        let relay = Relay {
            a: sa.local_addr()?,
            b: sb.local_addr()?,
            sender_real: Arc::default(),
            receiver_real: Arc::default(),
            state: Arc::default(),
        };

        // Receiver -> sender: unimpaired.
        {
            let (sa, sb) = (sa.clone(), sb.clone());
            let to = relay.sender_real.clone();
            let st = relay.state.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                loop {
                    // On Windows an ICMP port-unreachable surfaces as a recv error on UDP
                    // (WSAECONNRESET); keep going.
                    let Ok((n, _)) = sa.recv_from(&mut buf).await else {
                        continue;
                    };
                    let dest = *to.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(d) = dest
                        && sb.send_to(&buf[..n], d).await.is_ok()
                    {
                        st.back_bytes.fetch_add(n as u64, Ordering::Relaxed);
                    }
                }
            });
        }

        // Sender -> receiver: shaped.
        let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
        {
            let sb = sb.clone();
            let st = relay.state.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 2048];
                let mut last_depart = Instant::now();
                loop {
                    let Ok((n, _)) = sb.recv_from(&mut buf).await else {
                        continue;
                    };
                    let now = Instant::now();
                    let cap = st.cap_bps.load(Ordering::Relaxed);
                    let depart = if cap == 0 {
                        now.max(last_depart)
                    } else {
                        let start = now.max(last_depart);
                        if start - now > max_queue {
                            st.dropped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        start + Duration::from_secs_f64((n as f64 + 28.0) * 8.0 / cap as f64)
                    };
                    last_depart = depart;
                    if tx.send((depart, buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            });
        }
        {
            let to = relay.receiver_real.clone();
            let st = relay.state.clone();
            tokio::spawn(async move {
                while let Some((depart, pkt)) = rx.recv().await {
                    if depart > Instant::now() {
                        tokio::time::sleep_until(depart).await;
                    }
                    let dest = *to.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(d) = dest
                        && sa.send_to(&pkt, d).await.is_ok()
                    {
                        st.fwd_bytes.fetch_add(pkt.len() as u64, Ordering::Relaxed);
                    }
                }
            });
        }
        Ok(relay)
    }

    /// Rewrites the sender's (offer) candidates to relay A and remembers the real address.
    pub fn munge_offer(&self, sdp: &str) -> Result<String> {
        let (out, real) = rewrite_candidates(sdp, self.a)?;
        *self.sender_real.lock().unwrap_or_else(|e| e.into_inner()) = Some(real);
        Ok(out)
    }

    /// Rewrites the receiver's (answer) candidates to relay B and remembers the real address.
    pub fn munge_answer(&self, sdp: &str) -> Result<String> {
        let (out, real) = rewrite_candidates(sdp, self.b)?;
        *self.receiver_real.lock().unwrap_or_else(|e| e.into_inner()) = Some(real);
        Ok(out)
    }

    /// Sets the cap (0 = unlimited).
    pub fn set_cap(&self, bps: u64) {
        self.state.cap_bps.store(bps, Ordering::Relaxed);
    }
}

/// Replaces the address of every `a=candidate` line with `to`; returns the original address
/// (all candidates must name the same one: this spike gathers exactly one).
pub fn rewrite_candidates(sdp: &str, to: SocketAddr) -> Result<(String, SocketAddr)> {
    let mut real: Option<SocketAddr> = None;
    let mut out = String::with_capacity(sdp.len());
    for line in sdp.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = body.strip_prefix("a=candidate:") {
            let mut f: Vec<String> = rest.split(' ').map(str::to_owned).collect();
            anyhow::ensure!(f.len() >= 6, "short candidate line {body}");
            let addr: SocketAddr = format!("{}:{}", f[4], f[5]).parse()?;
            anyhow::ensure!(
                real.is_none_or(|r| r == addr),
                "more than one candidate address"
            );
            real = Some(addr);
            f[4] = to.ip().to_string();
            f[5] = to.port().to_string();
            out.push_str("a=candidate:");
            out.push_str(&f.join(" "));
            out.push_str(&line[body.len()..]);
        } else {
            out.push_str(line);
        }
    }
    Ok((out, real.context("no candidate in SDP")?))
}

/// Replaces only the IP of every `a=candidate` line with `ip`, keeping each port and everything
/// else. Used to advertise 10.0.2.2 (the emulator's alias for the host loopback) for a socket
/// that is bound to 127.0.0.1.
pub fn rewrite_candidate_ip(sdp: &str, ip: std::net::IpAddr) -> Result<String> {
    let mut out = String::with_capacity(sdp.len());
    let mut seen = false;
    for line in sdp.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = body.strip_prefix("a=candidate:") {
            let mut f: Vec<String> = rest.split(' ').map(str::to_owned).collect();
            anyhow::ensure!(f.len() >= 6, "short candidate line {body}");
            f[4] = ip.to_string();
            seen = true;
            out.push_str("a=candidate:");
            out.push_str(&f.join(" "));
            out.push_str(&line[body.len()..]);
        } else {
            out.push_str(line);
        }
    }
    anyhow::ensure!(seen, "no candidate in SDP");
    Ok(out)
}

/// The SDP without its `a=candidate` and `a=end-of-candidates` lines.
pub fn strip_candidates(sdp: &str) -> String {
    sdp.split_inclusive('\n')
        .filter(|l| !l.starts_with("a=candidate:") && !l.starts_with("a=end-of-candidates"))
        .collect()
}

/// The `a=candidate` lines of an SDP (for the report).
pub fn candidates(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter(|l| l.starts_with("a=candidate:"))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_host_candidate() {
        let sdp = "v=0\r\na=candidate:1 1 udp 2130706431 127.0.0.1 50000 typ host\r\na=end\r\n";
        let (out, real) = rewrite_candidates(sdp, "127.0.0.1:60000".parse().unwrap()).unwrap();
        assert_eq!(real, "127.0.0.1:50000".parse::<SocketAddr>().unwrap());
        assert_eq!(
            out,
            "v=0\r\na=candidate:1 1 udp 2130706431 127.0.0.1 60000 typ host\r\na=end\r\n"
        );
    }

    #[test]
    fn rewrites_only_the_ip() {
        let sdp = "v=0\r\na=candidate:1 1 udp 2130706431 127.0.0.1 50000 typ host\r\na=end-of-candidates\r\n";
        let out = rewrite_candidate_ip(sdp, "10.0.2.2".parse().unwrap()).unwrap();
        assert_eq!(
            out,
            "v=0\r\na=candidate:1 1 udp 2130706431 10.0.2.2 50000 typ host\r\na=end-of-candidates\r\n"
        );
        assert_eq!(strip_candidates(&out), "v=0\r\n");
        assert!(rewrite_candidate_ip("v=0\r\n", "10.0.2.2".parse().unwrap()).is_err());
    }

    #[test]
    fn no_candidate_is_an_error() {
        assert!(rewrite_candidates("v=0\r\n", "127.0.0.1:1".parse().unwrap()).is_err());
    }
}
