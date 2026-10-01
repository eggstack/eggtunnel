//! Server configuration, transport profile selection, and profile validation.
//!
//! This module is the single semantic owner of which server transport,
//! identity, bind, and runtime-policy combinations are accepted. Runtime
//! startup and embedder call sites validate through [`ServerBuilder::validate`]
//! so no second validation layer can drift.

use std::net::SocketAddr;

use eggtunnel_proto::MAX_FRAME_BYTES;

use crate::common::{BindPolicy, RuntimePolicy, SecretToken, TunnelError};

use super::Server;

/// Server ingress configuration. Certificate, key, and token material is
/// secret-bearing: the manual `Debug` implementation redacts it and the
/// private key is zeroized on drop.
#[derive(Clone)]
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    pub certificate_pem: Vec<u8>,
    pub private_key_pem: Vec<u8>,
    pub token: SecretToken,
    /// Non-loopback service binds require this explicit policy switch.
    pub allow_public_service_binds: bool,
}

impl Drop for ServerConfig {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.private_key_pem.zeroize();
    }
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("listen_addr", &self.listen_addr)
            .field("certificate_pem", &"[configured]")
            .field("private_key_pem", &"[REDACTED]")
            .field("token", &self.token)
            .field(
                "allow_public_service_binds",
                &self.allow_public_service_binds,
            )
            .finish()
    }
}

/// Transport and identity selection for [`ServerBuilder`].
pub enum ServerTransportProfile {
    TcpTls,
    #[cfg(feature = "quic-server")]
    Quic,
    #[cfg(feature = "websocket-server")]
    WebSocket,
}

/// Typed composition surface for server transport, bind, and runtime policy.
pub struct ServerBuilder {
    pub(super) config: ServerConfig,
    pub(super) bind_policy: BindPolicy,
    pub(super) profile: ServerTransportProfile,
    pub(super) runtime_policy: RuntimePolicy,
    #[cfg(feature = "mtls")]
    pub(super) trusted_client_ca: Option<Vec<u8>>,
}

impl ServerBuilder {
    pub fn new(config: ServerConfig) -> Self {
        let bind_policy = BindPolicy {
            allow_public_addresses: config.allow_public_service_binds,
            ..BindPolicy::default()
        };
        Self {
            config,
            bind_policy,
            profile: ServerTransportProfile::TcpTls,
            runtime_policy: RuntimePolicy::default(),
            #[cfg(feature = "mtls")]
            trusted_client_ca: None,
        }
    }

    pub fn bind_policy(mut self, policy: BindPolicy) -> Self {
        self.config.allow_public_service_binds = policy.allow_public_addresses;
        self.bind_policy = policy;
        self
    }

    pub fn transport(mut self, profile: ServerTransportProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn runtime_policy(mut self, policy: RuntimePolicy) -> Self {
        self.runtime_policy = policy;
        self
    }

    #[cfg(feature = "mtls")]
    pub fn client_ca_pem(mut self, pem: Vec<u8>) -> Self {
        self.trusted_client_ca = Some(pem);
        self
    }

    pub fn validate(&self) -> Result<(), TunnelError> {
        validate_server_profile(
            &self.config,
            &self.bind_policy,
            &self.profile,
            #[cfg(feature = "mtls")]
            self.trusted_client_ca.as_deref(),
            &self.runtime_policy,
        )
    }

    pub async fn bind(self) -> Result<Server, TunnelError> {
        self.validate()?;
        Server::bind_profile(
            self.config,
            self.bind_policy,
            self.profile,
            #[cfg(feature = "mtls")]
            self.trusted_client_ca,
            self.runtime_policy,
        )
        .await
    }
}

pub(super) fn validate_config(config: &ServerConfig) -> Result<(), TunnelError> {
    if config.certificate_pem.is_empty() || config.private_key_pem.is_empty() {
        return Err(TunnelError::Configuration(
            "server certificate and key are required",
        ));
    }
    if config.certificate_pem.len() > MAX_FRAME_BYTES
        || config.private_key_pem.len() > MAX_FRAME_BYTES
    {
        return Err(TunnelError::Configuration(
            "server TLS material exceeds configured size limit",
        ));
    }
    Ok(())
}

pub(super) fn validate_server_profile(
    config: &ServerConfig,
    bind_policy: &BindPolicy,
    profile: &ServerTransportProfile,
    #[cfg(feature = "mtls")] trusted_client_ca: Option<&[u8]>,
    runtime_policy: &RuntimePolicy,
) -> Result<(), TunnelError> {
    runtime_policy.validate()?;
    bind_policy.validate()?;
    if bind_policy.allow_public_addresses != config.allow_public_service_binds {
        return Err(TunnelError::Configuration(
            "bind policy public-address setting must match allow_public_service_binds",
        ));
    }
    validate_config(config)?;
    #[cfg(feature = "mtls")]
    if let Some(ca) = trusted_client_ca {
        if ca.is_empty() {
            return Err(TunnelError::Configuration(
                "trusted client CA bundle must not be empty",
            ));
        }
        if !matches!(profile, ServerTransportProfile::TcpTls) {
            return Err(TunnelError::Configuration(
                "mTLS is supported only with TCP/TLS",
            ));
        }
    }
    let _ = profile;
    Ok(())
}
