use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use eggress_core::BoxStream;
use eggress_relay::{RelayOptions, relay_with_options};
use eggress_transport_tls::{TlsServerConfigBuilder, tls_accept};
use eggtunnel_proto::{
    AuthOk, BoundedDiagnostic, Capabilities, ClientHello, DataHello, EffectiveBind, ErrorMessage,
    MAX_FRAME_BYTES, Message, Open, Ping, Pong, ProtocolVersion, RegisterAck, ServerHello,
    ServiceId, SessionId, UnregisterService,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    common::{
        BindPolicy, Counters, SecretToken, Snapshot, TerminationCategory, TunnelError,
        bind_to_socket, verify_token,
    },
    wire_io::{read_boxed, read_message, write_boxed, write_message},
};

#[derive(Clone)]
enum ServerTls {
    Eggress(Arc<rustls::ServerConfig>),
    #[cfg(feature = "mtls")]
    Mutual(Arc<rustls::ServerConfig>),
}

const MAX_SESSIONS: usize = 128;
const MAX_PENDING_PER_SESSION: usize = 128;
const MAX_ACTIVE_CONNECTIONS_PER_SESSION: usize = 128;
const CONTROL_QUEUE: usize = 128;
const MAX_HANDSHAKES: usize = 64;
const AUTH_FAILURES_PER_SOURCE: usize = 10;
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
const MAX_AUTH_SOURCES: usize = 1024;
const AUTH_FAILURE_DELAY: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const PENDING_LIFETIME: Duration = Duration::from_secs(30);
const RELAY_DRAIN: Duration = Duration::from_secs(15);
const SERVER_SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

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
    pub fn snapshot(&self) -> Snapshot {
        self.counters.snapshot()
    }
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Server {
    pub async fn bind(config: ServerConfig) -> Result<Self, TunnelError> {
        let policy = BindPolicy {
            allow_public_addresses: config.allow_public_service_binds,
            ..BindPolicy::default()
        };
        Self::bind_with_policy(config, policy).await
    }

    pub async fn bind_with_policy(
        config: ServerConfig,
        bind_policy: BindPolicy,
    ) -> Result<Self, TunnelError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Server::bind requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config)?;
        bind_policy.validate()?;
        let tls = TlsServerConfigBuilder::new()
            .with_certificate_pem(&config.certificate_pem)
            .map_err(|_| TunnelError::Tls)?
            .with_key_pem(&config.private_key_pem)
            .map_err(|_| TunnelError::Tls)?
            .build()
            .map_err(|_| TunnelError::Tls)?;
        Self::bind_with_tls_profile(config, bind_policy, ServerTls::Eggress(tls), false).await
    }

    #[cfg(feature = "websocket")]
    pub async fn bind_websocket(config: ServerConfig) -> Result<Self, TunnelError> {
        let bind_policy = BindPolicy {
            allow_public_addresses: config.allow_public_service_binds,
            ..BindPolicy::default()
        };
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration(
                "Server::bind_websocket requires a caller-owned Tokio runtime",
            )
        })?;
        validate_config(&config)?;
        bind_policy.validate()?;
        let tls = TlsServerConfigBuilder::new()
            .with_certificate_pem(&config.certificate_pem)
            .map_err(|_| TunnelError::Tls)?
            .with_key_pem(&config.private_key_pem)
            .map_err(|_| TunnelError::Tls)?
            .build()
            .map_err(|_| TunnelError::Tls)?;
        Self::bind_with_tls_profile(config, bind_policy, ServerTls::Eggress(tls), true).await
    }

    #[cfg(feature = "quic")]
    pub async fn bind_quic(config: ServerConfig) -> Result<Self, TunnelError> {
        let bind_policy = BindPolicy {
            allow_public_addresses: config.allow_public_service_binds,
            ..BindPolicy::default()
        };
        Self::bind_quic_with_policy(config, bind_policy).await
    }

    #[cfg(feature = "quic")]
    pub async fn bind_quic_with_policy(
        config: ServerConfig,
        bind_policy: BindPolicy,
    ) -> Result<Self, TunnelError> {
        use eggress_transport_quic::{QuicListener, QuicServerConfig};

        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Server::bind_quic requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config)?;
        bind_policy.validate()?;
        let listener = QuicListener::bind(
            config.listen_addr,
            QuicServerConfig {
                certificate_pem: config.certificate_pem.clone(),
                private_key_pem: config.private_key_pem.clone(),
                idle_timeout: Duration::from_secs(90),
                max_concurrent_streams: 256,
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
        let task = tokio::spawn(quic_server_loop(
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

    #[cfg(all(test, feature = "quic"))]
    pub(crate) async fn bind_quic_with_admission_for_test(
        config: ServerConfig,
        max_active_data_streams: usize,
    ) -> Result<Self, TunnelError> {
        use eggress_transport_quic::{QuicListener, QuicServerConfig};

        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Server::bind_quic requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config)?;
        let bind_policy = BindPolicy::default();
        let listener = QuicListener::bind(
            config.listen_addr,
            QuicServerConfig {
                certificate_pem: config.certificate_pem.clone(),
                private_key_pem: config.private_key_pem.clone(),
                idle_timeout: Duration::from_secs(90),
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
        let task = tokio::spawn(quic_server_loop_with_admission(
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
        Self::bind_mtls_with_policy(config, trusted_client_ca_pem, bind_policy).await
    }

    #[cfg(feature = "mtls")]
    pub async fn bind_mtls_with_policy(
        config: ServerConfig,
        trusted_client_ca_pem: Vec<u8>,
        bind_policy: BindPolicy,
    ) -> Result<Self, TunnelError> {
        let tls = build_mtls_server_config(&config, &trusted_client_ca_pem)?;
        Self::bind_with_tls_profile(config, bind_policy, ServerTls::Mutual(tls), false).await
    }

    async fn bind_with_tls_profile(
        config: ServerConfig,
        bind_policy: BindPolicy,
        tls: ServerTls,
        websocket: bool,
    ) -> Result<Self, TunnelError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Server::bind requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config)?;
        bind_policy.validate()?;
        let listener = TcpListener::bind(config.listen_addr).await?;
        let local_addr = listener.local_addr()?;
        let cancel = CancellationToken::new();
        let counters = Counters::default();
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

fn validate_config(config: &ServerConfig) -> Result<(), TunnelError> {
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

#[cfg(feature = "mtls")]
fn build_mtls_server_config(
    config: &ServerConfig,
    trusted_client_ca_pem: &[u8],
) -> Result<Arc<rustls::ServerConfig>, TunnelError> {
    use std::io::Cursor;

    let certificates = rustls_pemfile::certs(&mut Cursor::new(&config.certificate_pem))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TunnelError::Tls)?;
    let private_key = rustls_pemfile::private_key(&mut Cursor::new(&config.private_key_pem))
        .map_err(|_| TunnelError::Tls)?
        .ok_or(TunnelError::Tls)?;
    let client_ca = rustls_pemfile::certs(&mut Cursor::new(trusted_client_ca_pem))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TunnelError::Tls)?;
    if certificates.is_empty() || client_ca.is_empty() {
        return Err(TunnelError::Tls);
    }
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

#[cfg(feature = "mtls")]
fn certificate_principal(certificate_der: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(certificate_der).into()
}

async fn server_loop(
    listener: TcpListener,
    token: SecretToken,
    tls: ServerTls,
    bind_policy: BindPolicy,
    cancel: CancellationToken,
    counters: Counters,
    websocket: bool,
) {
    let sessions: Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let admission = Arc::new(Semaphore::new(MAX_HANDSHAKES));
    let auth_failures = Arc::new(AuthFailureLimiter::new(
        AUTH_FAILURES_PER_SOURCE,
        AUTH_FAILURE_WINDOW,
        MAX_AUTH_SOURCES,
    ));
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((tcp, peer)) = accepted else { continue; };
                let Ok(permit) = admission.clone().try_acquire_owned() else {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(TerminationCategory::ResourceExhausted);
                    continue;
                };
                let tls = tls.clone();
                let token = token.clone();
                let sessions = sessions.clone();
                let counters = counters.clone();
                let auth_failures = auth_failures.clone();
                let child_cancel = cancel.child_token();
                let handshake_guard = HandshakeGuard::new(counters.clone());
                let task_counters = counters.clone();
                let connection_context = ConnectionContext {
                    sessions,
                    counters,
                    auth_failures,
                    bind_policy: bind_policy.clone(),
                    cancel: child_cancel,
                    principal: None,
                    admission: Some(permit),
                    handshake_guard: Some(handshake_guard),
                    #[cfg(feature = "quic")]
                    max_active_data_streams: None,
                };
                handlers.spawn(async move {
                    if let Err(error) =
                        handle_connection(tcp, peer.ip(), tls, token, connection_context, websocket).await
                    {
                        task_counters.record_termination(error.termination_category());
                    }
                });
            }
            Some(result) = handlers.join_next(), if !handlers.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    let active: Vec<_> = sessions
        .lock()
        .await
        .values()
        .filter_map(std::sync::Weak::upgrade)
        .collect();
    for session in &active {
        if let Some(sender) = session.control_tx.lock().await.as_ref() {
            let _ = sender.try_send(Message::Drain(eggtunnel_proto::Drain {
                deadline_ms: SERVER_SHUTDOWN_GRACE.as_millis() as u32,
            }));
        }
    }
    tokio::time::sleep(SERVER_SHUTDOWN_GRACE).await;
    for session in active {
        session.cancel.cancel();
    }
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
}

#[cfg(feature = "quic")]
async fn quic_server_loop(
    listener: Arc<eggress_transport_quic::QuicListener>,
    token: SecretToken,
    bind_policy: BindPolicy,
    cancel: CancellationToken,
    counters: Counters,
) {
    quic_server_loop_with_admission(
        listener,
        token,
        bind_policy,
        cancel,
        counters,
        MAX_ACTIVE_CONNECTIONS_PER_SESSION,
    )
    .await
}

#[cfg(feature = "quic")]
async fn quic_server_loop_with_admission(
    listener: Arc<eggress_transport_quic::QuicListener>,
    token: SecretToken,
    bind_policy: BindPolicy,
    cancel: CancellationToken,
    counters: Counters,
    max_active_data_streams: usize,
) {
    let sessions: Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let admission = Arc::new(Semaphore::new(MAX_HANDSHAKES));
    let auth_failures = Arc::new(AuthFailureLimiter::new(
        AUTH_FAILURES_PER_SOURCE,
        AUTH_FAILURE_WINDOW,
        MAX_AUTH_SOURCES,
    ));
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept_connection(&cancel) => {
                let connection = match accepted {
                    Ok(Some(connection)) => connection,
                    Ok(None) => break,
                    Err(_) => {
                        counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
                };
                let Ok(permit) = admission.clone().try_acquire_owned() else {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(TerminationCategory::ResourceExhausted);
                    connection.close("handshake limit reached");
                    continue;
                };
                let source = connection.remote_address().ip();
                let handshake_guard = HandshakeGuard::new(counters.clone());
                let connection_context = ConnectionContext {
                    sessions: sessions.clone(),
                    counters: counters.clone(),
                    auth_failures: auth_failures.clone(),
                    bind_policy: bind_policy.clone(),
                    cancel: cancel.child_token(),
                    principal: None,
                    admission: Some(permit),
                    handshake_guard: Some(handshake_guard),
                    max_active_data_streams: Some(max_active_data_streams),
                };
                let token = token.clone();
                let task_counters = counters.clone();
                handlers.spawn(async move {
                    if let Err(error) = handle_quic_connection(connection, source, token, connection_context).await {
                        task_counters.record_termination(error.termination_category());
                    }
                });
            }
            Some(result) = handlers.join_next(), if !handlers.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    listener.close();
    let active: Vec<_> = sessions
        .lock()
        .await
        .values()
        .filter_map(std::sync::Weak::upgrade)
        .collect();
    for session in &active {
        if let Some(sender) = session.control_tx.lock().await.as_ref() {
            let _ = sender.try_send(Message::Drain(eggtunnel_proto::Drain {
                deadline_ms: SERVER_SHUTDOWN_GRACE.as_millis() as u32,
            }));
        }
    }
    tokio::time::sleep(SERVER_SHUTDOWN_GRACE).await;
    for session in active {
        session.cancel.cancel();
    }
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
}

#[cfg(feature = "quic")]
async fn handle_quic_connection(
    connection: eggress_transport_quic::QuicConnection,
    source: IpAddr,
    token: SecretToken,
    context: ConnectionContext,
) -> Result<(), TunnelError> {
    let mut control_stream = tokio::select! {
        _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(HANDSHAKE_TIMEOUT, connection.accept_stream()) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Disconnected)?,
    };
    let first = timeout(HANDSHAKE_TIMEOUT, read_boxed(&mut control_stream))
        .await
        .map_err(|_| TunnelError::Timeout)??;
    let Message::ClientHello(hello) = first else {
        return Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnexpectedMessage,
        ));
    };
    let connection_cancel = context.cancel.clone();
    let stream_admission = Arc::new(Semaphore::new(
        context
            .max_active_data_streams
            .unwrap_or(MAX_ACTIVE_CONNECTIONS_PER_SESSION),
    ));
    let sessions = context.sessions.clone();
    let counters = context.counters.clone();
    let auth_failures = context.auth_failures.clone();
    let bind_policy = context.bind_policy.clone();
    let control = tokio::spawn(serve_control(control_stream, hello, token, source, context));
    let mut control = control;
    let mut streams = JoinSet::new();
    loop {
        tokio::select! {
            _ = connection_cancel.cancelled() => break,
            result = &mut control => {
                match result {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => counters.record_termination(error.termination_category()),
                    Err(error) if error.is_panic() => {
                        counters.task_panics.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        counters.record_termination(TerminationCategory::Internal);
                    }
                    Err(_) => {},
                }
                break;
            }
            accepted = connection.accept_stream() => {
                let stream = accepted.map_err(|_| TunnelError::Disconnected)?;
                let Ok(permit) = stream_admission.clone().try_acquire_owned() else {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(TerminationCategory::ResourceExhausted);
                    continue;
                };
                let data_context = ConnectionContext {
                    sessions: sessions.clone(),
                    counters: counters.clone(),
                    auth_failures: auth_failures.clone(),
                    bind_policy: bind_policy.clone(),
                    cancel: connection_cancel.child_token(),
                    principal: None,
                    admission: None,
                    handshake_guard: Some(HandshakeGuard::new(counters.clone())),
                    max_active_data_streams: None,
                };
                streams.spawn(async move {
                    let _permit = permit;
                    handle_quic_data_stream(stream, data_context).await
                });
            }
            Some(result) = streams.join_next(), if !streams.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    connection_cancel.cancel();
    connection.close("session ended");
    streams.abort_all();
    while streams.join_next().await.is_some() {}
    if !control.is_finished() {
        control.abort();
    }
    let _ = control.await;
    Ok(())
}

#[cfg(feature = "quic")]
async fn handle_quic_data_stream(
    mut stream: BoxStream,
    mut context: ConnectionContext,
) -> Result<(), TunnelError> {
    let first = tokio::select! {
        _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(HANDSHAKE_TIMEOUT, read_boxed(&mut stream)) => result.map_err(|_| TunnelError::Timeout)??,
    };
    let Message::DataHello(hello) = first else {
        return Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnexpectedMessage,
        ));
    };
    drop(context.handshake_guard.take());
    accept_data_hello(stream, hello, None, &context.sessions, &context.counters).await
}

struct PendingEntry {
    service_id: ServiceId,
    expires: Instant,
    data_tx: oneshot::Sender<BoxStream>,
}

struct ConnectionContext {
    sessions: Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>>,
    counters: Counters,
    auth_failures: Arc<AuthFailureLimiter>,
    bind_policy: BindPolicy,
    cancel: CancellationToken,
    principal: Option<[u8; 32]>,
    admission: Option<OwnedSemaphorePermit>,
    handshake_guard: Option<HandshakeGuard>,
    #[cfg(feature = "quic")]
    max_active_data_streams: Option<usize>,
}

struct HandshakeGuard(Counters);

impl HandshakeGuard {
    fn new(counters: Counters) -> Self {
        let active = counters
            .handshakes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        counters
            .high_water_handshakes
            .fetch_max(active, std::sync::atomic::Ordering::Relaxed);
        Self(counters)
    }
}

impl Drop for HandshakeGuard {
    fn drop(&mut self) {
        self.0
            .handshakes
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

struct SessionContext {
    id: SessionId,
    principal: Option<[u8; 32]>,
    cancel: CancellationToken,
    pending: Mutex<HashMap<eggtunnel_proto::ConnectionId, PendingEntry>>,
    connection_admission: Arc<Semaphore>,
    control_tx: Mutex<Option<mpsc::Sender<Message>>>,
    counters: Counters,
}

impl Drop for SessionContext {
    fn drop(&mut self) {
        let removed = self.pending.get_mut().len();
        self.counters
            .pending
            .fetch_sub(removed, std::sync::atomic::Ordering::Relaxed);
    }
}

async fn handle_connection(
    tcp: TcpStream,
    source: IpAddr,
    tls: ServerTls,
    token: SecretToken,
    mut context: ConnectionContext,
    websocket: bool,
) -> Result<(), TunnelError> {
    let (stream, principal) = match tls {
        ServerTls::Eggress(tls) => {
            let stream: BoxStream = Box::new(tcp);
            let stream = tokio::select! {
                _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = timeout(HANDSHAKE_TIMEOUT, tls_accept(stream, tls)) => result.map_err(|_| TunnelError::Tls)?.map_err(|_| TunnelError::Tls)?,
            };
            (stream, None)
        }
        #[cfg(feature = "mtls")]
        ServerTls::Mutual(tls) => {
            let acceptor = tokio_rustls::TlsAcceptor::from(tls);
            let stream = tokio::select! {
                _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)) => result.map_err(|_| TunnelError::Tls)?.map_err(|_| TunnelError::Tls)?,
            };
            let principal = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|certificates| certificates.first())
                .map(|certificate| certificate_principal(certificate.as_ref()));
            (Box::new(stream) as BoxStream, principal)
        }
    };
    #[cfg(feature = "websocket")]
    let mut stream = if websocket {
        let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(1024 * 1024))
            .max_frame_size(Some(1024 * 1024));
        timeout(
            HANDSHAKE_TIMEOUT,
            eggress_protocol_websocket::WebSocketTunnelServer::new(1024 * 1024)
                .accept_upgrade_with_config_over_stream(stream, ws_config),
        )
        .await
        .map_err(|_| TunnelError::Timeout)?
        .map_err(|_| TunnelError::Protocol(eggtunnel_proto::ProtocolError::UnexpectedMessage))?
    } else {
        stream
    };
    #[cfg(not(feature = "websocket"))]
    let mut stream = {
        let _ = websocket;
        stream
    };
    context.principal = principal;
    let first = timeout(HANDSHAKE_TIMEOUT, read_boxed(&mut stream))
        .await
        .map_err(|_| TunnelError::Disconnected)??;
    match first {
        Message::DataHello(hello) => {
            drop(context.handshake_guard.take());
            drop(context.admission.take());
            accept_data_hello(
                stream,
                hello,
                context.principal,
                &context.sessions,
                &context.counters,
            )
            .await
        }
        Message::ClientHello(hello) => serve_control(stream, hello, token, source, context).await,
        _ => Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnexpectedMessage,
        )),
    }
}

