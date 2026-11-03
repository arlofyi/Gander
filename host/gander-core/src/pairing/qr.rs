// SPDX-License-Identifier: Apache-2.0
//! The flow A QR payload:
//!
//! ```text
//! gander://pair?v=1&id=<host_id>&a=<addr1>,<addr2>&p=<port>&n=<host name>&s=<secret b64url>&m=<magicdns>
//! ```
//!
//! Every value is percent-encoded (RFC 3986 unreserved characters stay literal, everything else,
//! including `:` of IPv6 addresses and UTF-8 bytes of the host name, becomes `%XX`). The comma
//! between addresses is literal. `m` is omitted when there is no MagicDNS name. Unknown keys are
//! ignored by the parser so later versions can add fields.
//!
//! The payload carries the window secret: [`QrPayload`] redacts it in `Debug`, has no `Display`,
//! and the URI must never be logged (security non-negotiable 9).

use std::fmt;
use std::net::IpAddr;

use crate::protocol::{HostId, PairingSecret};

/// Scheme and path of the payload.
pub const QR_PREFIX: &str = "gander://pair?";

/// Payload version written by this build.
pub const QR_VERSION: u32 = 1;

/// The contents of the pairing QR code.
#[derive(Clone, PartialEq, Eq)]
pub struct QrPayload {
    /// Host id (pinned by the phone before it sends anything).
    pub host_id: HostId,
    /// Addresses to try, in order.
    pub addresses: Vec<IpAddr>,
    /// Listener port.
    pub port: u16,
    /// Computer name, for display.
    pub host_name: String,
    /// The window secret.
    pub secret: PairingSecret,
    /// MagicDNS name, if known.
    pub magic_dns: Option<String>,
}

impl fmt::Debug for QrPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QrPayload")
            .field("host_id", &self.host_id)
            .field("addresses", &self.addresses)
            .field("port", &self.port)
            .field("host_name", &self.host_name)
            .field("secret", &self.secret)
            .field("magic_dns", &self.magic_dns)
            .finish()
    }
}

/// Why a payload could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum QrError {
    /// Not a `gander://pair?` URI.
    #[error("not a gander pairing code")]
    NotGander,
    /// `v` is missing or not a version this build reads.
    #[error("unsupported pairing code version")]
    Version,
    /// A required field is missing or malformed (the field name only, never its value).
    #[error("pairing code field `{0}` is missing or malformed")]
    Field(&'static str),
}

impl QrPayload {
    /// The URI to encode in the QR code. Never log the result.
    pub fn to_uri(&self) -> String {
        let addrs: Vec<String> = self
            .addresses
            .iter()
            .map(|a| encode(&a.to_string()))
            .collect();
        let mut uri = format!(
            "{QR_PREFIX}v={QR_VERSION}&id={}&a={}&p={}&n={}&s={}",
            encode(self.host_id.as_str()),
            addrs.join(","),
            self.port,
            encode(&self.host_name),
            encode(self.secret.expose()),
        );
        if let Some(m) = &self.magic_dns {
            uri.push_str("&m=");
            uri.push_str(&encode(m));
        }
        uri
    }

