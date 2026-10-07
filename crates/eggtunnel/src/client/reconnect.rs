//! Shared client reconnect and Session supervisor.
//!
//! Every transport (TCP/TLS, WebSocket, QUIC) runs the same outer lifecycle
//! through [`drive`]: attempt accounting, disconnected command draining,
//! bounded jittered backoff with reset on a ready Session, terminal
//! authentication/authorization handling, and Session counter/bind cleanup
//! before the next Session generation.
//!
//! Transport adapters own only connection establishment, closure, and creation
//! of the data-path transport object. The trait is private: it is not a public
//! plug-in point and it is not a stream-multiplexer abstraction.

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use eggress_core::BoxStream;
use tokio::{sync::mpsc, time::timeout};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "websocket-client")]
use crate::common::MAX_WEBSOCKET_FRAME_SIZE;
use crate::common::{ClientService, Counters, SecretToken, TimeoutPolicy, TunnelError};
use crate::endpoint::Endpoint;

use super::config::TargetConnector;
use super::service_state::ServiceState;
use super::{ClientCommand, ClientDataTransport, SessionRun, run_session};

/// What the supervisor should do after a Session or establishment attempt ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SessionDisposition {
    /// Session ended for a transport/protocol reason; retry with backoff.
    Retry,
    /// Authentication or authorization failed; terminal, do not retry.
    /// Shutdown/cancellation stops the loop through `backoff` returning
    /// `false`, so a cancelled retry sleep is never counted as a reconnect.
    Terminal,
}

type EstablishFuture<'a> = Pin<
    Box<dyn Future<Output = Result<(BoxStream, ClientDataTransport), TunnelError>> + Send + 'a>,
>;

/// Private transport adapter. It exists to share the reconnect lifecycle, not
/// to expose a transport plug-in surface.
pub(super) trait Transport {
    /// Structured-log transport label.
    fn label(&self) -> &'static str;
    /// Establish one authenticated control stream plus its data-path transport.
    fn establish<'a>(&'a mut self) -> EstablishFuture<'a>;
    /// Release transport resources for the finished attempt.
    fn close(&mut self);
}

/// Bounded reconnect state shared by every transport.
pub(super) struct ReconnectSupervisor {
    pub(super) services: ServiceState,
    /// Current reconnect delay. The authenticated Session loop resets it to
    /// the configured initial delay once a Session reaches the ready state.
    pub(super) reconnect_delay: Duration,
}

impl ReconnectSupervisor {
    pub(super) fn new(services: Vec<ClientService>, initial_delay: Duration) -> Self {
        Self {
            services: ServiceState::new(services),
            reconnect_delay: initial_delay,
        }
    }

    fn note_attempt(&self, counters: &Counters, transport: &str) {
        tracing::debug!(
            transport = transport,
            attempt = counters
                .reconnects
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_add(1),
            "client connection attempt"
        );
    }

    /// Apply every command that arrived while no Session was established.
    fn drain_disconnected_commands(&mut self, commands: &mut mpsc::Receiver<ClientCommand>) {
        while let Ok(command) = commands.try_recv() {
            super::apply_disconnected_command(&mut self.services, command);
        }
    }

    /// Record the ended attempt and decide whether the supervisor may retry.
    fn finish(
        &self,
        result: &Result<(), TunnelError>,
        counters: &Counters,
        transport: &str,
    ) -> SessionDisposition {
        // Session-scoped counters and binds are always cleared before the next
        // Session generation, including terminal authentication failures.
        counters
            .connected
            .store(0, std::sync::atomic::Ordering::Relaxed);
        counters
            .services
            .store(0, std::sync::atomic::Ordering::Relaxed);
        crate::common::with_bind_table_mut(&counters.binds, |binds| binds.clear());
        match result {
            // `ResourceExhausted` is a local, unrecoverable condition (the
            // Session generation counter is spent); retrying it forever would
            // back off silently and never surface the error.
            Err(
                TunnelError::Authentication
                | TunnelError::Authorization
                | TunnelError::ResourceExhausted
                | TunnelError::Configuration(_),
            ) => {
                if let Err(error) = result {
                    counters.record_termination(error.termination_category());
                }
                SessionDisposition::Terminal
            }
            Err(error) => {
                counters.record_termination(error.termination_category());
                tracing::warn!(
                    transport = transport,
                    termination = ?error.termination_category(),
                    "client Session ended"
                );
                SessionDisposition::Retry
            }
            Ok(()) => SessionDisposition::Retry,
        }
    }

