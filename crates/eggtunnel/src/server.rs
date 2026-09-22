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
        Self::bind_with_tls(config, bind_policy, ServerTls::Eggress(tls)).await
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
        Self::bind_with_tls(config, bind_policy, ServerTls::Mutual(tls)).await
    }

    async fn bind_with_tls(
        config: ServerConfig,
        bind_policy: BindPolicy,
        tls: ServerTls,
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
                };
                handlers.spawn(async move {
                    if let Err(error) =
                        handle_connection(tcp, peer.ip(), tls, token, connection_context).await
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
    let stream_admission = Arc::new(Semaphore::new(MAX_ACTIVE_CONNECTIONS_PER_SESSION));
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
) -> Result<(), TunnelError> {
    let (mut stream, principal) = match tls {
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
    use crate::{
        Client, ClientConfig, ClientIdentity, ClientService, TargetConnector, TargetContext,
        TargetError, TargetFuture, TargetStream,
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
}