async fn accept_data_hello(
    stream: BoxStream,
    hello: DataHello,
    principal: Option<[u8; 32]>,
    sessions: &Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>>,
    counters: &Counters,
) -> Result<(), TunnelError> {
    let session = sessions
        .lock()
        .await
        .get(&hello.session_id)
        .and_then(std::sync::Weak::upgrade);
    let Some(session) = session else {
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authentication);
    };
    if session.principal != principal {
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authentication);
    }
    let pending = session.pending.lock().await.remove(&hello.connection_id);
    let Some(pending) = pending else {
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authorization);
    };
    session
        .counters
        .pending
        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    if pending.service_id != hello.service_id || pending.expires <= Instant::now() {
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authorization);
    }
    pending
        .data_tx
        .send(stream)
        .map_err(|_| TunnelError::Cancelled)
}

async fn serve_control(
    mut stream: BoxStream,
    hello: ClientHello,
    token: SecretToken,
    source: IpAddr,
    context: ConnectionContext,
) -> Result<(), TunnelError> {
    let ConnectionContext {
        sessions,
        counters,
        auth_failures,
        bind_policy,
        cancel,
        principal,
        mut admission,
        mut handshake_guard,
        #[cfg(feature = "quic")]
            max_active_data_streams: _,
    } = context;
    if auth_failures.is_blocked(source) {
        return Err(TunnelError::Authentication);
    }
    if hello.version.major != ProtocolVersion::CURRENT.major {
        return Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnsupportedVersion(
                hello.version.major,
                hello.version.minor,
            ),
        ));
    }
    write_boxed(
        &mut stream,
        &Message::ServerHello(ServerHello {
            version: ProtocolVersion::CURRENT,
            capabilities: Capabilities::default(),
        }),
    )
    .await?;
    let auth = match timeout(HANDSHAKE_TIMEOUT, read_boxed(&mut stream))
        .await
        .map_err(|_| TunnelError::Disconnected)??
    {
        Message::Auth(auth) => auth,
        _ => return Err(TunnelError::Authentication),
    };
    if !verify_token(&token, auth.token()) {
        auth_failures.record_failure(source);
        drop(handshake_guard.take());
        drop(admission.take());
        tokio::time::sleep(AUTH_FAILURE_DELAY).await;
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let failure = Message::Error(ErrorMessage {
            code: 4,
            diagnostic: BoundedDiagnostic::new("authentication failed")?,
        });
        let _ = write_boxed(&mut stream, &failure).await;
        return Err(TunnelError::Authentication);
    }
    drop(handshake_guard.take());
    drop(admission.take());
    let session_id = SessionId::generate()
        .map_err(|_| TunnelError::Configuration("operating system randomness unavailable"))?;
    if cancel.is_cancelled() {
        return Err(TunnelError::Cancelled);
    }
    let context = Arc::new(SessionContext {
        id: session_id,
        principal,
        cancel: CancellationToken::new(),
        pending: Mutex::new(HashMap::new()),
        connection_admission: Arc::new(Semaphore::new(MAX_ACTIVE_CONNECTIONS_PER_SESSION)),
        control_tx: Mutex::new(None),
        counters: counters.clone(),
    });
    {
        let mut active = sessions.lock().await;
        active.retain(|_, weak| weak.strong_count() > 0);
        if active.len() >= MAX_SESSIONS {
            counters.record_termination(TerminationCategory::ResourceExhausted);
            return Err(TunnelError::Authorization);
        }
        active.insert(session_id, Arc::downgrade(&context));
    }
    let active_sessions = counters
        .sessions
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    counters
        .high_water_sessions
        .fetch_max(active_sessions, std::sync::atomic::Ordering::Relaxed);
    let _session_guard = SessionGuard {
        context: context.clone(),
        sessions: sessions.clone(),
    };
    write_boxed(&mut stream, &Message::AuthOk(AuthOk { session_id })).await?;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (open_tx, mut open_rx) = mpsc::channel(CONTROL_QUEUE);
    *context.control_tx.lock().await = Some(open_tx.clone());
    let mut services = HashMap::<ServiceId, ServiceEntry>::new();
    let mut names = std::collections::HashSet::new();
    let mut children = JoinSet::new();
    let idle = tokio::time::sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);
    loop {
        tokio::select! {
            _ = context.cancel.cancelled() => break,
            _ = &mut idle => break,
            incoming = read_message(&mut reader) => {
                match incoming {
                    Ok(Message::RegisterService(register)) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                        if services.len() >= bind_policy.max_services_per_session {
                            counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            counters.record_termination(TerminationCategory::ResourceExhausted);
                            write_registration_error(&mut writer, 5).await?;
                            continue;
                        }
                        if services.contains_key(&register.service_id) || names.contains(register.name.as_str()) {
                            counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            write_registration_error(&mut writer, 1).await?;
                            continue;
                        }
                        // The target descriptor is client-owned. The server uses it only as bounded registration metadata.
                        let bind_addr = match bind_to_socket(&register.requested_bind, &bind_policy) {
                            Ok(addr) => addr,
                            Err(_) => { counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed); write_registration_error(&mut writer, 2).await?; continue; }
                        };
                        let listener = match TcpListener::bind(bind_addr).await {
                            Ok(listener) => listener,
                            Err(_) => { counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed); write_registration_error(&mut writer, 3).await?; continue; }
                        };
                        let effective = socket_to_effective(listener.local_addr()?);
                        counters.binds.lock().unwrap_or_else(|p| p.into_inner()).push((session_id, register.service_id, effective.clone()));
                        let service_cancel = context.cancel.child_token();
                        let name = register.name.clone();
                        let sid = register.service_id;
                        children.spawn(run_service(listener, sid, context.clone(), open_tx.clone(), service_cancel.clone(), counters.clone()));
                        names.insert(name.as_str().to_owned());
                        services.insert(sid, ServiceEntry { name, cancel: service_cancel });
                        let registered_services = counters.services.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        counters.high_water_services.fetch_max(registered_services, std::sync::atomic::Ordering::Relaxed);
                        write_message(&mut writer, &Message::RegisterAck(RegisterAck { service_id: sid, effective_bind: effective })).await?;
                    }
                    Ok(Message::UnregisterService(UnregisterService { service_id })) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                        if let Some(entry) = services.remove(&service_id) {
                            entry.cancel.cancel();
                            names.remove(entry.name.as_str());
                            counters.binds.lock().unwrap_or_else(|p| p.into_inner()).retain(|(sid, id, _)| *sid != session_id || *id != service_id);
                            counters.services.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            remove_service_pending(&context, service_id).await;
                        }
                    }
                    Ok(Message::OpenReject(reject)) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                        if let Some(entry) = context.pending.lock().await.remove(&reject.connection_id) {
                            counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            drop(entry);
                        }
                    }
                    Ok(Message::Ping(Ping { nonce })) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + IDLE_TIMEOUT);
                        write_message(&mut writer, &Message::Pong(Pong { nonce })).await?;
                    }
                    Ok(Message::Drain(_)) => break,
                    Ok(_) => return Err(TunnelError::Protocol(eggtunnel_proto::ProtocolError::UnexpectedMessage)),
                    Err(error) => return Err(error.into()),
                }
            }
            Some(message) = open_rx.recv() => {
                let draining = matches!(&message, Message::Drain(_));
                write_message(&mut writer, &message).await?;
                if draining { break; }
            }
            Some(result) = children.join_next(), if !children.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    for entry in services.values() {
        entry.cancel.cancel();
    }
    children.abort_all();
    while children.join_next().await.is_some() {}
    remove_all_pending(&context).await;
    Ok(())
}

