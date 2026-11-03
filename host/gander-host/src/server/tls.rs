// SPDX-License-Identifier: Apache-2.0
//! rustls configuration: TLS 1.3 only, `ring` provider, the host certificate, ALPN `http/1.1`.
//!
//! [`PinnedHostVerifier`] is the client side of the pin (used by tests, `xtask` end-to-end runs and
//! any Rust client): the server certificate is accepted only when the SHA-256 of its SPKI equals
//! the expected host id. Names, dates and the system trust store are ignored by design.

use std::sync::Arc;

use gander_core::protocol::HostId;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};

use super::identity::{HostIdentity, host_id_for_cert};

/// ALPN protocol offered by the host.
pub const ALPN_HTTP11: &[u8] = b"http/1.1";

/// The crypto provider: `ring` (the default `aws-lc-rs` needs CMake/NASM to build).
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Server config: TLS 1.3 only, no client certificates, the host certificate.
pub fn server_config(identity: &HostIdentity) -> Result<Arc<ServerConfig>, rustls::Error> {
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.cert_der().clone()],
            identity.private_key_der(),
        )?;
    config.alpn_protocols = vec![ALPN_HTTP11.to_vec()];
    // No session resumption state is needed; tickets would only add key material to protect.
    config.send_tls13_tickets = 0;
    Ok(Arc::new(config))
}

/// Accepts exactly one server key: the one whose SPKI hashes to `expected`.
#[derive(Debug)]
pub struct PinnedHostVerifier {
    expected: HostId,
    algorithms: WebPkiSupportedAlgorithms,
}

impl PinnedHostVerifier {
    /// A verifier pinned to `expected`.
    pub fn new(expected: HostId) -> Self {
        Self {
            expected,
            algorithms: provider().signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for PinnedHostVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match host_id_for_cert(end_entity) {
            Some(id) if id == self.expected => Ok(ServerCertVerified::assertion()),
            Some(_) => Err(rustls::Error::General(
                "host certificate does not match the pinned host id".into(),
            )),
            None => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::BadEncoding,
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General(
            "TLS 1.2 is not used by Gander".into(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Client config pinned to `host_id`: TLS 1.3 only, ALPN `http/1.1`.
pub fn pinned_client_config(host_id: HostId) -> Result<Arc<ClientConfig>, rustls::Error> {
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedHostVerifier::new(host_id)))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN_HTTP11.to_vec()];
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_is_tls13_only() {
        let id = HostIdentity::generate().unwrap();
        let cfg = server_config(&id).unwrap();
        assert_eq!(cfg.alpn_protocols, vec![ALPN_HTTP11.to_vec()]);
        // TLS 1.2 is not even compiled in (rustls without the `tls12` feature), so the server
        // cannot negotiate it.
        assert!(pinned_client_config(id.host_id().clone()).is_ok());
    }
}
