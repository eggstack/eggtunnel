use super::*;

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

pub(crate) struct TcpTargetConnector;

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

/// Transport and identity selection for [`ClientBuilder`].
pub enum ClientTransportProfile {
    TcpTls,
    #[cfg(feature = "quic")]
    Quic,
    #[cfg(feature = "websocket")]
    WebSocket,
}

/// Typed composition surface for client transport, connector, and runtime policy.
pub struct ClientBuilder {
    config: ClientConfig,
    connector: Arc<dyn TargetConnector>,
    profile: ClientTransportProfile,
    policy: crate::common::RuntimePolicy,
    #[cfg(feature = "mtls")]
    identity: Option<ClientIdentity>,
    #[cfg(feature = "outbound-proxy")]
    outbound_proxy: Option<String>,
}

impl ClientBuilder {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            connector: Arc::new(TcpTargetConnector),
            profile: ClientTransportProfile::TcpTls,
            policy: crate::common::RuntimePolicy::default(),
            #[cfg(feature = "mtls")]
            identity: None,
            #[cfg(feature = "outbound-proxy")]
            outbound_proxy: None,
        }
    }

    pub fn with_connector(mut self, connector: Arc<dyn TargetConnector>) -> Self {
        self.connector = connector;
        self
    }

    pub fn transport(mut self, profile: ClientTransportProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn runtime_policy(mut self, policy: crate::common::RuntimePolicy) -> Self {
        self.policy = policy;
        self
    }

    #[cfg(feature = "mtls")]
    pub fn with_identity(mut self, identity: ClientIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    #[cfg(feature = "outbound-proxy")]
    pub fn outbound_proxy(mut self, proxy_chain: impl Into<String>) -> Self {
        self.outbound_proxy = Some(proxy_chain.into());
        self
    }

    pub fn validate(&self) -> Result<(), TunnelError> {
        validate_client_profile(
            &self.config,
            &self.profile,
            #[cfg(feature = "mtls")]
            self.identity.is_some(),
            #[cfg(feature = "outbound-proxy")]
            self.outbound_proxy.as_deref(),
            &self.policy,
        )
    }

    pub async fn start(self) -> Result<Client, TunnelError> {
        self.validate()?;
        Client::start_profile(
            self.config,
            self.connector,
            self.profile,
            #[cfg(feature = "mtls")]
            self.identity,
            #[cfg(feature = "outbound-proxy")]
            self.outbound_proxy,
            self.policy,
        )
        .await
    }
}
