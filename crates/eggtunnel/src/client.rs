use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use eggress_core::BoxStream;
use eggress_relay::{RelayOptions, relay_with_options};
use eggress_transport_tls::{TlsClientConfigBuilder, tls_connect};
use eggtunnel_proto::{
    Auth, AuthOk, Capabilities, ClientHello, DataHello, MAX_FRAME_BYTES, Message, Open, OpenReject,
    ProtocolVersion, RegisterService, ServerHello, ServiceId,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::{Semaphore, mpsc},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    common::{ClientService, Counters, SecretToken, Snapshot, TunnelError},
    wire_io::{read_boxed, read_message, write_boxed, write_message},
};

const MAX_SERVICES: usize = 64;
const MAX_OPEN_TASKS: usize = 128;
const CONTROL_QUEUE: usize = 128;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const RELAY_DRAIN: Duration = Duration::from_secs(15);
const SERVER_DRAIN_GRACE: Duration = Duration::from_secs(1);

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
}

enum ClientCommand {
    Unregister(ServiceId),
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
        self.commands
            .send(ClientCommand::Unregister(id))
            .await
            .map_err(|_| TunnelError::Cancelled)
    }
}

impl Client {
    pub async fn start(config: ClientConfig) -> Result<Self, TunnelError> {
        Self::start_with_connector(config, Arc::new(TcpTargetConnector)).await
    }

    /// Start a client using an application-provided target connector.
    pub async fn start_with_connector(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        let tls_config = build_tls_config(config.ca_pem.as_deref())?;
        Self::start_with_tls_config(config, connector, tls_config).await
    }

    #[cfg(feature = "mtls")]
    pub async fn start_with_mtls(
        config: ClientConfig,
        identity: ClientIdentity,
    ) -> Result<Self, TunnelError> {
        let tls_config = build_mtls_tls_config(&config, identity)?;
        Self::start_with_tls_config(config, Arc::new(TcpTargetConnector), tls_config).await
    }

    #[cfg(feature = "mtls")]
    pub async fn start_with_mtls_and_connector(
        config: ClientConfig,
        identity: ClientIdentity,
        connector: Arc<dyn TargetConnector>,
    ) -> Result<Self, TunnelError> {
        let tls_config = build_mtls_tls_config(&config, identity)?;
        Self::start_with_tls_config(config, connector, tls_config).await
    }

