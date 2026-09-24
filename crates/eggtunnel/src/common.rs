use std::fmt;
#[cfg(feature = "server")]
use std::net::SocketAddr;
#[cfg(any(feature = "client", feature = "server"))]
use std::time::Instant;
#[cfg(any(feature = "client", feature = "server"))]
use std::{
    sync::Arc,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use eggtunnel_proto::{EffectiveBind, RequestedBind, ServiceId, ServiceName, SessionId, TcpTarget};
use thiserror::Error;
use zeroize::Zeroize;

/// Secret credential storage with redacted formatting and best-effort clearing
/// on drop.
#[derive(Clone, Eq, PartialEq)]
pub struct SecretToken(Vec<u8>);

impl SecretToken {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, TunnelError> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > eggtunnel_proto::MAX_AUTH_TOKEN_BYTES {
            return Err(TunnelError::Configuration(
                "token must contain 1..=4096 bytes",
            ));
        }
        Ok(Self(bytes))
    }

    #[cfg(any(feature = "client", feature = "server"))]
    pub(crate) fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([REDACTED])")
    }
}

impl Drop for SecretToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Local service mapping configured by the private-side application.
#[derive(Clone, Debug)]
pub struct ClientService {
    pub id: ServiceId,
    pub name: ServiceName,
    pub requested_bind: RequestedBind,
    pub target: TcpTarget,
}

impl ClientService {
    pub fn new(
        id: ServiceId,
        name: ServiceName,
        requested_bind: RequestedBind,
        target: TcpTarget,
    ) -> Self {
        Self {
            id,
            name,
            requested_bind,
            target,
        }
    }
}

/// Server-side policy entry for a service name and bind request.
#[derive(Clone, Debug)]
pub struct ServiceSpec {
    pub id: ServiceId,
    pub name: ServiceName,
    pub requested_bind: RequestedBind,
}

/// Typed admission policy for server-owned service listeners.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindPolicy {
    pub allow_public_addresses: bool,
    /// Empty permits any address allowed by `allow_public_addresses`.
    pub allowed_addresses: Vec<[u8; 16]>,
    /// Empty permits every nonzero port; ranges are inclusive.
    pub allowed_port_ranges: Vec<(u16, u16)>,
    pub allow_ephemeral_ports: bool,
    pub max_services_per_session: usize,
}

impl BindPolicy {
    pub fn loopback_only() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), TunnelError> {
        if self.max_services_per_session == 0
            || self.max_services_per_session > 65_536
            || self
                .allowed_port_ranges
                .iter()
                .any(|(start, end)| start == &0 || start > end)
        {
            return Err(TunnelError::Configuration("bind policy is invalid"));
        }
        Ok(())
    }
}

impl Default for BindPolicy {
    fn default() -> Self {
        Self {
            allow_public_addresses: false,
            allowed_addresses: Vec::new(),
            allowed_port_ranges: Vec::new(),
            allow_ephemeral_ports: true,
            max_services_per_session: 64,
        }
    }
}