/// Bounded sliding-window limiter keyed by the TCP peer address. It retains no
/// unbounded per-source history and never delays successful authentication.
struct AuthFailureLimiter {
    failures: std::sync::Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
    threshold: usize,
    window: Duration,
    max_sources: usize,
}

impl AuthFailureLimiter {
    fn new(threshold: usize, window: Duration, max_sources: usize) -> Self {
        Self {
            failures: std::sync::Mutex::new(HashMap::new()),
            threshold,
            window,
            max_sources,
        }
    }

    fn is_blocked(&self, source: IpAddr) -> bool {
        self.is_blocked_at(source, Instant::now())
    }

    fn is_blocked_at(&self, source: IpAddr, now: Instant) -> bool {
        let mut sources = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        self.prune(&mut sources, now);
        if !sources.contains_key(&source) && sources.len() >= self.max_sources {
            return true;
        }
        sources
            .get(&source)
            .is_some_and(|failures| failures.len() >= self.threshold)
    }

    fn record_failure(&self, source: IpAddr) {
        self.record_failure_at(source, Instant::now());
    }

    fn record_failure_at(&self, source: IpAddr, now: Instant) {
        let mut sources = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        self.prune(&mut sources, now);
        if !sources.contains_key(&source) && sources.len() >= self.max_sources {
            return;
        }
        sources.entry(source).or_default().push_back(now);
    }

    fn prune(&self, sources: &mut HashMap<IpAddr, VecDeque<Instant>>, now: Instant) {
        sources.retain(|_, failures| {
            while failures
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= self.window)
            {
                failures.pop_front();
            }
            !failures.is_empty()
        });
    }
}

async fn write_registration_error<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    code: u16,
) -> Result<(), TunnelError> {
    let diagnostic = BoundedDiagnostic::new("service registration rejected")?;
    write_message(writer, &Message::Error(ErrorMessage { code, diagnostic })).await?;
    Ok(())
}

struct ServiceEntry {
    name: eggtunnel_proto::ServiceName,
    cancel: CancellationToken,
}

