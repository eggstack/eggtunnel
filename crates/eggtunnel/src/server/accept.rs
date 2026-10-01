//! Transport accept, handshake admission, and shutdown drain.
//!
//! The TCP/TLS/WebSocket accept loop and the QUIC accept loop share the
//! admission, Session registry, authentication throttle, and drain helpers
//! below. They differ only in transport-specific pre-authentication work
//! (TCP connect + TLS + optional WebSocket upgrade, versus a QUIC connection
//! and its control/data streams), which stays in the per-transport functions.

use std::{collections::HashSet, net::IpAddr, sync::Arc, time::Duration};

use eggress_core::BoxStream;
use eggtunnel_proto::Message;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "websocket-server")]
use crate::common::MAX_WEBSOCKET_FRAME_SIZE;
#[cfg(feature = "quic-server")]
use crate::common::TerminationCategory;
use crate::common::{BindPolicy, Counters, SecretToken, TunnelError};
use crate::wire_io::read_boxed;

use super::auth::{
    AUTH_FAILURE_WINDOW, AUTH_FAILURES_PER_SOURCE, AuthFailureLimiter, MAX_AUTH_SOURCES,
};
use super::control::{ControlAdmission, serve_control};
use super::pending::accept_data_hello;
use super::session::{
    HandshakeGuard, SessionContext, SessionRegistry, new_session_registry, record_saturation,
};
use super::tls::ServerTls;

/// Accept-loop-owned state shared by every transport.
pub(super) struct AcceptContext {
    pub(super) sessions: SessionRegistry,
    pub(super) counters: Counters,
    pub(super) auth_failures: Arc<AuthFailureLimiter>,
    pub(super) bind_policy: BindPolicy,
    pub(super) admission: Arc<Semaphore>,
}

impl AcceptContext {
    pub(super) fn new(bind_policy: BindPolicy, counters: Counters) -> Self {
        let admission = Arc::new(Semaphore::new(counters.policy.limits.accepted_handshakes));
        let auth_failures = Arc::new(AuthFailureLimiter::new(
            AUTH_FAILURES_PER_SOURCE,
            AUTH_FAILURE_WINDOW,
            MAX_AUTH_SOURCES,
        ));
        Self {
            sessions: new_session_registry(),
            counters,
            auth_failures,
            bind_policy,
            admission,
        }
    }

    /// Take one handshake admission permit, counting the rejection when the
    /// bounded handshake ceiling is already saturated.
    pub(super) fn admit(&self) -> Result<OwnedSemaphorePermit, ()> {
        match self.admission.clone().try_acquire_owned() {
            Ok(permit) => Ok(permit),
            Err(_) => {
                record_saturation(&self.counters);
                tracing::debug!(category = "handshake_admission", "connection rejected");
                Err(())
            }
        }
    }

    /// Bounded shutdown drain: stop admitting, notify live Sessions, wait the
    /// configured grace period, then force-cancel and reap every child task.
    pub(super) async fn drain(&self, handlers: &mut JoinSet<()>) {
        let deadline_ms = u32::try_from(self.counters.policy.timeouts.shutdown_grace.as_millis())
            .unwrap_or(u32::MAX);
        let deadline = tokio::time::Instant::now() + self.counters.policy.timeouts.shutdown_grace;
        let mut notified = HashSet::new();
        loop {
            let active = SessionContext::live(&self.sessions);
            for session in &active {
                if !notified.insert(session.id) {
                    continue;
                }
                if let Some(sender) = session.control_tx.lock().await.as_ref()
                    && sender
                        .try_send(Message::Drain(eggtunnel_proto::Drain { deadline_ms }))
                        .is_err()
                {
                    tracing::debug!(
                        category = "drain_queue_full",
                        "Session drain notification was not queued"
                    );
                }
            }
            if tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(25)))
                .await
                .is_err()
            {
                break;
            }
        }
        for session in SessionContext::live(&self.sessions) {
            session.cancel.cancel();
        }
        handlers.abort_all();
        while handlers.join_next().await.is_some() {}
    }
}

/// TCP/TLS (and optional WebSocket-tunnel) ingress accept loop.
pub(super) async fn server_loop(
    listener: TcpListener,
    token: SecretToken,
    tls: ServerTls,
    bind_policy: BindPolicy,
    cancel: CancellationToken,
    counters: Counters,
    websocket: bool,
) {
    let context = Arc::new(AcceptContext::new(bind_policy, counters));
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((tcp, peer)) = accepted else { continue; };
                let Ok(permit) = context.admit() else { continue; };
                let tls = tls.clone();
                let token = token.clone();
                let child_cancel = cancel.child_token();
                let task_counters = context.counters.clone();
                let task_context = context.clone();
                let handshake_guard = HandshakeGuard::new(context.counters.clone());
                handlers.spawn(async move {
                    if let Err(error) = handle_connection(
                        tcp,
                        peer.ip(),
                        tls,
                        token,
                        &task_context,
                        child_cancel,
                        permit,
                        handshake_guard,
                        websocket,
                    ).await
                    {
                        task_counters.record_termination(error.termination_category());
                        tracing::debug!(termination = ?error.termination_category(), "server connection ended");
                    }
                });
            }
            Some(result) = handlers.join_next(), if !handlers.is_empty() => {
                context.counters.record_join_result(&result);
            }
        }
    }
    drop(listener);
    context.drain(&mut handlers).await;
}

