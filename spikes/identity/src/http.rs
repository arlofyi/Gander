// SPDX-License-Identifier: Apache-2.0
//! Minimal HTTP/1.x client codec, just enough to speak to tailscaled's LocalAPI over a stream
//! (a Windows named pipe here). Parses incrementally so a response is complete as soon as its
//! `Content-Length` or final chunk has arrived, without relying on the server closing the pipe.

use anyhow::{Context, Result, bail};
use std::io::Read;

pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Build a `GET` request. `host: None` produces an HTTP/1.0 request with no `Host` header.
pub fn build_get(target: &str, host: Option<&str>, extra: &[(&str, &str)]) -> Vec<u8> {
    let version = if host.is_some() { "1.1" } else { "1.0" };
    let mut s = format!("GET {target} HTTP/{version}\r\n");
    if let Some(h) = host {
        s.push_str(&format!("Host: {h}\r\n"));
    }
    for (k, v) in extra {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    s.push_str("Accept: application/json\r\nConnection: close\r\n\r\n");
    s.into_bytes()
}

/// Read until a full response is available (or EOF), then parse it.
pub fn read_response(mut r: impl Read) -> Result<Response> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = r.read(&mut chunk).context("read from stream")?;
        if n == 0 {
            return match parse(&raw, true)? {
                Some(resp) => Ok(resp),
                None => bail!(
                    "stream closed before a complete response ({} bytes)",
                    raw.len()
                ),
            };
        }
        raw.extend_from_slice(&chunk[..n]);
        if raw.len() > MAX_RESPONSE_BYTES {
            bail!("response larger than {MAX_RESPONSE_BYTES} bytes");
        }
        if let Some(resp) = parse(&raw, false)? {
            return Ok(resp);
        }
    }
}

/// Parse `raw`. `Ok(None)` means "incomplete, read more". `eof` lets a response with no length
/// information end at the end of the stream.
pub fn parse(raw: &[u8], eof: bool) -> Result<Option<Response>> {
    let Some(head_end) = find(raw, b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = std::str::from_utf8(&raw[..head_end]).context("response head is not UTF-8")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let proto = parts.next().unwrap_or_default();
    if !proto.starts_with("HTTP/1.") {
        bail!("not an HTTP/1.x status line");
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .context("status code is not a number")?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let body = &raw[head_end + 4..];
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };

    let body = if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        match dechunk(body)? {
            Some(b) => b,
            None if eof => bail!("chunked body truncated"),
            None => return Ok(None),
        }
    } else if let Some(len) = header("content-length") {
        let len: usize = len.parse().context("bad Content-Length")?;
        if body.len() >= len {
            body[..len].to_vec()
        } else if eof {
            bail!("body truncated: {} of {len} bytes", body.len());
        } else {
            return Ok(None);
        }
    } else if eof {
        body.to_vec()
    } else {
        return Ok(None);
    };
    Ok(Some(Response { status, body }))
}

/// Decode a chunked body. `Ok(None)` if the final chunk has not arrived yet.
fn dechunk(mut body: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        let Some(eol) = find(body, b"\r\n") else {
            return Ok(None);
        };
        let size_line = std::str::from_utf8(&body[..eol]).context("chunk size is not UTF-8")?;
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16).context("bad chunk size")?;
        body = &body[eol + 2..];
        if size == 0 {
            // Trailers (if any) end with a blank line; no trailers means a bare CRLF.
            return Ok(
                (body.starts_with(b"\r\n") || find(body, b"\r\n\r\n").is_some()).then_some(out),
            );
        }
        if body.len() < size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_host_and_close() {
        let r = String::from_utf8(build_get(
            "/localapi/v0/status",
            Some("local-tailscaled.sock"),
            &[],
        ))
        .unwrap();
        assert!(
            r.starts_with("GET /localapi/v0/status HTTP/1.1\r\nHost: local-tailscaled.sock\r\n")
        );
        assert!(r.contains("Connection: close\r\n"));
        assert!(r.ends_with("\r\n\r\n"));
    }

    #[test]
    fn no_host_uses_http10() {
        let r = String::from_utf8(build_get("/x", None, &[])).unwrap();
        assert!(r.starts_with("GET /x HTTP/1.0\r\n"));
        assert!(!r.to_ascii_lowercase().contains("host:"));
    }

    #[test]
    fn content_length_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let r = parse(raw, false).unwrap().unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"{\"a\":1}");
    }

    #[test]
    fn incomplete_then_complete() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\"";
        assert!(parse(raw, false).unwrap().is_none());
        assert!(parse(raw, true).is_err());
    }

    #[test]
    fn chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n4\r\ndefg\r\n0\r\n\r\n";
        let r = parse(raw, false).unwrap().unwrap();
        assert_eq!(r.body, b"abcdefg");
        let partial = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n4\r\nde";
        assert!(parse(partial, false).unwrap().is_none());
    }

    #[test]
    fn error_status_and_eof_delimited_body() {
        let raw =
            b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\n\r\ninvalid localapi request";
        assert!(parse(raw, false).unwrap().is_none());
        let r = parse(raw, true).unwrap().unwrap();
        assert_eq!(r.status, 403);
        assert_eq!(r.body, b"invalid localapi request");
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse(b"SSH-2.0-x\r\n\r\n", true).is_err());
    }

    #[test]
    fn read_response_over_a_reader() {
        let raw: &[u8] = b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\nno";
        let r = read_response(raw).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (404, &b"no"[..]));
    }
}