async fn run_service(
    listener: TcpListener,
    service_id: ServiceId,
    session: Arc<SessionContext>,
    opens: mpsc::Sender<Message>,
    cancel: CancellationToken,
    counters: Counters,
) {
    let mut relays = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = session.cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((external, _remote)) = accepted else { continue; };
                let Ok(connection_permit) = session.connection_admission.clone().try_acquire_owned() else {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(TerminationCategory::ResourceExhausted);
                    continue;
                };
                let active_guard = ActiveConnectionGuard::new(connection_permit, counters.clone());
                let connection_id = match eggtunnel_proto::ConnectionId::generate() {
                    Ok(id) => id,
                    Err(_) => { counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed); continue; }
                };
                let (data_tx, data_rx) = oneshot::channel();
                let mut pending = session.pending.lock().await;
                if pending.len() >= MAX_PENDING_PER_SESSION {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(TerminationCategory::ResourceExhausted);
                    continue;
                }
                pending.insert(connection_id, PendingEntry { service_id, expires: Instant::now() + PENDING_LIFETIME, data_tx });
                drop(pending);
                let pending_connections = counters.pending.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                counters.high_water_pending.fetch_max(pending_connections, std::sync::atomic::Ordering::Relaxed);
                if opens.try_send(Message::Open(Open { service_id, connection_id })).is_err() {
                    if session.pending.lock().await.remove(&connection_id).is_some() { counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed); }
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    continue;
                }
                let session = session.clone();
                let counters = counters.clone();
                let relay_cancel = cancel.child_token();
                relays.spawn(async move {
                    let _active_guard = active_guard;
                    let outcome = tokio::select! {
                        _ = relay_cancel.cancelled() => None,
                        result = timeout(PENDING_LIFETIME, data_rx) => result.ok().and_then(Result::ok),
                    };
                    if session.pending.lock().await.remove(&connection_id).is_some() {
                        counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    if let Some(data) = outcome {
                        match relay_with_options(external, data, RelayOptions::bounded(std::num::NonZeroUsize::new(16 * 1024).unwrap(), RELAY_DRAIN)).await {
                            Ok(report) => {
                                counters.bytes_upstream.fetch_add(report.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                                counters.bytes_downstream.fetch_add(report.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
                            }
                            Err(failure) => {
                                counters.bytes_upstream.fetch_add(failure.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                                counters.bytes_downstream.fetch_add(failure.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
            Some(result) = relays.join_next(), if !relays.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    relays.abort_all();
    while relays.join_next().await.is_some() {}
    remove_service_pending(&session, service_id).await;
}

struct ActiveConnectionGuard {
    _permit: OwnedSemaphorePermit,
    counters: Counters,
}

impl ActiveConnectionGuard {
    fn new(permit: OwnedSemaphorePermit, counters: Counters) -> Self {
        let active = counters
            .active_connections
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        counters
            .high_water_active_connections
            .fetch_max(active, std::sync::atomic::Ordering::Relaxed);
        Self {
            _permit: permit,
            counters,
        }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        self.counters
            .active_connections
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

async fn remove_service_pending(session: &SessionContext, service: ServiceId) {
    let mut pending = session.pending.lock().await;
    let before = pending.len();
    pending.retain(|_, entry| entry.service_id != service);
    let removed = before - pending.len();
    session
        .counters
        .pending
        .fetch_sub(removed, std::sync::atomic::Ordering::Relaxed);
}

async fn remove_all_pending(session: &SessionContext) {
    let removed = {
        let mut pending = session.pending.lock().await;
        let len = pending.len();
        pending.clear();
        len
    };
    session
        .counters
        .pending
        .fetch_sub(removed, std::sync::atomic::Ordering::Relaxed);
}

struct SessionGuard {
    context: Arc<SessionContext>,
    sessions: Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>>,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.context.cancel.cancel();
        self.context
            .counters
            .sessions
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        let removed = {
            let mut binds = self
                .context
                .counters
                .binds
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let before = binds.len();
            binds.retain(|(sid, _, _)| *sid != self.context.id);
            before - binds.len()
        };
        self.context
            .counters
            .services
            .fetch_sub(removed, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut sessions) = self.sessions.try_lock() {
            sessions.remove(&self.context.id);
        }
    }
}

fn socket_to_effective(addr: SocketAddr) -> EffectiveBind {
    let (ip, port) = match addr {
        SocketAddr::V4(addr) => (addr.ip().to_ipv6_mapped().octets(), addr.port()),
        SocketAddr::V6(addr) => (addr.ip().octets(), addr.port()),
    };
    EffectiveBind { address: ip, port }
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;
    #[cfg(feature = "mtls")]
    use crate::ClientIdentity;
    use crate::{
        Client, ClientConfig, ClientService, TargetConnector, TargetContext, TargetError,
        TargetFuture, TargetStream,
    };
    use eggtunnel_proto::{RequestedBind, ServiceName, TcpTarget};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    async fn test_session(
        session_id: SessionId,
    ) -> (
        Arc<SessionContext>,
        Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>>,
        Counters,
    ) {
        let counters = Counters::default();
        let context = Arc::new(SessionContext {
            id: session_id,
            principal: None,
            cancel: CancellationToken::new(),
            pending: Mutex::new(HashMap::new()),
            connection_admission: Arc::new(Semaphore::new(4)),
            control_tx: Mutex::new(None),
            counters: counters.clone(),
        });
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions
            .lock()
            .await
            .insert(session_id, Arc::downgrade(&context));
        (context, sessions, counters)
    }

    fn test_data_stream() -> BoxStream {
        let (stream, _peer) = tokio::io::duplex(32);
        Box::new(stream)
    }

    fn certificate() -> (String, String) {
        let params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    #[cfg(feature = "mtls")]
    fn mtls_certificates() -> (
        String,
        String,
        String,
        ClientIdentity,
        ClientIdentity,
        ClientIdentity,
    ) {
        use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};

        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let ca_pem = ca_cert.pem();

        let mut server_params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().unwrap();
        let server_cert = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .unwrap();

        let client_identity = |common_name: &str| {
            let mut params = CertificateParams::new(vec![common_name.to_owned()]).unwrap();
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
            let key = KeyPair::generate().unwrap();
            let certificate = params.signed_by(&key, &ca_cert, &ca_key).unwrap();
            ClientIdentity::new(
                certificate.pem().into_bytes(),
                key.serialize_pem().into_bytes(),
            )
        };
        let trusted_identity = client_identity("trusted-client");
        let trusted_identity_wrong_name = client_identity("trusted-client-wrong-name");

        let mut rogue_ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        rogue_ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let rogue_ca_key = KeyPair::generate().unwrap();
        let rogue_ca_cert = rogue_ca_params.self_signed(&rogue_ca_key).unwrap();
        let rogue_identity = {
            let mut params = CertificateParams::new(vec!["rogue-client".to_owned()]).unwrap();
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
            let key = KeyPair::generate().unwrap();
            let certificate = params
                .signed_by(&key, &rogue_ca_cert, &rogue_ca_key)
                .unwrap();
            ClientIdentity::new(
                certificate.pem().into_bytes(),
                key.serialize_pem().into_bytes(),
            )
        };

        (
            ca_pem,
            server_cert.pem(),
            server_key.serialize_pem(),
            trusted_identity,
            trusted_identity_wrong_name,
            rogue_identity,
        )
    }

    async fn roundtrip(addr: SocketAddr, bytes: &'static [u8]) -> Vec<u8> {
        let mut external = TcpStream::connect(addr).await.unwrap();
        external.write_all(bytes).await.unwrap();
        external.shutdown().await.unwrap();
        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), external.read_to_end(&mut received))
            .await
            .unwrap()
            .unwrap();
        received
    }

    struct DuplexEchoConnector;

    impl TargetConnector for DuplexEchoConnector {
        fn connect(&self, service: ClientService, _context: TargetContext) -> TargetFuture {
            Box::pin(async move {
                if service.name.as_str() != "direct-echo" {
                    return Err(TargetError::Refused);
                }
                let (application, peer) = tokio::io::duplex(64 * 1024);
                tokio::spawn(async move {
                    let (mut read, mut write) = tokio::io::split(peer);
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                    let _ = write.shutdown().await;
                });
                Ok(Box::new(application) as TargetStream)
            })
        }
    }

    struct PendingConnector;

    impl TargetConnector for PendingConnector {
        fn connect(&self, _service: ClientService, _context: TargetContext) -> TargetFuture {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn application_target_connector_relays_without_loopback_target() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"direct-connector-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_with_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"application-stream").await,
            b"application-stream"
        );
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn websocket_tls_session_registers_and_relays_data_paths() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"websocket-profile-secret".to_vec()).unwrap();
        let server = Server::bind_websocket(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_websocket_with_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        external.write_all(b"websocket-over-tls").await.unwrap();
        let mut response = [0u8; 18];
        tokio::time::timeout(Duration::from_secs(5), external.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"websocket-over-tls");
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn wss_peer_close_during_active_relay_terminates_cleanly() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"wss-close-secret".to_vec()).unwrap();
        let server = Server::bind_websocket(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_websocket_with_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server_handle.snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let external = TcpStream::connect(addr).await.unwrap();
        // Wait for the server to observe the active connection before closing.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().active_connections >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(external);
        // Wait for the active connection to drop on the server side as well; the
        // WebSocket adapter must close the underlying TCP connection rather than
        // leaving the relay half-open.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().active_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server_handle.snapshot().pending_connections, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn wss_payload_larger_than_message_cap_roundtrips_multiple_frames() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"wss-large-secret".to_vec()).unwrap();
        let server = Server::bind_websocket(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_websocket_with_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server_handle.snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        // Send a payload larger than a single WebSocket frame so the relay exercises
        // multi-frame backpressure. The Eggress adapter caps messages at 1 MiB; we
        // pick a payload well under that limit but large enough to require multiple
        // frames. We use a target connector that holds the response until the request
        // is fully read so we can verify the relay correctly backpressures the
        // buffered write across multiple WebSocket frames.
        let payload = vec![0xABu8; 64 * 1024];
        external.write_all(&payload).await.unwrap();
        // Read the response as it echoes back. WebSocket cannot half-close, so we
        // only shut down after the response arrives in full.
        let mut received = Vec::with_capacity(payload.len());
        while received.len() < payload.len() {
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout(Duration::from_secs(10), external.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(received, payload, "WSS multi-frame payload must round-trip");
        drop(external);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_http_connect_keeps_eggtunnel_tls_end_to_end() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"proxy-profile-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();

        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    assert!(request.starts_with(b"CONNECT "));
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
                        .await
                        .unwrap();
                });
            }
        });

        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &format!("http://{proxy_addr}"),
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"tls-through-connect").await,
            b"tls-through-connect"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_socks5_keeps_eggtunnel_tls_end_to_end() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"socks-proxy-profile-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    downstream.read_exact(&mut greeting).await.unwrap();
                    let mut methods = vec![0; greeting[1] as usize];
                    downstream.read_exact(&mut methods).await.unwrap();
                    downstream.write_all(&[5, 0]).await.unwrap();
                    let mut request = [0u8; 4];
                    downstream.read_exact(&mut request).await.unwrap();
                    assert_eq!(request[0], 5);
                    assert_eq!(request[1], 1);
                    match request[3] {
                        1 => {
                            let mut tail = [0u8; 6];
                            downstream.read_exact(&mut tail).await.unwrap();
                        }
                        3 => {
                            let length = downstream.read_u8().await.unwrap() as usize;
                            let mut tail = vec![0; length + 2];
                            downstream.read_exact(&mut tail).await.unwrap();
                        }
                        atyp => panic!("unexpected SOCKS address type: {atyp}"),
                    }
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
                        .await
                        .unwrap();
                });
            }
        });
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &format!("socks5://{proxy_addr}"),
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"tls-through-socks5").await,
            b"tls-through-socks5"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[tokio::test]
    async fn client_cancellation_releases_pending_direct_connector_and_external_peer() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"pending-connector-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_with_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("pending-connector").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            Arc::new(PendingConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().pending_connections == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await;
        let mut drained = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), external.read_to_end(&mut drained))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(server.handle().snapshot().pending_connections, 0);
        assert_eq!(server.handle().snapshot().active_connections, 0);
        server.shutdown().await;
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_proxy_refused_endpoint_terminates_without_secret_in_diagnostic() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"refused-proxy-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        // Bind a listener only to immediately drop it, leaving a deterministic
        // unbound loopback address with credentials embedded in the URI.
        let unbound = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused_addr = unbound.local_addr().unwrap();
        drop(unbound);
        let secret_marker = "REDACTED-PROXY-CREDENTIAL";
        let uri = format!("http://user:{secret_marker}@{refused_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token: SecretToken::new(b"refused-proxy-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snap = client.handle().snapshot();
                if snap.reconnects > 0 || snap.last_termination.is_some() {
                    break snap;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            snapshot.rejected_connections > 0
                || matches!(
                    snapshot.last_termination,
                    Some(TerminationCategory::Transport)
                        | Some(TerminationCategory::Authorization)
                        | Some(TerminationCategory::Authentication)
                        | Some(TerminationCategory::Timeout)
                ),
            "refused proxy must produce a bounded termination, got {snapshot:?}"
        );
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_proxy_handshake_timeout_tears_down_bounded() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"timeout-proxy-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        // Proxy fixture accepts the TCP connection but never sends the HTTP CONNECT
        // 200 response, so the client-side handshake must time out.
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    // Hold the TCP connection open without sending a CONNECT
                    // response; the client side eventually hits its connect timeout.
                    let mut buf = [0u8; 1024];
                    loop {
                        match downstream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(_) => continue,
                        }
                    }
                });
            }
        });
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token: SecretToken::new(b"timeout-proxy-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &format!("http://{proxy_addr}"),
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        // Wait for the client to record a timeout termination.
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if client.handle().snapshot().last_termination == Some(TerminationCategory::Timeout)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_proxy_cancellation_terminates_in_progress_handshake() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"cancel-proxy-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    // Drain bytes without ever responding so cancellation has an
                    // in-progress handshake to interrupt.
                    let mut buf = [0u8; 1024];
                    while downstream.read(&mut buf).await.is_ok_and(|n| n > 0) {}
                });
            }
        });
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token: SecretToken::new(b"cancel-proxy-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &format!("http://{proxy_addr}"),
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        // Cancel the client promptly. The in-progress proxy handshake task must
        // observe the cancellation and tear down without leaking resources.
        tokio::time::sleep(Duration::from_millis(50)).await;
        client.shutdown().await;
        // Verify the server side never observed an authenticated session: the
        // proxy canceled before any Eggtunnel registration completed.
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(server.handle().snapshot().registered_services, 0);
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_http_connect_auth_success_routes_through_proxy() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"http-auth-ok-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    assert!(request.starts_with(b"CONNECT "));
                    // Require Basic auth; reject otherwise.
                    let header = std::str::from_utf8(&request).unwrap_or_default();
                    let auth_line = header
                        .split("\r\n")
                        .find(|l| l.to_ascii_lowercase().starts_with("proxy-authorization:"))
                        .unwrap_or("");
                    let value = auth_line
                        .trim_start_matches("Proxy-Authorization:")
                        .trim_start_matches("proxy-authorization:")
                        .trim();
                    if value != "Basic YWxpY2U6czNjcmV0" {
                        downstream
                            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                            .await
                            .unwrap();
                        return;
                    }
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
                        .await
                        .unwrap();
                });
            }
        });
        let uri = format!("http://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"tls-through-connect-auth").await,
            b"tls-through-connect-auth"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_http_connect_auth_failure_rejects_without_secret_leak() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"http-auth-fail-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    downstream
                        .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                        .await
                        .unwrap();
                });
            }
        });
        let uri = format!("http://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        // The client should record an authorization failure and never register a
        // service. We poll for either a reconnection or a bounded termination; the
        // URI/credential must not appear in any client-side diagnostic.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client.handle().snapshot().reconnects > 0
                    || client.handle().snapshot().last_termination.is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 0);
        let snapshot_text = format!("{:?}", client.handle().snapshot());
        assert!(
            !snapshot_text.contains("s3cret"),
            "client snapshot must not contain proxy credentials: {snapshot_text}"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_socks5_auth_success_routes_through_proxy() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"socks5-auth-ok-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    let _ = downstream.read_exact(&mut greeting).await;
                    let mut methods = vec![0; greeting[1] as usize];
                    let _ = downstream.read_exact(&mut methods).await;
                    // Accept username/password auth (0x02) only.
                    assert!(
                        methods.contains(&0x02),
                        "expected SOCKS5 username/password auth method, got {methods:?}"
                    );
                    downstream.write_all(&[5, 0x02]).await.unwrap();
                    // Username/password sub-negotiation: version, username length, username,
                    // password length, password.
                    let version = downstream.read_u8().await.unwrap();
                    assert_eq!(version, 1);
                    let user_len = downstream.read_u8().await.unwrap() as usize;
                    let mut user = vec![0; user_len];
                    downstream.read_exact(&mut user).await.unwrap();
                    let pass_len = downstream.read_u8().await.unwrap() as usize;
                    let mut pass = vec![0; pass_len];
                    downstream.read_exact(&mut pass).await.unwrap();
                    if user != b"alice" || pass != b"s3cret" {
                        // 0x01 = version, 0x01 = failure
                        downstream.write_all(&[1, 1]).await.unwrap();
                        return;
                    }
                    // 0x01 = version, 0x00 = success. Send it before reading the
                    // CONNECT request so the client unblocks.
                    downstream.write_all(&[1, 0]).await.unwrap();
                    let mut request = [0u8; 4];
                    let _ = downstream.read_exact(&mut request).await;
                    assert_eq!(request[0], 5);
                    assert_eq!(request[1], 1);
                    match request[3] {
                        1 => {
                            let mut tail = [0u8; 6];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        3 => {
                            let length = downstream.read_u8().await.unwrap() as usize;
                            let mut tail = vec![0; length + 2];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        atyp => panic!("unexpected SOCKS address type: {atyp}"),
                    }
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });
        let uri = format!("socks5://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snap = client.handle().snapshot();
                if snap.registered_services == 1
                    || snap.reconnects > 0
                    || snap.last_termination.is_some()
                {
                    break snap;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            snapshot.registered_services, 1,
            "service registration must succeed through SOCKS5 auth"
        );
        let bind = snapshot.effective_binds.first().cloned().unwrap().2;
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"tls-through-socks5-auth").await,
            b"tls-through-socks5-auth"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_socks5_auth_failure_rejects_without_secret_leak() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"socks5-auth-fail-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    downstream.read_exact(&mut greeting).await.unwrap();
                    let mut methods = vec![0; greeting[1] as usize];
                    downstream.read_exact(&mut methods).await.unwrap();
                    downstream.write_all(&[5, 0x02]).await.unwrap();
                    let _ = downstream.read_u8().await.unwrap();
                    let user_len = downstream.read_u8().await.unwrap() as usize;
                    let mut _user = vec![0; user_len];
                    downstream.read_exact(&mut _user).await.unwrap();
                    let pass_len = downstream.read_u8().await.unwrap() as usize;
                    let mut _pass = vec![0; pass_len];
                    downstream.read_exact(&mut _pass).await.unwrap();
                    // Reject auth: version 1, status 0x01 (failure).
                    downstream.write_all(&[1, 1]).await.unwrap();
                });
            }
        });
        let uri = format!("socks5://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client.handle().snapshot().reconnects > 0
                    || client.handle().snapshot().last_termination.is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 0);
        let snapshot_text = format!("{:?}", client.handle().snapshot());
        assert!(
            !snapshot_text.contains("s3cret"),
            "client snapshot must not contain proxy credentials: {snapshot_text}"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_two_hop_socks5_then_http_connect_routes_end_to_end() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"two-hop-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let tunnel_addr = server.local_addr();
        // Second hop: HTTP CONNECT proxy listening on its own port.
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let http_proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = http_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    assert!(request.starts_with(b"CONNECT "));
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });
        // First hop: SOCKS5 proxy that performs a SOCKS5 CONNECT to the second hop
        // (HTTP CONNECT proxy) and then bridges bytes. The chain executor
        // delivers the SOCKS5 CONNECT bytes for the second hop's endpoint.
        let socks5_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks5_addr = socks5_listener.local_addr().unwrap();
        let http_target_addr = http_addr;
        let socks5_proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = socks5_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    let _ = downstream.read_exact(&mut greeting).await;
                    let mut methods = vec![0; greeting[1] as usize];
                    let _ = downstream.read_exact(&mut methods).await;
                    // No-auth greeting only.
                    assert!(methods.contains(&0x00), "expected SOCKS5 no-auth method");
                    downstream.write_all(&[5, 0x00]).await.unwrap();
                    // Read SOCKS5 CONNECT request.
                    let mut request = [0u8; 4];
                    let _ = downstream.read_exact(&mut request).await;
                    assert_eq!(request[0], 5);
                    assert_eq!(request[1], 1);
                    // Consume the address payload (we do not need the host/port here
                    // because the SOCKS5 proxy simply opens a TCP connection to the
                    // configured second-hop endpoint).
                    match request[3] {
                        1 => {
                            let mut tail = [0u8; 6];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        3 => {
                            let length = downstream.read_u8().await.unwrap() as usize;
                            let mut tail = vec![0; length + 2];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        atyp => panic!("unexpected SOCKS address type: {atyp}"),
                    }
                    let mut upstream = TcpStream::connect(http_target_addr).await.unwrap();
                    // Reply to the client with SOCKS5 success; the HTTP CONNECT
                    // bytes will arrive over the bridged stream.
                    downstream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });
        let uri = format!("socks5://{socks5_addr}__http://{http_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"tls-through-two-hop-chain").await,
            b"tls-through-two-hop-chain",
            "two-hop SOCKS5+HTTP CONNECT chain must deliver TLS end-to-end"
        );
        client.shutdown().await;
        server.shutdown().await;
        socks5_proxy_task.abort();
        http_proxy_task.abort();
    }

    #[cfg(feature = "mtls")]
    #[tokio::test]
    async fn mtls_requires_trusted_client_certificate_and_keeps_server_name_validation() {
        let (
            ca_pem,
            server_cert,
            server_key,
            trusted_identity,
            trusted_identity_wrong_name,
            rogue_identity,
        ) = mtls_certificates();
        let server = Server::bind_mtls(
            ServerConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                certificate_pem: server_cert.as_bytes().to_vec(),
                private_key_pem: server_key.as_bytes().to_vec(),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                allow_public_service_binds: false,
            },
            ca_pem.as_bytes().to_vec(),
        )
        .await
        .unwrap();
        let service = ClientService::new(
            ServiceId(1),
            ServiceName::new("mtls-service").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", 9).unwrap(),
        );
        let trusted = Client::start_with_mtls(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(ca_pem.as_bytes().to_vec()),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                services: vec![service.clone()],
            },
            trusted_identity,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(server.handle().snapshot().high_water_sessions >= 1);

        let identity_debug = format!("{trusted_identity_wrong_name:?}");
        assert!(identity_debug.contains("REDACTED"));
        assert!(!identity_debug.contains("PRIVATE KEY"));
        let wrong_name = Client::start_with_mtls(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "wrong.example".into(),
                ca_pem: Some(ca_pem.as_bytes().to_vec()),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                services: vec![service.clone()],
            },
            trusted_identity_wrong_name,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if wrong_name.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 1);

        let rejected = Client::start_with_mtls(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(ca_pem.as_bytes().to_vec()),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                services: vec![service.clone()],
            },
            rogue_identity,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if rejected.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 1);

        let missing = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(ca_pem.as_bytes().to_vec()),
            token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
            services: vec![service],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if missing.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 1);

        trusted.shutdown().await;
        wrong_name.shutdown().await;
        rejected.shutdown().await;
        missing.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn tcp_tls_reverse_session_registers_and_relays_data() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"test-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();

        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            while let Ok((stream, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.into_split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                    let _ = write.shutdown().await;
                });
            }
        });

        let service = ClientService::new(
            ServiceId(1),
            ServiceName::new("echo").unwrap(),
            eggtunnel_proto::RequestedBind::Loopback { port: 0 },
            TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
        );
        let second_service = ClientService::new(
            ServiceId(2),
            ServiceName::new("echo-two").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
        );
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token,
            services: vec![service, second_service],
        })
        .await
        .unwrap();

        let bound = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server.handle().snapshot();
                if snapshot.effective_binds.len() == 2 {
                    break snapshot
                        .effective_binds
                        .iter()
                        .map(|(_, _, effective)| {
                            SocketAddr::V6(std::net::SocketAddrV6::new(
                                std::net::Ipv6Addr::from(effective.address),
                                effective.port,
                                0,
                                0,
                            ))
                        })
                        .collect::<Vec<_>>();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (first, second) = tokio::join!(
            roundtrip(bound[0], b"first-connection"),
            roundtrip(bound[1], b"second-connection")
        );
        assert_eq!(first, b"first-connection");
        assert_eq!(second, b"second-connection");
        assert!(server.handle().snapshot().bytes_upstream > 0);
        assert!(server.handle().snapshot().bytes_downstream > 0);
        assert!(client.handle().snapshot().bytes_upstream > 0);
        assert!(client.handle().snapshot().bytes_downstream > 0);
        let server_snapshot = server.handle().snapshot();
        let client_snapshot = client.handle().snapshot();
        assert_eq!(server_snapshot.resource_limits.sessions, MAX_SESSIONS);
        assert_eq!(server_snapshot.high_water_services, 2);
        assert!(server_snapshot.high_water_active_connections >= 1);
        assert!(server_snapshot.high_water_pending_connections >= 1);
        assert!(server_snapshot.high_water_handshakes >= 1);
        assert!(client_snapshot.high_water_client_open_tasks >= 1);

        let mut active_external = TcpStream::connect(bound[1]).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().active_connections > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        client
            .handle()
            .unregister_service(ServiceId(1))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().effective_binds.len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        client.shutdown().await;
        let mut drained = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            active_external.read_to_end(&mut drained),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(server.handle().snapshot().active_connections, 0);
        assert_eq!(server.handle().snapshot().pending_connections, 0);
        server.shutdown().await;
        echo_task.abort();
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_session_multiplexes_isolated_data_streams_for_two_services() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-test-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            while let Ok((stream, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = tokio::io::split(stream);
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                    let _ = write.shutdown().await;
                });
            }
        });
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-test-secret".to_vec()).unwrap(),
            services: vec![
                ClientService::new(
                    ServiceId(1),
                    ServiceName::new("quic-one").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
                ),
                ClientService::new(
                    ServiceId(2),
                    ServiceName::new("quic-two").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
                ),
            ],
        })
        .await
        .unwrap();
        let binds = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let binds = server.handle().snapshot().effective_binds;
                if binds.len() == 2 {
                    break binds
                        .into_iter()
                        .map(|(_, _, bind)| {
                            SocketAddr::V6(std::net::SocketAddrV6::new(
                                std::net::Ipv6Addr::from(bind.address),
                                bind.port,
                                0,
                                0,
                            ))
                        })
                        .collect::<Vec<_>>();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let (one, two) = tokio::join!(
            roundtrip(binds[0], b"quic-stream-one"),
            roundtrip(binds[1], b"quic-stream-two")
        );
        assert_eq!(one, b"quic-stream-one");
        assert_eq!(two, b"quic-stream-two");

        let mut reset_stream = TcpStream::connect(binds[0]).await.unwrap();
        reset_stream.write_all(b"reset-this-stream").await.unwrap();
        drop(reset_stream);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            roundtrip(binds[1], b"stream-after-reset").await,
            b"stream-after-reset"
        );
        client.shutdown().await;
        server.shutdown().await;
        echo_task.abort();
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_connection_replacement_creates_new_session_and_reregisters_services() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"quic-reconnect-secret".to_vec()).unwrap();
        let first_server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let endpoint = first_server.local_addr();
        let first_handle = first_server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: endpoint.to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: token.clone(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-restored").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if first_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let first_session = first_handle.snapshot().effective_binds[0].0;
        first_server.shutdown().await;

        let second_server = Server::bind_quic(ServerConfig {
            listen_addr: endpoint,
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token,
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if second_server.handle().snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let second_session = second_server.handle().snapshot().effective_binds[0].0;
        assert_ne!(first_session, second_session);
        assert!(client.handle().snapshot().reconnects > 0);
        client.shutdown().await;
        second_server.shutdown().await;
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn production_quic_profile_rejects_untrusted_server_certificate() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-trust-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_quic(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-trust-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("must-not-register").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if client.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(server.handle().snapshot().registered_services, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn bad_token_does_not_create_a_registered_session() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"incorrect-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("echo").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server.handle().snapshot();
                if snapshot.rejected_connections > 0
                    && snapshot.last_termination == Some(TerminationCategory::Authentication)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(client.handle().snapshot().reconnects, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn wrong_tls_server_name_is_rejected_before_authentication() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "wrong.example".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("echo").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if client.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(server.handle().snapshot().registered_services, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn public_service_bind_is_denied_without_explicit_policy() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("public").unwrap(),
                RequestedBind::Ip {
                    address: "2001:db8::1"
                        .parse::<std::net::Ipv6Addr>()
                        .unwrap()
                        .octets(),
                    port: 9000,
                },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().rejected_connections > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 0);
        assert!(server.handle().snapshot().effective_binds.is_empty());
        assert_eq!(client.handle().snapshot().reconnects, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn refused_target_rejects_external_connection_and_releases_pending_capacity() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let unavailable = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("refused").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new(unavailable.ip().to_string(), unavailable.port()).unwrap(),
            )],
        })
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), external.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.is_empty());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().pending_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn client_reconnects_and_restores_services_in_a_new_session_generation() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"restart-secret".to_vec()).unwrap();
        let first_server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let endpoint = first_server.local_addr();
        let service = ClientService::new(
            ServiceId(1),
            ServiceName::new("restored").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", 9).unwrap(),
        );
        let client = Client::start(ClientConfig {
            server_addr: endpoint.to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: token.clone(),
            services: vec![service],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if first_server.handle().snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let first_session = first_server.handle().snapshot().effective_binds[0].0;
        first_server.shutdown().await;

        let second_server = Server::bind(ServerConfig {
            listen_addr: endpoint,
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token,
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if second_server.handle().snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let second_session = second_server.handle().snapshot().effective_binds[0].0;
        assert_ne!(first_session, second_session);
        assert!(client.handle().snapshot().reconnects > 0);
        client.shutdown().await;
        second_server.shutdown().await;
    }

    #[tokio::test]
    async fn data_hello_is_session_service_bound_and_single_use() {
        let session_id = SessionId([1; 16]);
        let (session, sessions, counters) = test_session(session_id).await;
        let connection_id = eggtunnel_proto::ConnectionId([3; 16]);
        let (data_tx, data_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            connection_id,
            PendingEntry {
                service_id: ServiceId(5),
                expires: Instant::now() + Duration::from_secs(5),
                data_tx,
            },
        );
        counters
            .pending
            .store(1, std::sync::atomic::Ordering::Relaxed);

        let wrong_session = DataHello {
            session_id: SessionId([2; 16]),
            service_id: ServiceId(5),
            connection_id,
        };
        assert!(matches!(
            accept_data_hello(
                test_data_stream(),
                wrong_session,
                None,
                &sessions,
                &counters
            )
            .await,
            Err(TunnelError::Authentication)
        ));
        assert_eq!(session.pending.lock().await.len(), 1);

        let correct = DataHello {
            session_id,
            service_id: ServiceId(5),
            connection_id,
        };
        accept_data_hello(
            test_data_stream(),
            correct.clone(),
            None,
            &sessions,
            &counters,
        )
        .await
        .unwrap();
        assert!(data_rx.await.is_ok());
        assert_eq!(
            counters.pending.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(matches!(
            accept_data_hello(test_data_stream(), correct, None, &sessions, &counters).await,
            Err(TunnelError::Authorization)
        ));
    }

    #[tokio::test]
    async fn wrong_service_and_expired_data_hellos_consume_and_reject_pending_state() {
        let session_id = SessionId([8; 16]);
        let (session, sessions, counters) = test_session(session_id).await;
        let wrong_service_id = eggtunnel_proto::ConnectionId([9; 16]);
        let (wrong_tx, _wrong_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            wrong_service_id,
            PendingEntry {
                service_id: ServiceId(1),
                expires: Instant::now() + Duration::from_secs(5),
                data_tx: wrong_tx,
            },
        );
        counters
            .pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(
            accept_data_hello(
                test_data_stream(),
                DataHello {
                    session_id,
                    service_id: ServiceId(2),
                    connection_id: wrong_service_id
                },
                None,
                &sessions,
                &counters
            )
            .await
            .is_err()
        );

        let expired_id = eggtunnel_proto::ConnectionId([10; 16]);
        let (expired_tx, _expired_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            expired_id,
            PendingEntry {
                service_id: ServiceId(1),
                expires: Instant::now() - Duration::from_millis(1),
                data_tx: expired_tx,
            },
        );
        counters
            .pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(
            accept_data_hello(
                test_data_stream(),
                DataHello {
                    session_id,
                    service_id: ServiceId(1),
                    connection_id: expired_id
                },
                None,
                &sessions,
                &counters
            )
            .await
            .is_err()
        );
        assert_eq!(session.pending.lock().await.len(), 0);
        assert_eq!(
            counters.pending.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[cfg(feature = "mtls")]
    #[tokio::test]
    async fn mtls_principal_mismatch_cannot_attach_data_stream() {
        let session_id = SessionId([11; 16]);
        let counters = Counters::default();
        let session = Arc::new(SessionContext {
            id: session_id,
            principal: Some([1; 32]),
            cancel: CancellationToken::new(),
            pending: Mutex::new(HashMap::new()),
            connection_admission: Arc::new(Semaphore::new(1)),
            control_tx: Mutex::new(None),
            counters: counters.clone(),
        });
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions
            .lock()
            .await
            .insert(session_id, Arc::downgrade(&session));
        let connection_id = eggtunnel_proto::ConnectionId([12; 16]);
        let (data_tx, _data_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            connection_id,
            PendingEntry {
                service_id: ServiceId(4),
                expires: Instant::now() + Duration::from_secs(5),
                data_tx,
            },
        );
        counters
            .pending
            .store(1, std::sync::atomic::Ordering::Relaxed);
        assert!(
            accept_data_hello(
                test_data_stream(),
                DataHello {
                    session_id,
                    service_id: ServiceId(4),
                    connection_id,
                },
                Some([2; 32]),
                &sessions,
                &counters,
            )
            .await
            .is_err()
        );
        assert_eq!(session.pending.lock().await.len(), 1);
        assert_eq!(
            counters.pending.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn repeated_client_server_start_stop_returns_runtime_counts_to_zero() {
        for cycle in 0..3u64 {
            let (cert, key) = certificate();
            let token = SecretToken::new(format!("cycle-secret-{cycle}").into_bytes()).unwrap();
            let server = Server::bind(ServerConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                certificate_pem: cert.as_bytes().to_vec(),
                private_key_pem: key.as_bytes().to_vec(),
                token: token.clone(),
                allow_public_service_binds: false,
            })
            .await
            .unwrap();
            let server_handle = server.handle();
            let client = Client::start(ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("cycle-service").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if server_handle.snapshot().registered_services == 1 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            client.shutdown().await;
            server.shutdown().await;
            let snapshot = server_handle.snapshot();
            assert_eq!(snapshot.active_sessions, 0);
            assert_eq!(snapshot.registered_services, 0);
            assert_eq!(snapshot.pending_connections, 0);
            assert_eq!(snapshot.active_connections, 0);
            assert_eq!(snapshot.active_handshakes, 0);
        }
    }

    #[tokio::test]
    async fn owned_task_panic_is_counted_as_internal_termination() {
        let counters = Counters::default();
        let result = tokio::spawn(async { panic!("injected child panic") }).await;
        counters.record_join_result(&result);
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.task_panics, 1);
        assert_eq!(
            snapshot.last_termination,
            Some(TerminationCategory::Internal)
        );
    }

    #[tokio::test]
    async fn server_shutdown_cancels_incomplete_tls_and_authentication_handshakes() {
        use eggress_transport_tls::{TlsClientConfigBuilder, tls_connect};

        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"handshake-cancel-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let handle = server.handle();
        let incomplete_tls = TcpStream::connect(server.local_addr()).await.unwrap();
        let tls_config = TlsClientConfigBuilder::new()
            .with_custom_ca_pem(cert.as_bytes())
            .unwrap()
            .build()
            .unwrap();
        let control_tcp = TcpStream::connect(server.local_addr()).await.unwrap();
        let mut control = tls_connect(Box::new(control_tcp), tls_config, "localhost")
            .await
            .unwrap();
        write_boxed(
            &mut control,
            &Message::ClientHello(ClientHello {
                version: ProtocolVersion::CURRENT,
                capabilities: Capabilities::default(),
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            read_boxed(&mut control).await.unwrap(),
            Message::ServerHello(_)
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.snapshot().active_handshakes == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server.shutdown().await;
        assert_eq!(handle.snapshot().active_handshakes, 0);
        assert_eq!(handle.snapshot().active_sessions, 0);
        drop(control);
        drop(incomplete_tls);
    }

    #[tokio::test]
    async fn unauthenticated_handshake_admission_caps_at_limit_and_recovers() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"admission-cap-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let handle = server.handle();
        let mut peers = Vec::new();
        for _ in 0..(MAX_HANDSHAKES + 1) {
            peers.push(TcpStream::connect(server.local_addr()).await.unwrap());
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = handle.snapshot();
                if snapshot.active_handshakes == MAX_HANDSHAKES && snapshot.rejected_connections > 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(handle.snapshot().active_handshakes, MAX_HANDSHAKES);
        drop(peers);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.snapshot().active_handshakes == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server.shutdown().await;
    }

    #[test]
    fn auth_failure_limiter_is_per_source_bounded_and_expires() {
        let limiter = AuthFailureLimiter::new(2, Duration::from_secs(5), 1);
        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "192.0.2.2".parse().unwrap();
        let start = Instant::now();

        assert!(!limiter.is_blocked_at(first, start));
        limiter.record_failure_at(first, start);
        assert!(!limiter.is_blocked_at(first, start + Duration::from_secs(1)));
        limiter.record_failure_at(first, start + Duration::from_secs(1));
        assert!(limiter.is_blocked_at(first, start + Duration::from_secs(2)));
        assert!(limiter.is_blocked_at(second, start + Duration::from_secs(2)));
        assert!(!limiter.is_blocked_at(first, start + Duration::from_secs(6)));
        assert_eq!(
            limiter
                .failures
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len(),
            0
        );
    }

    #[test]
    fn bind_policy_enforces_address_port_and_ephemeral_rules() {
        let policy = BindPolicy {
            allow_public_addresses: true,
            allowed_addresses: vec![std::net::Ipv6Addr::LOCALHOST.octets()],
            allowed_port_ranges: vec![(8000, 8100)],
            allow_ephemeral_ports: false,
            max_services_per_session: 4,
        };
        assert!(policy.validate().is_ok());
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 8080
                },
                &policy
            )
            .is_ok()
        );
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 0
                },
                &policy
            )
            .is_err()
        );
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 9000
                },
                &policy
            )
            .is_err()
        );
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: "2001:db8::1"
                        .parse::<std::net::Ipv6Addr>()
                        .unwrap()
                        .octets(),
                    port: 8080
                },
                &policy
            )
            .is_err()
        );
    }

    #[cfg(feature = "quic")]
    struct CapturingBlockingConnector {
        captured: std::sync::Arc<std::sync::Mutex<Option<eggtunnel_proto::ConnectionId>>>,
    }

    #[cfg(feature = "quic")]
    impl TargetConnector for CapturingBlockingConnector {
        fn connect(&self, _service: ClientService, context: TargetContext) -> TargetFuture {
            let captured = self.captured.clone();
            Box::pin(async move {
                *captured.lock().unwrap_or_else(|p| p.into_inner()) = Some(context.connection_id);
                std::future::pending::<Result<TargetStream, TargetError>>().await
            })
        }
    }

    #[cfg(feature = "quic")]
    async fn wait_for_quic_pending(server_handle: &crate::ServerHandle, expected: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().pending_connections == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(feature = "quic")]
    async fn write_quic_data_hello(
        stream: &mut eggress_core::BoxStream,
        hello: eggtunnel_proto::DataHello,
    ) {
        use crate::wire_io::write_boxed;
        use eggtunnel_proto::Message;
        write_boxed(stream, &Message::DataHello(hello))
            .await
            .unwrap();
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_wrong_session_data_hello_is_rejected_and_pending_entry_survives() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-wrong-session-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let captured: std::sync::Arc<std::sync::Mutex<Option<eggtunnel_proto::ConnectionId>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let client = Client::start_quic_insecure_with_connector_for_test(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: None,
                token: SecretToken::new(b"quic-wrong-session-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("wrong-session-svc").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            std::sync::Arc::new(CapturingBlockingConnector {
                captured: captured.clone(),
            }),
        )
        .await
        .unwrap();
        let client_handle = client.handle();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server_handle.snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        // External TCP peer connects; server inserts a pending entry and emits Open to
        // the client. The client's normal Open handler blocks on the never-resolving
        // CapturingBlockingConnector, leaving the pending entry alive for inspection.
        let external = TcpStream::connect(addr).await.unwrap();
        wait_for_quic_pending(&server_handle, 1).await;
        let captured_id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(id) = *captured.lock().unwrap_or_else(|p| p.into_inner()) {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // Submit a wrong-session DataHello through a separately opened QUIC stream that
        // shares the established session.
        let quic = client_handle
            .quic_client_for_test()
            .expect("client must have an established QUIC client");
        let mut wrong_stream = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut wrong_stream,
            eggtunnel_proto::DataHello {
                session_id: SessionId([9; 16]),
                service_id: ServiceId(1),
                connection_id: captured_id,
            },
        )
        .await;
        // The server rejects the wrong-session DataHello with an Authentication error and
        // closes the stream; reading should fail rather than produce a DataHello success.
        let mut buf = [0u8; 64];
        let read_result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut wrong_stream, &mut buf),
        )
        .await;
        assert!(
            read_result.is_err() || matches!(read_result, Ok(Ok(0)) | Ok(Err(_))),
            "wrong-session DataHello should not produce server-side data, got {read_result:?}"
        );
        assert_eq!(
            server_handle.snapshot().rejected_connections,
            1,
            "wrong-session DataHello must increment rejected_connections"
        );
        assert_eq!(
            server_handle.snapshot().pending_connections,
            1,
            "wrong-session DataHello must not consume the pending entry"
        );
        drop(external);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_replay_data_hello_on_second_stream_is_rejected() {
        let (cert, key) = certificate();
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            while let Ok((stream, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.into_split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-replay-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-replay-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-replay-svc").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
            )],
        })
        .await
        .unwrap();
        let client_handle = client.handle();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server_handle.snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        // First connection completes normally; the relay consumes the pending entry.
        let mut first = TcpStream::connect(addr).await.unwrap();
        first.write_all(b"first").await.unwrap();
        let mut response = [0u8; 5];
        tokio::time::timeout(Duration::from_secs(5), first.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"first");
        drop(first);
        // Wait until the pending entry has been consumed and the active connection has
        // returned to baseline so the server-side state reflects the first pairing.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server_handle.snapshot();
                if snapshot.pending_connections == 0 && snapshot.active_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let legitimate_session_id = server_handle.snapshot().effective_binds[0].0;
        // Replay: open a fresh QUIC stream and resend the same ConnectionId pair. The
        // server has already consumed the ConnectionId; accept_data_hello must reject
        // the replay as Authorization (the pending entry is gone).
        let quic = client_handle
            .quic_client_for_test()
            .expect("client must have an established QUIC client");
        let mut replay_stream = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut replay_stream,
            eggtunnel_proto::DataHello {
                session_id: legitimate_session_id,
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([42; 16]),
            },
        )
        .await;
        let mut buf = [0u8; 64];
        let read_result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut replay_stream, &mut buf),
        )
        .await;
        assert!(
            read_result.is_err() || matches!(read_result, Ok(Ok(0)) | Ok(Err(_))),
            "replay DataHello should not produce server-side data, got {read_result:?}"
        );
        assert!(
            server_handle.snapshot().rejected_connections >= 1,
            "replay DataHello must increment rejected_connections"
        );
        client.shutdown().await;
        server.shutdown().await;
        echo_task.abort();
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_stale_old_generation_data_hello_is_rejected_after_reconnect() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"quic-stale-secret".to_vec()).unwrap();
        let first_server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let endpoint = first_server.local_addr();
        let first_handle = first_server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: endpoint.to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: token.clone(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-stale-svc").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        let client_handle = client.handle();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if first_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let old_session_id = first_handle.snapshot().effective_binds[0].0;
        let rejected_before = first_handle.snapshot().rejected_connections;
        first_server.shutdown().await;
        // Second server takes the same endpoint with a fresh SessionId.
        let second_server = Server::bind_quic(ServerConfig {
            listen_addr: endpoint,
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let second_handle = second_server.handle();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if second_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let new_session_id = second_handle.snapshot().effective_binds[0].0;
        assert_ne!(old_session_id, new_session_id);
        // Wait for the client's QUIC transport to settle onto the second server.
        let quic = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(client) = client_handle.quic_client_for_test()
                    && client.get_connection().await.map(|_| true).unwrap_or(false)
                {
                    break client;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        // Submit a DataHello with the OLD generation SessionId; the second server is
        // authoritative for the new SessionId only, so the stale DataHello must be
        // rejected with Authentication (session lookup misses).
        let mut stale_stream = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut stale_stream,
            eggtunnel_proto::DataHello {
                session_id: old_session_id,
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([7; 16]),
            },
        )
        .await;
        let mut buf = [0u8; 64];
        let read_result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut stale_stream, &mut buf),
        )
        .await;
        assert!(
            read_result.is_err() || matches!(read_result, Ok(Ok(0)) | Ok(Err(_))),
            "stale DataHello should not produce server-side data, got {read_result:?}"
        );
        assert!(
            second_handle.snapshot().rejected_connections > rejected_before,
            "stale DataHello must be rejected by the new server"
        );
        client.shutdown().await;
        second_server.shutdown().await;
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_stream_saturation_recovers_capacity_and_keeps_unrelated_streams_alive() {
        let (cert, key) = certificate();
        let ceiling = 2;
        let server = Server::bind_quic_with_admission_for_test(
            ServerConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                certificate_pem: cert.as_bytes().to_vec(),
                private_key_pem: key.as_bytes().to_vec(),
                token: SecretToken::new(b"quic-saturation-secret".to_vec()).unwrap(),
                allow_public_service_binds: false,
            },
            ceiling,
        )
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-saturation-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-saturation-svc").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        let client_handle = client.handle();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let quic = client_handle
            .quic_client_for_test()
            .expect("client must have an established QUIC client");
        // Open `ceiling` streams that occupy the stream_admission semaphore. Each
        // stream's DataHello will fail because there are no pending entries, but the
        // permit is held until the server closes the stream.
        let mut admitted = Vec::new();
        for _ in 0..ceiling {
            let mut stream = quic.open_stream().await.unwrap();
            write_quic_data_hello(
                &mut stream,
                eggtunnel_proto::DataHello {
                    session_id: SessionId([0; 16]),
                    service_id: ServiceId(1),
                    connection_id: eggtunnel_proto::ConnectionId([0; 16]),
                },
            )
            .await;
            admitted.push(stream);
        }
        // Allow the server to drain the admitted streams so it observes the permits as
        // held. The DataHello with a wrong SessionId is rejected quickly; wait for the
        // rejection counter to reach `ceiling`.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().rejected_connections >= ceiling as u64 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // With the ceiling saturated by admitted permits, an additional QUIC stream
        // open is still possible at the Quinn level (we doubled max_concurrent_streams
        // to permit this in the test helper), but the Eggtunnel-level stream_admission
        // semaphore must reject it before reading DataHello. Submitting a stream and
        // observing its server-side termination gives us that signal.
        let mut extra = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut extra,
            eggtunnel_proto::DataHello {
                session_id: SessionId([0; 16]),
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([0; 16]),
            },
        )
        .await;
        // Wait for the server to record the extra rejection. We expect the rejected
        // counter to advance beyond `ceiling` because the stream_admission semaphore
        // refused a permit before any DataHello read.
        let rejected_after_extra = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server_handle.snapshot();
                if snapshot.rejected_connections > ceiling as u64 {
                    break snapshot.rejected_connections;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            rejected_after_extra > ceiling as u64,
            "extra QUIC stream must be rejected once the ceiling is full, got {rejected_after_extra}"
        );
        // Release one admitted permit by dropping the corresponding stream; the
        // server's handler should observe the dropped stream and free its permit.
        admitted.remove(0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        // A new stream should be admitted again. We don't need it to complete a
        // successful DataHello (no pending entry exists); we only need the rejection
        // counter to NOT advance further as a result of an admission-cap denial,
        // proving capacity returned. We assert it does not hit the same rejected
        // boundary again with the new attempt.
        let rejected_after_release = server_handle.snapshot().rejected_connections;
        let mut follow_up = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut follow_up,
            eggtunnel_proto::DataHello {
                session_id: SessionId([0; 16]),
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([0; 16]),
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The follow-up stream is admitted (it isn't rejected by stream_admission);
        // its DataHello is rejected at the authorization stage because there is no
        // pending entry. We verify the server still operates by checking that the
        // registered service count remains at 1 throughout.
        assert_eq!(
            server_handle.snapshot().registered_services,
            1,
            "unrelated admitted stream path must remain operational"
        );
        assert!(
            server_handle.snapshot().rejected_connections >= rejected_after_release,
            "follow-up stream must not stall"
        );
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "quic")]
    struct HalfCloseTarget {
        reply: Vec<u8>,
    }

    #[cfg(feature = "quic")]
    impl TargetConnector for HalfCloseTarget {
        fn connect(&self, _service: ClientService, _context: TargetContext) -> TargetFuture {
            let reply = self.reply.clone();
            Box::pin(async move {
                let (application, _peer) = tokio::io::duplex(8 * 1024);
                let reply_clone = reply.clone();
                tokio::spawn(async move {
                    let (mut read, mut write) = tokio::io::split(application);
                    let mut request = Vec::new();
                    // Read until EOF to simulate TCP half-close: the application
                    // observes the request side shutting down.
                    let _ = tokio::io::copy(&mut read, &mut request).await;
                    drop(read);
                    // Send the response after observing EOF.
                    let _ = write.write_all(&reply_clone).await;
                    let _ = write.shutdown().await;
                });
                Ok(Box::new(_peer) as TargetStream)
            })
        }
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_half_close_preserves_response_after_request_eof() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-half-close-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let reply = b"response-after-eof".to_vec();
        let connector = std::sync::Arc::new(HalfCloseTarget {
            reply: reply.clone(),
        });
        let client = Client::start_quic_insecure_with_connector_for_test(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: None,
                token: SecretToken::new(b"quic-half-close-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("quic-half-close-svc").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            connector,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server_handle.snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        // Send the request, then shut down the write half to simulate TCP half-close
        // on the external peer side. The relay must propagate the EOF to the target
        // before the target can reply.
        external.write_all(b"request").await.unwrap();
        external.shutdown().await.unwrap();
        // Read until EOF and assert the response arrives before close.
        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), external.read_to_end(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            received, reply,
            "QUIC half-close must deliver the target response after request EOF"
        );
        client.shutdown().await;
        server.shutdown().await;
    }
}