/// QUIC ingress accept loop using the shared admission and drain helpers.
#[cfg(feature = "quic-server")]
pub(super) async fn quic_server_loop(
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

#[cfg(feature = "quic-server")]
pub(super) async fn quic_server_loop_with_admission(
    listener: Arc<eggress_transport_quic::QuicListener>,
    token: SecretToken,
    bind_policy: BindPolicy,
    cancel: CancellationToken,
    counters: Counters,
    max_active_data_streams: usize,
) {
    let context = Arc::new(AcceptContext::new(bind_policy, counters));
    let mut handlers = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept_connection(&cancel) => {
                let connection = match accepted {
                    Ok(Some(connection)) => connection,
                    Ok(None) => break,
                    Err(_) => {
                        context.counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
                };
                let Ok(permit) = context.admit() else {
                    connection.close("handshake limit reached");
                    continue;
                };
                let source = connection.remote_address().ip();
                let child_cancel = cancel.child_token();
                let token = token.clone();
                let task_counters = context.counters.clone();
                let task_context = context.clone();
                let handshake_guard = HandshakeGuard::new(context.counters.clone());
                handlers.spawn(async move {
                    if let Err(error) = handle_quic_connection(
                        connection,
                        source,
                        token,
                        &task_context,
                        child_cancel,
                        permit,
                        handshake_guard,
                        max_active_data_streams,
                    ).await
                    {
                        task_counters.record_termination(error.termination_category());
                    }
                });
            }
            Some(result) = handlers.join_next(), if !handlers.is_empty() => {
                context.counters.record_join_result(&result);
            }
        }
    }
    listener.close();
    context.drain(&mut handlers).await;
}

/// Transport-specific pre-authentication work for TCP/TLS and WebSocket.
///
/// The first framed message decides the connection role: `ClientHello` starts
/// an authenticated control Session, `DataHello` claims a pending
/// ConnectionId, and anything else fails closed.
#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    tcp: TcpStream,
    source: IpAddr,
    tls: ServerTls,
    token: SecretToken,
    context: &Arc<AcceptContext>,
    cancel: CancellationToken,
    permit: OwnedSemaphorePermit,
    handshake_guard: HandshakeGuard,
    websocket: bool,
) -> Result<(), TunnelError> {
    let handshake_timeout = context.counters.policy.timeouts.handshake;
    let (stream, principal) = accept_tls(tcp, tls, &cancel, handshake_timeout).await?;
    tracing::debug!(
        transport = if websocket {
            "websocket_tls"
        } else {
            "tcp_tls"
        },
        "server TLS established"
    );
    let mut stream = upgrade_websocket(stream, websocket, handshake_timeout).await?;
    let first = timeout(handshake_timeout, read_boxed(&mut stream))
        .await
        .map_err(|_| TunnelError::Timeout)??;
    match first {
        Message::DataHello(hello) => {
            // Hold handshake admission until the correlation outcome so a
            // sustained bogus-`DataHello` flood is bounded by
            // `accepted_handshakes` instead of spinning short tasks limited
            // only by the scheduler.
            let _permit = permit;
            let _handshake_guard = handshake_guard;
            accept_data_hello(
                stream,
                hello,
                principal,
                &context.sessions,
                &context.counters,
            )
            .await
        }
        Message::ClientHello(hello) => {
            serve_control(ControlAdmission {
                stream,
                hello,
                token,
                source,
                sessions: context.sessions.clone(),
                counters: context.counters.clone(),
                auth_failures: context.auth_failures.clone(),
                bind_policy: context.bind_policy.clone(),
                cancel,
                principal,
                admission: Some(permit),
                handshake_guard: Some(handshake_guard),
            })
            .await
        }
        _ => {
            context
                .counters
                .rejected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err(TunnelError::Protocol(
                eggtunnel_proto::ProtocolError::UnexpectedMessage,
            ))
        }
    }
}

