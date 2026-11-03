// SPDX-License-Identifier: Apache-2.0
//! ICE socket addresses: the host's bound addresses, one UDP port each from the firewall range.
//!
//! The installer's firewall rule (WP-1A) allows inbound UDP only on `[firewall] udp_ports`
//! (default 50000-50099), so the engine never binds an ephemeral port: for every address it
//! probes the range (starting at a rotating offset, so concurrent sessions spread out) and hands
//! webrtc-rs the first free `ip:port`. webrtc-rs binds exactly that address (only wildcards are
//! expanded, and wildcards are refused here). After gathering the session checks that the
//! candidates carry those ports ([`candidates_within`]); a port taken in between is retried.
//!
//! When every port of the range is busy on an address, the session is refused with
//! [`EngineError::PortsExhausted`] (protocol code `stream_unavailable`); nothing falls back to an
//! ephemeral port.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU16, Ordering};

use gander_core::config::UdpPortRange;

use crate::error::EngineError;

static NEXT_OFFSET: AtomicU16 = AtomicU16::new(0);

/// One free `ip:port` per address, ports inside `range`. Refuses wildcards and an empty list.
pub fn pick_udp_addrs(ips: &[IpAddr], range: UdpPortRange) -> Result<Vec<SocketAddr>, EngineError> {
    if ips.is_empty() {
        return Err(EngineError::NoBindAddress);
    }
    let span = u32::from(range.end() - range.start()) + 1;
    let offset = u32::from(NEXT_OFFSET.fetch_add(1, Ordering::Relaxed)) % span;
    let mut out = Vec::with_capacity(ips.len());
    for &ip in ips {
        if ip.is_unspecified() {
            return Err(EngineError::Wildcard(ip));
        }
        let mut found = None;
        for k in 0..span {
            let port = range.start() + ((offset + k) % span) as u16;
            let addr = SocketAddr::new(ip, port);
            // Probe: a bind that succeeds proves the port is free now; the socket is closed
            // again so webrtc-rs can bind it.
            if UdpSocket::bind(addr).is_ok() {
                found = Some(addr);
                break;
            }
        }
        out.push(found.ok_or(EngineError::PortsExhausted { range, ip })?);
    }
    Ok(out)
}

/// The `ip:port` of every `a=candidate` line of an SDP (or of bare candidate strings).
pub fn candidate_addrs(sdp: &str) -> Vec<SocketAddr> {
    sdp.lines()
        .filter_map(|l| crate::sdp::parse_candidate(l).map(|c| c.addr))
        .collect()
}

/// True when every candidate in `sdp` is one of `bound` (and there is at least one).
pub fn candidates_within(sdp: &str, bound: &[SocketAddr]) -> bool {
    let c = candidate_addrs(sdp);
    !c.is_empty() && c.iter().all(|a| bound.contains(a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn lo() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    /// A range of `n` ports no other test in this process uses.
    fn range(base: u16, n: u16) -> UdpPortRange {
        UdpPortRange::new(base, base + n - 1).expect("valid range")
    }

    #[test]
    fn picks_inside_the_range_and_skips_busy_ports() {
        let r = range(52_710, 3);
        let _a = UdpSocket::bind((lo(), 52_710)).expect("bind");
        let _b = UdpSocket::bind((lo(), 52_711)).expect("bind");
        for _ in 0..5 {
            let got = pick_udp_addrs(&[lo()], r).expect("one port free");
            assert_eq!(got, vec![SocketAddr::new(lo(), 52_712)]);
        }
    }

    #[test]
    fn exhausted_range_is_refused_not_widened() {
        let r = range(52_720, 2);
        let _a = UdpSocket::bind((lo(), 52_720)).expect("bind");
        let _b = UdpSocket::bind((lo(), 52_721)).expect("bind");
        match pick_udp_addrs(&[lo()], r) {
            Err(EngineError::PortsExhausted { range, ip }) => {
                assert_eq!(range, r);
                assert_eq!(ip, lo());
            }
            other => panic!("expected PortsExhausted, got {other:?}"),
        }
    }

    #[test]
    fn wildcards_and_empty_lists_are_refused() {
        let r = range(52_730, 10);
        assert!(matches!(
            pick_udp_addrs(&["0.0.0.0".parse().unwrap()], r),
            Err(EngineError::Wildcard(_))
        ));
        assert!(matches!(
            pick_udp_addrs(&["::".parse().unwrap()], r),
            Err(EngineError::Wildcard(_))
        ));
        assert!(matches!(
            pick_udp_addrs(&[], r),
            Err(EngineError::NoBindAddress)
        ));
    }

    #[test]
    fn candidate_check() {
        let sdp = "v=0\r\na=candidate:1 1 udp 2130706431 127.0.0.1 50001 typ host\r\n";
        let bound = [SocketAddr::new(lo(), 50001)];
        assert!(candidates_within(sdp, &bound));
        assert!(!candidates_within(sdp, &[SocketAddr::new(lo(), 50002)]));
        assert!(!candidates_within("v=0\r\n", &bound));
    }
}
