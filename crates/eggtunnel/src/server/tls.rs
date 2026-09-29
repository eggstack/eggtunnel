//! Server TLS and mTLS material construction.
//!
//! Peer identity attachment (`Principal`) is derived from the negotiated mTLS
//! leaf certificate; without mTLS there is no peer identity and the Principal
//! stays `None`.

use std::sync::Arc;

use eggress_transport_tls::TlsServerConfigBuilder;

use crate::common::TunnelError;

use super::config::ServerConfig;

/// Server-side TLS flavor selected by the validated transport profile.
#[derive(Clone)]
pub enum ServerTls {
    Eggress(Arc<rustls::ServerConfig>),
    #[cfg(feature = "mtls")]
    Mutual(Arc<rustls::ServerConfig>),
}

pub(super) fn build_server_tls(
    config: &ServerConfig,
) -> Result<Arc<rustls::ServerConfig>, TunnelError> {
    TlsServerConfigBuilder::new()
        .with_certificate_pem(&config.certificate_pem)
        .map_err(|_| TunnelError::Tls)?
        .with_key_pem(&config.private_key_pem)
        .map_err(|_| TunnelError::Tls)?
        .build()
        .map_err(|_| TunnelError::Tls)
}

#[cfg(feature = "mtls")]
pub(super) fn build_mtls_server_config(
    config: &ServerConfig,
    trusted_client_ca_pem: &[u8],
) -> Result<Arc<rustls::ServerConfig>, TunnelError> {
    let certificates =
        crate::pem::certificates(&config.certificate_pem).map_err(|_| TunnelError::Tls)?;
    let private_key =
        crate::pem::private_key(&config.private_key_pem).map_err(|_| TunnelError::Tls)?;
    let client_ca =
        crate::pem::certificates(trusted_client_ca_pem).map_err(|_| TunnelError::Tls)?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in client_ca {
        roots.add(cert).map_err(|_| TunnelError::Tls)?;
    }
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|_| TunnelError::Tls)?;
    let tls = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates, private_key)
        .map_err(|_| TunnelError::Tls)?;
    Ok(Arc::new(tls))
}

/// Opaque authenticated peer identity: a SHA-256 digest of the verified mTLS
/// leaf certificate. It is never used as a routing or policy key today beyond
/// exact DataHello/Principal equality checks.
#[cfg(feature = "mtls")]
pub(super) fn certificate_principal(certificate_der: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(certificate_der).into()
}