#[cfg(feature = "mtls")]
async fn accept_tls(
    tcp: TcpStream,
    tls: ServerTls,
    cancel: &CancellationToken,
    handshake_timeout: Duration,
) -> Result<(BoxStream, Option<[u8; 32]>), TunnelError> {
    use super::tls::certificate_principal;

    match tls {
        ServerTls::Eggress(tls) => {
            let stream: BoxStream = Box::new(tcp);
            let stream = tokio::select! {
                _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = timeout(handshake_timeout, eggress_transport_tls::tls_accept(stream, tls)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
            };
            Ok((stream, None))
        }
        ServerTls::Mutual(tls) => {
            let acceptor = tokio_rustls::TlsAcceptor::from(tls);
            let stream = tokio::select! {
                _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = timeout(handshake_timeout, acceptor.accept(tcp)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
            };
            let principal = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|certificates| certificates.first())
                .map(|certificate| certificate_principal(certificate.as_ref()));
            Ok((Box::new(stream) as BoxStream, principal))
        }
    }
}

#[cfg(not(feature = "mtls"))]
async fn accept_tls(
    tcp: TcpStream,
    tls: ServerTls,
    cancel: &CancellationToken,
    handshake_timeout: Duration,
) -> Result<(BoxStream, Option<[u8; 32]>), TunnelError> {
    let ServerTls::Eggress(tls) = tls;
    let stream: BoxStream = Box::new(tcp);
    let stream = tokio::select! {
        _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(handshake_timeout, eggress_transport_tls::tls_accept(stream, tls)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
    };
    Ok((stream, None))
}

#[cfg(feature = "websocket-server")]
async fn upgrade_websocket(
    stream: BoxStream,
    websocket: bool,
    handshake_timeout: Duration,
) -> Result<BoxStream, TunnelError> {
    if !websocket {
        return Ok(stream);
    }
    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_WEBSOCKET_FRAME_SIZE))
        .max_frame_size(Some(MAX_WEBSOCKET_FRAME_SIZE));
    timeout(
        handshake_timeout,
        eggress_protocol_websocket::WebSocketTunnelServer::new(MAX_WEBSOCKET_FRAME_SIZE)
            .accept_upgrade_with_config_over_stream(stream, ws_config),
    )
    .await
    .map_err(|_| TunnelError::Timeout)?
    .map_err(|_| TunnelError::Protocol(eggtunnel_proto::ProtocolError::UnexpectedMessage))
}

#[cfg(not(feature = "websocket-server"))]
async fn upgrade_websocket(
    stream: BoxStream,
    websocket: bool,
    _handshake_timeout: Duration,
) -> Result<BoxStream, TunnelError> {
    let _ = websocket;
    Ok(stream)
}

#[cfg(feature = "quic-server")]
#[allow(clippy::too_many_arguments)]
async fn handle_quic_connection(
    connection: eggress_transport_quic::QuicConnection,
    source: IpAddr,
    token: SecretToken,
    context: &Arc<AcceptContext>,
    connection_cancel: CancellationToken,
    permit: OwnedSemaphorePermit,
    handshake_guard: HandshakeGuard,
    max_active_data_streams: usize,
) -> Result<(), TunnelError> {
    let handshake_timeout = context.counters.policy.timeouts.handshake;
    let mut control_stream = tokio::select! {
        _ = connection_cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(handshake_timeout, connection.accept_stream()) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Disconnected)?,
    };
    let first = timeout(handshake_timeout, read_boxed(&mut control_stream))
        .await
        .map_err(|_| TunnelError::Timeout)??;
    let Message::ClientHello(hello) = first else {
        context
            .counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnexpectedMessage,
        ));
    };
    // The control stream has spent its handshake slot on `ClientHello`; data
    // streams are admitted by the QUIC transport's own stream ceiling.
    let stream_admission = Arc::new(Semaphore::new(max_active_data_streams));
    let counters = context.counters.clone();
    let mut control = tokio::spawn(serve_control(ControlAdmission {
        stream: control_stream,
        hello,
        token,
        source,
        sessions: context.sessions.clone(),
        counters: counters.clone(),
        auth_failures: context.auth_failures.clone(),
        bind_policy: context.bind_policy.clone(),
        cancel: connection_cancel.clone(),
        principal: None,
        admission: Some(permit),
        handshake_guard: Some(handshake_guard),
    }));
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
                let data_cancel = connection_cancel.child_token();
                let data_context = context.clone();
                let data_counters = counters.clone();
                streams.spawn(async move {
                    let _permit = permit;
                    handle_quic_data_stream(stream, data_cancel, data_context, data_counters).await
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
    while let Some(result) = streams.join_next().await {
        counters.record_join_result(&result);
    }
    if !control.is_finished() {
        control.abort();
    }
    let _ = control.await;
    Ok(())
}

#[cfg(feature = "quic-server")]
async fn handle_quic_data_stream(
    mut stream: BoxStream,
    cancel: CancellationToken,
    context: Arc<AcceptContext>,
    counters: Counters,
) -> Result<(), TunnelError> {
    let handshake_timeout = counters.policy.timeouts.handshake;
    let first = tokio::select! {
        _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
        result = timeout(handshake_timeout, read_boxed(&mut stream)) => result.map_err(|_| TunnelError::Timeout)??,
    };
    let Message::DataHello(hello) = first else {
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnexpectedMessage,
        ));
    };
    accept_data_hello(stream, hello, None, &context.sessions, &counters).await
}
