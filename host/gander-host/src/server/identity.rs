// SPDX-License-Identifier: Apache-2.0
//! Host identity: an ECDSA P-256 key pair and a self-signed certificate.
//!
//! - The **host id** is the base64url (no padding) SHA-256 of the certificate's
//!   SubjectPublicKeyInfo. Clients pin it; the system trust store is never consulted.
//! - The private key (PKCS#8) is stored in `<dir>/host.key` (`%LOCALAPPDATA%\Gander` in the
//!   application). On Windows the bytes are DPAPI-protected for the current user and the file is
//!   written with an owner-only DACL (`gander_win::config_store`); on Unix with mode 0600.
//! - The certificate is not stored: it is re-issued from the key at every start (CN = host id,
//!   10-year validity). The SPKI, and so the pin, depends only on the key.
//! - Key rotation is unsupported in v1 (a new key means re-pairing).
//!
//! File format of `host.key`: `GANDERK1` magic, one scheme byte (`1` = DPAPI, `0` = plain), then
//! the payload.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use gander_core::protocol::HostId;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PublicKeyData,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// File name of the key inside the identity directory.
pub const KEY_FILE: &str = "host.key";

const MAGIC: &[u8; 8] = b"GANDERK1";
const SCHEME_PLAIN: u8 = 0;
const SCHEME_DPAPI: u8 = 1;

/// Certificate validity.
const VALIDITY_DAYS: i64 = 3653;

/// Errors loading or creating the identity.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityError {
    /// Reading or writing the key file failed.
    #[error("host key file {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// The key file is not in the expected format, or cannot be decrypted by this user.
    #[error("host key file {0} is corrupt or belongs to another user")]
    Corrupt(PathBuf),
    /// Key generation or certificate creation failed.
    #[error("certificate: {0}")]
    Certificate(String),
}

/// The host's key pair and certificate.
pub struct HostIdentity {
    key_pkcs8: Vec<u8>,
    cert: CertificateDer<'static>,
    spki: Vec<u8>,
    host_id: HostId,
}

impl std::fmt::Debug for HostIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key.
        f.debug_struct("HostIdentity")
            .field("host_id", &self.host_id)
            .finish_non_exhaustive()
    }
}

impl Clone for HostIdentity {
    fn clone(&self) -> Self {
        Self {
            key_pkcs8: self.key_pkcs8.clone(),
            cert: self.cert.clone(),
            spki: self.spki.clone(),
            host_id: self.host_id.clone(),
        }
    }
}

/// Base64url SHA-256 of an SPKI: the host id.
pub fn host_id_for_spki(spki_der: &[u8]) -> HostId {
    let digest = ring::digest::digest(&ring::digest::SHA256, spki_der);
    HostId(URL_SAFE_NO_PAD.encode(digest.as_ref()))
}

/// The host id a certificate pins to, or `None` when the DER is not a certificate.
pub fn host_id_for_cert(cert_der: &[u8]) -> Option<HostId> {
    spki_from_cert(cert_der).map(host_id_for_spki)
}

/// Extracts the SubjectPublicKeyInfo element (tag and length included) from an X.509 DER
/// certificate. A minimal DER walk: Certificate -> TBSCertificate -> skip version, serial,
/// signature, issuer, validity, subject -> SPKI.
pub fn spki_from_cert(der: &[u8]) -> Option<&[u8]> {
    let cert = Der::parse(der).filter(|e| e.tag == 0x30)?;
    let tbs = Der::parse(cert.contents).filter(|e| e.tag == 0x30)?;
    let mut rest = tbs.contents;
    // Optional [0] version.
    if rest.first() == Some(&0xa0) {
        rest = Der::parse(rest)?.rest;
    }
    // serial, signature, issuer, validity, subject
    for _ in 0..5 {
        rest = Der::parse(rest)?.rest;
    }
    let spki = Der::parse(rest).filter(|e| e.tag == 0x30)?;
    Some(spki.whole)
}

/// One DER element: tag, the whole encoding, its contents and what follows it.
struct Der<'a> {
    tag: u8,
    whole: &'a [u8],
    contents: &'a [u8],
    rest: &'a [u8],
}

impl<'a> Der<'a> {
    fn parse(input: &'a [u8]) -> Option<Self> {
        let tag = *input.first()?;
        let first = *input.get(1)?;
        let (len, header) = if first < 0x80 {
            (usize::from(first), 2)
        } else {
            let n = usize::from(first & 0x7f);
            if n == 0 || n > 4 {
                return None;
            }
            let mut len = 0usize;
            for i in 0..n {
                len = (len << 8) | usize::from(*input.get(2 + i)?);
            }
            (len, 2 + n)
        };
        let end = header.checked_add(len)?;
        let whole = input.get(..end)?;
        Some(Self {
            tag,
            whole,
            contents: &whole[header..],
            rest: &input[end..],
        })
    }
}

