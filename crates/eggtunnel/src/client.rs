use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use eggress_core::BoxStream;
use eggress_relay::{RelayOptions, relay_with_options};
use eggress_transport_tls::{TlsClientConfigBuilder, tls_connect};
use eggtunnel_proto::{
    Auth, AuthOk, Capabilities, ClientHello, DataHello, EffectiveBind, ErrorMessage,
    MAX_FRAME_BYTES, Message, Open, OpenReject, ProtocolVersion, RegisterService, ServerHello,
    ServiceId,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{Semaphore, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    common::{ClientService, Counters, SecretToken, Snapshot, TunnelError},
    wire_io::{read_boxed, read_message, write_boxed, write_message},
};

#[derive(Clone)]
enum ClientDataTransport {
    TcpTls {
        server_addr: String,
        server_name: String,
        tls: Arc<rustls::ClientConfig>,
        websocket: bool,
        #[cfg(feature = "outbound-proxy")]
        outbound: Option<Arc<eggress_outbound::OutboundConnector>>,
    },
    #[cfg(feature = "quic")]
    Quic(eggress_transport_quic::QuicConnection),
}

/// Async byte stream implemented by an application-provided target connector.
pub trait ApplicationStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> ApplicationStream for T {}

/// Transport-neutral stream returned by a [`TargetConnector`].
pub type TargetStream = Box<dyn ApplicationStream>;

#[derive(Clone)]
pub struct TargetContext {
    pub session_id: eggtunnel_proto::SessionId,
    pub connection_id: eggtunnel_proto::ConnectionId,
    pub cancellation: CancellationToken,
}

#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    #[error("application target refused the connection")]
    Refused,
    #[error("application target failed")]
    Failed,
}

pub type TargetFuture = Pin<Box<dyn Future<Output = Result<TargetStream, TargetError>> + Send>>;

/// Resolves a trusted, client-owned service to an application stream.
pub trait TargetConnector: Send + Sync + 'static {
    fn connect(&self, service: ClientService, context: TargetContext) -> TargetFuture;
}

struct TcpTargetConnector;

impl TargetConnector for TcpTargetConnector {
    fn connect(&self, service: ClientService, _context: TargetContext) -> TargetFuture {
        Box::pin(async move {
            TcpStream::connect((service.target.host(), service.target.port()))
                .await
                .map(|stream| Box::new(stream) as TargetStream)
                .map_err(|_| TargetError::Refused)
        })
    }
}

#[derive(Clone)]
pub struct ClientConfig {
    /// `host:port` endpoint; DNS resolution is performed by Tokio on connect.
    pub server_addr: String,
    pub tls_server_name: String,
    /// If absent, the Eggress system-root verifier is used.
    pub ca_pem: Option<Vec<u8>>,
    pub token: SecretToken,
    pub services: Vec<ClientService>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("server_addr", &self.server_addr)
            .field("tls_server_name", &self.tls_server_name)
            .field("ca_pem", &self.ca_pem.as_ref().map(|_| "[configured]"))
            .field("token", &self.token)
            .field("services", &self.services.len())
            .finish()
    }
}

/// Transport and identity selection for [`ClientBuilder`].
pub enum ClientTransportProfile {
    TcpTls,
    #[cfg(feature = "quic")]
    Quic,
    #[cfg(feature = "websocket")]
    WebSocket,
}

/// Typed composition surface for client transport, connector, and runtime policy.
pub struct ClientBuilder {
    config: ClientConfig,
    connector: Arc<dyn TargetConnector>,
    profile: ClientTransportProfile,
    policy: crate::common::RuntimePolicy,
    #[cfg(feature = "mtls")]
    identity: Option<ClientIdentity>,
    #[cfg(feature = "outbound-proxy")]
    outbound_proxy: Option<String>,
}