impl ServiceSpec {
    pub fn new(id: ServiceId, name: ServiceName, requested_bind: RequestedBind) -> Self {
        Self {
            id,
            name,
            requested_bind,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub connected: bool,
    pub active_sessions: usize,
    pub registered_services: usize,
    pub pending_connections: usize,
    pub active_connections: usize,
    pub active_client_open_tasks: usize,
    pub active_handshakes: usize,
    pub high_water_sessions: usize,
    pub high_water_services: usize,
    pub high_water_pending_connections: usize,
    pub high_water_active_connections: usize,
    pub high_water_client_open_tasks: usize,
    pub high_water_handshakes: usize,
    pub task_panics: u64,
    pub last_termination: Option<TerminationCategory>,
    pub heartbeat: HeartbeatSnapshot,
    pub resource_limits: ResourceLimits,
    pub reconnects: u64,
    pub rejected_connections: u64,
    pub bytes_upstream: u64,
    pub bytes_downstream: u64,
    pub effective_binds: Vec<(SessionId, ServiceId, EffectiveBind)>,
}

/// Bounded heartbeat health for the current client Session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeartbeatSnapshot {
    pub session_generation: u64,
    pub last_pong_age_ms: Option<u64>,
    pub latest_rtt_ms: Option<u64>,
    pub missed_heartbeats: u64,
}

#[cfg(any(feature = "client", feature = "server"))]
#[derive(Default)]
struct HeartbeatState {
    last_pong_at: Option<Instant>,
    latest_rtt_ms: Option<u64>,
    missed_heartbeats: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminationCategory {
    Clean,
    Cancelled,
    Timeout,
    Authentication,
    Authorization,
    Protocol,
    Transport,
    Target,
    ResourceExhausted,
    PeerClosed,
    Internal,
}

/// Immutable finite ceilings used by a runtime profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceLimits {
    pub sessions: usize,
    pub services_per_session: usize,
    pub pending_per_session: usize,
    pub active_connections_per_session: usize,
    pub accepted_handshakes: usize,
    pub client_open_tasks: usize,
    pub control_queue: usize,
    pub client_command_queue: usize,
}

impl ResourceLimits {
    pub fn validate(&self) -> Result<(), TunnelError> {
        const MAX_CONFIGURED_LIMIT: usize = 65_536;
        let values = [
            self.sessions,
            self.services_per_session,
            self.pending_per_session,
            self.active_connections_per_session,
            self.accepted_handshakes,
            self.client_open_tasks,
            self.control_queue,
            self.client_command_queue,
        ];
        if values
            .iter()
            .any(|value| *value == 0 || *value > MAX_CONFIGURED_LIMIT)
        {
            return Err(TunnelError::Configuration(
                "resource limits must be in 1..=65536",
            ));
        }
        Ok(())
    }
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            sessions: 128,
            services_per_session: 64,
            pending_per_session: 128,
            active_connections_per_session: 128,
            accepted_handshakes: 64,
            client_open_tasks: 128,
            control_queue: 128,
            client_command_queue: 32,
        }
    }
}

/// Finite timeout and retry policy. Durations are monotonic runtime bounds;
/// protocol framing and credential limits are intentionally not configurable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeoutPolicy {
    pub connect: std::time::Duration,
    pub handshake: std::time::Duration,
    pub control_idle: std::time::Duration,
    pub pending_connection: std::time::Duration,
    pub relay_drain: std::time::Duration,
    pub shutdown_grace: std::time::Duration,
    pub reconnect_initial: std::time::Duration,
    pub reconnect_max: std::time::Duration,
    pub heartbeat_interval: std::time::Duration,
}

impl TimeoutPolicy {
    pub fn validate(&self) -> Result<(), TunnelError> {
        const MAX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(86_400);
        let values = [
            self.connect,
            self.handshake,
            self.control_idle,
            self.pending_connection,
            self.relay_drain,
            self.shutdown_grace,
            self.reconnect_initial,
            self.reconnect_max,
            self.heartbeat_interval,
        ];
        if values
            .iter()
            .any(|value| value.is_zero() || *value > MAX_TIMEOUT)
            || self.reconnect_initial > self.reconnect_max
            || self.heartbeat_interval >= self.control_idle
        {
            return Err(TunnelError::Configuration("timeout policy is invalid"));
        }
        Ok(())
    }
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            connect: std::time::Duration::from_secs(10),
            handshake: std::time::Duration::from_secs(10),
            control_idle: std::time::Duration::from_secs(90),
            pending_connection: std::time::Duration::from_secs(30),
            relay_drain: std::time::Duration::from_secs(15),
            shutdown_grace: std::time::Duration::from_secs(1),
            reconnect_initial: std::time::Duration::from_millis(500),
            reconnect_max: std::time::Duration::from_secs(30),
            heartbeat_interval: std::time::Duration::from_secs(20),
        }
    }
}

/// Caller-selected bounded runtime policy. Defaults match the pre-M008 runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimePolicy {
    pub limits: ResourceLimits,
    pub timeouts: TimeoutPolicy,
}

impl RuntimePolicy {
    pub fn validate(&self) -> Result<(), TunnelError> {
        self.limits.validate()?;
        self.timeouts.validate()
    }
}