impl HostIdentity {
    /// Generates a fresh identity (not stored).
    pub fn generate() -> Result<Self, IdentityError> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|e| IdentityError::Certificate(e.to_string()))?;
        Self::from_key_pair(&key)
    }

    fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, IdentityError> {
        let der = PrivatePkcs8KeyDer::from(pkcs8.to_vec());
        let key = KeyPair::from_pkcs8_der_and_sign_algo(&der, &PKCS_ECDSA_P256_SHA256)
            .map_err(|e| IdentityError::Certificate(e.to_string()))?;
        Self::from_key_pair(&key)
    }

    fn from_key_pair(key: &KeyPair) -> Result<Self, IdentityError> {
        let spki = key.subject_public_key_info();
        let host_id = host_id_for_spki(&spki);
        let mut params = CertificateParams::new(Vec::<String>::new())
            .map_err(|e| IdentityError::Certificate(e.to_string()))?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host_id.as_str());
        params.distinguished_name = dn;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(VALIDITY_DAYS);
        let cert = params
            .self_signed(key)
            .map_err(|e| IdentityError::Certificate(e.to_string()))?;
        Ok(Self {
            key_pkcs8: key.serialize_der(),
            cert: cert.der().clone(),
            spki,
            host_id,
        })
    }

    /// Loads `<dir>/host.key`, or generates and stores a new key when the file does not exist.
    /// A file that exists but cannot be read or decrypted is an error, never silently replaced
    /// (that would change the host id and break every pairing).
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        let path = dir.join(KEY_FILE);
        let io = |source| IdentityError::Io {
            path: path.clone(),
            source,
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                let pkcs8 =
                    decode_key_file(&bytes).ok_or_else(|| IdentityError::Corrupt(path.clone()))?;
                Self::from_pkcs8(&pkcs8).map_err(|_| IdentityError::Corrupt(path.clone()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Self::generate()?;
                let file = encode_key_file(&id.key_pkcs8).map_err(io)?;
                write_owner_only(&path, &file).map_err(io)?;
                Ok(id)
            }
            Err(e) => Err(io(e)),
        }
    }

    /// The host id clients pin.
    pub fn host_id(&self) -> &HostId {
        &self.host_id
    }

    /// The certificate (DER).
    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    /// The SubjectPublicKeyInfo (DER).
    pub fn spki_der(&self) -> &[u8] {
        &self.spki
    }

    /// The private key for rustls. Handle with care; never log.
    pub fn private_key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_pkcs8.clone()))
    }
}

fn encode_key_file(pkcs8: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = MAGIC.to_vec();
    #[cfg(windows)]
    {
        out.push(SCHEME_DPAPI);
        out.extend(gander_win::dpapi::protect(pkcs8)?);
    }
    #[cfg(not(windows))]
    {
        out.push(SCHEME_PLAIN);
        out.extend_from_slice(pkcs8);
    }
    Ok(out)
}

fn decode_key_file(bytes: &[u8]) -> Option<Vec<u8>> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let (&scheme, payload) = rest.split_first()?;
    match scheme {
        SCHEME_PLAIN => Some(payload.to_vec()),
        #[cfg(windows)]
        SCHEME_DPAPI => gander_win::dpapi::unprotect(payload).ok(),
        _ => None,
    }
}

#[cfg(windows)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use gander_core::config::SecureFileStore as _;
    gander_win::config_store::WindowsConfigStore.write_owner_only(path, bytes)
}

#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(any(windows, unix)))]
fn write_owner_only(_path: &Path, _bytes: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "no owner-only file store on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("gander-id-test-{}", ulid::Ulid::new()))
    }

    #[test]
    fn spki_walk_matches_key_and_id_is_b64url_sha256() {
        let id = HostIdentity::generate().unwrap();
        let spki = spki_from_cert(id.cert_der()).unwrap();
        assert_eq!(spki, id.spki_der());
        assert_eq!(host_id_for_cert(id.cert_der()).as_ref(), Some(id.host_id()));
        assert_eq!(id.host_id().as_str().len(), 43);
        assert!(!id.host_id().as_str().contains(['=', '+', '/']));
        assert!(spki_from_cert(b"\x30\x03\x02\x01\x01").is_none());
        assert!(spki_from_cert(b"").is_none());
    }

    #[test]
    fn debug_never_prints_the_key() {
        let id = HostIdentity::generate().unwrap();
        let s = format!("{id:?}");
        assert!(s.contains(id.host_id().as_str()));
        assert!(!s.contains("key_pkcs8"));
    }

    #[test]
    fn load_or_create_is_stable_and_protected() {
        let dir = temp_dir();
        let a = HostIdentity::load_or_create(&dir).unwrap();
        let b = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(
            a.host_id(),
            b.host_id(),
            "same key, same pin across restarts"
        );
        let raw = std::fs::read(dir.join(KEY_FILE)).unwrap();
        assert!(raw.starts_with(MAGIC));
        #[cfg(windows)]
        {
            assert_eq!(raw[8], SCHEME_DPAPI);
            // The plaintext PKCS#8 must not appear in the file.
            let pkcs8 = &a.key_pkcs8;
            assert!(!raw.windows(pkcs8.len()).any(|w| w == pkcs8.as_slice()));
        }
        // A corrupt file is an error, not a silent re-key.
        std::fs::write(dir.join(KEY_FILE), b"GANDERK1\x01garbage").unwrap();
        assert!(matches!(
            HostIdentity::load_or_create(&dir),
            Err(IdentityError::Corrupt(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
