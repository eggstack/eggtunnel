//! Server runtime coordinator.
//!
//! This module owns only the embeddable surface and bind orchestration:
//! [`Server`], [`ServerHandle`], the convenience constructors, and the
//! hand-off to the per-transport accept loop. Runtime responsibilities live in
//! private sibling modules:
//!
//! - [`config`]: configuration, transport profile, and profile validation;
//! - [`tls`]: TLS/mTLS material construction and peer identity derivation;
//! - [`accept`]: transport accept, handshake admission, and shutdown drain;
//! - [`auth`]: authentication throttling and Principal attachment;
//! - [`session`]: authenticated Session registry and lifetime ownership;
//! - [`control`]: the authenticated control loop (Service lifecycle + drain);
//! - [`pending`]: `ConnectionId` / `DataHello` correlation;
//! - [`service`]: service listener accept and relay-task ownership.

use std::net::SocketAddr;

use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::common::{BindPolicy, Counters, RuntimePolicy, TunnelError};

mod accept;
mod auth;
mod config;
mod control;
mod pending;
mod service;
mod session;
mod tls;

pub use config::{ServerBuilder, ServerConfig, ServerTransportProfile};

use accept::server_loop;
use tls::{ServerTls, build_server_tls};

#[cfg(all(test, feature = "client"))]
include!("server_tests.rs");

pub struct Server {
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
    handle: ServerHandle,
    local_addr: SocketAddr,
}

#[derive(Clone)]
pub struct ServerHandle {
    cancel: CancellationToken,
    counters: Counters,
}

impl ServerHandle {
    pub fn snapshot(&self) -> crate::common::Snapshot {
        self.counters.snapshot()
    }
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Server {
    pub async fn bind(config: ServerConfig) -> Result<Self, TunnelError> {
        ServerBuilder::new(config).bind().await
    }

    pub async fn bind_with_policy(
        config: ServerConfig,
        bind_policy: BindPolicy,
    ) -> Result<Self, TunnelError> {
        ServerBuilder::new(config)
            .bind_policy(bind_policy)
            .bind()
            .await
    }

    #[cfg(feature = "websocket-server")]
    pub async fn bind_websocket(config: ServerConfig) -> Result<Self, TunnelError> {
        ServerBuilder::new(config)
            .transport(ServerTransportProfile::WebSocket)
            .bind()
            .await
    }

    #[cfg(feature = "quic-server")]
    pub async fn bind_quic(config: ServerConfig) -> Result<Self, TunnelError> {
        ServerBuilder::new(config)
            .transport(ServerTransportProfile::Quic)
            .bind()
            .await
    }

    #[cfg(feature = "quic-server")]
    pub async fn bind_quic_with_policy(
        config: ServerConfig,
        bind_policy: BindPolicy,
    ) -> Result<Self, TunnelError> {
        ServerBuilder::new(config)
            .bind_policy(bind_policy)
            .transport(ServerTransportProfile::Quic)
            .bind()
            .await
    }

    #[cfg(all(test, feature = "quic-server"))]
    pub(crate) async fn bind_quic_with_admission_for_test(
        config: ServerConfig,
        max_active_data_streams: usize,
    ) -> Result<Self, TunnelError> {
        use eggress_transport_quic::{QuicListener, QuicServerConfig};

        require_caller_runtime("Server::bind_quic requires a caller-owned Tokio runtime")?;
        config::validate_config(&config)?;
        let bind_policy = BindPolicy::default();
        let listener = QuicListener::bind(
            config.listen_addr,
            QuicServerConfig {
                certificate_pem: config.certificate_pem.clone(),
                private_key_pem: config.private_key_pem.clone(),
                idle_timeout: std::time::Duration::from_secs(90),
                max_concurrent_streams: max_active_data_streams.max(1) as u32 * 2,
                alpn_protocols: Vec::new(),
            },
        )
        .await
        .map_err(|_| TunnelError::Tls)?;
        let local_addr = listener.local_addr().map_err(|_| TunnelError::Tls)?;
        let cancel = CancellationToken::new();
        let counters = Counters::default();
        let handle = ServerHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
        };
        let task = tokio::spawn(accept::quic_server_loop_with_admission(
            listener,
            config.token.clone(),
            bind_policy,
            cancel.clone(),
            counters,
            max_active_data_streams,
        ));
        Ok(Self {
            cancel,
            task: Some(task),
            handle,
            local_addr,
        })
    }