    /// Parses a scanned URI.
    pub fn parse(uri: &str) -> Result<Self, QrError> {
        let query = uri.strip_prefix(QR_PREFIX).ok_or(QrError::NotGander)?;
        let (mut v, mut id, mut a, mut p, mut n, mut s, mut m) =
            (None, None, None, None, None, None, None);
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let slot = match key {
                "v" => &mut v,
                "id" => &mut id,
                "a" => &mut a,
                "p" => &mut p,
                "n" => &mut n,
                "s" => &mut s,
                "m" => &mut m,
                _ => continue,
            };
            *slot = Some(value);
        }
        if v.and_then(|v| v.parse::<u32>().ok()) != Some(QR_VERSION) {
            return Err(QrError::Version);
        }
        let field = |name: &'static str, raw: Option<&str>| -> Result<String, QrError> {
            decode(raw.ok_or(QrError::Field(name))?).ok_or(QrError::Field(name))
        };
        let host_id = field("id", id)?;
        if host_id.is_empty() {
            return Err(QrError::Field("id"));
        }
        let addresses = a
            .ok_or(QrError::Field("a"))?
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| decode(s).and_then(|d| d.parse::<IpAddr>().ok()))
            .collect::<Option<Vec<_>>>()
            .ok_or(QrError::Field("a"))?;
        if addresses.is_empty() {
            return Err(QrError::Field("a"));
        }
        let port = p
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p != 0)
            .ok_or(QrError::Field("p"))?;
        let secret = field("s", s)?;
        if super::b64url_fixed::<{ super::SECRET_LEN }>(&secret).is_none() {
            return Err(QrError::Field("s"));
        }
        let magic_dns = match m {
            None | Some("") => None,
            Some(raw) => Some(decode(raw).ok_or(QrError::Field("m"))?),
        };
        Ok(Self {
            host_id: HostId::from(host_id),
            addresses,
            port,
            host_name: field("n", n)?,
            secret: PairingSecret::new(secret),
            magic_dns,
        })
    }
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> QrPayload {
        QrPayload {
            host_id: HostId::from("uSb4Dq0KZ3fV8nXgT1oY2wL5rM9cE6pA7hJ0iN3sQ4k"),
            addresses: vec![
                "100.101.102.103".parse().unwrap(),
                "fd7a:115c:a1e0::1".parse().unwrap(),
            ],
            port: 42637,
            host_name: "Studio PC & Co (Lærke)".into(),
            // Not a real secret: a fixed placeholder of the right shape.
            secret: PairingSecret::new(super::super::b64url(&[0u8; 32])),
            magic_dns: Some("studio-pc.tail0000.ts.net".into()),
        }
    }

    #[test]
    fn roundtrip_and_shape() {
        let p = payload();
        let uri = p.to_uri();
        assert!(uri.starts_with("gander://pair?v=1&id=uSb4"));
        assert!(uri.contains("&a=100.101.102.103,fd7a%3A115c%3Aa1e0%3A%3A1&p=42637&"));
        assert!(uri.contains("&n=Studio%20PC%20%26%20Co%20%28L%C3%A6rke%29&"));
        assert!(uri.ends_with("&m=studio-pc.tail0000.ts.net"));
        assert_eq!(QrPayload::parse(&uri).unwrap(), p);
        let mut no_m = p.clone();
        no_m.magic_dns = None;
        assert!(!no_m.to_uri().contains("&m="));
        assert_eq!(QrPayload::parse(&no_m.to_uri()).unwrap(), no_m);
    }

    #[test]
    fn debug_redacts_the_secret() {
        let p = payload();
        let dbg = format!("{p:?}");
        assert!(dbg.contains("PairingSecret(<redacted>)"));
        assert!(!dbg.contains(p.secret.expose()));
    }

    #[test]
    fn malformed_payloads() {
        let uri = payload().to_uri();
        assert_eq!(QrPayload::parse("https://x"), Err(QrError::NotGander));
        assert_eq!(
            QrPayload::parse(&uri.replace("v=1", "v=2")),
            Err(QrError::Version)
        );
        assert_eq!(
            QrPayload::parse(&uri.replace("p=42637", "p=0")),
            Err(QrError::Field("p"))
        );
        assert_eq!(
            QrPayload::parse(&uri.replace("100.101.102.103", "999.1.1.1")),
            Err(QrError::Field("a"))
        );
        let short_secret = format!("{}&s=AAAA", uri.split("&s=").next().unwrap());
        assert_eq!(QrPayload::parse(&short_secret), Err(QrError::Field("s")));
        // Unknown keys are ignored.
        assert!(QrPayload::parse(&format!("{uri}&x=1")).is_ok());
    }
}
