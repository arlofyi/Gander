// SPDX-License-Identifier: Apache-2.0
//! Small SDP and candidate-line helpers.

use std::net::SocketAddr;

/// The parts of an ICE candidate line the engine looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `udp` or `tcp` (lower case).
    pub protocol: String,
    /// Connection address.
    pub addr: SocketAddr,
    /// `host`, `srflx`, `prflx` or `relay`.
    pub typ: String,
}

/// Parses `a=candidate:...`, `candidate:...` or the bare attribute value. `None` for anything
/// else, including mDNS (`.local`) addresses.
pub fn parse_candidate(line: &str) -> Option<Candidate> {
    let body = line.trim();
    let body = body.strip_prefix("a=").unwrap_or(body);
    let body = body.strip_prefix("candidate:").unwrap_or(body);
    let f: Vec<&str> = body.split_whitespace().collect();
    if f.len() < 8 || f[6] != "typ" {
        return None;
    }
    let ip: std::net::IpAddr = f[4].parse().ok()?;
    let port: u16 = f[5].parse().ok()?;
    Some(Candidate {
        protocol: f[2].to_ascii_lowercase(),
        addr: SocketAddr::new(ip, port),
        typ: f[7].to_ascii_lowercase(),
    })
}

/// The `a=candidate` lines of an SDP, without the `a=` prefix.
pub fn candidate_lines(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter_map(|l| l.trim_end().strip_prefix("a="))
        .filter(|l| l.starts_with("candidate:"))
        .map(str::to_owned)
        .collect()
}

/// The SDP without candidates that are not plain host candidates (`srflx`, `relay`, mDNS).
/// Host-only on both sides is a protocol rule; the phone's answer is filtered the same way.
pub fn keep_host_candidates(sdp: &str) -> String {
    sdp.split_inclusive('\n')
        .filter(|l| {
            let body = l.trim_end();
            !body.starts_with("a=candidate:")
                || parse_candidate(body).is_some_and(|c| c.typ == "host")
        })
        .collect()
}

/// The SDP without any candidate or end-of-candidates line.
pub fn strip_candidates(sdp: &str) -> String {
    sdp.split_inclusive('\n')
        .filter(|l| !l.starts_with("a=candidate:") && !l.starts_with("a=end-of-candidates"))
        .collect()
}

/// The `a=mid` of the first media section (BUNDLE tag), for trickled candidates.
pub fn first_mid(sdp: &str) -> Option<String> {
    sdp.lines()
        .find_map(|l| l.trim_end().strip_prefix("a=mid:"))
        .map(str::to_owned)
}

/// Replaces the address of every candidate line with `to` and returns the original address
/// (test relay; every candidate must name the same address).
pub fn rewrite_candidates(sdp: &str, to: SocketAddr) -> Option<(String, SocketAddr)> {
    let mut real: Option<SocketAddr> = None;
    let mut out = String::with_capacity(sdp.len());
    for line in sdp.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = body.strip_prefix("a=candidate:") {
            let mut f: Vec<String> = rest.split(' ').map(str::to_owned).collect();
            if f.len() < 6 {
                return None;
            }
            let addr: SocketAddr = format!("{}:{}", f[4], f[5]).parse().ok()?;
            if real.is_some_and(|r| r != addr) {
                return None;
            }
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
    Some((out, real?))
}

/// The payload types and encoding names of the video `a=rtpmap` lines.
pub fn video_codecs(sdp: &str) -> Vec<(u8, String)> {
    let mut in_video = false;
    let mut out = Vec::new();
    for l in sdp.lines() {
        if l.starts_with("m=") {
            in_video = l.starts_with("m=video") && !l.starts_with("m=video 0 ");
        } else if in_video && let Some(rest) = l.strip_prefix("a=rtpmap:") {
            let mut it = rest.split_whitespace();
            if let (Some(pt), Some(name)) = (it.next(), it.next())
                && let Ok(pt) = pt.parse()
            {
                out.push((pt, name.split('/').next().unwrap_or("").to_owned()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDP: &str = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=mid:0\r\na=rtpmap:102 H264/90000\r\n\
a=candidate:1 1 udp 2130706431 127.0.0.1 50000 typ host\r\n\
a=candidate:2 1 udp 1694498815 203.0.113.9 50000 typ srflx raddr 0.0.0.0 rport 0\r\n\
a=candidate:3 1 udp 2130706431 abc.local 50000 typ host\r\na=end-of-candidates\r\n";

    #[test]
    fn parses_and_filters_candidates() {
        let c = parse_candidate("candidate:1 1 UDP 2130706431 127.0.0.1 50000 typ host").unwrap();
        assert_eq!(c.protocol, "udp");
        assert_eq!(c.typ, "host");
        assert_eq!(c.addr, "127.0.0.1:50000".parse::<SocketAddr>().unwrap());
        assert!(parse_candidate("candidate:3 1 udp 1 abc.local 5 typ host").is_none());
        let kept = keep_host_candidates(SDP);
        assert_eq!(candidate_lines(&kept).len(), 1);
        assert!(kept.contains("a=end-of-candidates"));
        assert_eq!(candidate_lines(&strip_candidates(SDP)).len(), 0);
        assert_eq!(first_mid(SDP).as_deref(), Some("0"));
        assert_eq!(video_codecs(SDP), vec![(102, "H264".to_owned())]);
    }

    #[test]
    fn rewrites_one_address() {
        let one = "a=candidate:1 1 udp 2130706431 127.0.0.1 50000 typ host\r\n";
        let (out, real) = rewrite_candidates(one, "127.0.0.1:60000".parse().unwrap()).unwrap();
        assert_eq!(real, "127.0.0.1:50000".parse::<SocketAddr>().unwrap());
        assert!(out.contains("127.0.0.1 60000 typ host"));
    }
}