impl ClientBuilder {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            connector: Arc::new(TcpTargetConnector),
            profile: ClientTransportProfile::TcpTls,
            policy: crate::common::RuntimePolicy::default(),
            #[cfg(feature = "mtls")]
            identity: None,
            #[cfg(feature = "outbound-proxy")]
            outbound_proxy: None,
        }
    }

    pub fn with_connector(mut self, connector: Arc<dyn TargetConnector>) -> Self {
        self.connector = connector;
        self
    }

    pub fn transport(mut self, profile: ClientTransportProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn runtime_policy(mut self, policy: crate::common::RuntimePolicy) -> Self {
        self.policy = policy;
        self
    }

    #[cfg(feature = "mtls")]
    pub fn with_identity(mut self, identity: ClientIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    #[cfg(feature = "outbound-proxy")]
    pub fn outbound_proxy(mut self, proxy_chain: impl Into<String>) -> Self {
        self.outbound_proxy = Some(proxy_chain.into());
        self
    }

    pub fn validate(&self) -> Result<(), TunnelError> {
        validate_client_profile(
            &self.config,
            &self.profile,
            #[cfg(feature = "mtls")]
            self.identity.is_some(),
            #[cfg(feature = "outbound-proxy")]
            self.outbound_proxy.as_deref(),
            &self.policy,
        )
    }

    pub async fn start(self) -> Result<Client, TunnelError> {
        self.validate()?;
        Client::start_profile(
            self.config,
            self.connector,
            self.profile,
            #[cfg(feature = "mtls")]
            self.identity,
            #[cfg(feature = "outbound-proxy")]
            self.outbound_proxy,
            self.policy,
        )
        .await
    }
}

pub struct Client {
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
    handle: ClientHandle,
}

#[derive(Clone)]
pub struct ClientHandle {
    cancel: CancellationToken,
    counters: Counters,
    commands: mpsc::Sender<ClientCommand>,
    #[cfg(feature = "quic")]
    quic_client: Arc<std::sync::Mutex<Option<Arc<eggress_transport_quic::QuicClient>>>>,
}

enum ClientCommand {
    Register {
        service: ClientService,
        generation: u64,
        reply: oneshot::Sender<Result<EffectiveBind, TunnelError>>,
    },
    Unregister {
        id: ServiceId,
        reply: oneshot::Sender<Result<(), TunnelError>>,
    },
}

impl ClientHandle {
    pub fn snapshot(&self) -> Snapshot {
        self.counters.snapshot()
    }
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
    /// Ask the active Session to unregister a service. The configured mapping
    /// is removed and will not be restored after reconnect.
    pub async fn unregister_service(&self, id: ServiceId) -> Result<(), TunnelError> {
        let (reply, response) = oneshot::channel();
        tokio::select! {
            _ = self.cancel.cancelled() => return Err(TunnelError::Cancelled),
            result = self.commands.send(ClientCommand::Unregister { id, reply }) => result.map_err(|_| TunnelError::Disconnected)?,
        }
        tokio::select! {
            _ = self.cancel.cancelled() => Err(TunnelError::Cancelled),
            result = response => result.unwrap_or(Err(TunnelError::Disconnected)),
        }
    }

    /// Register a Service in the current authenticated Session. Only a
    /// server-acknowledged Service becomes desired state for reconnects.
    pub async fn register_service(
        &self,
        service: ClientService,
    ) -> Result<EffectiveBind, TunnelError> {
        if self
            .counters
            .connected
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            return Err(TunnelError::Disconnected);
        }
        let generation = self
            .counters
            .session_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        let (reply, response) = oneshot::channel();
        tokio::select! {
            _ = self.cancel.cancelled() => return Err(TunnelError::Cancelled),
            result = self.commands.send(ClientCommand::Register { service, generation, reply }) => result.map_err(|_| TunnelError::Disconnected)?,
        }
        tokio::select! {
            _ = self.cancel.cancelled() => Err(TunnelError::Cancelled),
            result = response => result.unwrap_or(Err(TunnelError::Disconnected)),
        }
    }

    #[cfg(all(test, feature = "quic"))]
    pub(crate) fn quic_client_for_test(&self) -> Option<Arc<eggress_transport_quic::QuicClient>> {
        self.quic_client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Client {
    async fn start_profile(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
        profile: ClientTransportProfile,
        #[cfg(feature = "mtls")] identity: Option<ClientIdentity>,
        #[cfg(feature = "outbound-proxy")] outbound_proxy: Option<String>,
        policy: crate::common::RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        validate_client_profile(
            &config,
            &profile,
            #[cfg(feature = "mtls")]
            identity.is_some(),
            #[cfg(feature = "outbound-proxy")]
            outbound_proxy.as_deref(),
            &policy,
        )?;
        #[cfg(feature = "mtls")]
        if let Some(identity) = identity {
            let tls = build_mtls_tls_config(&config, identity)?;
            return Self::start_with_tls_config(
                config,
                connector,
                tls,
                false,
                #[cfg(feature = "outbound-proxy")]
                None,
                policy,
            )
            .await;
        }
        match profile {
            ClientTransportProfile::TcpTls => {
                let tls = build_tls_config(config.ca_pem.as_deref())?;
                #[cfg(feature = "outbound-proxy")]
                let outbound = outbound_proxy
                    .as_deref()
                    .map(parse_outbound_proxy)
                    .transpose()?;
                Self::start_with_tls_config(
                    config,
                    connector,
                    tls,
                    false,
                    #[cfg(feature = "outbound-proxy")]
                    outbound,
                    policy,
                )
                .await
            }
            #[cfg(feature = "quic")]
            ClientTransportProfile::Quic => {
                Self::start_quic_profile(config, connector, false, policy).await
            }
            #[cfg(feature = "websocket")]
            ClientTransportProfile::WebSocket => {
                let tls = build_tls_config(config.ca_pem.as_deref())?;
                #[cfg(feature = "outbound-proxy")]
                let outbound = outbound_proxy
                    .as_deref()
                    .map(parse_outbound_proxy)
                    .transpose()?;
                Self::start_with_tls_config(
                    config,
                    connector,
                    tls,
                    true,
                    #[cfg(feature = "outbound-proxy")]
                    outbound,
                    policy,
                )
                .await
            }
        }
    }

    pub async fn start(config: ClientConfig) -> Result<Self, TunnelError> {
        ClientBuilder::new(config).start().await
    }

    /// Start a client using an application-provided target connector.
    pub async fn start_with_connector(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .with_connector(connector)
            .start()
            .await
    }

    #[cfg(feature = "websocket")]
    pub async fn start_websocket(config: ClientConfig) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .transport(ClientTransportProfile::WebSocket)
            .start()
            .await
    }

    #[cfg(feature = "websocket")]
    pub async fn start_websocket_with_connector(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .with_connector(connector)
            .transport(ClientTransportProfile::WebSocket)
            .start()
            .await
    }

    #[cfg(feature = "outbound-proxy")]
    pub async fn start_with_outbound_proxy(
        config: ClientConfig,
        proxy_chain: &str,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .outbound_proxy(proxy_chain)
            .start()
            .await
    }

    #[cfg(feature = "outbound-proxy")]
    pub async fn start_with_outbound_proxy_and_connector(
        config: ClientConfig,
        proxy_chain: &str,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .with_connector(connector)
            .outbound_proxy(proxy_chain)
            .start()
            .await
    }

    #[cfg(all(feature = "outbound-proxy", feature = "websocket"))]
    pub async fn start_websocket_with_outbound_proxy(
        config: ClientConfig,
        proxy_chain: &str,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .transport(ClientTransportProfile::WebSocket)
            .outbound_proxy(proxy_chain)
            .start()
            .await
    }

    #[cfg(all(feature = "outbound-proxy", feature = "websocket"))]
    pub async fn start_websocket_with_outbound_proxy_and_connector(
        config: ClientConfig,
        proxy_chain: &str,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .with_connector(connector)
            .transport(ClientTransportProfile::WebSocket)
            .outbound_proxy(proxy_chain)
            .start()
            .await
    }

    #[cfg(feature = "quic")]
    pub async fn start_quic(config: ClientConfig) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .transport(ClientTransportProfile::Quic)
            .start()
            .await
    }

    #[cfg(feature = "quic")]
    pub async fn start_quic_with_connector(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .with_connector(connector)
            .transport(ClientTransportProfile::Quic)
            .start()
            .await
    }

    #[cfg(feature = "quic")]
    async fn start_quic_profile(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
        insecure: bool,
        policy: crate::common::RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Client::start_quic requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config, &policy)?;
        if config.ca_pem.is_some() {
            return Err(TunnelError::Configuration(
                "Eggress QUIC currently uses platform roots; custom CA bundles are unsupported",
            ));
        }
        let cancel = CancellationToken::new();
        let counters = Counters::with_policy(policy);
        let (command_tx, command_rx) = mpsc::channel(policy.limits.client_command_queue);
        let handle = ClientHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
            commands: command_tx,
            #[cfg(feature = "quic")]
            quic_client: Arc::new(std::sync::Mutex::new(None)),
        };
        let task_cancel = cancel.clone();
        let task_handle = handle.clone();
        let task = tokio::spawn(async move {
            quic_reconnect_loop(
                config,
                connector,
                insecure,
                task_cancel,
                counters,
                command_rx,
                task_handle,
            )
            .await;
        });
        Ok(Self {
            cancel,
            task: Some(task),
            handle,
        })
    }

    #[cfg(all(test, feature = "quic"))]
    pub(crate) async fn start_quic_insecure_for_test(
        config: ClientConfig,
    ) -> Result<Self, TunnelError> {
        Self::start_quic_profile(
            config,
            Arc::new(TcpTargetConnector),
            true,
            Default::default(),
        )
        .await
    }

    #[cfg(all(test, feature = "quic"))]
    pub(crate) async fn start_quic_insecure_with_connector_for_test(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        Self::start_quic_profile(config, connector, true, Default::default()).await
    }

    #[cfg(feature = "mtls")]
    pub async fn start_with_mtls(
        config: ClientConfig,
        identity: ClientIdentity,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .with_identity(identity)
            .start()
            .await
    }

    #[cfg(feature = "mtls")]
    pub async fn start_with_mtls_and_connector(
        config: ClientConfig,
        identity: ClientIdentity,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .with_connector(connector)
            .with_identity(identity)
            .start()
            .await
    }

    async fn start_with_tls_config(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
        tls_config: Arc<rustls::ClientConfig>,
        websocket: bool,
        #[cfg(feature = "outbound-proxy")] outbound: Option<
            Arc<eggress_outbound::OutboundConnector>,
        >,
        policy: crate::common::RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Client::start requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config, &policy)?;
        let cancel = CancellationToken::new();
        let counters = Counters::with_policy(policy);
        let (command_tx, command_rx) = mpsc::channel(policy.limits.client_command_queue);
        let handle = ClientHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
            commands: command_tx,
            #[cfg(feature = "quic")]
            quic_client: Arc::new(std::sync::Mutex::new(None)),
        };
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            reconnect_loop(
                config,
                tls_config,
                connector,
                websocket,
                #[cfg(feature = "outbound-proxy")]
                outbound,
                task_cancel,
                counters,
                command_rx,
            )
            .await;
        });
        Ok(Self {
            cancel,
            task: Some(task),
            handle,
        })
    }

    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }

    pub async fn shutdown(mut self) {
        tracing::info!("client shutdown requested");
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

#[cfg(feature = "outbound-proxy")]
pub fn validate_outbound_proxy(proxy_chain: &str) -> Result<(), TunnelError> {
    parse_outbound_proxy(proxy_chain).map(|_| ())
}

#[cfg(feature = "outbound-proxy")]
fn parse_outbound_proxy(
    proxy_chain: &str,
) -> Result<Arc<eggress_outbound::OutboundConnector>, TunnelError> {
    eggress_outbound::OutboundConnector::from_pproxy_uri(proxy_chain)
        .map(Arc::new)
        .map_err(|_| TunnelError::Configuration("invalid outbound proxy chain"))
}

#[cfg(feature = "mtls")]
pub struct ClientIdentity {
    pub certificate_pem: Vec<u8>,
    private_key_pem: Vec<u8>,
}

#[cfg(feature = "mtls")]
impl ClientIdentity {
    pub fn new(certificate_pem: Vec<u8>, private_key_pem: Vec<u8>) -> Self {
        Self {
            certificate_pem,
            private_key_pem,
        }
    }
}

#[cfg(feature = "mtls")]
impl std::fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity")
            .field("certificate_pem", &"[configured]")
            .field("private_key_pem", &"[REDACTED]")
            .finish()
    }
}