    /// Bounded jittered wait before the next attempt. Returns `false` when the
    /// caller was cancelled while waiting. A cancelled sleep is never counted
    /// as a reconnect: the counter increments only after the sleep completes.
    async fn backoff(&mut self, counters: &Counters, cancel: &CancellationToken) -> bool {
        if cancel.is_cancelled() {
            return false;
        }
        let jitter = random_jitter_ms(self.reconnect_delay);
        let delay = (self.reconnect_delay + Duration::from_millis(jitter))
            .min(counters.policy.timeouts.reconnect_max);
        tokio::select! {
            _ = cancel.cancelled() => return false,
            _ = tokio::time::sleep(delay) => {}
        }
        counters
            .reconnects
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.reconnect_delay = self
            .reconnect_delay
            .saturating_mul(2)
            .min(counters.policy.timeouts.reconnect_max);
        true
    }
}

/// Everything the supervisor needs that is not transport specific.
pub(super) struct Driver<'a> {
    pub(super) token: &'a SecretToken,
    pub(super) connector: Arc<dyn TargetConnector>,
    pub(super) cancel: &'a CancellationToken,
    pub(super) counters: &'a Counters,
    pub(super) commands: &'a mut mpsc::Receiver<ClientCommand>,
}

/// Run the shared reconnect/Session lifecycle for one transport adapter.
pub(super) async fn drive<T: Transport>(
    mut transport: T,
    mut supervisor: ReconnectSupervisor,
    driver: Driver<'_>,
) {
    loop {
        if driver.cancel.is_cancelled() {
            break;
        }
        supervisor.note_attempt(driver.counters, transport.label());
        supervisor.drain_disconnected_commands(driver.commands);
        let established = {
            let establishing = transport.establish();
            tokio::pin!(establishing);
            loop {
                tokio::select! {
                    _ = driver.cancel.cancelled() => break Err(TunnelError::Cancelled),
                    result = &mut establishing => break result,
                    Some(command) = driver.commands.recv() => {
                        super::apply_disconnected_command(&mut supervisor.services, command);
                    }
                }
            }
        };
        let result = match established {
            Ok((stream, data_transport)) => {
                tokio::select! {
                    _ = driver.cancel.cancelled() => Err(TunnelError::Cancelled),
                    result = run_session(stream, SessionRun {
                        token: driver.token,
                        transport: data_transport,
                        connector: driver.connector.clone(),
                        cancel: driver.cancel,
                        counters: driver.counters,
                        reconnect_delay: &mut supervisor.reconnect_delay,
                        service_state: &mut supervisor.services,
                        commands: &mut *driver.commands,
                    }) => result,
                }
            }
            Err(error) => Err(error),
        };
        transport.close();
        match supervisor.finish(&result, driver.counters, transport.label()) {
            SessionDisposition::Terminal => break,
            SessionDisposition::Retry => {}
        }
        if !supervisor.backoff(driver.counters, driver.cancel).await {
            break;
        }
    }
}

