// SPDX-License-Identifier: Apache-2.0
//! The short authentication string (SAS) shown on the host and on the phone during pairing.
//!
//! ```text
//! digest = SHA-256( "gander-sas-v1" || host_spki || device_nonce || host_nonce )
//! v20    = first 20 bits of digest, big-endian  = d[0] << 12 | d[1] << 4 | d[2] >> 4
//! SAS    = v20 mod 1_000_000, zero-padded to 6 decimal digits, shown "DDD DDD"
//! ```
//!
//! Byte-level decisions (the spec leaves them open; `protocol/test-vectors/sas.json` pins them for
//! every implementation, including the Android client):
//!
//! - `"gander-sas-v1"` is its 13 ASCII bytes, no terminator, no length prefix.
//! - `host_spki` is the **DER-encoded SubjectPublicKeyInfo** of the host certificate the client
//!   actually saw in the TLS handshake (the same bytes whose SHA-256 is the host id), not its
//!   hash. DER is self-delimiting, so the plain concatenation is unambiguous. In flow B a man in
//!   the middle presents a different certificate, so the phone hashes a different SPKI than the
//!   host does and the two codes differ.
//! - `device_nonce` and `host_nonce` are the raw 16 bytes (base64url-decoded), not their text.
//! - The spec says "first 20 bits ... zero-padded to 6 digits", but 20 bits reach 1 048 575, which
//!   has 7 digits. The value is therefore reduced modulo 1 000 000 (a 2^20 / 10^6 bias of under
//!   5 percent on the lowest 48 576 codes, irrelevant for a one-shot visual comparison).

use std::fmt;

use super::NONCE_LEN;

/// Domain-separation label hashed first.
pub const SAS_LABEL: &[u8] = b"gander-sas-v1";

/// A 6-digit short authentication string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sas(u32);

impl Sas {
    /// The numeric value, `0..1_000_000`.
    pub fn value(self) -> u32 {
        self.0
    }

    /// The six digits without the space, for example `"482913"`.
    pub fn digits(self) -> String {
        format!("{:06}", self.0)
    }
}

impl fmt::Display for Sas {
    /// The display form, `"482 913"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:03} {:03}", self.0 / 1000, self.0 % 1000)
    }
}

/// The SHA-256 the SAS is taken from (exposed for the test vectors).
pub fn sas_digest(
    host_spki_der: &[u8],
    device_nonce: &[u8; NONCE_LEN],
    host_nonce: &[u8; NONCE_LEN],
) -> [u8; 32] {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(SAS_LABEL);
    ctx.update(host_spki_der);
    ctx.update(device_nonce);
    ctx.update(host_nonce);
    let mut out = [0u8; 32];
    out.copy_from_slice(ctx.finish().as_ref());
    out
}

/// The first 20 bits of `digest`, big-endian.
pub fn first_20_bits(digest: &[u8; 32]) -> u32 {
    (u32::from(digest[0]) << 12) | (u32::from(digest[1]) << 4) | (u32::from(digest[2]) >> 4)
}

/// Derives the SAS from the host SPKI (DER) and both nonces (raw bytes).
pub fn derive_sas(
    host_spki_der: &[u8],
    device_nonce: &[u8; NONCE_LEN],
    host_nonce: &[u8; NONCE_LEN],
) -> Sas {
    Sas(first_20_bits(&sas_digest(host_spki_der, device_nonce, host_nonce)) % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_groups_three_and_three_with_zero_padding() {
        assert_eq!(Sas(482_913).to_string(), "482 913");
        assert_eq!(Sas(7).to_string(), "000 007");
        assert_eq!(Sas(7).digits(), "000007");
        assert_eq!(Sas(100_000).to_string(), "100 000");
    }

    #[test]
    fn first_20_bits_is_big_endian() {
        let mut d = [0u8; 32];
        d[0] = 0xAB;
        d[1] = 0xCD;
        d[2] = 0xEF;
        assert_eq!(first_20_bits(&d), 0xABCDE);
        d = [0xFF; 32];
        assert_eq!(first_20_bits(&d), 0xFFFFF);
        assert_eq!(derive_sas_value_of(&d), 48_575);
    }

    fn derive_sas_value_of(d: &[u8; 32]) -> u32 {
        first_20_bits(d) % 1_000_000
    }

    #[test]
    fn every_input_changes_the_code_space() {
        let spki = b"\x30\x03\x02\x01\x01";
        let a = [1u8; 16];
        let b = [2u8; 16];
        let base = sas_digest(spki, &a, &b);
        assert_ne!(base, sas_digest(b"\x30\x03\x02\x01\x02", &a, &b));
        assert_ne!(base, sas_digest(spki, &b, &b));
        assert_ne!(base, sas_digest(spki, &a, &a));
        // Swapping the nonces changes the digest (order is fixed: device then host).
        assert_ne!(base, sas_digest(spki, &b, &a));
    }
}