#[cfg(feature = "mtls")]
impl Drop for ClientIdentity {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.private_key_pem.zeroize();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn validate_client_profile(
    config: &ClientConfig,
    _profile: &ClientTransportProfile,
    #[cfg(feature = "mtls")] has_identity: bool,
    #[cfg(feature = "outbound-proxy")] outbound_proxy: Option<&str>,
    policy: &crate::common::RuntimePolicy,
) -> Result<(), TunnelError> {
    policy.validate()?;
    validate_config(config, policy)?;
    #[cfg(feature = "mtls")]
    let _has_identity = has_identity;
    #[cfg(not(feature = "mtls"))]
    let _has_identity = false;
    #[cfg(feature = "outbound-proxy")]
    let _has_outbound_proxy = outbound_proxy.is_some();
    #[cfg(not(feature = "outbound-proxy"))]
    let _has_outbound_proxy = false;
    #[cfg(feature = "quic")]
    if matches!(_profile, ClientTransportProfile::Quic)
        && (config.ca_pem.is_some() || _has_identity || _has_outbound_proxy)
    {
        return Err(TunnelError::Configuration(
            "Eggress QUIC currently supports platform roots and bearer auth only",
        ));
    }
    #[cfg(feature = "websocket")]
    if matches!(_profile, ClientTransportProfile::WebSocket) && _has_identity {
        return Err(TunnelError::Configuration(
            "WebSocket transport currently does not support mTLS",
        ));
    }
    #[cfg(all(feature = "mtls", feature = "outbound-proxy"))]
    if _has_identity && outbound_proxy.is_some() {
        return Err(TunnelError::Configuration(
            "outbound proxy mode currently does not support mTLS",
        ));
    }
    #[cfg(feature = "outbound-proxy")]
    if let Some(chain) = outbound_proxy {
        let _ = parse_outbound_proxy(chain)?;
    }
    #[cfg(not(any(feature = "quic", feature = "websocket")))]
    let _ = _profile;
    Ok(())
}

fn validate_config(
    config: &ClientConfig,
    policy: &crate::common::RuntimePolicy,
) -> Result<(), TunnelError> {
    if !valid_endpoint(&config.server_addr) {
        return Err(TunnelError::Configuration(
            "server_addr must be a host:port endpoint",
        ));
    }
    if config.tls_server_name.is_empty() || config.tls_server_name.len() > 253 {
        return Err(TunnelError::Configuration("TLS server name is invalid"));
    }
    if config.services.len() > policy.limits.services_per_session {
        return Err(TunnelError::Configuration(
            "service count exceeds the configured per-session limit",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for service in &config.services {
        if !ids.insert(service.id) || !names.insert(service.name.as_str()) {
            return Err(TunnelError::Configuration(
                "service IDs and names must be unique",
            ));
        }
    }
    if config
        .ca_pem
        .as_ref()
        .is_some_and(|pem| pem.len() > MAX_FRAME_BYTES)
    {
        return Err(TunnelError::Configuration(
            "custom CA bundle exceeds configured size limit",
        ));
    }
    Ok(())
}

fn valid_endpoint(endpoint: &str) -> bool {
    let (host, port) = if endpoint.starts_with('[') {
        let Some(end) = endpoint.find(']') else {
            return false;
        };
        if endpoint.as_bytes().get(end + 1) != Some(&b':') {
            return false;
        }
        (&endpoint[1..end], &endpoint[end + 2..])
    } else {
        let Some((host, port)) = endpoint.rsplit_once(':') else {
            return false;
        };
        (host, port)
    };
    !host.is_empty()
        && !host.chars().any(char::is_whitespace)
        && port.parse::<u16>().is_ok_and(|port| port != 0)
}

fn build_tls_config(ca_pem: Option<&[u8]>) -> Result<Arc<rustls::ClientConfig>, TunnelError> {
    let builder = TlsClientConfigBuilder::new();
    let builder = match ca_pem {
        Some(pem) => builder.with_custom_ca_pem(pem),
        None => builder.with_system_roots(),
    }
    .map_err(|_| TunnelError::Tls)?;
    builder.build().map_err(|_| TunnelError::Tls)
}

#[cfg(feature = "mtls")]
fn build_mtls_tls_config(
    config: &ClientConfig,
    identity: ClientIdentity,
) -> Result<Arc<rustls::ClientConfig>, TunnelError> {
    let mut roots = rustls::RootCertStore::empty();
    if let Some(ca_pem) = config.ca_pem.as_deref() {
        let ca_certs = crate::pem::certificates(ca_pem).map_err(|_| TunnelError::Tls)?;
        for cert in ca_certs {
            roots.add(cert).map_err(|_| TunnelError::Tls)?;
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    let certificates =
        crate::pem::certificates(&identity.certificate_pem).map_err(|_| TunnelError::Tls)?;
    let private_key =
        crate::pem::private_key(&identity.private_key_pem).map_err(|_| TunnelError::Tls)?;
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certificates, private_key)
        .map_err(|_| TunnelError::Tls)?;
    Ok(Arc::new(tls))
}

#[allow(clippy::too_many_arguments)]
async fn reconnect_loop(
    mut config: ClientConfig,
    tls: Arc<rustls::ClientConfig>,
    connector: Arc<dyn TargetConnector>,
    websocket: bool,
    #[cfg(feature = "outbound-proxy")] outbound: Option<Arc<eggress_outbound::OutboundConnector>>,
    cancel: CancellationToken,
    counters: Counters,
    mut commands: mpsc::Receiver<ClientCommand>,
) {
    let mut delay = counters.policy.timeouts.reconnect_initial;
    'reconnect: loop {
        if cancel.is_cancelled() {
            break;
        }
        tracing::debug!(
            attempt = counters
                .reconnects
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_add(1),
            "client connection attempt"
        );
        while let Ok(command) = commands.try_recv() {
            apply_disconnected_command(&mut config.services, command);
        }
        let outcome = tokio::select! {
            _ = cancel.cancelled() => break,
            result = connect_server(&config.server_addr, counters.policy.timeouts.connect, #[cfg(feature = "outbound-proxy")] outbound.as_deref()) => result,
        };
        let result = match outcome {
            Ok(stream) => {
                let tls_result = tokio::select! {
                    _ = cancel.cancelled() => break 'reconnect,
                    result = timeout(counters.policy.timeouts.handshake, tls_connect(stream, tls.clone(), &config.tls_server_name)) => result,
                };
                match tls_result {
                    Ok(Ok(stream)) => {
                        tracing::debug!(
                            transport = if websocket {
                                "websocket_tls"
                            } else {
                                "tcp_tls"
                            },
                            "client transport established"
                        );
                        let stream_result: Result<BoxStream, TunnelError> = async {
                            #[allow(unused_mut)]
                            let mut stream = stream;
                            #[cfg(feature = "websocket")]
                            if websocket {
                                let url = format!("wss://{}", config.server_addr);
                                let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                                    .max_message_size(Some(1024 * 1024))
                                    .max_frame_size(Some(1024 * 1024));
                                stream = timeout(
                                    counters.policy.timeouts.handshake,
                                    eggress_protocol_websocket::WebSocketTunnelClient::new(
                                        1024 * 1024,
                                    )
                                    .connect_over_stream_with_config(&url, stream, ws_config),
                                )
                                .await
                                .map_err(|_| TunnelError::Timeout)?
                                .map_err(|_| TunnelError::Tls)?;
                            }
                            #[cfg(not(feature = "websocket"))]
                            let _ = websocket;
                            Ok(stream)
                        }
                        .await;
                        match stream_result {
                            Ok(stream) => tokio::select! {
                                _ = cancel.cancelled() => Err(TunnelError::Cancelled),
                                result = run_session(stream, SessionRun {
                                token: &config.token,
                                transport: ClientDataTransport::TcpTls {
                                    server_addr: config.server_addr.clone(),
                                    server_name: config.tls_server_name.clone(),
                                    tls: tls.clone(),
                                    websocket,
                                    #[cfg(feature = "outbound-proxy")]
                                    outbound: outbound.clone(),
                                },
                                connector: connector.clone(),
                                cancel: &cancel,
                                counters: &counters,
                                reconnect_delay: &mut delay,
                                services: &mut config.services,
                                commands: &mut commands,
                                }) => result,
                            },
                            Err(error) => Err(error),
                        }
                    }
                    Err(_) => Err(TunnelError::Timeout),
                    Ok(Err(_)) => Err(TunnelError::Tls),
                }
            }
            Err(error) => Err(error),
        };
        if matches!(
            &result,
            Err(TunnelError::Authentication | TunnelError::Authorization)
        ) {
            if let Err(error) = &result {
                counters.record_termination(error.termination_category());
            }
            break;
        }
        if let Err(error) = &result {
            counters.record_termination(error.termination_category());
            tracing::warn!(termination = ?error.termination_category(), "client Session ended");
        }
        counters
            .connected
            .store(0, std::sync::atomic::Ordering::Relaxed);
        counters
            .services
            .store(0, std::sync::atomic::Ordering::Relaxed);
        counters
            .binds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        if cancel.is_cancelled() {
            break;
        }
        counters
            .reconnects
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let jitter_ms = random_jitter_ms(delay);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(delay + Duration::from_millis(jitter_ms)) => {}
        }
        delay = delay
            .saturating_mul(2)
            .min(counters.policy.timeouts.reconnect_max);
    }
}

async fn connect_server(
    endpoint: &str,
    connect_timeout: Duration,
    #[cfg(feature = "outbound-proxy")] outbound: Option<&eggress_outbound::OutboundConnector>,
) -> Result<BoxStream, TunnelError> {
    #[cfg(feature = "outbound-proxy")]
    if let Some(outbound) = outbound {
        use eggress_outbound::OutboundConnectErrorKind;

        let (host, port) = split_endpoint(endpoint).ok_or(TunnelError::Configuration(
            "server_addr must be a host:port endpoint",
        ))?;
        let (stream, _) = outbound
            .connect_tcp_timeout_detailed(host, port, connect_timeout)
            .await
            .map_err(|error| match error.kind() {
                OutboundConnectErrorKind::Authentication => TunnelError::Authentication,
                OutboundConnectErrorKind::Policy => TunnelError::Authorization,
                OutboundConnectErrorKind::Timeout => TunnelError::Timeout,
                _ => TunnelError::Disconnected,
            })?;
        return Ok(stream);
    }
    let tcp = timeout(connect_timeout, TcpStream::connect(endpoint))
        .await
        .map_err(|_| TunnelError::Timeout)?
        .map_err(|_| TunnelError::Disconnected)?;
    Ok(Box::new(tcp))
}

#[cfg(feature = "quic")]
async fn quic_reconnect_loop(
    mut config: ClientConfig,
    connector: Arc<dyn TargetConnector>,
    insecure: bool,
    cancel: CancellationToken,
    counters: Counters,
    mut commands: mpsc::Receiver<ClientCommand>,
    handle: ClientHandle,
) {
    use eggress_transport_quic::{QuicClient, QuicClientConfig};

    let Some((host, port)) = split_endpoint(&config.server_addr) else {
        return;
    };
    let mut delay = counters.policy.timeouts.reconnect_initial;
    'reconnect: loop {
        if cancel.is_cancelled() {
            break;
        }
        tracing::debug!(
            attempt = counters
                .reconnects
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_add(1),
            "QUIC client connection attempt"
        );
        while let Ok(command) = commands.try_recv() {
            apply_disconnected_command(&mut config.services, command);
        }
        let quic_config = QuicClientConfig {
            server_name: config.tls_server_name.clone(),
            insecure,
            idle_timeout: counters.policy.timeouts.control_idle,
            max_concurrent_streams: counters.policy.limits.client_open_tasks.saturating_add(1)
                as u32,
            ..QuicClientConfig::default()
        };
        let quic = tokio::select! {
            _ = cancel.cancelled() => break,
            result = timeout(counters.policy.timeouts.connect, QuicClient::connect(host, port, quic_config)) => {
                match result {
                    Ok(Ok(client)) => client,
                    _ => {
                        tracing::debug!(category = "quic_connect", "QUIC connection attempt failed");
                        record_quic_reconnect(&counters, &cancel, &mut delay).await;
                        continue;
                    }
                }
            }
        };
        *handle.quic_client.lock().unwrap_or_else(|p| p.into_inner()) = Some(quic.clone());
        let session = async {
            let connection = timeout(counters.policy.timeouts.connect, quic.get_connection())
                .await
                .map_err(|_| TunnelError::Timeout)?
                .map_err(|_| TunnelError::Tls)?;
            let control = timeout(counters.policy.timeouts.connect, connection.open_stream())
                .await
                .map_err(|_| TunnelError::Timeout)?
                .map_err(|_| TunnelError::Disconnected)?;
            tracing::debug!(transport = "quic", "QUIC transport established");
            run_session(
                control,
                SessionRun {
                    token: &config.token,
                    transport: ClientDataTransport::Quic(connection.clone()),
                    connector: connector.clone(),
                    cancel: &cancel,
                    counters: &counters,
                    reconnect_delay: &mut delay,
                    services: &mut config.services,
                    commands: &mut commands,
                },
            )
            .await
        };
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(TunnelError::Cancelled),
            result = session => result,
        };
        quic.close();
        if matches!(
            &result,
            Err(TunnelError::Authentication | TunnelError::Authorization)
        ) {
            if let Err(error) = &result {
                counters.record_termination(error.termination_category());
            }
            break 'reconnect;
        }
        if let Err(error) = &result {
            counters.record_termination(error.termination_category());
            tracing::warn!(termination = ?error.termination_category(), "QUIC client Session ended");
        }
        if cancel.is_cancelled() {
            break 'reconnect;
        }
        record_quic_reconnect(&counters, &cancel, &mut delay).await;
        *handle.quic_client.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[cfg(feature = "quic")]
async fn record_quic_reconnect(
    counters: &Counters,
    cancel: &CancellationToken,
    delay: &mut Duration,
) {
    counters
        .connected
        .store(0, std::sync::atomic::Ordering::Relaxed);
    counters
        .services
        .store(0, std::sync::atomic::Ordering::Relaxed);
    counters
        .binds
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    if cancel.is_cancelled() {
        return;
    }
    counters
        .reconnects
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let jitter = random_jitter_ms(*delay);
    tokio::select! {
        _ = cancel.cancelled() => {},
        _ = tokio::time::sleep(*delay + Duration::from_millis(jitter)) => {}
    }
    *delay = delay
        .saturating_mul(2)
        .min(counters.policy.timeouts.reconnect_max);
}

#[cfg(any(feature = "quic", feature = "outbound-proxy"))]
fn split_endpoint(endpoint: &str) -> Option<(&str, u16)> {
    let (host, port) = if endpoint.starts_with('[') {
        let end = endpoint.find(']')?;
        if endpoint.as_bytes().get(end + 1) != Some(&b':') {
            return None;
        }
        (&endpoint[1..end], &endpoint[end + 2..])
    } else {
        endpoint.rsplit_once(':')?
    };
    Some((host, port.parse().ok()?))
}

fn random_jitter_ms(delay: Duration) -> u64 {
    let max = (delay.as_millis() / 4).min(7_500) as u64;
    if max == 0 {
        return 0;
    }
    let mut random = [0; 8];
    if getrandom::fill(&mut random).is_ok() {
        u64::from_ne_bytes(random) % (max + 1)
    } else {
        0
    }
}

struct SessionRun<'a> {
    token: &'a SecretToken,
    transport: ClientDataTransport,
    connector: Arc<dyn TargetConnector>,
    cancel: &'a CancellationToken,
    counters: &'a Counters,
    reconnect_delay: &'a mut Duration,
    services: &'a mut Vec<ClientService>,
    commands: &'a mut mpsc::Receiver<ClientCommand>,
}

async fn run_session(mut stream: BoxStream, context: SessionRun<'_>) -> Result<(), TunnelError> {
    let SessionRun {
        token,
        transport,
        connector,
        cancel,
        counters,
        reconnect_delay,
        services,
        commands,
    } = context;
    handshake_write(
        &mut stream,
        &Message::ClientHello(ClientHello {
            version: ProtocolVersion::CURRENT,
            capabilities: Capabilities::default(),
        }),
        counters.policy.timeouts.handshake,
    )
    .await?;
    match handshake_read(&mut stream, counters.policy.timeouts.handshake).await? {
        Message::ServerHello(ServerHello { version, .. })
            if version.major == ProtocolVersion::CURRENT.major => {}
        _ => {
            return Err(TunnelError::Protocol(
                eggtunnel_proto::ProtocolError::UnexpectedMessage,
            ));
        }
    }
    let auth = Auth::new(token.expose().to_vec())?;
    handshake_write(
        &mut stream,
        &Message::Auth(auth),
        counters.policy.timeouts.handshake,
    )
    .await?;
    let session_id = match handshake_read(&mut stream, counters.policy.timeouts.handshake).await? {
        Message::AuthOk(AuthOk { session_id }) => session_id,
        _ => return Err(TunnelError::Authentication),
    };
    let generation = counters.begin_session()?;
    tracing::info!(
        session_generation = generation,
        "authenticated client Session established"
    );

    for service in services.iter() {
        let registration = RegisterService {
            service_id: service.id,
            name: service.name.clone(),
            requested_bind: service.requested_bind.clone(),
            target: service.target.clone(),
        };
        handshake_write(
            &mut stream,
            &Message::RegisterService(registration),
            counters.policy.timeouts.handshake,
        )
        .await?;
        match handshake_read(&mut stream, counters.policy.timeouts.handshake).await? {
            Message::RegisterAck(ack) if ack.service_id == service.id => {
                counters
                    .binds
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((session_id, service.id, ack.effective_bind.clone()));
                tracing::info!(service_id = service.id.0, service_name = service.name.as_str(), effective_address = %std::net::Ipv6Addr::from(ack.effective_bind.address), effective_port = ack.effective_bind.port, "initial Service registered");
            }
            Message::Error(_) => return Err(TunnelError::Authorization),
            _ => return Err(TunnelError::Authorization),
        }
    }

    counters
        .connected
        .store(1, std::sync::atomic::Ordering::Relaxed);
    counters
        .services
        .store(services.len(), std::sync::atomic::Ordering::Relaxed);
    counters
        .sessions
        .store(1, std::sync::atomic::Ordering::Relaxed);
    *reconnect_delay = counters.policy.timeouts.reconnect_initial;
    let _connected_guard = CounterGuard::new(counters.sessions.clone());
    tracing::info!(
        session_generation = generation,
        registered_services = services.len(),
        "client Session ready"
    );
    let mut active_services: HashMap<ServiceId, ClientService> =
        services.iter().cloned().map(|s| (s.id, s)).collect();
    let mut pending_registrations = HashMap::<
        ServiceId,
        (
            ClientService,
            u64,
            Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>,
        ),
    >::new();
    let semaphore = Arc::new(Semaphore::new(counters.policy.limits.client_open_tasks));
    let (out_tx, mut out_rx) = mpsc::channel(counters.policy.limits.control_queue);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut opens = JoinSet::new();
    let session_cancel = CancellationToken::new();
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + counters.policy.timeouts.heartbeat_interval,
        counters.policy.timeouts.heartbeat_interval,
    );
    let mut heartbeat_nonce = 0u64;
    let mut heartbeat_outstanding: Option<(u64, tokio::time::Instant)> = None;
    let mut registration_deadline: Option<tokio::time::Instant> = None;
    let mut registration_timed_out = false;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                session_cancel.cancel();
                let drain = Message::Drain(eggtunnel_proto::Drain { deadline_ms: counters.policy.timeouts.relay_drain.as_millis() as u32 });
                let _ = timeout(Duration::from_millis(250), write_message(&mut writer, &drain)).await;
                break;
            }
            _ = heartbeat.tick() => {
                if heartbeat_outstanding.is_some() {
                    counters.record_heartbeat_missed();
                    tracing::debug!(session_generation = generation, "heartbeat response missed");
                } else {
                    heartbeat_nonce = heartbeat_nonce.wrapping_add(1);
                    match out_tx.try_send(Message::Ping(eggtunnel_proto::Ping { nonce: heartbeat_nonce })) {
                        Ok(()) => heartbeat_outstanding = Some((heartbeat_nonce, tokio::time::Instant::now())),
                        Err(_) => counters.record_heartbeat_missed(),
                    }
                }
            }
            _ = async {
                if let Some(deadline) = registration_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if registration_deadline.is_some() => {
                if let Some((_, _, reply)) = pending_registrations.values_mut().next()
                    && let Some(reply) = reply.take()
                {
                    let _ = reply.send(Err(TunnelError::Timeout));
                }
                registration_timed_out = true;
                tracing::debug!(session_generation = generation, "Service registration acknowledgement timed out");
                break;
            }
            incoming = read_message(&mut reader) => {
                match incoming {
                    Ok(Message::Open(open)) => {
                        let Some(service) = active_services.get(&open.service_id).cloned() else {
                            counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            counters.record_termination(crate::common::TerminationCategory::Authorization);
                            tracing::debug!(category = "unknown_service", service_id = open.service_id.0, "Open rejected");
                            let _ = out_tx.try_send(Message::OpenReject(OpenReject { connection_id: open.connection_id, code: 1 }));
                            continue;
                        };
                        let permit = semaphore.clone().try_acquire_owned();
                        let Ok(permit) = permit else {
                            counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            counters.record_termination(crate::common::TerminationCategory::ResourceExhausted);
                            tracing::debug!(category = "open_task_admission", service_id = open.service_id.0, "Open rejected");
                            let _ = out_tx.try_send(Message::OpenReject(OpenReject { connection_id: open.connection_id, code: 2 }));
                            continue;
                        };
                        let service_cancel = session_cancel.child_token();
                        let out = out_tx.clone();
                        let open_guard = OpenTaskGuard::new(
                            counters.open_tasks.clone(),
                            counters.high_water_open_tasks.clone(),
                        );
                        let context = OpenContext {
                            transport: transport.clone(),
                            session_id,
                            cancel: service_cancel,
                            out,
                            counters: (*counters).clone(),
                            connector: connector.clone(),
                        };
                        opens.spawn(async move {
                            let _permit = permit;
                            let _open_guard = open_guard;
                            handle_open(open, service, context).await;
                        });
                    }
                    Ok(Message::Ping(ping)) => { let _ = out_tx.try_send(Message::Pong(eggtunnel_proto::Pong { nonce: ping.nonce })); }
                    Ok(Message::Pong(pong)) => {
                        if let Some((nonce, sent_at)) = heartbeat_outstanding
                            && nonce == pong.nonce
                        {
                            counters.record_heartbeat_pong(sent_at.into_std());
                            heartbeat_outstanding = None;
                        }
                    }
                    Ok(Message::RegisterAck(ack)) => {
                        registration_deadline = None;
                        let Some((service, ack_generation, reply)) = pending_registrations.remove(&ack.service_id) else {
                            return Err(TunnelError::Protocol(eggtunnel_proto::ProtocolError::UnexpectedMessage));
                        };
                        if ack_generation != generation {
                            if let Some(reply) = reply {
                                let _ = reply.send(Err(TunnelError::Disconnected));
                            }
                            continue;
                        }
                        if reply.as_ref().is_none_or(oneshot::Sender::is_closed) {
                            write_message(&mut writer, &Message::UnregisterService(eggtunnel_proto::UnregisterService { service_id: ack.service_id })).await?;
                            continue;
                        }
                        counters.binds.lock().unwrap_or_else(|p| p.into_inner()).push((session_id, service.id, ack.effective_bind.clone()));
                        active_services.insert(service.id, service.clone());
                        services.push(service.clone());
                        counters.services.store(active_services.len(), std::sync::atomic::Ordering::Relaxed);
                        counters.high_water_services.fetch_max(active_services.len(), std::sync::atomic::Ordering::Relaxed);
                        tracing::info!(service_id = service.id.0, service_name = service.name.as_str(), effective_address = %std::net::Ipv6Addr::from(ack.effective_bind.address), effective_port = ack.effective_bind.port, session_generation = generation, "Service registered");
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(ack.effective_bind));
                        }
                    }
                    Ok(Message::Error(error)) => {
                        registration_deadline = None;
                        let pending_id = pending_registrations.keys().next().copied();
                        if let Some((_, _, reply)) = pending_id.and_then(|id| pending_registrations.remove(&id)) {
                            let error = registration_error(error);
                            tracing::debug!(registration_error = ?error.termination_category(), session_generation = generation, "Service registration rejected");
                            if let Some(reply) = reply {
                                let _ = reply.send(Err(error));
                            }
                        } else {
                            return Err(TunnelError::Authorization);
                        }
                    }
                    Ok(Message::Drain(_)) => {
                        tracing::info!(session_generation = generation, "server requested Session drain");
                        session_cancel.cancel();
                        break;
                    }
                    Ok(_) => return Err(TunnelError::Protocol(eggtunnel_proto::ProtocolError::UnexpectedMessage)),
                    Err(error) => return Err(error.into()),
                }
            }
            Some(message) = out_rx.recv() => { write_message(&mut writer, &message).await?; }
            Some(command) = commands.recv() => {
                match command {
                    ClientCommand::Register { service, generation: command_generation, reply } => {
                        if command_generation != generation {
                            let _ = reply.send(Err(TunnelError::Disconnected));
                            continue;
                        }
                        let duplicate = services.iter().any(|existing| existing.id == service.id || existing.name == service.name)
                            || pending_registrations.values().any(|(pending, _, _)| pending.id == service.id || pending.name == service.name);
                        if duplicate {
                            let _ = reply.send(Err(TunnelError::ServiceAlreadyExists));
                            continue;
                        }
                        if !pending_registrations.is_empty() {
                            let _ = reply.send(Err(TunnelError::ResourceExhausted));
                            continue;
                        }
                        if services.len().saturating_add(pending_registrations.len()) >= counters.policy.limits.services_per_session {
                            let _ = reply.send(Err(TunnelError::ResourceExhausted));
                            continue;
                        }
                        let registration = RegisterService {
                            service_id: service.id,
                            name: service.name.clone(),
                            requested_bind: service.requested_bind.clone(),
                            target: service.target.clone(),
                        };
                        if reply.is_closed() {
                            continue;
                        }
                        match timeout(
                            counters.policy.timeouts.handshake,
                            write_message(&mut writer, &Message::RegisterService(registration)),
                        )
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => return Err(error.into()),
                            Err(_) => {
                                let _ = reply.send(Err(TunnelError::Timeout));
                                registration_timed_out = true;
                                break;
                            }
                        }
                        registration_deadline = Some(
                            tokio::time::Instant::now() + counters.policy.timeouts.handshake,
                        );
                        pending_registrations.insert(service.id, (service, command_generation, Some(reply)));
                    }
                    ClientCommand::Unregister { id, reply } => {
                        if let Some((_, _, register_reply)) = pending_registrations.get_mut(&id)
                            && let Some(register_reply) = register_reply.take()
                        {
                            let _ = register_reply.send(Err(TunnelError::Cancelled));
                        }
                        let was_present = active_services.remove(&id).is_some();
                        if was_present {
                            services.retain(|service| service.id != id);
                            counters.services.store(active_services.len(), std::sync::atomic::Ordering::Relaxed);
                            counters.binds.lock().unwrap_or_else(|p| p.into_inner()).retain(|(_, service_id, _)| *service_id != id);
                            tracing::info!(service_id = id.0, session_generation = generation, "Service unregistered");
                        }
                        write_message(&mut writer, &Message::UnregisterService(eggtunnel_proto::UnregisterService { service_id: id })).await?;
                        let _ = reply.send(Ok(()));
                    }
                }
            }
            Some(result) = opens.join_next(), if !opens.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    let _ = timeout(counters.policy.timeouts.shutdown_grace, async {
        while opens.join_next().await.is_some() {}
    })
    .await;
    opens.abort_all();
    while opens.join_next().await.is_some() {}
    counters
        .connected
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let pending_error = if registration_timed_out {
        TunnelError::Timeout
    } else if cancel.is_cancelled() {
        TunnelError::Cancelled
    } else {
        TunnelError::Disconnected
    };
    for (_, (_, _, reply)) in pending_registrations {
        if let Some(reply) = reply {
            let _ = reply.send(Err(match pending_error {
                TunnelError::Cancelled => TunnelError::Cancelled,
                _ => TunnelError::Disconnected,
            }));
        }
    }
    if registration_timed_out {
        Err(TunnelError::Timeout)
    } else {
        Ok(())
    }
}