/// Jitter is at most a quarter of the current delay and is capped at 7.5 s so
/// backoff stays bounded under load.
pub(super) fn random_jitter_ms(delay: Duration) -> u64 {
    let max = (delay.as_millis() / 4).min(7_500) as u64;
    if max == 0 {
        return 0;
    }
    let mut random = [0; 8];
    let range = max + 1;
    let zone = u64::MAX - (u64::MAX % range);
    for _ in 0..4 {
        if let Err(error) = getrandom::fill(&mut random) {
            tracing::warn!(%error, "reconnect jitter randomness unavailable");
            return max;
        }
        let value = u64::from_ne_bytes(random);
        if value < zone {
            return value % range;
        }
    }
    max
}

/// TCP/TLS and WebSocket establishment over one already-validated endpoint.
pub(super) struct StreamTransport {
    endpoint: Endpoint,
    server_name: String,
    tls: Arc<rustls::ClientConfig>,
    websocket: bool,
    timeouts: TimeoutPolicy,
    cancel: CancellationToken,
    #[cfg(feature = "outbound-proxy")]
    outbound: Option<Arc<eggress_outbound::OutboundConnector>>,
}

impl StreamTransport {
    pub(super) fn new(
        endpoint: Endpoint,
        server_name: String,
        tls: Arc<rustls::ClientConfig>,
        websocket: bool,
        timeouts: TimeoutPolicy,
        cancel: CancellationToken,
        #[cfg(feature = "outbound-proxy")] outbound: Option<
            Arc<eggress_outbound::OutboundConnector>,
        >,
    ) -> Self {
        Self {
            endpoint,
            server_name,
            tls,
            websocket,
            timeouts,
            cancel,
            #[cfg(feature = "outbound-proxy")]
            outbound,
        }
    }
}

impl Transport for StreamTransport {
    fn label(&self) -> &'static str {
        if self.websocket {
            "websocket_tls"
        } else {
            "tcp_tls"
        }
    }

    fn establish<'a>(&'a mut self) -> EstablishFuture<'a> {
        Box::pin(async move {
            let tcp = tokio::select! {
                _ = self.cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = connect_tcp(&self.endpoint, self.timeouts.connect, #[cfg(feature = "outbound-proxy")] self.outbound.as_deref()) => result,
            }?;
            let stream = tokio::select! {
                _ = self.cancel.cancelled() => return Err(TunnelError::Cancelled),
                result = timeout(self.timeouts.handshake, eggress_transport_tls::tls_connect(tcp, self.tls.clone(), &self.server_name)) => {
                    result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?
                },
            };
            tracing::debug!(transport = self.label(), "client transport established");
            let stream = if self.websocket {
                #[cfg(feature = "websocket-client")]
                {
                    let url = self.endpoint.websocket_url();
                    let ws_config =
                        tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                            .max_message_size(Some(MAX_WEBSOCKET_FRAME_SIZE))
                            .max_frame_size(Some(MAX_WEBSOCKET_FRAME_SIZE));
                    timeout(
                        self.timeouts.handshake,
                        eggress_protocol_websocket::WebSocketTunnelClient::new(
                            MAX_WEBSOCKET_FRAME_SIZE,
                        )
                        .connect_over_stream_with_config(&url, stream, ws_config),
                    )
                    .await
                    .map_err(|_| TunnelError::Timeout)?
                    .map_err(|_| TunnelError::Tls)?
                }
                #[cfg(not(feature = "websocket-client"))]
                {
                    let _ = stream;
                    return Err(TunnelError::Configuration(
                        "WebSocket transport is not enabled in this build",
                    ));
                }
            } else {
                stream
            };
            Ok((
                stream,
                ClientDataTransport::TcpTls {
                    endpoint: self.endpoint.clone(),
                    server_name: self.server_name.clone(),
                    tls: self.tls.clone(),
                    websocket: self.websocket,
                    #[cfg(feature = "outbound-proxy")]
                    outbound: self.outbound.clone(),
                },
            ))
        })
    }

    fn close(&mut self) {}
}

