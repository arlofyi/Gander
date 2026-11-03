// SPDX-License-Identifier: Apache-2.0
//! Query strings, decoded exactly once.
//!
//! The path-resolution spec requires the protocol path to reach the resolver after exactly ONE
//! percent-decoding. A form decoder (`serde_urlencoded`) would also turn `+` into a space, so the
//! routes parse the raw query here: split on `&` and the first `=`, percent-decode each part
//! once, keep values as bytes (the resolver itself reports invalid UTF-8 as `bad_path`). `+` is a
//! literal plus sign; clients encode spaces as `%20`. Unknown parameters are ignored (protocol
//! compatibility rules); for a repeated parameter the first occurrence wins.

use gander_core::error::ErrorCode;

use crate::server::ApiError;

/// A parsed query string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawQuery {
    pairs: Vec<(Vec<u8>, Vec<u8>)>,
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes `s` once. `None` for a malformed escape (`%`, `%4`, `%zz`).
pub fn percent_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let hi = hex(*s.get(i + 1)?)?;
            let lo = hex(*s.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

fn bad(message: &str) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, message)
}

impl RawQuery {
    /// Parses the raw query (without `?`). A malformed percent escape is `bad_request`.
    pub fn parse(query: Option<&str>) -> Result<Self, ApiError> {
        let mut pairs = Vec::new();
        for part in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            let k = percent_decode(k.as_bytes())
                .ok_or_else(|| bad("The query string has a malformed percent escape."))?;
            let v = percent_decode(v.as_bytes())
                .ok_or_else(|| bad("The query string has a malformed percent escape."))?;
            pairs.push((k, v));
        }
        Ok(Self { pairs })
    }

    /// The decoded bytes of `key`, if present.
    pub fn bytes(&self, key: &str) -> Option<&[u8]> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key.as_bytes())
            .map(|(_, v)| v.as_slice())
    }

    /// `path` as bytes (empty when absent: the share root).
    pub fn path(&self) -> &[u8] {
        self.bytes("path").unwrap_or_default()
    }

    /// The value of `key` as UTF-8; `bad_request` when it is not.
    pub fn str(&self, key: &str) -> Result<Option<&str>, ApiError> {
        self.bytes(key)
            .map(|v| std::str::from_utf8(v).map_err(|_| bad("A query parameter is not UTF-8.")))
            .transpose()
    }

    /// The value of `key` as a non-empty string (an empty value counts as absent).
    pub fn non_empty(&self, key: &str) -> Result<Option<&str>, ApiError> {
        Ok(self.str(key)?.filter(|s| !s.is_empty()))
    }

    /// The value of `key` as an unsigned number; `bad_request` when it is not one.
    pub fn number<T: std::str::FromStr>(&self, key: &str) -> Result<Option<T>, ApiError> {
        self.non_empty(key)?
            .map(|s| {
                s.parse::<T>()
                    .map_err(|_| bad("A numeric query parameter is not a number."))
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_once_and_keeps_plus() {
        let q = RawQuery::parse(Some("path=a%2520b%2Fc+d&limit=5&x")).unwrap();
        assert_eq!(q.path(), b"a%20b/c+d");
        assert_eq!(q.number::<u32>("limit").unwrap(), Some(5));
        assert_eq!(q.bytes("x"), Some(&b""[..]));
        assert_eq!(q.bytes("missing"), None);
    }

    #[test]
    fn invalid_utf8_reaches_the_caller_as_bytes() {
        let q = RawQuery::parse(Some("path=%FF%FE&sort=%FF")).unwrap();
        assert_eq!(q.path(), [0xFF, 0xFE]);
        assert!(q.str("sort").is_err());
    }

    #[test]
    fn malformed_escapes_are_refused() {
        for bad in ["path=%", "path=%4", "path=%zz", "%G1=x"] {
            assert!(RawQuery::parse(Some(bad)).is_err(), "{bad}");
        }
        assert!(
            RawQuery::parse(Some("limit=abc"))
                .unwrap()
                .number::<u32>("limit")
                .is_err()
        );
    }

    #[test]
    fn first_occurrence_wins() {
        let q = RawQuery::parse(Some("path=a&path=b")).unwrap();
        assert_eq!(q.path(), b"a");
    }
}
