// SPDX-License-Identifier: Apache-2.0
//! Identifier and secret newtypes shared by the protocol and the config files.
//!
//! Identifiers are opaque strings on the wire (`dev_01J...`, `shr_01J...`, `ses_01J...`). The
//! newtypes stop a share id being passed where a device id is expected; they do not validate the
//! format, because clients must treat ids as opaque.
//!
//! Secrets ([`BearerToken`], [`PairingSecret`]) have a redacting `Debug` and no `Display`, so they
//! cannot end up in a log line by accident (security non-negotiable "no secrets in logs").

use std::fmt;

use serde::{Deserialize, Serialize};

/// An RFC 3339 timestamp. Serialised as a string (`"2026-10-30T09:12:44Z"`).
pub type Timestamp = time::OffsetDateTime;

macro_rules! opaque_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// The id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

opaque_id!(
    /// Host id: base64url (no padding) SHA-256 of the host certificate's SubjectPublicKeyInfo.
    HostId
);
opaque_id!(
    /// Paired device id, `dev_` + ULID.
    DeviceId
);
opaque_id!(
    /// Share id, `shr_` + ULID.
    ShareId
);
opaque_id!(
    /// Stream session id, `ses_` + ULID.
    SessionId
);
opaque_id!(
    /// Pending pairing id returned by `POST /v1/pair`.
    PairingId
);
opaque_id!(
    /// App catalogue entry id, for example `powerpoint`.
    AppId
);

macro_rules! secret_string {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps a secret value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The secret value. Call sites must not log it.
            pub fn expose(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

secret_string!(
    /// Per-device bearer token: 32 random bytes, base64url without padding.
    ///
    /// Plaintext exists only in the `GET /v1/pair/{id}` and `POST /v1/token/rotate` responses and
    /// on the client; the host stores only its SHA-256 ([`crate::config::TokenHash`]).
    BearerToken
);
secret_string!(
    /// Pairing-window secret carried in the QR code: 32 random bytes, base64url.
    PairingSecret
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_in_debug() {
        let token = BearerToken::new("do-not-print-me");
        let secret = PairingSecret::new("nor-me");
        assert_eq!(format!("{token:?}"), "BearerToken(<redacted>)");
        assert_eq!(format!("{secret:?}"), "PairingSecret(<redacted>)");
        assert!(!format!("{:?}", Some(&token)).contains("do-not"));
    }

    #[test]
    fn ids_are_transparent_strings() {
        let id = ShareId::from("shr_01");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"shr_01\"");
    }
}