#[cfg(any(feature = "client", feature = "server"))]
#[derive(Clone, Default)]
pub(crate) struct Counters {
    pub policy: Arc<RuntimePolicy>,
    pub connected: Arc<AtomicUsize>,
    pub sessions: Arc<AtomicUsize>,
    pub services: Arc<AtomicUsize>,
    pub pending: Arc<AtomicUsize>,
    pub active_connections: Arc<AtomicUsize>,
    pub open_tasks: Arc<AtomicUsize>,
    pub handshakes: Arc<AtomicUsize>,
    pub high_water_sessions: Arc<AtomicUsize>,
    pub high_water_services: Arc<AtomicUsize>,
    pub high_water_pending: Arc<AtomicUsize>,
    pub high_water_active_connections: Arc<AtomicUsize>,
    pub high_water_open_tasks: Arc<AtomicUsize>,
    pub high_water_handshakes: Arc<AtomicUsize>,
    pub task_panics: Arc<AtomicU64>,
    pub last_termination: Arc<std::sync::Mutex<Option<TerminationCategory>>>,
    pub reconnects: Arc<AtomicU64>,
    pub rejected: Arc<AtomicU64>,
    pub bytes_upstream: Arc<AtomicU64>,
    pub bytes_downstream: Arc<AtomicU64>,
    pub binds: Arc<std::sync::Mutex<Vec<(SessionId, ServiceId, EffectiveBind)>>>,
    pub session_generation: Arc<AtomicU64>,
    heartbeat: Arc<std::sync::Mutex<HeartbeatState>>,
}

#[cfg(any(feature = "client", feature = "server"))]
impl Counters {
    pub fn with_policy(policy: RuntimePolicy) -> Self {
        Self {
            policy: Arc::new(policy),
            ..Self::default()
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            connected: self.connected.load(Ordering::Relaxed) > 0,
            active_sessions: self.sessions.load(Ordering::Relaxed),
            registered_services: self.services.load(Ordering::Relaxed),
            pending_connections: self.pending.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            active_client_open_tasks: self.open_tasks.load(Ordering::Relaxed),
            active_handshakes: self.handshakes.load(Ordering::Relaxed),
            high_water_sessions: self.high_water_sessions.load(Ordering::Relaxed),
            high_water_services: self.high_water_services.load(Ordering::Relaxed),
            high_water_pending_connections: self.high_water_pending.load(Ordering::Relaxed),
            high_water_active_connections: self
                .high_water_active_connections
                .load(Ordering::Relaxed),
            high_water_client_open_tasks: self.high_water_open_tasks.load(Ordering::Relaxed),
            high_water_handshakes: self.high_water_handshakes.load(Ordering::Relaxed),
            task_panics: self.task_panics.load(Ordering::Relaxed),
            last_termination: *self
                .last_termination
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
            resource_limits: self.policy.limits,
            heartbeat: {
                let heartbeat = self.heartbeat.lock().unwrap_or_else(|p| p.into_inner());
                HeartbeatSnapshot {
                    session_generation: self.session_generation.load(Ordering::Relaxed),
                    last_pong_age_ms: heartbeat
                        .last_pong_at
                        .map(|instant| instant.elapsed().as_millis().min(u64::MAX as u128) as u64),
                    latest_rtt_ms: heartbeat.latest_rtt_ms,
                    missed_heartbeats: heartbeat.missed_heartbeats,
                }
            },
            reconnects: self.reconnects.load(Ordering::Relaxed),
            rejected_connections: self.rejected.load(Ordering::Relaxed),
            bytes_upstream: self.bytes_upstream.load(Ordering::Relaxed),
            bytes_downstream: self.bytes_downstream.load(Ordering::Relaxed),
            effective_binds: self.binds.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        }
    }

    pub fn record_termination(&self, category: TerminationCategory) {
        *self
            .last_termination
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(category);
    }