fn apply_disconnected_command(services: &mut Vec<ClientService>, command: ClientCommand) {
    match command {
        ClientCommand::Register { reply, .. } => {
            let _ = reply.send(Err(TunnelError::Disconnected));
        }
        ClientCommand::Unregister { id, reply } => {
            services.retain(|service| service.id != id);
            tracing::info!(
                service_id = id.0,
                "desired Service unregistered while disconnected"
            );
            let _ = reply.send(Ok(()));
        }
    }
}

fn registration_error(error: ErrorMessage) -> TunnelError {
    match error.code {
        1 => TunnelError::ServiceAlreadyExists,
        5 => TunnelError::ResourceExhausted,
        _ => TunnelError::Authorization,
    }
}

async fn handshake_read(
    stream: &mut BoxStream,
    deadline: Duration,
) -> Result<Message, TunnelError> {
    timeout(deadline, read_boxed(stream))
        .await
        .map_err(|_| TunnelError::Timeout)?
        .map_err(Into::into)
}

async fn handshake_write(
    stream: &mut BoxStream,
    message: &Message,
    deadline: Duration,
) -> Result<(), TunnelError> {
    timeout(deadline, write_boxed(stream, message))
        .await
        .map_err(|_| TunnelError::Timeout)?
        .map_err(Into::into)
}

