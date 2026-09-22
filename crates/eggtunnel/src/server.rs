use std::{
    collections::HashMap,
    net::SocketAddr,
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
    common::{Counters, SecretToken, Snapshot, TunnelError, bind_to_socket, verify_token},
    wire_io::{read_boxed, read_message, write_boxed, write_message},
};

const MAX_SESSIONS: usize = 128;
const MAX_SERVICES_PER_SESSION: usize = 64;
const MAX_PENDING_PER_SESSION: usize = 128;
const MAX_ACTIVE_CONNECTIONS_PER_SESSION: usize = 128;
const CONTROL_QUEUE: usize = 128;
const MAX_HANDSHAKES: usize = 64;
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
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Server::bind requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config)?;
        let tls = TlsServerConfigBuilder::new()
            .with_certificate_pem(&config.certificate_pem)
            .map_err(|_| TunnelError::Tls)?
            .with_key_pem(&config.private_key_pem)
            .map_err(|_| TunnelError::Tls)?
            .build()
            .map_err(|_| TunnelError::Tls)?;
        let listener = TcpListener::bind(config.listen_addr).await?;
        let local_addr = listener.local_addr()?;
        let cancel = CancellationToken::new();
        let counters = Counters::default();
        let handle = ServerHandle {
            cancel: cancel.clone(),
            counters: counters.clone(),
        };
        let task_cancel = cancel.clone();
        let task = tokio::spawn(server_loop(listener, config, tls, task_cancel, counters));
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

async fn server_loop(
    listener: TcpListener,
    config: ServerConfig,
    tls: Arc<rustls::ServerConfig>,
    cancel: CancellationToken,
    counters: Counters,
) {
    let sessions: Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let admission = Arc::new(Semaphore::new(MAX_HANDSHAKES));
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((tcp, _peer)) = accepted else { continue; };
                let Ok(permit) = admission.clone().try_acquire_owned() else {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    continue;
                };
                let tls = tls.clone();
                let token = config.token.clone();
                let sessions = sessions.clone();
                let counters = counters.clone();
                let service_policy = config.allow_public_service_binds;
                let child_cancel = cancel.child_token();
                handlers.spawn(async move {
                    let _permit = permit;
                    let _ = handle_connection(tcp, tls, token, sessions, counters, service_policy, child_cancel).await;
                });
            }
            Some(_) = handlers.join_next(), if !handlers.is_empty() => {}
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

struct PendingEntry {
    service_id: ServiceId,
    expires: Instant,
    data_tx: oneshot::Sender<BoxStream>,
}

struct SessionContext {
    id: SessionId,
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
    tls: Arc<rustls::ServerConfig>,
    token: SecretToken,
    sessions: Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>>,
    counters: Counters,
    allow_public_binds: bool,
    cancel: CancellationToken,
) -> Result<(), TunnelError> {
    let stream: BoxStream = Box::new(tcp);
    let mut stream = tokio::select! {
        _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(HANDSHAKE_TIMEOUT, tls_accept(stream, tls)) => result.map_err(|_| TunnelError::Tls)?.map_err(|_| TunnelError::Tls)?,
    };
    let first = timeout(HANDSHAKE_TIMEOUT, read_boxed(&mut stream))
        .await
        .map_err(|_| TunnelError::Disconnected)??;
    match first {
        Message::DataHello(hello) => accept_data_hello(stream, hello, &sessions, &counters).await,
        Message::ClientHello(hello) => {
            serve_control(
                stream,
                hello,
                token,
                sessions,
                counters,
                allow_public_binds,
                cancel,
            )
            .await
        }
        _ => Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnexpectedMessage,
        )),
    }
}

async fn accept_data_hello(
    stream: BoxStream,
    hello: DataHello,
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
    sessions: Arc<Mutex<HashMap<SessionId, std::sync::Weak<SessionContext>>>>,
    counters: Counters,
    allow_public_binds: bool,
    cancel: CancellationToken,
) -> Result<(), TunnelError> {
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
    let session_id = SessionId::generate()
        .map_err(|_| TunnelError::Configuration("operating system randomness unavailable"))?;
    if cancel.is_cancelled() {
        return Err(TunnelError::Cancelled);
    }
    let context = Arc::new(SessionContext {
        id: session_id,
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
            return Err(TunnelError::Authorization);
        }
        active.insert(session_id, Arc::downgrade(&context));
    }
    counters
        .sessions
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                        if services.len() >= MAX_SERVICES_PER_SESSION || services.contains_key(&register.service_id) || names.contains(register.name.as_str()) {
                            counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            write_registration_error(&mut writer, 1).await?;
                            continue;
                        }
                        // The target descriptor is client-owned. The server uses it only as bounded registration metadata.
                        let bind_addr = match bind_to_socket(&register.requested_bind, allow_public_binds) {
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
                        counters.services.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            Some(_) = children.join_next(), if !children.is_empty() => {}
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
                    continue;
                }
                pending.insert(connection_id, PendingEntry { service_id, expires: Instant::now() + PENDING_LIFETIME, data_tx });
                drop(pending);
                counters.pending.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            Some(_) = relays.join_next(), if !relays.is_empty() => {}
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
        counters
            .active_connections
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    use crate::{Client, ClientConfig, ClientService};
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
                if server.handle().snapshot().rejected_connections > 0 {
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
            accept_data_hello(test_data_stream(), wrong_session, &sessions, &counters).await,
            Err(TunnelError::Authentication)
        ));
        assert_eq!(session.pending.lock().await.len(), 1);

        let correct = DataHello {
            session_id,
            service_id: ServiceId(5),
            connection_id,
        };
        accept_data_hello(test_data_stream(), correct.clone(), &sessions, &counters)
            .await
            .unwrap();
        assert!(data_rx.await.is_ok());
        assert_eq!(
            counters.pending.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(matches!(
            accept_data_hello(test_data_stream(), correct, &sessions, &counters).await,
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
}