    #[cfg(feature = "mtls")]
    pub async fn bind_mtls(
        config: ServerConfig,
        trusted_client_ca_pem: Vec<u8>,
    ) -> Result<Self, TunnelError> {
        let bind_policy = BindPolicy {
            allow_public_addresses: config.allow_public_service_binds,
            ..BindPolicy::default()
        };
        ServerBuilder::new(config)
            .bind_policy(bind_policy)
            .client_ca_pem(trusted_client_ca_pem)
            .bind()
            .await
    }

    #[cfg(feature = "mtls")]
    pub async fn bind_mtls_with_policy(
        config: ServerConfig,
        trusted_client_ca_pem: Vec<u8>,
        bind_policy: BindPolicy,
    ) -> Result<Self, TunnelError> {
        ServerBuilder::new(config)
            .bind_policy(bind_policy)
            .client_ca_pem(trusted_client_ca_pem)
            .bind()
            .await
    }

    async fn bind_profile(
        config: ServerConfig,
        bind_policy: BindPolicy,
        profile: ServerTransportProfile,
        #[cfg(feature = "mtls")] trusted_client_ca: Option<Vec<u8>>,
        runtime_policy: RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        match profile {
            ServerTransportProfile::TcpTls => {
                let tls = {
                    #[cfg(feature = "mtls")]
                    if let Some(client_ca) = trusted_client_ca.as_deref() {
                        ServerTls::Mutual(tls::build_mtls_server_config(&config, client_ca)?)
                    } else {
                        ServerTls::Eggress(build_server_tls(&config)?)
                    }
                    #[cfg(not(feature = "mtls"))]
                    {
                        ServerTls::Eggress(build_server_tls(&config)?)
                    }
                };
                Self::bind_with_tls_profile(config, bind_policy, tls, false, runtime_policy).await
            }
            #[cfg(feature = "websocket-server")]
            ServerTransportProfile::WebSocket => {
                let tls = ServerTls::Eggress(build_server_tls(&config)?);
                Self::bind_with_tls_profile(config, bind_policy, tls, true, runtime_policy).await
            }
            #[cfg(feature = "quic-server")]
            ServerTransportProfile::Quic => {
                Self::bind_quic_profile(config, bind_policy, runtime_policy).await
            }
        }
    }

    #[cfg(feature = "quic-server")]
    async fn bind_quic_profile(
        config: ServerConfig,
        bind_policy: BindPolicy,
        runtime_policy: RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        use eggress_transport_quic::{QuicListener, QuicServerConfig};

        let listener = QuicListener::bind(
            config.listen_addr,
            QuicServerConfig {
                certificate_pem: config.certificate_pem.clone(),
                private_key_pem: config.private_key_pem.clone(),
                idle_timeout: runtime_policy.timeouts.control_idle,
                max_concurrent_streams: runtime_policy
                    .limits
                    .active_connections_per_session
                    .saturating_add(1) as u32,
                alpn_protocols: Vec::new(),
            },
        )
        .await
        .map_err(|_| TunnelError::Tls)?;
        let local_addr = listener.local_addr().map_err(|_| TunnelError::Tls)?;
        let cancel = CancellationToken::new();
        let counters = Counters::with_policy(runtime_policy);
        let handle = ServerHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
        };
        let task = tokio::spawn(accept::quic_server_loop(
            listener,
            config.token.clone(),
            bind_policy,
            cancel.clone(),
            counters,
        ));
        Ok(Self {
            cancel,
            task: Some(task),
            handle,
            local_addr,
        })
    }

    async fn bind_with_tls_profile(
        config: ServerConfig,
        bind_policy: BindPolicy,
        tls: ServerTls,
        websocket: bool,
        runtime_policy: RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        require_caller_runtime("Server::bind requires a caller-owned Tokio runtime")?;
        config::validate_config(&config)?;
        bind_policy.validate()?;
        let listener = TcpListener::bind(config.listen_addr).await?;
        let local_addr = listener.local_addr()?;
        let cancel = CancellationToken::new();
        let counters = Counters::with_policy(runtime_policy);
        let handle = ServerHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
        };
        let task_cancel = cancel.clone();
        let task = tokio::spawn(server_loop(
            listener,
            config.token.clone(),
            tls,
            bind_policy,
            task_cancel,
            counters,
            websocket,
        ));
        Ok(Self {
            cancel,
            task: Some(task),
            handle,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn handle(&self) -> ServerHandle {
        self.handle.clone()
    }

    pub async fn shutdown(mut self) {
        tracing::info!("server shutdown requested");
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The library never installs a runtime; the caller owns it.
fn require_caller_runtime(message: &'static str) -> Result<(), TunnelError> {
    tokio::runtime::Handle::try_current()
        .map(|_| ())
        .map_err(|_| TunnelError::Configuration(message))
}