struct OpenContext {
    transport: ClientDataTransport,
    connector: Arc<dyn TargetConnector>,
    session_id: eggtunnel_proto::SessionId,
    cancel: CancellationToken,
    out: mpsc::Sender<Message>,
    counters: Counters,
}

async fn handle_open(open: Open, service: ClientService, context: OpenContext) {
    let OpenContext {
        transport,
        connector,
        session_id,
        cancel,
        out,
        counters,
    } = context;
    let result = async {
        let target_context = TargetContext {
            session_id,
            connection_id: open.connection_id,
            cancellation: cancel.clone(),
        };
        let target = tokio::select! {
            _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
            result = timeout(counters.policy.timeouts.connect, connector.connect(service.clone(), target_context)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Target)?,
        };
        let mut data = match transport {
            ClientDataTransport::TcpTls { server_addr, server_name, tls, websocket, #[cfg(feature = "outbound-proxy")] outbound } => {
                let stream = tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = connect_server(&server_addr, counters.policy.timeouts.connect, #[cfg(feature = "outbound-proxy")] outbound.as_deref()) => result?,
                };
                #[allow(unused_mut)]
                let mut stream = tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = timeout(counters.policy.timeouts.handshake, tls_connect(stream, tls, &server_name)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
                };
                #[cfg(feature = "websocket")]
                if websocket {
                    let url = format!("wss://{server_addr}");
                    let ws_client = eggress_protocol_websocket::WebSocketTunnelClient::new(1024 * 1024);
                    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(1024 * 1024))
                        .max_frame_size(Some(1024 * 1024));
                    stream = tokio::select! {
                        _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                        result = timeout(counters.policy.timeouts.handshake, ws_client.connect_over_stream_with_config(&url, stream, ws_config)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
                    };
                }
                #[cfg(not(feature = "websocket"))]
                let _ = websocket;
                stream
            }
            #[cfg(feature = "quic")]
            ClientDataTransport::Quic(connection) => {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = timeout(counters.policy.timeouts.connect, connection.open_stream()) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Disconnected)?,
                }
            }
        };
        write_boxed(&mut data, &Message::DataHello(DataHello { session_id, service_id: service.id, connection_id: open.connection_id })).await?;
        match relay_with_options(target, data, RelayOptions::bounded(std::num::NonZeroUsize::new(16 * 1024).unwrap(), counters.policy.timeouts.relay_drain)).await {
            Ok(report) => {
                counters.bytes_upstream.fetch_add(report.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                counters.bytes_downstream.fetch_add(report.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
            }
            Err(failure) => {
                counters.bytes_upstream.fetch_add(failure.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                counters.bytes_downstream.fetch_add(failure.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok::<(), TunnelError>(())
    }.await;
    if let Err(error) = result {
        counters.record_termination(error.termination_category());
        tracing::debug!(service_id = service.id.0, termination = ?error.termination_category(), "client data Open ended");
        if !cancel.is_cancelled() {
            let _ = out.try_send(Message::OpenReject(OpenReject {
                connection_id: open.connection_id,
                code: 1,
            }));
        }
    }
}

struct CounterGuard(Arc<std::sync::atomic::AtomicUsize>);
impl CounterGuard {
    fn new(value: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self(value)
    }
}

struct OpenTaskGuard(Arc<std::sync::atomic::AtomicUsize>);

impl OpenTaskGuard {
    fn new(
        current: Arc<std::sync::atomic::AtomicUsize>,
        high_water: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        let active = current.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        high_water.fetch_max(active, std::sync::atomic::Ordering::Relaxed);
        Self(current)
    }
}

impl Drop for OpenTaskGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}
impl Drop for CounterGuard {
    fn drop(&mut self) {
        self.0.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggtunnel_proto::{RequestedBind, ServiceName, TcpTarget};

    fn config(server_addr: &str) -> ClientConfig {
        ClientConfig {
            server_addr: server_addr.to_owned(),
            tls_server_name: "localhost".to_owned(),
            ca_pem: None,
            token: SecretToken::new(b"test-token".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("one").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 80).unwrap(),
            )],
        }
    }

    #[test]
    fn client_validation_rejects_invalid_endpoint_without_server_feature() {
        assert!(validate_config(&config("missing-port"), &Default::default()).is_err());
        assert!(validate_config(&config("localhost:0"), &Default::default()).is_err());
        assert!(validate_config(&config("localhost:443"), &Default::default()).is_ok());
    }

    #[test]
    fn client_validation_rejects_duplicate_service_identity() {
        let mut config = config("localhost:443");
        config.services.push(config.services[0].clone());
        assert!(validate_config(&config, &Default::default()).is_err());
    }

    #[test]
    fn client_config_debug_redacts_bearer_token_for_tracing_callers() {
        let marker = "client-token-must-not-appear";
        let mut config = config("localhost:443");
        config.token = SecretToken::new(marker.as_bytes().to_vec()).unwrap();
        let formatted = format!("{config:?}");
        assert!(!formatted.contains(marker));
        assert!(formatted.contains("REDACTED"));
    }

    fn service(id: u64, name: &str, target_port: u16) -> ClientService {
        ClientService::new(
            ServiceId(id),
            ServiceName::new(name).unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", target_port).unwrap(),
        )
    }

    async fn fake_connected_client() -> (
        ClientHandle,
        Counters,
        JoinHandle<Result<(), TunnelError>>,
        BoxStream,
    ) {
        fake_connected_client_with_policy(crate::RuntimePolicy::default()).await
    }

    async fn fake_connected_client_with_policy(
        policy: crate::RuntimePolicy,
    ) -> (
        ClientHandle,
        Counters,
        JoinHandle<Result<(), TunnelError>>,
        BoxStream,
    ) {
        let token = SecretToken::new(b"dynamic-registration-test".to_vec()).unwrap();
        let initial_service = service(1, "initial", 80);
        let counters = Counters::with_policy(policy);
        let cancel = CancellationToken::new();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (commands, command_rx) = mpsc::channel(32);
        let handle = ClientHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
            commands,
            #[cfg(feature = "quic")]
            quic_client: Arc::new(std::sync::Mutex::new(None)),
        };
        let task_counters = counters.clone();
        let task_cancel = cancel.clone();
        let task_token = token.clone();
        let task = tokio::spawn(async move {
            let mut services = vec![initial_service];
            let mut command_rx = command_rx;
            let mut reconnect_delay = Duration::from_millis(500);
            run_session(
                Box::new(client_io),
                SessionRun {
                    token: &task_token,
                    transport: ClientDataTransport::TcpTls {
                        server_addr: "localhost:443".into(),
                        server_name: "localhost".into(),
                        tls: build_tls_config(None).unwrap(),
                        websocket: false,
                        #[cfg(feature = "outbound-proxy")]
                        outbound: None,
                    },
                    connector: Arc::new(TcpTargetConnector),
                    cancel: &task_cancel,
                    counters: &task_counters,
                    reconnect_delay: &mut reconnect_delay,
                    services: &mut services,
                    commands: &mut command_rx,
                },
            )
            .await
        });
        let mut peer: BoxStream = Box::new(server_io);
        assert!(matches!(
            read_boxed(&mut peer).await.unwrap(),
            Message::ClientHello(_)
        ));
        write_boxed(
            &mut peer,
            &Message::ServerHello(ServerHello {
                version: ProtocolVersion::CURRENT,
                capabilities: Capabilities::default(),
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            read_boxed(&mut peer).await.unwrap(),
            Message::Auth(_)
        ));
        write_boxed(
            &mut peer,
            &Message::AuthOk(AuthOk {
                session_id: eggtunnel_proto::SessionId::generate().unwrap(),
            }),
        )
        .await
        .unwrap();
        let Message::RegisterService(initial) = read_boxed(&mut peer).await.unwrap() else {
            panic!("expected initial registration")
        };
        write_boxed(
            &mut peer,
            &Message::RegisterAck(eggtunnel_proto::RegisterAck {
                service_id: initial.service_id,
                effective_bind: EffectiveBind {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 31001,
                },
            }),
        )
        .await
        .unwrap();
        (handle, counters, task, peer)
    }

    #[tokio::test]
    async fn dynamic_registration_returns_disconnected_if_ack_is_lost() {
        let (handle, _, task, mut peer) = fake_connected_client().await;

        let register_handle = handle.clone();
        let register = tokio::spawn(async move {
            register_handle
                .register_service(service(2, "dynamic", 81))
                .await
        });
        let dynamic = tokio::time::timeout(Duration::from_secs(2), read_boxed(&mut peer))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(dynamic, Message::RegisterService(_)));
        drop(peer);
        assert!(matches!(
            register.await.unwrap(),
            Err(TunnelError::Disconnected)
        ));
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn cancelled_registration_is_unregistered_and_never_becomes_desired_state() {
        let (handle, counters, task, mut peer) = fake_connected_client().await;
        let register_handle = handle.clone();
        let register = tokio::spawn(async move {
            register_handle
                .register_service(service(2, "cancelled", 81))
                .await
        });
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), read_boxed(&mut peer))
                .await
                .unwrap()
                .unwrap(),
            Message::RegisterService(_)
        ));
        register.abort();
        write_boxed(
            &mut peer,
            &Message::RegisterAck(eggtunnel_proto::RegisterAck {
                service_id: ServiceId(2),
                effective_bind: EffectiveBind {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 31002,
                },
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), read_boxed(&mut peer))
                .await
                .unwrap()
                .unwrap(),
            Message::UnregisterService(eggtunnel_proto::UnregisterService {
                service_id: ServiceId(2)
            })
        ));
        assert_eq!(counters.snapshot().registered_services, 1);
        assert!(
            counters
                .snapshot()
                .effective_binds
                .iter()
                .all(|(_, service_id, _)| *service_id != ServiceId(2))
        );
        drop(peer);
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn command_from_an_older_session_generation_cannot_register() {
        let (handle, counters, task, mut peer) = fake_connected_client().await;
        let (reply, response) = oneshot::channel();
        let generation = counters
            .session_generation
            .load(std::sync::atomic::Ordering::Relaxed);
        handle
            .commands
            .send(ClientCommand::Register {
                service: service(2, "stale-generation", 82),
                generation: generation.saturating_sub(1),
                reply,
            })
            .await
            .unwrap();
        assert!(matches!(
            response.await.unwrap(),
            Err(TunnelError::Disconnected)
        ));
        assert_eq!(counters.snapshot().registered_services, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), read_boxed(&mut peer))
                .await
                .is_err()
        );
        handle.shutdown();
        assert!(task.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn dynamic_registration_ack_timeout_is_typed_and_closes_session() {
        let mut policy = crate::RuntimePolicy::default();
        policy.timeouts.handshake = Duration::from_millis(100);
        let (handle, _, task, mut peer) = fake_connected_client_with_policy(policy).await;
        let register_handle = handle.clone();
        let register = tokio::spawn(async move {
            register_handle
                .register_service(service(2, "no-ack", 82))
                .await
        });
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), read_boxed(&mut peer))
                .await
                .unwrap()
                .unwrap(),
            Message::RegisterService(_)
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), register)
                .await
                .unwrap()
                .unwrap(),
            Err(TunnelError::Timeout)
        ));
        assert!(matches!(task.await.unwrap(), Err(TunnelError::Timeout)));
    }

    #[tokio::test]
    async fn heartbeat_tracks_rtt_misses_and_recovery_with_one_probe() {
        let mut policy = crate::RuntimePolicy::default();
        policy.timeouts.control_idle = Duration::from_secs(2);
        policy.timeouts.heartbeat_interval = Duration::from_millis(50);
        let (handle, counters, task, mut peer) = fake_connected_client_with_policy(policy).await;
        let Message::Ping(first) =
            tokio::time::timeout(Duration::from_secs(1), read_boxed(&mut peer))
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected heartbeat Ping")
        };
        write_boxed(
            &mut peer,
            &Message::Pong(eggtunnel_proto::Pong { nonce: first.nonce }),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if counters.snapshot().heartbeat.latest_rtt_ms.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let Message::Ping(unanswered) = read_boxed(&mut peer).await.unwrap() else {
            panic!("expected next heartbeat Ping")
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if counters.snapshot().heartbeat.missed_heartbeats > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        write_boxed(
            &mut peer,
            &Message::Pong(eggtunnel_proto::Pong {
                nonce: unanswered.nonce,
            }),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if counters.snapshot().heartbeat.missed_heartbeats == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(counters.snapshot().heartbeat.last_pong_age_ms.is_some());
        handle.shutdown();
        drop(peer);
        let _ = task.await;
    }

    #[test]
    fn client_builder_applies_custom_service_ceiling() {
        let mut config = config("localhost:443");
        config.services.push(ClientService::new(
            ServiceId(2),
            ServiceName::new("two").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", 81).unwrap(),
        ));
        let mut policy = crate::common::RuntimePolicy::default();
        policy.limits.services_per_session = 1;
        let builder = ClientBuilder::new(config).runtime_policy(policy);
        assert!(builder.validate().is_err());
    }

    #[test]
    fn client_builder_accepts_tcp_tls_and_default_policy() {
        assert!(
            ClientBuilder::new(config("localhost:443"))
                .validate()
                .is_ok()
        );
        let mut dynamic_only = config("localhost:443");
        dynamic_only.services.clear();
        assert!(ClientBuilder::new(dynamic_only).validate().is_ok());
        let mut custom_ca = config("localhost:443");
        custom_ca.ca_pem = Some(b"custom CA".to_vec());
        assert!(ClientBuilder::new(custom_ca).validate().is_ok());
    }

    #[cfg(feature = "mtls")]
    #[test]
    fn client_builder_accepts_tcp_mtls() {
        let builder = ClientBuilder::new(config("localhost:443"))
            .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()));
        assert!(builder.validate().is_ok());
    }

    #[cfg(feature = "quic")]
    #[test]
    fn client_profile_validator_rejects_quic_custom_ca() {
        let mut config = config("localhost:443");
        config.ca_pem = Some(b"custom CA".to_vec());
        let builder = ClientBuilder::new(config).transport(ClientTransportProfile::Quic);
        assert!(builder.validate().is_err());
    }

    #[cfg(feature = "quic")]
    #[test]
    fn client_profile_validator_accepts_quic_defaults() {
        assert!(
            ClientBuilder::new(config("localhost:443"))
                .transport(ClientTransportProfile::Quic)
                .validate()
                .is_ok()
        );
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn client_profile_validator_accepts_websocket_defaults() {
        assert!(
            ClientBuilder::new(config("localhost:443"))
                .transport(ClientTransportProfile::WebSocket)
                .validate()
                .is_ok()
        );
    }

    #[cfg(all(feature = "quic", feature = "outbound-proxy"))]
    #[test]
    fn client_profile_validator_rejects_quic_proxy() {
        let builder = ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::Quic)
            .outbound_proxy("socks5://localhost:1080");
        assert!(builder.validate().is_err());
    }

    #[cfg(all(feature = "quic", feature = "mtls"))]
    #[test]
    fn client_profile_validator_rejects_quic_mtls() {
        let builder = ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::Quic)
            .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()));
        assert!(builder.validate().is_err());
    }

    #[cfg(all(feature = "websocket", feature = "outbound-proxy"))]
    #[test]
    fn client_profile_validator_accepts_websocket_proxy() {
        let builder = ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::WebSocket)
            .outbound_proxy("socks5://localhost:1080");
        assert!(builder.validate().is_ok());
    }

    #[cfg(all(feature = "websocket", feature = "mtls"))]
    #[test]
    fn client_profile_validator_rejects_websocket_mtls() {
        let builder = ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::WebSocket)
            .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()));
        assert!(builder.validate().is_err());
    }

    #[cfg(all(feature = "mtls", feature = "outbound-proxy"))]
    #[test]
    fn client_profile_validator_rejects_mtls_proxy() {
        let builder = ClientBuilder::new(config("localhost:443"))
            .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()))
            .outbound_proxy("socks5://localhost:1080");
        assert!(builder.validate().is_err());
    }
}
