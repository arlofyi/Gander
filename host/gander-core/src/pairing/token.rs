// SPDX-License-Identifier: Apache-2.0
//! Bearer tokens: hashing, constant-time comparison and the lookup over `devices.toml`.
//!
//! A token is 32 random bytes from the OS CSPRNG, base64url without padding (43 characters). The
//! host stores only SHA-256 of the token's **text form** (the exact bytes the client sends after
//! `Bearer `), as [`TokenHash`]. Lookup is by hash with a constant-time comparison against every
//! record (no early exit), so the time taken does not depend on which record matched or on how
//! many bytes of a guess were right; an unknown token costs one hash plus one pass over the table.

use crate::config::{DeviceRecord, DevicesFile, TokenHash};
use crate::protocol::BearerToken;

use super::{SECRET_LEN, b64url};

/// Longest `Authorization` token text examined; anything longer is refused unhashed.
pub const MAX_TOKEN_TEXT: usize = 256;

/// Builds a token from 32 CSPRNG bytes.
pub fn token_from_bytes(bytes: &[u8; SECRET_LEN]) -> BearerToken {
    BearerToken::new(b64url(bytes))
}

/// SHA-256 of the token text.
pub fn hash_token_text(text: &str) -> TokenHash {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    TokenHash(out)
}

/// SHA-256 of a token.
pub fn hash_token(token: &BearerToken) -> TokenHash {
    hash_token_text(token.expose())
}

/// Constant-time equality of two byte strings of equal length (`false`, early, when the lengths
/// differ: lengths are public here, both sides are fixed-size hashes or secrets).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// Whether `token` hashes to `hash`, in constant time.
pub fn verify_token(token: &BearerToken, hash: &TokenHash) -> bool {
    ct_eq(hash_token(token).as_bytes(), hash.as_bytes())
}

/// Finds the record (active or revoked) whose hash equals `hash`. Every record is compared.
pub fn find_by_hash<'a>(devices: &'a DevicesFile, hash: &TokenHash) -> Option<&'a DeviceRecord> {
    let mut found = None;
    for record in &devices.devices {
        if ct_eq(record.token_hash.as_bytes(), hash.as_bytes()) && found.is_none() {
            found = Some(record);
        }
    }
    found
}

/// Extracts the token text from an `Authorization` header value.
///
/// `Ok(None)`: no bearer credentials at all; `Err(())`: present but malformed (wrong scheme,
/// empty, too long, characters outside base64url).
#[allow(clippy::result_unit_err)]
pub fn parse_bearer(header: Option<&str>) -> Result<Option<&str>, ()> {
    let Some(value) = header else {
        return Ok(None);
    };
    let value = value.trim();
    let (scheme, rest) = value.split_once(' ').ok_or(())?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(());
    }
    let token = rest.trim();
    if token.is_empty()
        || token.len() > MAX_TOKEN_TEXT
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(());
    }
    Ok(Some(token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeviceRecord;

    /// Per-run token bytes: tests never embed token material.
    fn fresh_bytes(seed: u8) -> [u8; 32] {
        use std::hash::{BuildHasher, Hasher};
        let mut out = [0u8; 32];
        for (i, chunk) in out.chunks_mut(8).enumerate() {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u8(seed);
            h.write_usize(i);
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
        out
    }

    fn record(id: &str, token: &BearerToken) -> DeviceRecord {
        DeviceRecord {
            id: id.into(),
            name: "Pixel 9".into(),
            platform: "android".into(),
            model: "Pixel 9".into(),
            app_version: "0.1.0".into(),
            token_hash: hash_token(token),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            last_seen_at: None,
            last_address: None,
            network_identity: None,
            revoked_at: None,
        }
    }

    #[test]
    fn token_shape_and_hash() {
        let t = token_from_bytes(&fresh_bytes(1));
        assert_eq!(t.expose().len(), 43);
        let h = hash_token(&t);
        // SHA-256 of the text form, cross-checked with ring directly.
        let direct = ring::digest::digest(&ring::digest::SHA256, t.expose().as_bytes());
        assert_eq!(h.as_bytes().as_slice(), direct.as_ref());
        assert!(verify_token(&t, &h));
        let other = token_from_bytes(&fresh_bytes(2));
        assert!(!verify_token(&other, &h));
    }

    #[test]
    fn ct_eq_semantics() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        let a = [0u8; 32];
        let mut b = a;
        for i in 0..32 {
            b[i] = 1;
            assert!(!ct_eq(&a, &b), "differs at {i}");
            b[i] = 0;
        }
    }

    #[test]
    fn lookup_finds_active_and_revoked_and_nothing_else() {
        let t1 = token_from_bytes(&fresh_bytes(1));
        let t2 = token_from_bytes(&fresh_bytes(2));
        let t3 = token_from_bytes(&fresh_bytes(3));
        let mut file = DevicesFile::default();
        file.devices.push(record("dev_a", &t1));
        let mut revoked = record("dev_b", &t2);
        revoked.revoked_at = Some(time::OffsetDateTime::UNIX_EPOCH);
        file.devices.push(revoked);
        assert_eq!(
            find_by_hash(&file, &hash_token(&t1)).unwrap().id.as_str(),
            "dev_a"
        );
        assert!(find_by_hash(&file, &hash_token(&t2)).unwrap().is_revoked());
        assert!(find_by_hash(&file, &hash_token(&t3)).is_none());
    }

    #[test]
    fn bearer_header_parsing() {
        assert_eq!(parse_bearer(None), Ok(None));
        assert_eq!(parse_bearer(Some("Bearer abc-_9")), Ok(Some("abc-_9")));
        assert_eq!(parse_bearer(Some("bearer  abc ")), Ok(Some("abc")));
        assert_eq!(parse_bearer(Some("Basic abc")), Err(()));
        assert_eq!(parse_bearer(Some("Bearer")), Err(()));
        assert_eq!(parse_bearer(Some("Bearer ")), Err(()));
        assert_eq!(parse_bearer(Some("Bearer a+b/c=")), Err(()));
        assert_eq!(
            parse_bearer(Some(&format!("Bearer {}", "a".repeat(257)))),
            Err(())
        );
    }
}