pub(super) async fn connect_tcp(
    endpoint: &Endpoint,
    connect_timeout: Duration,
    #[cfg(feature = "outbound-proxy")] outbound: Option<&eggress_outbound::OutboundConnector>,
) -> Result<BoxStream, TunnelError> {
    #[cfg(feature = "outbound-proxy")]
    if let Some(outbound) = outbound {
        use eggress_outbound::OutboundConnectErrorKind;

        let (stream, _) = outbound
            .connect_tcp_timeout_detailed(endpoint.host(), endpoint.port(), connect_timeout)
            .await
            .map_err(|error| match error.kind() {
                OutboundConnectErrorKind::Authentication => TunnelError::Authentication,
                OutboundConnectErrorKind::Policy => TunnelError::Authorization,
                OutboundConnectErrorKind::Timeout => TunnelError::Timeout,
                _ => TunnelError::Disconnected,
            })?;
        return Ok(stream);
    }
    let tcp = timeout(
        connect_timeout,
        tokio::net::TcpStream::connect(endpoint.as_str()),
    )
    .await
    .map_err(|_| TunnelError::Timeout)?
    .map_err(TunnelError::Io)?;
    Ok(Box::new(tcp))
}

/// QUIC establishment. The adapter owns connection replacement: a new QUIC
/// connection invalidates every stream, pending correlation, and Session
/// derived from the previous one, so `close` always releases the old client.
#[cfg(feature = "quic-client")]
pub(super) struct QuicTransport {
    endpoint: Endpoint,
    server_name: String,
    insecure: bool,
    timeouts: TimeoutPolicy,
    max_concurrent_streams: usize,
    slot: Arc<std::sync::Mutex<Option<Arc<eggress_transport_quic::QuicClient>>>>,
    active: Option<Arc<eggress_transport_quic::QuicClient>>,
}

#[cfg(feature = "quic-client")]
impl QuicTransport {
    pub(super) fn new(
        endpoint: Endpoint,
        server_name: String,
        insecure: bool,
        timeouts: TimeoutPolicy,
        max_concurrent_streams: usize,
        slot: Arc<std::sync::Mutex<Option<Arc<eggress_transport_quic::QuicClient>>>>,
    ) -> Self {
        Self {
            endpoint,
            server_name,
            insecure,
            timeouts,
            max_concurrent_streams,
            slot,
            active: None,
        }
    }
}

