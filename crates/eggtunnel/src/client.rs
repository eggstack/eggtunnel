use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

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

mod config;
pub use config::{
    ApplicationStream, ClientBuilder, ClientConfig, ClientTransportProfile, TargetConnector,
    TargetContext, TargetError, TargetFuture, TargetStream,
};
mod service_state;
use service_state::{AckDisposition, ServiceState};
mod heartbeat;
use heartbeat::HeartbeatState;
mod open;
use open::{OpenContext, handle_open};

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
            Arc::new(config::TcpTargetConnector),
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
    let mut service_state = ServiceState::new(std::mem::take(&mut config.services));
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
            apply_disconnected_command(&mut service_state, command);
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
                                service_state: &mut service_state,
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

    let mut service_state = ServiceState::new(std::mem::take(&mut config.services));
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
            apply_disconnected_command(&mut service_state, command);
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
                    service_state: &mut service_state,
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
    service_state: &'a mut ServiceState,
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
        service_state,
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

    service_state.clear_active();
    for service in service_state.desired().iter() {
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

    service_state.activate_initial();
    counters
        .connected
        .store(1, std::sync::atomic::Ordering::Relaxed);
    counters.services.store(
        service_state.desired().len(),
        std::sync::atomic::Ordering::Relaxed,
    );
    counters
        .sessions
        .store(1, std::sync::atomic::Ordering::Relaxed);
    *reconnect_delay = counters.policy.timeouts.reconnect_initial;
    let _connected_guard = CounterGuard::new(counters.sessions.clone());
    tracing::info!(
        session_generation = generation,
        registered_services = service_state.desired().len(),
        "client Session ready"
    );
    let semaphore = Arc::new(Semaphore::new(counters.policy.limits.client_open_tasks));
    let (out_tx, mut out_rx) = mpsc::channel(counters.policy.limits.control_queue);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut opens = JoinSet::new();
    let session_cancel = CancellationToken::new();
    let mut heartbeat_ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + counters.policy.timeouts.heartbeat_interval,
        counters.policy.timeouts.heartbeat_interval,
    );
    let mut heartbeat = HeartbeatState::new();
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
            _ = heartbeat_ticker.tick() => {
                if heartbeat.has_outstanding() {
                    counters.record_heartbeat_missed();
                    tracing::debug!(session_generation = generation, "heartbeat response missed");
                } else {
                    let nonce = heartbeat.next_nonce();
                    match out_tx.try_send(Message::Ping(eggtunnel_proto::Ping { nonce })) {
                        Ok(()) => heartbeat.mark_sent(nonce, tokio::time::Instant::now()),
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
                if let Some(pending) = service_state.pending_mut()
                    && let Some(reply) = pending.reply.take()
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
                        let Some(service) = service_state.active().get(&open.service_id).cloned() else {
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
                        if let Some(sent_at) = heartbeat.matching_pong(pong.nonce)
                        {
                            counters.record_heartbeat_pong(sent_at.into_std());
                        }
                    }
                    Ok(Message::RegisterAck(ack)) => {
                        registration_deadline = None;
                        let (service, reply) = match service_state.take_ack(ack.service_id, generation) {
                            AckDisposition::Unexpected => return Err(TunnelError::Protocol(eggtunnel_proto::ProtocolError::UnexpectedMessage)),
                            AckDisposition::Stale(reply) => { if let Some(reply) = reply { let _ = reply.send(Err(TunnelError::Disconnected)); } continue; }
                            AckDisposition::Abandoned => {
                                write_message(&mut writer, &Message::UnregisterService(eggtunnel_proto::UnregisterService { service_id: ack.service_id })).await?;
                                continue;
                            }
                            AckDisposition::Commit(service, reply) => (service, reply),
                        };
                        counters.binds.lock().unwrap_or_else(|p| p.into_inner()).push((session_id, service.id, ack.effective_bind.clone()));
                        service_state.commit(service.clone());
                        counters.services.store(service_state.active().len(), std::sync::atomic::Ordering::Relaxed);
                        counters.high_water_services.fetch_max(service_state.active().len(), std::sync::atomic::Ordering::Relaxed);
                        tracing::info!(service_id = service.id.0, service_name = service.name.as_str(), effective_address = %std::net::Ipv6Addr::from(ack.effective_bind.address), effective_port = ack.effective_bind.port, session_generation = generation, "Service registered");
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(ack.effective_bind));
                        }
                    }
                    Ok(Message::Error(error)) => {
                        registration_deadline = None;
                        if let Some(reply) = service_state.reject() {
                            let error = registration_error(error);
                            tracing::debug!(registration_error = ?error.termination_category(), session_generation = generation, "Service registration rejected");
                            if let Some(reply) = reply { let _ = reply.send(Err(error)); }
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
                        if service_state.desired().len().saturating_add(usize::from(service_state.pending().is_some())) >= counters.policy.limits.services_per_session {
                            let _ = reply.send(Err(TunnelError::ResourceExhausted));
                            continue;
                        }
                        if reply.is_closed() {
                            continue;
                        }
                        if let Err((error, reply)) = service_state.begin(service.clone(), command_generation, reply) {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        let registration = RegisterService {
                            service_id: service.id,
                            name: service.name.clone(),
                            requested_bind: service.requested_bind.clone(),
                            target: service.target.clone(),
                        };
                        match timeout(
                            counters.policy.timeouts.handshake,
                            write_message(&mut writer, &Message::RegisterService(registration)),
                        )
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => return Err(error.into()),
                            Err(_) => {
                                if let Some(reply) = service_state.pending_mut().and_then(|pending| pending.reply.take()) { let _ = reply.send(Err(TunnelError::Timeout)); }
                                registration_timed_out = true;
                                break;
                            }
                        }
                        registration_deadline = Some(
                            tokio::time::Instant::now() + counters.policy.timeouts.handshake,
                        );
                    }
                    ClientCommand::Unregister { id, reply } => {
                        let was_present = service_state.unregister(id);
                        if was_present {
                            counters.services.store(service_state.active().len(), std::sync::atomic::Ordering::Relaxed);
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
    service_state.finish_pending(match pending_error {
        TunnelError::Cancelled => TunnelError::Cancelled,
        _ => TunnelError::Disconnected,
    });
    service_state.clear_active();
    if registration_timed_out {
        Err(TunnelError::Timeout)
    } else {
        Ok(())
    }
}

fn apply_disconnected_command(services: &mut ServiceState, command: ClientCommand) {
    match command {
        ClientCommand::Register { reply, .. } => {
            let _ = reply.send(Err(TunnelError::Disconnected));
        }
        ClientCommand::Unregister { id, reply } => {
            services.unregister(id);
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
#[path = "client/tests.rs"]
mod tests;
