use std::fmt;
#[cfg(feature = "server")]
use std::net::SocketAddr;
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
    pub reconnects: u64,
    pub rejected_connections: u64,
    pub bytes_upstream: u64,
    pub bytes_downstream: u64,
    pub effective_binds: Vec<(SessionId, ServiceId, EffectiveBind)>,
}

#[cfg(any(feature = "client", feature = "server"))]
#[derive(Clone, Default)]
pub(crate) struct Counters {
    pub connected: Arc<AtomicUsize>,
    pub sessions: Arc<AtomicUsize>,
    pub services: Arc<AtomicUsize>,
    pub pending: Arc<AtomicUsize>,
    pub active_connections: Arc<AtomicUsize>,
    pub reconnects: Arc<AtomicU64>,
    pub rejected: Arc<AtomicU64>,
    pub bytes_upstream: Arc<AtomicU64>,
    pub bytes_downstream: Arc<AtomicU64>,
    pub binds: Arc<std::sync::Mutex<Vec<(SessionId, ServiceId, EffectiveBind)>>>,
}

#[cfg(any(feature = "client", feature = "server"))]
impl Counters {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            connected: self.connected.load(Ordering::Relaxed) > 0,
            active_sessions: self.sessions.load(Ordering::Relaxed),
            registered_services: self.services.load(Ordering::Relaxed),
            pending_connections: self.pending.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            rejected_connections: self.rejected.load(Ordering::Relaxed),
            bytes_upstream: self.bytes_upstream.load(Ordering::Relaxed),
            bytes_downstream: self.bytes_downstream.load(Ordering::Relaxed),
            effective_binds: self.binds.lock().unwrap_or_else(|p| p.into_inner()).clone(),
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
}

#[cfg(feature = "server")]
pub(crate) fn verify_token(expected: &SecretToken, received: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    expected.expose().len() == received.len() && bool::from(expected.expose().ct_eq(received))
}

#[cfg(feature = "server")]
pub(crate) fn bind_to_socket(
    request: &RequestedBind,
    allow_public: bool,
) -> Result<SocketAddr, TunnelError> {
    use std::net::{Ipv6Addr, SocketAddrV6};
    let addr = match request {
        RequestedBind::Loopback { port } => {
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, *port, 0, 0))
        }
        RequestedBind::Ip { address, port } => {
            let ip = Ipv6Addr::from(*address);
            if !allow_public && !ip.is_loopback() {
                return Err(TunnelError::Authorization);
            }
            SocketAddr::V6(SocketAddrV6::new(ip, *port, 0, 0))
        }
    };
    Ok(addr)
}