    pub fn begin_session(&self) -> Result<u64, TunnelError> {
        let generation = self
            .session_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| TunnelError::ResourceExhausted)?
            + 1;
        *self.heartbeat.lock().unwrap_or_else(|p| p.into_inner()) = HeartbeatState::default();
        Ok(generation)
    }

    pub fn record_heartbeat_missed(&self) {
        let mut heartbeat = self.heartbeat.lock().unwrap_or_else(|p| p.into_inner());
        heartbeat.missed_heartbeats = heartbeat.missed_heartbeats.saturating_add(1);
    }

    pub fn record_heartbeat_pong(&self, sent_at: Instant) {
        let mut heartbeat = self.heartbeat.lock().unwrap_or_else(|p| p.into_inner());
        heartbeat.last_pong_at = Some(Instant::now());
        heartbeat.latest_rtt_ms = Some(sent_at.elapsed().as_millis().min(u64::MAX as u128) as u64);
        heartbeat.missed_heartbeats = 0;
    }

    pub fn record_join_result<T>(&self, result: &Result<T, tokio::task::JoinError>) {
        if result.as_ref().is_err_and(tokio::task::JoinError::is_panic) {
            self.task_panics.fetch_add(1, Ordering::Relaxed);
            self.record_termination(TerminationCategory::Internal);
        }
    }
}

#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("invalid configuration: {0}")]
    Configuration(&'static str),
    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS setup or handshake failed")]
    Tls,
    #[error("protocol error: {0}")]
    Protocol(#[from] eggtunnel_proto::ProtocolError),
    #[error("server authentication failed")]
    Authentication,
    #[error("service was rejected by server policy")]
    Authorization,
    #[error("server connection ended")]
    Disconnected,
    #[error("operation was cancelled")]
    Cancelled,
    #[error("operation timed out")]
    Timeout,
    #[error("local target rejected or failed the connection")]
    Target,
    #[error("runtime resource limit was reached")]
    ResourceExhausted,
    #[error("service identifier or name is already registered")]
    ServiceAlreadyExists,
    #[error("peer closed the connection")]
    PeerClosed,
}

impl TunnelError {
    pub fn termination_category(&self) -> TerminationCategory {
        match self {
            Self::Cancelled => TerminationCategory::Cancelled,
            Self::Timeout => TerminationCategory::Timeout,
            Self::Target => TerminationCategory::Target,
            Self::ResourceExhausted => TerminationCategory::ResourceExhausted,
            Self::PeerClosed => TerminationCategory::PeerClosed,
            Self::Authentication => TerminationCategory::Authentication,
            Self::Authorization => TerminationCategory::Authorization,
            Self::ServiceAlreadyExists => TerminationCategory::Authorization,
            Self::Protocol(_) => TerminationCategory::Protocol,
            Self::Io(_) | Self::Tls | Self::Disconnected => TerminationCategory::Transport,
            Self::Configuration(_) => TerminationCategory::Internal,
        }
    }
}

#[cfg(feature = "server")]
pub(crate) fn verify_token(expected: &SecretToken, received: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    expected.expose().len() == received.len() && bool::from(expected.expose().ct_eq(received))
}

#[cfg(feature = "server")]
pub(crate) fn bind_to_socket(
    request: &RequestedBind,
    policy: &BindPolicy,
) -> Result<SocketAddr, TunnelError> {
    use std::net::{Ipv6Addr, SocketAddrV6};
    let addr = match request {
        RequestedBind::Loopback { port } => {
            if !policy.permits_port(*port) {
                return Err(TunnelError::Authorization);
            }
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, *port, 0, 0))
        }
        RequestedBind::Ip { address, port } => {
            let ip = Ipv6Addr::from(*address);
            if !policy.permits_address(*address, ip.is_loopback()) || !policy.permits_port(*port) {
                return Err(TunnelError::Authorization);
            }
            SocketAddr::V6(SocketAddrV6::new(ip, *port, 0, 0))
        }
    };
    Ok(addr)
}

impl BindPolicy {
    #[cfg(feature = "server")]
    fn permits_address(&self, address: [u8; 16], is_loopback: bool) -> bool {
        (is_loopback || self.allow_public_addresses)
            && (self.allowed_addresses.is_empty() || self.allowed_addresses.contains(&address))
    }