    async fn start_with_tls_config(
        config: ClientConfig,
        connector: Arc<dyn TargetConnector>,
        tls_config: Arc<rustls::ClientConfig>,
    ) -> Result<Self, TunnelError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Client::start requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config)?;
        let cancel = CancellationToken::new();
        let counters = Counters::default();
        let (command_tx, command_rx) = mpsc::channel(32);
        let handle = ClientHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
            commands: command_tx,
        };
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            reconnect_loop(
                config,
                tls_config,
                connector,
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
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
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

fn validate_config(config: &ClientConfig) -> Result<(), TunnelError> {
    if !valid_endpoint(&config.server_addr) {
        return Err(TunnelError::Configuration(
            "server_addr must be a host:port endpoint",
        ));
    }
    if config.tls_server_name.is_empty() || config.tls_server_name.len() > 253 {
        return Err(TunnelError::Configuration("TLS server name is invalid"));
    }
    if config.services.is_empty() || config.services.len() > MAX_SERVICES {
        return Err(TunnelError::Configuration("service count must be 1..=64"));
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
    use std::io::Cursor;

    let mut roots = rustls::RootCertStore::empty();
    if let Some(ca_pem) = config.ca_pem.as_deref() {
        let ca_certs = rustls_pemfile::certs(&mut Cursor::new(ca_pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TunnelError::Tls)?;
        for cert in ca_certs {
            roots.add(cert).map_err(|_| TunnelError::Tls)?;
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    let certificates = rustls_pemfile::certs(&mut Cursor::new(&identity.certificate_pem))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TunnelError::Tls)?;
    let private_key = rustls_pemfile::private_key(&mut Cursor::new(&identity.private_key_pem))
        .map_err(|_| TunnelError::Tls)?
        .ok_or(TunnelError::Tls)?;
    if certificates.is_empty() {
        return Err(TunnelError::Tls);
    }
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certificates, private_key)
        .map_err(|_| TunnelError::Tls)?;
    Ok(Arc::new(tls))
}

async fn reconnect_loop(
    mut config: ClientConfig,
    tls: Arc<rustls::ClientConfig>,
    connector: Arc<dyn TargetConnector>,
    cancel: CancellationToken,
    counters: Counters,
    mut commands: mpsc::Receiver<ClientCommand>,
) {
    let mut delay = Duration::from_millis(500);
    'reconnect: loop {
        if cancel.is_cancelled() {
            break;
        }
        while let Ok(command) = commands.try_recv() {
            apply_client_command(&mut config.services, command);
        }
        let outcome = tokio::select! {
            _ = cancel.cancelled() => break,
            result = timeout(CONNECT_TIMEOUT, TcpStream::connect(config.server_addr.as_str())) => result,
        };
        let result = match outcome {
            Ok(Ok(tcp)) => {
                let stream: BoxStream = Box::new(tcp);
                let tls_result = tokio::select! {
                    _ = cancel.cancelled() => break 'reconnect,
                    result = timeout(HANDSHAKE_TIMEOUT, tls_connect(stream, tls.clone(), &config.tls_server_name)) => result,
                };
                match tls_result {
                    Ok(Ok(stream)) => {
                        tokio::select! {
                            _ = cancel.cancelled() => Err(TunnelError::Cancelled),
                            result = run_session(stream, SessionRun {
                                server_addr: &config.server_addr,
                                tls_server_name: &config.tls_server_name,
                                token: &config.token,
                                tls: tls.clone(),
                                connector: connector.clone(),
                                cancel: &cancel,
                                counters: &counters,
                                reconnect_delay: &mut delay,
                                services: &mut config.services,
                                commands: &mut commands,
                            }) => result,
                        }
                    }
                    _ => Err(TunnelError::Tls),
                }
            }
            _ => Err(TunnelError::Disconnected),
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
        delay = (delay * 2).min(Duration::from_secs(30));
    }
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
    server_addr: &'a str,
    tls_server_name: &'a str,
    token: &'a SecretToken,
    tls: Arc<rustls::ClientConfig>,
    connector: Arc<dyn TargetConnector>,
    cancel: &'a CancellationToken,
    counters: &'a Counters,
    reconnect_delay: &'a mut Duration,
    services: &'a mut Vec<ClientService>,
    commands: &'a mut mpsc::Receiver<ClientCommand>,
}

async fn run_session(mut stream: BoxStream, context: SessionRun<'_>) -> Result<(), TunnelError> {
    let SessionRun {
        server_addr,
        tls_server_name,
        token,
        tls,
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
    )
    .await?;
    match handshake_read(&mut stream).await? {
        Message::ServerHello(ServerHello { version, .. })
            if version.major == ProtocolVersion::CURRENT.major => {}
        _ => {
            return Err(TunnelError::Protocol(
                eggtunnel_proto::ProtocolError::UnexpectedMessage,
            ));
        }
    }
    let auth = Auth::new(token.expose().to_vec())?;
    handshake_write(&mut stream, &Message::Auth(auth)).await?;
    let session_id = match handshake_read(&mut stream).await? {
        Message::AuthOk(AuthOk { session_id }) => session_id,
        _ => return Err(TunnelError::Authentication),
    };

    for service in services.iter() {
        let registration = RegisterService {
            service_id: service.id,
            name: service.name.clone(),
            requested_bind: service.requested_bind.clone(),
            target: service.target.clone(),
        };
        handshake_write(&mut stream, &Message::RegisterService(registration)).await?;
        match handshake_read(&mut stream).await? {
            Message::RegisterAck(ack) if ack.service_id == service.id => {
                counters
                    .binds
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((session_id, service.id, ack.effective_bind));
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
    *reconnect_delay = Duration::from_millis(500);
    let _connected_guard = CounterGuard::new(counters.sessions.clone());
    let mut active_services: HashMap<ServiceId, ClientService> =
        services.iter().cloned().map(|s| (s.id, s)).collect();
    let semaphore = Arc::new(Semaphore::new(MAX_OPEN_TASKS));
    let (out_tx, mut out_rx) = mpsc::channel(CONTROL_QUEUE);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut opens = JoinSet::new();
    let session_cancel = CancellationToken::new();
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(20),
        Duration::from_secs(20),
    );
    let mut heartbeat_nonce = 0u64;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                session_cancel.cancel();
                let drain = Message::Drain(eggtunnel_proto::Drain { deadline_ms: RELAY_DRAIN.as_millis() as u32 });
                let _ = timeout(Duration::from_millis(250), write_message(&mut writer, &drain)).await;
                break;
            }
            _ = heartbeat.tick() => {
                heartbeat_nonce = heartbeat_nonce.wrapping_add(1);
                let _ = out_tx.try_send(Message::Ping(eggtunnel_proto::Ping { nonce: heartbeat_nonce }));
            }
            incoming = read_message(&mut reader) => {
                match incoming {
                    Ok(Message::Open(open)) => {
                        let Some(service) = active_services.get(&open.service_id).cloned() else {
                            counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            counters.record_termination(crate::common::TerminationCategory::Authorization);
                            let _ = out_tx.try_send(Message::OpenReject(OpenReject { connection_id: open.connection_id, code: 1 }));
                            continue;
                        };
                        let permit = semaphore.clone().try_acquire_owned();
                        let Ok(permit) = permit else {
                            counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            counters.record_termination(crate::common::TerminationCategory::ResourceExhausted);
                            let _ = out_tx.try_send(Message::OpenReject(OpenReject { connection_id: open.connection_id, code: 2 }));
                            continue;
                        };
                        let service_cancel = session_cancel.child_token();
                        let server_addr = server_addr.to_owned();
                        let server_name = tls_server_name.to_owned();
                        let out = out_tx.clone();
                        let open_tls = tls.clone();
                        let open_guard = OpenTaskGuard::new(
                            counters.open_tasks.clone(),
                            counters.high_water_open_tasks.clone(),
                        );
                        let context = OpenContext {
                            server_addr,
                            server_name,
                            tls: open_tls,
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
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Drain(_)) => {
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
                    ClientCommand::Unregister(id) => {
                        let was_present = active_services.remove(&id).is_some();
                        if was_present {
                            apply_client_command(services, ClientCommand::Unregister(id));
                            counters.services.store(active_services.len(), std::sync::atomic::Ordering::Relaxed);
                            counters.binds.lock().unwrap_or_else(|p| p.into_inner()).retain(|(_, service_id, _)| *service_id != id);
                            write_message(&mut writer, &Message::UnregisterService(eggtunnel_proto::UnregisterService { service_id: id })).await?;
                        }
                    }
                }
            }
            Some(result) = opens.join_next(), if !opens.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    let _ = timeout(SERVER_DRAIN_GRACE, async {
        while opens.join_next().await.is_some() {}
    })
    .await;
    opens.abort_all();
    while opens.join_next().await.is_some() {}
    counters
        .connected
        .store(0, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

fn apply_client_command(services: &mut Vec<ClientService>, command: ClientCommand) {
    match command {
        ClientCommand::Unregister(id) => services.retain(|service| service.id != id),
    }
}

async fn handshake_read(stream: &mut BoxStream) -> Result<Message, TunnelError> {
    timeout(HANDSHAKE_TIMEOUT, read_boxed(stream))
        .await
        .map_err(|_| TunnelError::Disconnected)?
        .map_err(Into::into)
}

async fn handshake_write(stream: &mut BoxStream, message: &Message) -> Result<(), TunnelError> {
    timeout(HANDSHAKE_TIMEOUT, write_boxed(stream, message))
        .await
        .map_err(|_| TunnelError::Disconnected)?
        .map_err(Into::into)
}

struct OpenContext {
    server_addr: String,
    server_name: String,
    tls: Arc<rustls::ClientConfig>,
    connector: Arc<dyn TargetConnector>,
    session_id: eggtunnel_proto::SessionId,
    cancel: CancellationToken,
    out: mpsc::Sender<Message>,
    counters: Counters,
}

async fn handle_open(open: Open, service: ClientService, context: OpenContext) {
    let OpenContext {
        server_addr,
        server_name,
        tls,
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
            result = timeout(CONNECT_TIMEOUT, connector.connect(service.clone(), target_context)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Target)?,
        };
        let tcp = tokio::select! {
            _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
            result = timeout(CONNECT_TIMEOUT, TcpStream::connect(server_addr.as_str())) => result.map_err(|_| TunnelError::Disconnected)??,
        };
        let stream: BoxStream = Box::new(tcp);
        let mut data = tokio::select! {
            _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
            result = timeout(HANDSHAKE_TIMEOUT, tls_connect(stream, tls, &server_name)) => result.map_err(|_| TunnelError::Tls)?.map_err(|_| TunnelError::Tls)?,
        };
        write_boxed(&mut data, &Message::DataHello(DataHello { session_id, service_id: service.id, connection_id: open.connection_id })).await?;
        match relay_with_options(target, data, RelayOptions::bounded(std::num::NonZeroUsize::new(16 * 1024).unwrap(), RELAY_DRAIN)).await {
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
    if result.is_err() && !cancel.is_cancelled() {
        let _ = out.try_send(Message::OpenReject(OpenReject {
            connection_id: open.connection_id,
            code: 1,
        }));
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
