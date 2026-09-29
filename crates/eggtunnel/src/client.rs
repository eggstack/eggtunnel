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
    endpoint::Endpoint,
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
mod reconnect;
#[cfg(feature = "quic-client")]
use reconnect::QuicTransport;
use reconnect::{Driver, ReconnectSupervisor, StreamTransport, drive};

#[derive(Clone)]
enum ClientDataTransport {
    TcpTls {
        endpoint: Endpoint,
        server_name: String,
        tls: Arc<rustls::ClientConfig>,
        websocket: bool,
        #[cfg(feature = "outbound-proxy")]
        outbound: Option<Arc<eggress_outbound::OutboundConnector>>,
    },
    #[cfg(feature = "quic-client")]
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
    #[cfg(feature = "quic-client")]
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

    #[cfg(all(test, feature = "quic-client"))]
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
            #[cfg(feature = "quic-client")]
            ClientTransportProfile::Quic => {
                Self::start_quic_profile(config, connector, false, policy).await
            }
            #[cfg(feature = "websocket-client")]
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

    #[cfg(feature = "websocket-client")]
    pub async fn start_websocket(config: ClientConfig) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .transport(ClientTransportProfile::WebSocket)
            .start()
            .await
    }

    #[cfg(feature = "websocket-client")]
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

    #[cfg(all(feature = "outbound-proxy", feature = "websocket-client"))]
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

    #[cfg(all(feature = "outbound-proxy", feature = "websocket-client"))]
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

    #[cfg(feature = "quic-client")]
    pub async fn start_quic(config: ClientConfig) -> Result<Self, TunnelError> {
        ClientBuilder::new(config)
            .transport(ClientTransportProfile::Quic)
            .start()
            .await
    }

    #[cfg(feature = "quic-client")]
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

    #[cfg(feature = "quic-client")]
    async fn start_quic_profile(
        mut config: ClientConfig,
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
        let endpoint = Endpoint::parse(&config.server_addr)
            .map_err(|_| TunnelError::Configuration("server_addr must be a host:port endpoint"))?;
        let cancel = CancellationToken::new();
        let counters = Counters::with_policy(policy);
        let (command_tx, command_rx) = mpsc::channel(policy.limits.client_command_queue);
        let handle = ClientHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
            commands: command_tx,
            #[cfg(feature = "quic-client")]
            quic_client: Arc::new(std::sync::Mutex::new(None)),
        };
        let task_cancel = cancel.clone();
        let task_token = config.token.clone();
        let task_endpoint = endpoint;
        let task_name = config.tls_server_name.clone();
        let task_timeouts = policy.timeouts;
        let task_streams = policy.limits.client_open_tasks;
        let task_slot = handle.quic_client.clone();
        let mut command_rx = command_rx;
        let task = tokio::spawn(async move {
            let supervisor = ReconnectSupervisor::new(
                std::mem::take(&mut config.services),
                task_timeouts.reconnect_initial,
            );
            drive(
                QuicTransport::new(
                    task_endpoint,
                    task_name,
                    insecure,
                    task_timeouts,
                    task_streams,
                    task_slot,
                ),
                supervisor,
                Driver {
                    token: &task_token,
                    connector,
                    cancel: &task_cancel,
                    counters: &counters,
                    commands: &mut command_rx,
                },
            )
            .await;
        });
        Ok(Self {
            cancel,
            task: Some(task),
            handle,
        })
    }

    #[cfg(all(test, feature = "quic-client"))]
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

    #[cfg(all(test, feature = "quic-client"))]
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
        mut config: ClientConfig,
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
        let endpoint = Endpoint::parse(&config.server_addr)
            .map_err(|_| TunnelError::Configuration("server_addr must be a host:port endpoint"))?;
        let cancel = CancellationToken::new();
        let counters = Counters::with_policy(policy);
        let (command_tx, command_rx) = mpsc::channel(policy.limits.client_command_queue);
        let handle = ClientHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
            commands: command_tx,
            #[cfg(feature = "quic-client")]
            quic_client: Arc::new(std::sync::Mutex::new(None)),
        };
        let task_cancel = cancel.clone();
        let task_token = config.token.clone();
        let task_name = config.tls_server_name.clone();
        let task_timeouts = policy.timeouts;
        let mut task_services = std::mem::take(&mut config.services);
        let mut command_rx = command_rx;
        let task = tokio::spawn(async move {
            let supervisor = ReconnectSupervisor::new(
                std::mem::take(&mut task_services),
                task_timeouts.reconnect_initial,
            );
            drive(
                StreamTransport::new(
                    endpoint,
                    task_name,
                    tls_config,
                    websocket,
                    task_timeouts,
                    task_cancel.clone(),
                    #[cfg(feature = "outbound-proxy")]
                    outbound,
                ),
                supervisor,
                Driver {
                    token: &task_token,
                    connector,
                    cancel: &task_cancel,
                    counters: &counters,
                    commands: &mut command_rx,
                },
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
    profile: &ClientTransportProfile,
    #[cfg(feature = "mtls")] has_identity: bool,
    #[cfg(feature = "outbound-proxy")] outbound_proxy: Option<&str>,
    policy: &crate::common::RuntimePolicy,
) -> Result<(), TunnelError> {
    policy.validate()?;
    validate_config(config, policy)?;
    let _ = profile;
    #[cfg(feature = "mtls")]
    let _has_identity = has_identity;
    #[cfg(not(feature = "mtls"))]
    let _has_identity = false;
    #[cfg(feature = "outbound-proxy")]
    let _has_outbound_proxy = outbound_proxy.is_some();
    #[cfg(not(feature = "outbound-proxy"))]
    let _has_outbound_proxy = false;
    #[cfg(feature = "quic-client")]
    if matches!(profile, ClientTransportProfile::Quic)
        && (config.ca_pem.is_some() || _has_identity || _has_outbound_proxy)
    {
        return Err(TunnelError::Configuration(
            "Eggress QUIC currently supports platform roots and bearer auth only",
        ));
    }
    #[cfg(feature = "websocket-client")]
    if matches!(profile, ClientTransportProfile::WebSocket) && _has_identity {
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
    Ok(())
}

fn validate_config(
    config: &ClientConfig,
    policy: &crate::common::RuntimePolicy,
) -> Result<(), TunnelError> {
    Endpoint::parse(&config.server_addr)
        .map_err(|_| TunnelError::Configuration("server_addr must be a host:port endpoint"))?;
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

struct SessionRun<'a> {
    token: &'a SecretToken,
    transport: ClientDataTransport,
    connector: Arc<dyn TargetConnector>,
    cancel: &'a CancellationToken,
    counters: &'a Counters,
    /// Desired-Service state and the in-flight registration transaction.
    service_state: &'a mut ServiceState,
    /// Reconnect backoff progression; reset by the ready Session.
    reconnect_delay: &'a mut Duration,
    commands: &'a mut mpsc::Receiver<ClientCommand>,
}

async fn run_session(mut stream: BoxStream, context: SessionRun<'_>) -> Result<(), TunnelError> {
    let SessionRun {
        token,
        transport,
        connector,
        cancel,
        counters,
        service_state,
        reconnect_delay,
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
#[path = "client/qualification_tests.rs"]
mod qualification_tests;
#[cfg(test)]
#[path = "client/tests.rs"]
mod tests;