    #[cfg(feature = "server")]
    fn permits_port(&self, port: u16) -> bool {
        if port == 0 {
            return self.allow_ephemeral_ports;
        }
        self.allowed_port_ranges.is_empty()
            || self
                .allowed_port_ranges
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&port))
    }
}

#[cfg(test)]
mod runtime_policy_tests {
    use super::*;

    #[test]
    fn defaults_pin_the_pre_m008_effective_runtime_policy() {
        assert_eq!(
            ResourceLimits::default(),
            ResourceLimits {
                sessions: 128,
                services_per_session: 64,
                pending_per_session: 128,
                active_connections_per_session: 128,
                accepted_handshakes: 64,
                client_open_tasks: 128,
                control_queue: 128,
                client_command_queue: 32,
            }
        );
        assert_eq!(
            TimeoutPolicy::default(),
            TimeoutPolicy {
                connect: std::time::Duration::from_secs(10),
                handshake: std::time::Duration::from_secs(10),
                control_idle: std::time::Duration::from_secs(90),
                pending_connection: std::time::Duration::from_secs(30),
                relay_drain: std::time::Duration::from_secs(15),
                shutdown_grace: std::time::Duration::from_secs(1),
                reconnect_initial: std::time::Duration::from_millis(500),
                reconnect_max: std::time::Duration::from_secs(30),
                heartbeat_interval: std::time::Duration::from_secs(20),
            }
        );
        RuntimePolicy::default().validate().unwrap();
    }

    #[test]
    fn policy_rejects_zero_overflow_and_impossible_values() {
        let mut limits = ResourceLimits {
            pending_per_session: 0,
            ..ResourceLimits::default()
        };
        assert!(limits.validate().is_err());
        limits.pending_per_session = usize::MAX;
        assert!(limits.validate().is_err());

        let mut timeouts = TimeoutPolicy {
            handshake: std::time::Duration::ZERO,
            ..TimeoutPolicy::default()
        };
        assert!(timeouts.validate().is_err());
        timeouts = TimeoutPolicy {
            reconnect_max: std::time::Duration::from_millis(100),
            ..TimeoutPolicy::default()
        };
        assert!(timeouts.validate().is_err());
        timeouts = TimeoutPolicy {
            heartbeat_interval: TimeoutPolicy::default().control_idle,
            ..TimeoutPolicy::default()
        };
        assert!(timeouts.validate().is_err());
        timeouts = TimeoutPolicy {
            connect: std::time::Duration::MAX,
            ..TimeoutPolicy::default()
        };
        assert!(timeouts.validate().is_err());
    }

    #[cfg(any(feature = "client", feature = "server"))]
    #[test]
    fn heartbeat_snapshot_tracks_only_bounded_current_session_health() {
        let counters = Counters::default();
        let generation = counters.begin_session().unwrap();
        counters.record_heartbeat_missed();
        assert_eq!(counters.snapshot().heartbeat.missed_heartbeats, 1);
        counters.record_heartbeat_pong(Instant::now() - std::time::Duration::from_millis(5));
        let heartbeat = counters.snapshot().heartbeat;
        assert_eq!(heartbeat.session_generation, generation);
        assert_eq!(heartbeat.missed_heartbeats, 0);
        assert!(heartbeat.last_pong_age_ms.is_some());
        assert!(heartbeat.latest_rtt_ms.is_some());

        let next_generation = counters.begin_session().unwrap();
        let reset = counters.snapshot().heartbeat;
        assert_eq!(reset.session_generation, next_generation);
        assert_eq!(reset.last_pong_age_ms, None);
        assert_eq!(reset.latest_rtt_ms, None);
        assert_eq!(reset.missed_heartbeats, 0);
    }

    #[cfg(any(feature = "client", feature = "server"))]
    #[test]
    fn session_generation_exhaustion_fails_closed() {
        let counters = Counters::default();
        counters
            .session_generation
            .store(u64::MAX, Ordering::Relaxed);
        assert!(matches!(
            counters.begin_session(),
            Err(TunnelError::ResourceExhausted)
        ));
    }
}
