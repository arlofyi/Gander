// SPDX-License-Identifier: Apache-2.0
//! `devices.toml`: paired device records (pairing spec, "Tokens").
//!
//! Only SHA-256(token) is stored, never the token. Revoked records are kept (with `revoked_at`)
//! for 30 days as an audit trail.
//!
//! ```toml
//! version = 1
//!
//! [[device]]
//! id = "dev_01JC0000000000000000000000"
//! name = "Pixel 9"
//! platform = "android"
//! model = "Pixel 9"
//! app_version = "0.1.0"
//! token_hash = "<64 lower-case hex digits>"
//! created_at = "2026-11-02T20:11:03Z"
//! last_seen_at = "2026-11-02T21:00:00Z"
//! last_address = "100.101.102.104"
//!
//! [device.network_identity]
//! login = "alice@example.com"
//! node_id = "nXXXXXX"
//! node_name = "pixel-9"
//! ```

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{ConfigError, ConfigFile, default_version, find_duplicate};
use crate::protocol::{DeviceId, Timestamp};

const FILE: &str = "devices.toml";

/// SHA-256 of a bearer token, stored as 64 lower-case hex digits.
///
/// Computing the hash and the constant-time comparison belong to the pairing package; this type
/// only carries the 32 bytes. `Debug` shows a short prefix only.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenHash(pub [u8; 32]);

/// A token hash string that is not 64 hex digits.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("token_hash must be 64 hex digits")]
pub struct TokenHashError;

impl TokenHash {
    /// The raw hash bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lower-case hex form.
    pub fn to_hex(&self) -> String {
        use fmt::Write as _;
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TokenHash({}...)", &self.to_hex()[..8])
    }
}

impl FromStr for TokenHash {
    type Err = TokenHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(TokenHashError);
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16).ok_or(TokenHashError)?;
            let lo = (pair[1] as char).to_digit(16).ok_or(TokenHashError)?;
            // Both digits are < 16, so the value fits a byte.
            out[i] = u8::try_from(hi * 16 + lo).map_err(|_| TokenHashError)?;
        }
        Ok(TokenHash(out))
    }
}