#[cfg(feature = "quic-client")]
impl Transport for QuicTransport {
    fn label(&self) -> &'static str {
        "quic"
    }

    fn establish<'a>(&'a mut self) -> EstablishFuture<'a> {
        Box::pin(async move {
            use eggress_transport_quic::{QuicClient, QuicClientConfig};

            let max_concurrent_streams =
                u32::try_from(self.max_concurrent_streams).map_err(|_| {
                    TunnelError::Configuration("QUIC stream limit exceeds protocol range")
                })?;
            let connect = async {
                let quic = QuicClient::connect(
                    self.endpoint.host(),
                    self.endpoint.port(),
                    QuicClientConfig {
                        server_name: self.server_name.clone(),
                        insecure: self.insecure,
                        idle_timeout: self.timeouts.control_idle,
                        max_concurrent_streams,
                        ..QuicClientConfig::default()
                    },
                )
                .await
                .map_err(|_| TunnelError::Tls)?;
                *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(quic.clone());
                self.active = Some(quic);
                let connection = self
                    .active
                    .as_ref()
                    .expect("quic client was just stored")
                    .get_connection()
                    .await
                    .map_err(|_| TunnelError::Tls)?;
                let control = connection
                    .open_stream()
                    .await
                    .map_err(|_| TunnelError::Disconnected)?;
                Ok::<_, TunnelError>((control, connection))
            };
            let (control, connection) = timeout(self.timeouts.connect, connect)
                .await
                .map_err(|_| TunnelError::Timeout)??;
            tracing::debug!(transport = "quic", "QUIC transport established");
            Ok((control, ClientDataTransport::Quic(connection)))
        })
    }

    fn close(&mut self) {
        if let Some(quic) = self.active.take() {
            quic.close();
        }
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::RuntimePolicy;

    fn supervisor() -> ReconnectSupervisor {
        ReconnectSupervisor::new(Vec::new(), Duration::from_millis(500))
    }

    #[test]
    fn supervisor_starts_from_the_configured_initial_delay() {
        assert_eq!(supervisor().reconnect_delay, Duration::from_millis(500));
    }

    #[tokio::test]
    async fn backoff_progression_is_shared_and_bounded_for_every_transport() {
        // The delay is owned by the supervisor, so TCP/TLS, WebSocket, and
        // QUIC all progress through the same sequence up to the same ceiling.
        let mut policy = RuntimePolicy::default();
        policy.timeouts.reconnect_initial = Duration::from_millis(2);
        policy.timeouts.reconnect_max = Duration::from_millis(32);
        let counters = Counters::with_policy(policy);
        let cancel = CancellationToken::new();
        let mut state = ReconnectSupervisor::new(Vec::new(), policy.timeouts.reconnect_initial);
        assert_eq!(state.reconnect_delay, policy.timeouts.reconnect_initial);

        let mut expected = policy.timeouts.reconnect_initial;
        for _ in 0..10 {
            assert!(state.backoff(&counters, &cancel).await);
            expected = expected
                .saturating_mul(2)
                .min(policy.timeouts.reconnect_max);
            assert_eq!(state.reconnect_delay, expected);
        }
        assert_eq!(state.reconnect_delay, policy.timeouts.reconnect_max);
        assert_eq!(
            counters
                .reconnects
                .load(std::sync::atomic::Ordering::Relaxed),
            10
        );
    }

    #[tokio::test]
    async fn shutdown_interrupts_the_retry_sleep_without_counting_a_reconnect() {
        let counters = Counters::with_policy(RuntimePolicy::default());
        let cancel = CancellationToken::new();
        let mut state = supervisor();
        state.reconnect_delay = Duration::from_secs(30);
        cancel.cancel();
        assert!(!state.backoff(&counters, &cancel).await);
        assert_eq!(
            counters
                .reconnects
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn authentication_and_authorization_failures_are_terminal_and_clear_session_state() {
        let counters = Counters::with_policy(RuntimePolicy::default());
        counters
            .connected
            .store(1, std::sync::atomic::Ordering::Relaxed);
        counters
            .services
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let state = supervisor();
        for error in [
            TunnelError::Authentication,
            TunnelError::Authorization,
            // Local Session-generation exhaustion is unrecoverable: retrying
            // it would back off forever and never surface the error.
            TunnelError::ResourceExhausted,
        ] {
            counters
                .connected
                .store(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                state.finish(&Err(error), &counters, "tcp_tls"),
                SessionDisposition::Terminal
            );
            assert_eq!(
                counters
                    .connected
                    .load(std::sync::atomic::Ordering::Relaxed),
                0
            );
            assert_eq!(
                counters.services.load(std::sync::atomic::Ordering::Relaxed),
                0
            );
        }
        assert_eq!(
            *counters
                .last_termination
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
            Some(crate::common::TerminationCategory::ResourceExhausted)
        );
        assert_eq!(
            state.finish(&Err(TunnelError::Disconnected), &counters, "quic"),
            SessionDisposition::Retry
        );
        assert_eq!(
            state.finish(&Ok(()), &counters, "quic"),
            SessionDisposition::Retry
        );
    }

    #[test]
    fn jitter_is_bounded_by_a_quarter_of_the_delay() {
        for delay in [
            Duration::from_millis(0),
            Duration::from_millis(3),
            Duration::from_millis(1_000),
            Duration::from_secs(60),
        ] {
            let max = (delay.as_millis() / 4).min(7_500);
            for _ in 0..64 {
                assert!(u128::from(random_jitter_ms(delay)) <= max);
            }
        }
    }
}
