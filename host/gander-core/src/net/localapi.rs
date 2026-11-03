// SPDX-License-Identifier: Apache-2.0
//! HTTP/1.1 framing for the Tailscale LocalAPI, independent of the transport (a named pipe on
//! Windows, a Unix socket elsewhere).
//!
//! Requests carry `Host: local-tailscaled.sock` and never `Origin` or `Referer` (tailscaled refuses
//! those). Only `GET` is used; only the stable `whois` and `status` calls plus `prefs` are made.

use std::net::SocketAddr;

use super::tailscale::TailscaleError;

/// `Host` header tailscaled expects on LocalAPI requests.
pub const LOCALAPI_HOST: &str = "local-tailscaled.sock";

/// Largest LocalAPI response accepted (status on a big tailnet is a few hundred KiB).
pub const MAX_RESPONSE: usize = 8 * 1024 * 1024;

/// Path of the WhoIs call for a TCP peer: `addr=<ip>:<port>` percent-encoded (WP-46 findings;
/// IPv6 as `[ip]:port`).
pub fn whois_path(addr: SocketAddr) -> String {
    format!(
        "/localapi/v0/whois?addr={}&proto=tcp",
        percent_encode(&addr.to_string())
    )
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Path of the status call (peers included).
pub const STATUS_PATH: &str = "/localapi/v0/status";

/// Path of the prefs call.
pub const PREFS_PATH: &str = "/localapi/v0/prefs";

/// The bytes of a `GET` request for `path`, with `Connection: close`.
pub fn build_get(path: &str) -> Vec<u8> {
    format!(
        "GET {path} HTTP/1.1\r\nHost: {LOCALAPI_HOST}\r\nUser-Agent: gander\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

/// A parsed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Decoded body.
    pub body: Vec<u8>,
}

/// Parses a complete response read until the server closed the connection. Handles
/// `Content-Length`, `Transfer-Encoding: chunked` and read-to-close bodies.
pub fn parse_response(raw: &[u8]) -> Result<Response, TailscaleError> {
    let bad = |m: &str| TailscaleError::Parse(format!("localapi http: {m}"));
    let head_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("no header terminator"))?;
    let head = std::str::from_utf8(&raw[..head_end]).map_err(|_| bad("header not utf-8"))?;
    let rest = &raw[head_end + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| bad("empty"))?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(bad("not HTTP/1.x"));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad("bad status"))?;
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.parse::<usize>().map_err(|_| bad("content-length"))?);
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    let body = if chunked {
        decode_chunked(rest).ok_or_else(|| bad("bad chunked body"))?
    } else if let Some(len) = content_length {
        if rest.len() < len {
            return Err(bad("truncated body"));
        }
        rest[..len].to_vec()
    } else {
        rest.to_vec()
    };
    Ok(Response { status, body })
}

fn decode_chunked(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&data[..line_end]).ok()?;
        let size_text = size_text.split(';').next()?.trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if data.len() < size + 2 || out.len() + size > MAX_RESPONSE {
            return None;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_shape() {
        let r = String::from_utf8(build_get(&whois_path(
            "[fd7a:115c:a1e0::1]:443".parse().unwrap(),
        )))
        .unwrap();
        assert!(r.starts_with(
            "GET /localapi/v0/whois?addr=%5Bfd7a%3A115c%3Aa1e0%3A%3A1%5D%3A443&proto=tcp HTTP/1.1\r\n"
        ));
        assert_eq!(
            whois_path("100.64.0.1:5000".parse().unwrap()),
            "/localapi/v0/whois?addr=100.64.0.1%3A5000&proto=tcp"
        );
        assert!(r.contains("\r\nHost: local-tailscaled.sock\r\n"));
        assert!(!r.to_ascii_lowercase().contains("origin"));
        assert!(!r.to_ascii_lowercase().contains("referer"));
        assert!(r.ends_with("\r\n\r\n"));
    }

    #[test]
    fn parses_content_length_chunked_and_close() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"{}"[..]));
        let r = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4;x=1\r\n\":1}\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(r.body, b"{\"a\":1}");
        let r = parse_response(b"HTTP/1.0 404 Not Found\r\n\r\nno match for IP:port").unwrap();
        assert_eq!(r.status, 404);
        assert_eq!(r.body, b"no match for IP:port");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_response(b"garbage").is_err());
        assert!(parse_response(b"SSH-2.0\r\n\r\n").is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}").is_err());
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err()
        );
    }
}