impl Serialize for TokenHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for TokenHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Contents of `devices.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevicesFile {
    /// Schema version, always 1.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Device records, active and revoked.
    #[serde(default, rename = "device", skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DeviceRecord>,
}

impl Default for DevicesFile {
    fn default() -> Self {
        Self {
            version: super::CONFIG_VERSION,
            devices: Vec::new(),
        }
    }
}

/// One paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRecord {
    /// Device id (`dev_` + ULID).
    pub id: DeviceId,
    /// Name from pairing.
    pub name: String,
    /// Platform from pairing.
    pub platform: String,
    /// Model from pairing.
    pub model: String,
    /// Client version at pairing (or last seen).
    pub app_version: String,
    /// SHA-256 of the current token.
    pub token_hash: TokenHash,
    /// When the owner approved the device.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: Timestamp,
    /// Last authenticated request.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_seen_at: Option<Timestamp>,
    /// Source address of the last request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_address: Option<IpAddr>,
    /// Private-network identity recorded at approval (Tailscale adapter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_identity: Option<RecordedIdentity>,
    /// When the device was revoked; revoked records are kept 30 days.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub revoked_at: Option<Timestamp>,
}

/// Network identity captured at approval time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedIdentity {
    /// Login, compared on every request (mismatch -> 403 `network_identity_mismatch`).
    pub login: String,
    /// Node id; mismatch is a warning unless `strict_node_binding`.
    pub node_id: String,
    /// Node name, for display.
    pub node_name: String,
}

impl DeviceRecord {
    /// Whether the device has been revoked.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

impl DevicesFile {
    /// Devices that are not revoked.
    pub fn active(&self) -> impl Iterator<Item = &DeviceRecord> {
        self.devices.iter().filter(|d| !d.is_revoked())
    }

    /// Looks up a device by id (revoked or not).
    pub fn get(&self, id: &DeviceId) -> Option<&DeviceRecord> {
        self.devices.iter().find(|d| &d.id == id)
    }

    /// Mutable lookup by id.
    pub fn get_mut(&mut self, id: &DeviceId) -> Option<&mut DeviceRecord> {
        self.devices.iter_mut().find(|d| &d.id == id)
    }
}

impl ConfigFile for DevicesFile {
    const FILE_NAME: &'static str = FILE;

    fn version(&self) -> u32 {
        self.version
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if let Some(dup) = find_duplicate(self.devices.iter().map(|d| &d.id)) {
            return Err(ConfigError::invalid(
                FILE,
                format!("duplicate device id {dup}"),
            ));
        }
        if find_duplicate(self.devices.iter().map(|d| &d.token_hash)).is_some() {
            // The hash itself is not printed.
            return Err(ConfigError::invalid(FILE, "two devices share a token hash"));
        }
        if self.devices.iter().any(|d| d.id.as_str().is_empty()) {
            return Err(ConfigError::invalid(FILE, "device id must not be empty"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A per-run, non-secret fake hash: test fixtures never carry real token material.
    fn fake_hash(seed: u8) -> TokenHash {
        use std::hash::{BuildHasher, Hasher};
        let mut out = [0u8; 32];
        for (i, chunk) in out.chunks_mut(8).enumerate() {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u8(seed);
            h.write_usize(i);
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
        TokenHash(out)
    }

    fn record(id: &str, seed: u8) -> DeviceRecord {
        DeviceRecord {
            id: id.into(),
            name: "Pixel 9".into(),
            platform: "android".into(),
            model: "Pixel 9".into(),
            app_version: "0.1.0".into(),
            token_hash: fake_hash(seed),
            created_at: time::OffsetDateTime::from_unix_timestamp(1_793_000_000).unwrap(),
            last_seen_at: None,
            last_address: Some("100.101.102.104".parse().unwrap()),
            network_identity: Some(RecordedIdentity {
                login: "alice@example.com".into(),
                node_id: "n1".into(),
                node_name: "pixel-9".into(),
            }),
            revoked_at: None,
        }
    }

    #[test]
    fn roundtrips_through_toml() {
        let mut file = DevicesFile::default();
        file.devices.push(record("dev_a", 1));
        let mut revoked = record("dev_b", 2);
        revoked.revoked_at =
            Some(time::OffsetDateTime::from_unix_timestamp(1_793_100_000).unwrap());
        file.devices.push(revoked);
        let text = file.to_toml_string().unwrap();
        assert!(text.contains("[[device]]"));
        let back = DevicesFile::from_toml_str(&text).unwrap();
        assert_eq!(back, file);
        assert_eq!(back.active().count(), 1);
        assert!(back.get(&"dev_b".into()).unwrap().is_revoked());
    }

    #[test]
    fn token_hash_hex_and_debug() {
        let h = fake_hash(7);
        let hex = h.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(hex.parse::<TokenHash>().unwrap(), h);
        assert_eq!(hex.to_uppercase().parse::<TokenHash>().unwrap(), h);
        assert!(!format!("{h:?}").contains(&hex));
        assert!("abc".parse::<TokenHash>().is_err());
        assert!("zz".repeat(32).parse::<TokenHash>().is_err());
    }

    #[test]
    fn duplicates_are_refused() {
        let mut file = DevicesFile::default();
        file.devices.push(record("dev_a", 1));
        file.devices.push(record("dev_a", 2));
        assert!(matches!(file.validate(), Err(ConfigError::Invalid { .. })));
        file.devices[1].id = "dev_b".into();
        file.devices[1].token_hash = file.devices[0].token_hash.clone();
        let err = file.validate().unwrap_err().to_string();
        assert!(!err.contains(&file.devices[0].token_hash.to_hex()));
    }
}
