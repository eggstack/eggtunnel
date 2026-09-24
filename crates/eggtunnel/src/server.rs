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

const AUTH_FAILURES_PER_SOURCE: usize = 10;
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
const MAX_AUTH_SOURCES: usize = 1024;
const AUTH_FAILURE_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const MAX_SESSIONS: usize = 128;
#[cfg(test)]
const MAX_HANDSHAKES: usize = 64;

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
    #[cfg(feature = "quic")]
    Quic,
    #[cfg(feature = "websocket")]
    WebSocket,
}

/// Typed composition surface for server transport, bind, and runtime policy.
pub struct ServerBuilder {
    config: ServerConfig,
    bind_policy: BindPolicy,
    profile: ServerTransportProfile,
    runtime_policy: crate::common::RuntimePolicy,
    #[cfg(feature = "mtls")]
    trusted_client_ca: Option<Vec<u8>>,
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
            runtime_policy: crate::common::RuntimePolicy::default(),
            #[cfg(feature = "mtls")]
            trusted_client_ca: None,
        }
    }

    pub fn bind_policy(mut self, policy: BindPolicy) -> Self {
        self.bind_policy = policy;
        self
    }

    pub fn transport(mut self, profile: ServerTransportProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn runtime_policy(mut self, policy: crate::common::RuntimePolicy) -> Self {
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

    #[cfg(feature = "websocket")]
    pub async fn bind_websocket(config: ServerConfig) -> Result<Self, TunnelError> {
        ServerBuilder::new(config)
            .transport(ServerTransportProfile::WebSocket)
            .bind()
            .await
    }

    #[cfg(feature = "quic")]
    pub async fn bind_quic(config: ServerConfig) -> Result<Self, TunnelError> {
        ServerBuilder::new(config)
            .transport(ServerTransportProfile::Quic)
            .bind()
            .await
    }

    #[cfg(feature = "quic")]
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
        runtime_policy: crate::common::RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        match profile {
            ServerTransportProfile::TcpTls => {
                let tls = {
                    #[cfg(feature = "mtls")]
                    if let Some(client_ca) = trusted_client_ca.as_deref() {
                        ServerTls::Mutual(build_mtls_server_config(&config, client_ca)?)
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
            #[cfg(feature = "websocket")]
            ServerTransportProfile::WebSocket => {
                let tls = ServerTls::Eggress(build_server_tls(&config)?);
                Self::bind_with_tls_profile(config, bind_policy, tls, true, runtime_policy).await
            }
            #[cfg(feature = "quic")]
            ServerTransportProfile::Quic => {
                Self::bind_quic_profile(config, bind_policy, runtime_policy).await
            }
        }
    }

    #[cfg(feature = "quic")]
    async fn bind_quic_profile(
        config: ServerConfig,
        bind_policy: BindPolicy,
        runtime_policy: crate::common::RuntimePolicy,
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

    async fn bind_with_tls_profile(
        config: ServerConfig,
        bind_policy: BindPolicy,
        tls: ServerTls,
        websocket: bool,
        runtime_policy: crate::common::RuntimePolicy,
    ) -> Result<Self, TunnelError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TunnelError::Configuration("Server::bind requires a caller-owned Tokio runtime")
        })?;
        validate_config(&config)?;
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

fn validate_server_profile(
    config: &ServerConfig,
    bind_policy: &BindPolicy,
    profile: &ServerTransportProfile,
    #[cfg(feature = "mtls")] trusted_client_ca: Option<&[u8]>,
    runtime_policy: &crate::common::RuntimePolicy,
) -> Result<(), TunnelError> {
    runtime_policy.validate()?;
    bind_policy.validate()?;
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

fn build_server_tls(config: &ServerConfig) -> Result<Arc<rustls::ServerConfig>, TunnelError> {
    TlsServerConfigBuilder::new()
        .with_certificate_pem(&config.certificate_pem)
        .map_err(|_| TunnelError::Tls)?
        .with_key_pem(&config.private_key_pem)
        .map_err(|_| TunnelError::Tls)?
        .build()
        .map_err(|_| TunnelError::Tls)
}

#[cfg(feature = "mtls")]
fn build_mtls_server_config(
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
    let admission = Arc::new(Semaphore::new(counters.policy.limits.accepted_handshakes));
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
                deadline_ms: counters.policy.timeouts.shutdown_grace.as_millis() as u32,
            }));
        }
    }
    tokio::time::sleep(counters.policy.timeouts.shutdown_grace).await;
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
    let max_streams = counters.policy.limits.active_connections_per_session;
    quic_server_loop_with_admission(listener, token, bind_policy, cancel, counters, max_streams)
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
    let admission = Arc::new(Semaphore::new(counters.policy.limits.accepted_handshakes));
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
                deadline_ms: counters.policy.timeouts.shutdown_grace.as_millis() as u32,
            }));
        }
    }
    tokio::time::sleep(counters.policy.timeouts.shutdown_grace).await;
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
    let handshake_timeout = context.counters.policy.timeouts.handshake;
    let mut control_stream = tokio::select! {
        _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(handshake_timeout, connection.accept_stream()) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Disconnected)?,
    };
    let first = timeout(handshake_timeout, read_boxed(&mut control_stream))
        .await
        .map_err(|_| TunnelError::Timeout)??;
    let Message::ClientHello(hello) = first else {
        return Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnexpectedMessage,
        ));
    };
    let connection_cancel = context.cancel.clone();
    let stream_admission = Arc::new(Semaphore::new(
        context.max_active_data_streams.unwrap_or(
            context
                .counters
                .policy
                .limits
                .active_connections_per_session,
        ),
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
    let handshake_timeout = context.counters.policy.timeouts.handshake;
    let first = tokio::select! {
        _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(handshake_timeout, read_boxed(&mut stream)) => result.map_err(|_| TunnelError::Timeout)??,
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
    let handshake_timeout = context.counters.policy.timeouts.handshake;
    let (stream, principal) = match tls {
        ServerTls::Eggress(tls) => {
            let stream: BoxStream = Box::new(tcp);
            let stream = tokio::select! {
                _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = timeout(handshake_timeout, tls_accept(stream, tls)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
            };
            (stream, None)
        }
        #[cfg(feature = "mtls")]
        ServerTls::Mutual(tls) => {
            let acceptor = tokio_rustls::TlsAcceptor::from(tls);
            let stream = tokio::select! {
                _ = context.cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = timeout(handshake_timeout, acceptor.accept(tcp)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
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
            handshake_timeout,
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
    let first = timeout(handshake_timeout, read_boxed(&mut stream))
        .await
        .map_err(|_| TunnelError::Timeout)??;
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
    let auth = match timeout(counters.policy.timeouts.handshake, read_boxed(&mut stream))
        .await
        .map_err(|_| TunnelError::Timeout)??
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
        connection_admission: Arc::new(Semaphore::new(
            counters.policy.limits.active_connections_per_session,
        )),
        control_tx: Mutex::new(None),
        counters: counters.clone(),
    });
    {
        let mut active = sessions.lock().await;
        active.retain(|_, weak| weak.strong_count() > 0);
        if active.len() >= counters.policy.limits.sessions {
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
    let (open_tx, mut open_rx) = mpsc::channel(counters.policy.limits.control_queue);
    *context.control_tx.lock().await = Some(open_tx.clone());
    let mut services = HashMap::<ServiceId, ServiceEntry>::new();
    let mut names = std::collections::HashSet::new();
    let mut children = JoinSet::new();
    let idle_timeout = counters.policy.timeouts.control_idle;
    let idle = tokio::time::sleep(idle_timeout);
    tokio::pin!(idle);
    loop {
        tokio::select! {
            _ = context.cancel.cancelled() => break,
            _ = &mut idle => break,
            incoming = read_message(&mut reader) => {
                match incoming {
                    Ok(Message::RegisterService(register)) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        if services.len() >= bind_policy.max_services_per_session.min(counters.policy.limits.services_per_session) {
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
                        idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        if let Some(entry) = services.remove(&service_id) {
                            entry.cancel.cancel();
                            names.remove(entry.name.as_str());
                            counters.binds.lock().unwrap_or_else(|p| p.into_inner()).retain(|(sid, id, _)| *sid != session_id || *id != service_id);
                            counters.services.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            remove_service_pending(&context, service_id).await;
                        }
                    }
                    Ok(Message::OpenReject(reject)) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        if let Some(entry) = context.pending.lock().await.remove(&reject.connection_id) {
                            counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            drop(entry);
                        }
                    }
                    Ok(Message::Ping(Ping { nonce })) => {
                idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
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
                let pending_lifetime = counters.policy.timeouts.pending_connection;
                if pending.len() >= counters.policy.limits.pending_per_session {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(TerminationCategory::ResourceExhausted);
                    continue;
                }
                pending.insert(connection_id, PendingEntry { service_id, expires: Instant::now() + pending_lifetime, data_tx });
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
                        result = timeout(pending_lifetime, data_rx) => result.ok().and_then(Result::ok),
                    };
                    if session.pending.lock().await.remove(&connection_id).is_some() {
                        counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    if let Some(data) = outcome {
                        match relay_with_options(external, data, RelayOptions::bounded(std::num::NonZeroUsize::new(16 * 1024).unwrap(), counters.policy.timeouts.relay_drain)).await {
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
include!("server_tests.rs");
