#![forbid(unsafe_code)]
//! Eggtunnel standalone operator CLI.
//!
//! The CLI is a thin adapter over the library: TOML syntax plus
//! command-line overrides plus environment/file inputs are resolved
//! **once** into a redacted in-memory snapshot, validated through the
//! canonical library builders, and then launched from the snapshot.
//! Secrets never appear in arguments, JSON, `Debug`, or diagnostics.

use std::{env, fs, net::SocketAddr, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use eggtunnel::{
    BindPolicy, ClientBuilder, ClientConfig, ClientIdentity, ClientService, ClientTransportProfile,
    Endpoint, RuntimePolicy, SecretToken, ServerBuilder, ServerConfig, ServerTransportProfile,
    proto::{RequestedBind, ServiceId, ServiceName, TcpTarget},
};
use serde::Deserialize;

const CHECK_SCHEMA: &str = "eggtunnel.check/v1";
const EVENT_SCHEMA: &str = "eggtunnel.events/v1";
const MIN_SNAPSHOT_INTERVAL_SECS: u64 = 5;

#[derive(Parser)]
#[command(name = "eggtunnel", about = "Authenticated TCP reverse tunnel")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Version,
    Check {
        config: PathBuf,
        /// Emit machine-readable redacted JSON instead of human text.
        #[arg(long)]
        json: bool,
    },
    Client {
        config: PathBuf,
        #[command(flatten)]
        overrides: ClientOverrides,
        /// Emit machine-readable redacted JSON events instead of human text.
        #[arg(long)]
        json: bool,
        /// Emit a bounded snapshot event every N seconds (minimum 5).
        /// Requires `--json`.
        #[arg(long, requires = "json")]
        snapshot_interval_secs: Option<u64>,
    },
    Server {
        config: PathBuf,
        #[command(flatten)]
        overrides: ServerOverrides,
        /// Emit machine-readable redacted JSON events instead of human text.
        #[arg(long)]
        json: bool,
        /// Emit a bounded snapshot event every N seconds (minimum 5).
        /// Requires `--json`.
        #[arg(long, requires = "json")]
        snapshot_interval_secs: Option<u64>,
    },
}

/// Non-secret client overrides. Precedence: CLI > TOML > built-in default.
/// No secret value is accepted here; `token_env`/`outbound_proxy_env` name
/// the environment variables to read, exactly like their TOML counterparts.
#[derive(Args, Clone, Debug, Default)]
struct ClientOverrides {
    /// `host:port` of the server ingress (DNS, IPv4, or `[IPv6]:port`).
    #[arg(long)]
    server_addr: Option<String>,
    /// TLS server name (SNI + verification).
    #[arg(long)]
    tls_server_name: Option<String>,
    /// `tcp_tls` | `quic` | `websocket_tls`.
    #[arg(long)]
    transport: Option<String>,
    /// Custom CA bundle path (TCP/TLS and WebSocket only).
    #[arg(long)]
    ca_cert: Option<PathBuf>,
    /// Environment variable holding the bearer token.
    #[arg(long)]
    token_env: Option<String>,
    /// Environment variable holding the outbound proxy chain.
    #[arg(long)]
    outbound_proxy_env: Option<String>,
    /// mTLS client certificate path (requires `--client-key`).
    #[arg(long)]
    client_cert: Option<PathBuf>,
    /// mTLS client key path (requires `--client-cert`).
    #[arg(long)]
    client_key: Option<PathBuf>,
    /// Requested server-side bind port. Only valid when the file defines
    /// exactly one service, so the selector is deterministic.
    #[arg(long)]
    bind_port: Option<u16>,
}

/// Non-secret server overrides. Same precedence and secrecy rules.
#[derive(Args, Clone, Debug, Default)]
struct ServerOverrides {
    /// Ingress socket address, e.g. `127.0.0.1:443`.
    #[arg(long)]
    listen_addr: Option<String>,
    /// `tcp_tls` | `quic` | `websocket_tls`.
    #[arg(long)]
    transport: Option<String>,
    /// Server certificate chain path.
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    /// Server private key path.
    #[arg(long)]
    tls_key: Option<PathBuf>,
    /// Trusted client CA path for mTLS (TCP/TLS only).
    #[arg(long)]
    client_ca: Option<PathBuf>,
    /// Environment variable holding the bearer token.
    #[arg(long)]
    token_env: Option<String>,
    /// Enable non-loopback service binds (one-way: cannot disable a TOML
    /// `allow_public_service_binds = true` from the command line).
    #[arg(long)]
    allow_public_service_binds: bool,
}

/// Stable coarse failure categories for JSON output and exit status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorCategory {
    ConfigParse,
    ConfigResolution,
    MissingSecretReference,
    TlsMaterial,
    ProfileValidation,
    BindValidation,
    RuntimeStart,
    Transport,
    Authentication,
}

impl ErrorCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::ConfigParse => "config_parse",
            Self::ConfigResolution => "config_resolution",
            Self::MissingSecretReference => "missing_secret_reference",
            Self::TlsMaterial => "tls_material",
            Self::ProfileValidation => "profile_validation",
            Self::BindValidation => "bind_validation",
            Self::RuntimeStart => "runtime_start",
            Self::Transport => "transport",
            Self::Authentication => "authentication",
        }
    }
}

/// CLI boundary error. Carries no secret: messages name variables/paths,
/// never values or key material.
#[derive(Debug)]
struct CliError {
    category: ErrorCategory,
    message: String,
}

impl CliError {
    fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.category.as_str(), self.message)
    }
}

impl std::error::Error for CliError {}

impl From<eggtunnel::TunnelError> for CliError {
    fn from(error: eggtunnel::TunnelError) -> Self {
        // `TunnelError` messages are static strings or transport
        // descriptions; they never carry token/key/proxy values.
        let category = match error {
            eggtunnel::TunnelError::Tls => ErrorCategory::TlsMaterial,
            eggtunnel::TunnelError::Configuration(_) => ErrorCategory::ProfileValidation,
            eggtunnel::TunnelError::Authentication => ErrorCategory::Authentication,
            _ => ErrorCategory::RuntimeStart,
        };
        Self::new(category, error.to_string())
    }
}

#[derive(Deserialize)]
struct FileConfig {
    mode: String,
    #[serde(default = "default_transport")]
    transport: String,
    token_env: String,
    #[serde(default)]
    listen_addr: Option<String>,
    #[serde(default)]
    tls_cert: Option<PathBuf>,
    #[serde(default)]
    tls_key: Option<PathBuf>,
    #[serde(default)]
    allow_public_service_binds: bool,
    #[serde(default)]
    server_addr: Option<String>,
    #[serde(default)]
    tls_server_name: Option<String>,
    #[serde(default)]
    ca_cert: Option<PathBuf>,
    #[serde(default)]
    outbound_proxy_env: Option<String>,
    #[serde(default)]
    client_cert: Option<PathBuf>,
    #[serde(default)]
    client_key: Option<PathBuf>,
    #[serde(default)]
    client_ca: Option<PathBuf>,
    #[serde(default)]
    services: Vec<FileService>,
}

fn default_transport() -> String {
    "tcp_tls".to_owned()
}

#[derive(Deserialize)]
struct FileService {
    id: u64,
    name: String,
    target_host: String,
    target_port: u16,
    #[serde(default)]
    bind_port: u16,
}

/// Stage 1: parse TOML syntax. No environment, file-content, or semantic
/// validation happens here.
fn read_config(path: &PathBuf) -> Result<FileConfig, CliError> {
    let text = fs::read_to_string(path).map_err(|_| {
        CliError::new(
            ErrorCategory::ConfigParse,
            "configuration file is missing or unreadable",
        )
    })?;
    toml::from_str(&text).map_err(|_| {
        CliError::new(
            ErrorCategory::ConfigParse,
            "configuration is not valid TOML",
        )
    })
}

/// Stage 2: apply non-secret command-line overrides over TOML fields.
fn apply_client_overrides(
    config: &mut FileConfig,
    overrides: &ClientOverrides,
) -> Result<(), CliError> {
    if let Some(value) = &overrides.server_addr {
        config.server_addr = Some(value.clone());
    }
    if let Some(value) = &overrides.tls_server_name {
        config.tls_server_name = Some(value.clone());
    }
    if let Some(value) = &overrides.transport {
        config.transport = value.clone();
    }
    if let Some(value) = &overrides.ca_cert {
        config.ca_cert = Some(value.clone());
    }
    if let Some(value) = &overrides.token_env {
        config.token_env = value.clone();
    }
    if let Some(value) = &overrides.outbound_proxy_env {
        config.outbound_proxy_env = Some(value.clone());
    }
    if let Some(value) = &overrides.client_cert {
        config.client_cert = Some(value.clone());
    }
    if let Some(value) = &overrides.client_key {
        config.client_key = Some(value.clone());
    }
    if let Some(port) = overrides.bind_port {
        if config.services.len() != 1 {
            return Err(CliError::new(
                ErrorCategory::ConfigResolution,
                "--bind-port requires exactly one configured service",
            ));
        }
        config.services[0].bind_port = port;
    }
    Ok(())
}

fn apply_server_overrides(
    config: &mut FileConfig,
    overrides: &ServerOverrides,
) -> Result<(), CliError> {
    if let Some(value) = &overrides.listen_addr {
        config.listen_addr = Some(value.clone());
    }
    if let Some(value) = &overrides.transport {
        config.transport = value.clone();
    }
    if let Some(value) = &overrides.tls_cert {
        config.tls_cert = Some(value.clone());
    }
    if let Some(value) = &overrides.tls_key {
        config.tls_key = Some(value.clone());
    }
    if let Some(value) = &overrides.client_ca {
        config.client_ca = Some(value.clone());
    }
    if let Some(value) = &overrides.token_env {
        config.token_env = value.clone();
    }
    if overrides.allow_public_service_binds {
        config.allow_public_service_binds = true;
    }
    Ok(())
}

/// Read one environment variable exactly once. The value is moved into a
/// secret-bearing type at the call site and never logged. The lookup is
/// injectable so tests can prove single-resolution snapshot semantics
/// without touching ambient process state.
fn load_token_with(
    env_name: &str,
    var: &dyn Fn(&str) -> Result<String, env::VarError>,
) -> Result<SecretToken, CliError> {
    if env_name.is_empty() {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "token_env must name an environment variable",
        ));
    }
    let value = var(env_name).map_err(|_| {
        CliError::new(
            ErrorCategory::MissingSecretReference,
            format!("required environment variable {env_name} is not set"),
        )
    })?;
    SecretToken::new(value.into_bytes()).map_err(|_| {
        CliError::new(
            ErrorCategory::ConfigResolution,
            format!("environment variable {env_name} holds an unusable token"),
        )
    })
}

/// Read one file exactly once. Paths appear in errors; contents never do.
fn read_material(path: &Option<PathBuf>, what: &'static str) -> Result<Option<Vec<u8>>, CliError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let bytes = fs::read(path).map_err(|_| {
        CliError::new(
            ErrorCategory::ConfigResolution,
            format!("{what} file is missing or unreadable"),
        )
    })?;
    if bytes.is_empty() {
        return Err(CliError::new(
            ErrorCategory::TlsMaterial,
            format!("{what} file must not be empty"),
        ));
    }
    Ok(Some(bytes))
}

fn read_required_material(path: &Option<PathBuf>, what: &'static str) -> Result<Vec<u8>, CliError> {
    read_material(path, what)?.ok_or_else(|| {
        CliError::new(
            ErrorCategory::ConfigResolution,
            format!("{what} is required but not configured"),
        )
    })
}

/// Stage 3 (client): resolve every environment reference and file exactly
/// once into a redacted snapshot. Later stages consume the snapshot and
/// never re-read inputs, so concurrent input changes cannot affect a
/// running process.
struct ResolvedClient {
    server_addr: Endpoint,
    tls_server_name: String,
    ca_pem: Option<Vec<u8>>,
    token: SecretToken,
    services: Vec<ClientService>,
    transport: ClientTransportProfile,
    outbound_proxy: Option<String>,
    identity: Option<ClientIdentity>,
}

impl std::fmt::Debug for ResolvedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedClient")
            .field("server_addr", &self.server_addr.as_str())
            .field("tls_server_name", &self.tls_server_name)
            .field("ca_pem", &self.ca_pem.as_ref().map(|_| "[configured]"))
            .field("token", &"[REDACTED]")
            .field("services", &self.services.len())
            .field(
                "transport",
                &match self.transport {
                    ClientTransportProfile::TcpTls => "tcp_tls",
                    ClientTransportProfile::Quic => "quic",
                    ClientTransportProfile::WebSocket => "websocket_tls",
                },
            )
            .field(
                "outbound_proxy",
                &self.outbound_proxy.as_ref().map(|_| "[configured]"),
            )
            .field("identity", &self.identity.as_ref().map(|_| "[configured]"))
            .finish()
    }
}

struct ResolvedServer {
    listen_addr: SocketAddr,
    certificate_pem: Vec<u8>,
    private_key_pem: Vec<u8>,
    token: SecretToken,
    transport: ServerTransportProfile,
    allow_public_service_binds: bool,
    client_ca: Option<Vec<u8>>,
}

impl std::fmt::Debug for ResolvedServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedServer")
            .field("listen_addr", &self.listen_addr)
            .field("certificate_pem", &"[configured]")
            .field("private_key_pem", &"[REDACTED]")
            .field("token", &"[REDACTED]")
            .field(
                "transport",
                &match self.transport {
                    ServerTransportProfile::TcpTls => "tcp_tls",
                    ServerTransportProfile::Quic => "quic",
                    ServerTransportProfile::WebSocket => "websocket_tls",
                },
            )
            .field(
                "allow_public_service_binds",
                &self.allow_public_service_binds,
            )
            .field(
                "client_ca",
                &self.client_ca.as_ref().map(|_| "[configured]"),
            )
            .finish()
    }
}

fn client_transport(name: &str) -> Result<ClientTransportProfile, CliError> {
    match name {
        "tcp_tls" => Ok(ClientTransportProfile::TcpTls),
        "quic" => Ok(ClientTransportProfile::Quic),
        "websocket_tls" => Ok(ClientTransportProfile::WebSocket),
        _ => Err(CliError::new(
            ErrorCategory::Transport,
            "transport must be 'tcp_tls', 'quic', or 'websocket_tls'",
        )),
    }
}

fn server_transport(name: &str) -> Result<ServerTransportProfile, CliError> {
    match name {
        "tcp_tls" => Ok(ServerTransportProfile::TcpTls),
        "quic" => Ok(ServerTransportProfile::Quic),
        "websocket_tls" => Ok(ServerTransportProfile::WebSocket),
        _ => Err(CliError::new(
            ErrorCategory::Transport,
            "transport must be 'tcp_tls', 'quic', or 'websocket_tls'",
        )),
    }
}

fn resolve_client_services(config: &FileConfig) -> Result<Vec<ClientService>, CliError> {
    if config.services.is_empty() {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "client config requires at least one service",
        ));
    }
    config
        .services
        .iter()
        .map(|service| {
            Ok(ClientService::new(
                ServiceId(service.id),
                ServiceName::new(service.name.clone()).map_err(|_| {
                    CliError::new(ErrorCategory::ConfigResolution, "service name is invalid")
                })?,
                RequestedBind::Loopback {
                    port: service.bind_port,
                },
                TcpTarget::new(service.target_host.clone(), service.target_port).map_err(|_| {
                    CliError::new(ErrorCategory::ConfigResolution, "service target is invalid")
                })?,
            ))
        })
        .collect()
}

fn resolve_client(config: &FileConfig) -> Result<ResolvedClient, CliError> {
    resolve_client_with(config, &|name| env::var(name))
}

fn resolve_client_with(
    config: &FileConfig,
    var: &dyn Fn(&str) -> Result<String, env::VarError>,
) -> Result<ResolvedClient, CliError> {
    if config.mode != "client" {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "mode must be 'client' for the client command",
        ));
    }
    let transport = client_transport(&config.transport)?;
    let server_addr =
        Endpoint::parse(config.server_addr.as_deref().ok_or_else(|| {
            CliError::new(ErrorCategory::ConfigResolution, "missing server_addr")
        })?)
        .map_err(|_| CliError::new(ErrorCategory::BindValidation, "server_addr is invalid"))?;
    let tls_server_name = config
        .tls_server_name
        .clone()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::ConfigResolution,
                "client config requires tls_server_name",
            )
        })?;
    if config.client_cert.is_some() != config.client_key.is_some() {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "client_cert and client_key must be configured together",
        ));
    }
    let proxy_name = config
        .outbound_proxy_env
        .as_deref()
        .filter(|name| !name.is_empty());
    if config
        .outbound_proxy_env
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "outbound_proxy_env must name an environment variable",
        ));
    }
    // Each environment variable and file below is read exactly once; the
    // resolved snapshot owns the values from here on.
    let token = load_token_with(&config.token_env, var)?;
    let ca_pem = read_material(&config.ca_cert, "CA bundle")?;
    let proxy = proxy_name
        .map(|name| {
            var(name)
                .map_err(|_| {
                    CliError::new(
                        ErrorCategory::MissingSecretReference,
                        format!("outbound proxy variable {name} is missing"),
                    )
                })
                .and_then(|value| {
                    if value.trim().is_empty() {
                        Err(CliError::new(
                            ErrorCategory::ConfigResolution,
                            format!("outbound proxy variable {name} is empty"),
                        ))
                    } else {
                        Ok(value)
                    }
                })
        })
        .transpose()?;
    let identity = match (&config.client_cert, &config.client_key) {
        (Some(cert), Some(key)) => Some(ClientIdentity::new(
            read_required_material(&Some(cert.clone()), "client certificate")?,
            read_required_material(&Some(key.clone()), "client key")?,
        )),
        (None, None) => None,
        _ => unreachable!("cert/key pairing checked above"),
    };
    Ok(ResolvedClient {
        server_addr,
        tls_server_name,
        ca_pem,
        token,
        services: resolve_client_services(config)?,
        transport,
        outbound_proxy: proxy,
        identity,
    })
}

fn resolve_server(config: &FileConfig) -> Result<ResolvedServer, CliError> {
    resolve_server_with(config, &|name| env::var(name))
}

fn resolve_server_with(
    config: &FileConfig,
    var: &dyn Fn(&str) -> Result<String, env::VarError>,
) -> Result<ResolvedServer, CliError> {
    if config.mode != "server" {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "mode must be 'server' for the server command",
        ));
    }
    if config.outbound_proxy_env.is_some() {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "outbound_proxy is only valid in client mode",
        ));
    }
    let transport = server_transport(&config.transport)?;
    let listen_addr = config
        .listen_addr
        .as_deref()
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::ConfigResolution,
                "server config requires listen_addr",
            )
        })?
        .parse()
        .map_err(|_| {
            CliError::new(
                ErrorCategory::BindValidation,
                "listen_addr must be a socket address such as 127.0.0.1:443",
            )
        })?;
    let token = load_token_with(&config.token_env, var)?;
    let certificate_pem = read_required_material(&config.tls_cert, "TLS certificate")?;
    let private_key_pem = read_required_material(&config.tls_key, "TLS key")?;
    let client_ca = read_material(&config.client_ca, "client CA")?;
    Ok(ResolvedServer {
        listen_addr,
        certificate_pem,
        private_key_pem,
        token,
        transport,
        allow_public_service_binds: config.allow_public_service_binds,
        client_ca,
    })
}

/// Stage 4: lower the resolved snapshot into the canonical library
/// builders. No environment or file access happens here.
fn client_builder(resolved: ResolvedClient) -> ClientBuilder {
    let config = ClientConfig {
        server_addr: resolved.server_addr.as_str().to_owned(),
        tls_server_name: resolved.tls_server_name,
        ca_pem: resolved.ca_pem,
        token: resolved.token,
        services: resolved.services,
    };
    let mut builder = ClientBuilder::new(config)
        .transport(resolved.transport)
        .runtime_policy(RuntimePolicy::default());
    if let Some(proxy) = resolved.outbound_proxy {
        builder = builder.outbound_proxy(proxy);
    }
    if let Some(identity) = resolved.identity {
        builder = builder.with_identity(identity);
    }
    builder
}

fn server_builder(resolved: ResolvedServer) -> ServerBuilder {
    let server = ServerConfig {
        listen_addr: resolved.listen_addr,
        certificate_pem: resolved.certificate_pem,
        private_key_pem: resolved.private_key_pem,
        token: resolved.token,
        allow_public_service_binds: resolved.allow_public_service_binds,
    };
    let mut builder = ServerBuilder::new(server)
        .transport(resolved.transport)
        .runtime_policy(RuntimePolicy::default())
        .bind_policy(BindPolicy {
            allow_public_addresses: resolved.allow_public_service_binds,
            ..BindPolicy::default()
        });
    if let Some(client_ca) = resolved.client_ca {
        builder = builder.client_ca_pem(client_ca);
    }
    builder
}

fn transport_name_client(profile: &ClientTransportProfile) -> &'static str {
    match profile {
        ClientTransportProfile::TcpTls => "tcp_tls",
        ClientTransportProfile::Quic => "quic",
        ClientTransportProfile::WebSocket => "websocket_tls",
    }
}

fn transport_name_server(profile: &ServerTransportProfile) -> &'static str {
    match profile {
        ServerTransportProfile::TcpTls => "tcp_tls",
        ServerTransportProfile::Quic => "quic",
        ServerTransportProfile::WebSocket => "websocket_tls",
    }
}

// ---------------------------------------------------------------------------
// Machine-readable output (all fields non-secret by construction).
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct CheckReport {
    schema: &'static str,
    ok: bool,
    mode: String,
    transport: String,
    services: usize,
    custom_ca: bool,
    mtls: bool,
    outbound_proxy: bool,
    error: Option<CheckError>,
}

#[derive(serde::Serialize)]
struct CheckError {
    category: &'static str,
    message: String,
}

fn check_report_ok(config: &FileConfig, transport: &str) -> CheckReport {
    CheckReport {
        schema: CHECK_SCHEMA,
        ok: true,
        mode: config.mode.clone(),
        transport: transport.to_owned(),
        services: config.services.len(),
        custom_ca: config.ca_cert.is_some() || config.client_ca.is_some(),
        mtls: config.client_cert.is_some() || config.client_ca.is_some(),
        outbound_proxy: config.outbound_proxy_env.is_some(),
        error: None,
    }
}

fn check_report_err(config: Option<&FileConfig>, error: &CliError) -> CheckReport {
    CheckReport {
        schema: CHECK_SCHEMA,
        ok: false,
        mode: config.map(|c| c.mode.clone()).unwrap_or_default(),
        transport: config.map(|c| c.transport.clone()).unwrap_or_default(),
        services: config.map(|c| c.services.len()).unwrap_or_default(),
        custom_ca: config.is_some_and(|c| c.ca_cert.is_some() || c.client_ca.is_some()),
        mtls: config.is_some_and(|c| c.client_cert.is_some() || c.client_ca.is_some()),
        outbound_proxy: config.is_some_and(|c| c.outbound_proxy_env.is_some()),
        error: Some(CheckError {
            category: error.category.as_str(),
            message: error.message.clone(),
        }),
    }
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string(value)
            .unwrap_or_else(|_| "{\"schema\":\"eggtunnel.error/v1\"}".to_owned())
    );
}

fn snapshot_event(snapshot: &eggtunnel::Snapshot) -> serde_json::Value {
    serde_json::json!({
        "schema": EVENT_SCHEMA,
        "event": "snapshot",
        "connected": snapshot.connected,
        "active_sessions": snapshot.active_sessions,
        "registered_services": snapshot.registered_services,
        "pending_connections": snapshot.pending_connections,
        "active_connections": snapshot.active_connections,
        "active_client_open_tasks": snapshot.active_client_open_tasks,
        "active_handshakes": snapshot.active_handshakes,
        "reconnects": snapshot.reconnects,
        "rejected_connections": snapshot.rejected_connections,
        "bytes_upstream": snapshot.bytes_upstream,
        "bytes_downstream": snapshot.bytes_downstream,
        "task_panics": snapshot.task_panics,
        "last_termination": snapshot.last_termination.map(|t| format!("{t:?}")),
        "missed_heartbeats": snapshot.heartbeat.missed_heartbeats,
        "session_generation": snapshot.heartbeat.session_generation,
        "effective_binds": snapshot.effective_binds.iter().map(|(session, service, bind)| {
            serde_json::json!({
                "session": format!("{session:?}"),
                "service_id": service.0,
                "address": std::net::Ipv6Addr::from(bind.address).to_string(),
                "port": bind.port,
            })
        }).collect::<Vec<_>>(),
    })
}

fn validate_snapshot_interval(value: Option<u64>) -> Result<Option<std::time::Duration>, CliError> {
    let Some(secs) = value else {
        return Ok(None);
    };
    if secs < MIN_SNAPSHOT_INTERVAL_SECS {
        return Err(CliError::new(
            ErrorCategory::ConfigResolution,
            format!("snapshot interval must be at least {MIN_SNAPSHOT_INTERVAL_SECS} seconds"),
        ));
    }
    Ok(Some(std::time::Duration::from_secs(secs)))
}

// ---------------------------------------------------------------------------
// Command implementations: resolve once, validate through the library,
// launch from the snapshot.
// ---------------------------------------------------------------------------

fn run_check(path: &PathBuf, json: bool) -> Result<(), CliError> {
    let config = read_config(path)?;
    let outcome = match config.mode.as_str() {
        "client" => resolve_client(&config).and_then(|resolved| {
            let transport = match &resolved.transport {
                ClientTransportProfile::TcpTls => "tcp_tls",
                ClientTransportProfile::Quic => "quic",
                ClientTransportProfile::WebSocket => "websocket_tls",
            };
            client_builder(resolved)
                .validate()
                .map_err(CliError::from)?;
            Ok(transport)
        }),
        "server" => resolve_server(&config).and_then(|resolved| {
            let transport = match &resolved.transport {
                ServerTransportProfile::TcpTls => "tcp_tls",
                ServerTransportProfile::Quic => "quic",
                ServerTransportProfile::WebSocket => "websocket_tls",
            };
            server_builder(resolved)
                .validate()
                .map_err(CliError::from)?;
            Ok(transport)
        }),
        _ => Err(CliError::new(
            ErrorCategory::ConfigResolution,
            "mode must be 'client' or 'server'",
        )),
    };
    match outcome {
        Ok(transport) => {
            if json {
                print_json(&check_report_ok(&config, transport));
            } else {
                println!("configuration is structurally valid");
            }
            Ok(())
        }
        Err(error) => {
            if json {
                print_json(&check_report_err(Some(&config), &error));
            }
            Err(error)
        }
    }
}

async fn run_server(
    path: &PathBuf,
    overrides: &ServerOverrides,
    json: bool,
    snapshot_interval: Option<u64>,
) -> Result<(), CliError> {
    // Every flag is validated before the first side effect: an invalid
    // interval must not open listeners and then fail.
    let snapshot_every = validate_snapshot_interval(snapshot_interval)?;
    let mut config = read_config(path)?;
    apply_server_overrides(&mut config, overrides)?;
    let resolved = resolve_server(&config)?;
    let transport = transport_name_server(&resolved.transport).to_owned();
    let server = server_builder(resolved)
        .bind()
        .await
        .map_err(CliError::from)?;
    let addr = server.local_addr();
    if json {
        print_json(&serde_json::json!({
            "schema": EVENT_SCHEMA,
            "event": "startup",
            "version": env!("CARGO_PKG_VERSION"),
            "mode": "server",
            "transport": transport,
        }));
        print_json(&serde_json::json!({
            "schema": EVENT_SCHEMA,
            "event": "server_listening",
            "addr": addr.to_string(),
        }));
    } else {
        println!("server listening on {addr}");
    }
    let handle = server.handle();
    let mut printed = std::collections::HashSet::new();
    let mut refresh = tokio::time::interval(std::time::Duration::from_millis(250));
    let mut snapshot_tick = futures_time_tick(snapshot_every);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = refresh.tick() => {
                for (session, service, bind) in handle.snapshot().effective_binds {
                    let key = (session, service, bind.address, bind.port);
                    if printed.insert(key) {
                        if json {
                            print_json(&serde_json::json!({
                                "schema": EVENT_SCHEMA,
                                "event": "service_bind",
                                "session": format!("{session:?}"),
                                "service_id": service.0,
                                "address": std::net::Ipv6Addr::from(bind.address).to_string(),
                                "port": bind.port,
                            }));
                        } else {
                            println!("service {} session {:?} listening on [{}]:{}", service.0, session, std::net::Ipv6Addr::from(bind.address), bind.port);
                        }
                    }
                }
            }
            _ = snapshot_tick.tick(), if snapshot_every.is_some() => {
                print_json(&snapshot_event(&handle.snapshot()));
            }
        }
    }
    if json {
        print_json(&serde_json::json!({
            "schema": EVENT_SCHEMA,
            "event": "shutdown",
            "reason": "signal",
        }));
    }
    server.shutdown().await;
    Ok(())
}

/// A ticker that only fires when an interval is configured. Without an
/// interval it never ticks, keeping the `select!` branch dormant.
fn futures_time_tick(interval: Option<std::time::Duration>) -> tokio::time::Interval {
    // Far-future first tick; the branch is only enabled when configured.
    let start = tokio::time::Instant::now()
        + interval.unwrap_or(std::time::Duration::from_secs(86400 * 365));
    tokio::time::interval_at(
        start,
        interval.unwrap_or(std::time::Duration::from_secs(86400 * 365)),
    )
}

async fn run_client(
    path: &PathBuf,
    overrides: &ClientOverrides,
    json: bool,
    snapshot_interval: Option<u64>,
) -> Result<(), CliError> {
    // Every flag is validated before the first side effect: an invalid
    // interval must not start the runtime and then fail.
    let snapshot_every = validate_snapshot_interval(snapshot_interval)?;
    let mut config = read_config(path)?;
    apply_client_overrides(&mut config, overrides)?;
    let resolved = resolve_client(&config)?;
    let transport = transport_name_client(&resolved.transport).to_owned();
    let service_count = resolved.services.len();
    let client = client_builder(resolved)
        .start()
        .await
        .map_err(CliError::from)?;
    if json {
        print_json(&serde_json::json!({
            "schema": EVENT_SCHEMA,
            "event": "startup",
            "version": env!("CARGO_PKG_VERSION"),
            "mode": "client",
            "transport": transport,
            "services": service_count,
        }));
    } else {
        println!("client started; waiting for authenticated session");
    }
    let handle = client.handle();
    let mut refresh = tokio::time::interval(std::time::Duration::from_millis(250));
    let mut snapshot_tick = futures_time_tick(snapshot_every);
    let mut was_connected = false;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = refresh.tick() => {
                let snapshot = handle.snapshot();
                if json {
                    if snapshot.connected && !was_connected {
                        print_json(&serde_json::json!({
                            "schema": EVENT_SCHEMA,
                            "event": "session_ready",
                            "generation": snapshot.heartbeat.session_generation,
                            "registered_services": snapshot.registered_services,
                        }));
                    } else if !snapshot.connected && was_connected {
                        print_json(&serde_json::json!({
                            "schema": EVENT_SCHEMA,
                            "event": "session_lost",
                            "termination": snapshot.last_termination.map(|t| format!("{t:?}")),
                            "reconnects": snapshot.reconnects,
                        }));
                    }
                }
                was_connected = snapshot.connected;
            }
            _ = snapshot_tick.tick(), if snapshot_every.is_some() => {
                print_json(&snapshot_event(&handle.snapshot()));
            }
        }
    }
    if json {
        print_json(&serde_json::json!({
            "schema": EVENT_SCHEMA,
            "event": "shutdown",
            "reason": "signal",
        }));
    }
    client.shutdown().await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Version => {
            println!("eggtunnel {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Check { config: path, json } => {
            run_check(&path, json)?;
            Ok(())
        }
        Command::Server {
            config: path,
            overrides,
            json,
            snapshot_interval_secs,
        } => {
            run_server(&path, &overrides, json, snapshot_interval_secs).await?;
            Ok(())
        }
        Command::Client {
            config: path,
            overrides,
            json,
            snapshot_interval_secs,
        } => {
            run_client(&path, &overrides, json, snapshot_interval_secs).await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Map-backed environment provider: proves resolution reads each name
    /// through one injectable lookup and never touches ambient state.
    #[derive(Default)]
    struct TestEnv {
        vars: HashMap<String, String>,
        reads: std::cell::RefCell<Vec<String>>,
    }

    impl TestEnv {
        fn with(mut self, name: &str, value: &str) -> Self {
            self.vars.insert(name.to_owned(), value.to_owned());
            self
        }
        fn provider(&self) -> impl Fn(&str) -> Result<String, env::VarError> + '_ {
            |name| {
                self.reads.borrow_mut().push(name.to_owned());
                self.vars
                    .get(name)
                    .cloned()
                    .ok_or(env::VarError::NotPresent)
            }
        }
        fn read_count(&self, name: &str) -> usize {
            self.reads.borrow().iter().filter(|n| *n == name).count()
        }
    }

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "eggtunnel-cli-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, bytes).unwrap();
        path
    }

    fn client_toml(token_env: &str) -> String {
        format!(
            r#"mode = "client"
transport = "tcp_tls"
server_addr = "tunnel.example.net:9443"
tls_server_name = "tunnel.example.net"
token_env = "{token_env}"

[[services]]
id = 1
name = "web"
target_host = "127.0.0.1"
target_port = 8080
bind_port = 0
"#
        )
    }

    fn write_toml(body: &str) -> PathBuf {
        temp_file("config.toml", body.as_bytes())
    }

    fn file_config(body: &str) -> FileConfig {
        let path = write_toml(body);
        let config = read_config(&path).unwrap();
        fs::remove_file(path).ok();
        config
    }

    #[test]
    fn overrides_win_over_toml_without_touching_unrelated_fields() {
        let env = TestEnv::default().with("TOKEN", "override-token-value");
        let mut config = file_config(&client_toml("TOKEN"));
        apply_client_overrides(
            &mut config,
            &ClientOverrides {
                server_addr: Some("other.example:10443".to_owned()),
                tls_server_name: Some("other.example".to_owned()),
                transport: Some("tcp_tls".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        let resolved = resolve_client_with(&config, &env.provider()).unwrap();
        assert_eq!(resolved.server_addr.as_str(), "other.example:10443");
        assert_eq!(resolved.tls_server_name, "other.example");
        // Untouched fields keep TOML values.
        assert_eq!(resolved.services.len(), 1);
        assert_eq!(resolved.services[0].name.as_str(), "web");
    }

    #[test]
    fn toml_only_resolution_matches_the_library_builder_profile() {
        let env = TestEnv::default().with("TOKEN", "toml-only-token");
        let config = file_config(&client_toml("TOKEN"));
        let resolved = resolve_client_with(&config, &env.provider()).unwrap();
        client_builder(resolved).validate().unwrap();
    }

    #[test]
    fn bind_port_override_requires_exactly_one_service() {
        let env = TestEnv::default().with("TOKEN", "bind-port-token");
        let mut config = file_config(&client_toml("TOKEN"));
        config.services.clear();
        let error = apply_client_overrides(
            &mut config,
            &ClientOverrides {
                bind_port: Some(8080),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.category, ErrorCategory::ConfigResolution);

        let mut config = file_config(&client_toml("TOKEN"));
        config.services.push(FileService {
            id: 2,
            name: "two".to_owned(),
            target_host: "127.0.0.1".to_owned(),
            target_port: 8081,
            bind_port: 0,
        });
        let error = apply_client_overrides(
            &mut config,
            &ClientOverrides {
                bind_port: Some(8080),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.category, ErrorCategory::ConfigResolution);

        let mut config = file_config(&client_toml("TOKEN"));
        apply_client_overrides(
            &mut config,
            &ClientOverrides {
                bind_port: Some(18080),
                ..Default::default()
            },
        )
        .unwrap();
        let resolved = resolve_client_with(&config, &env.provider()).unwrap();
        assert_eq!(
            resolved.services[0].requested_bind,
            RequestedBind::Loopback { port: 18080 }
        );
    }

    #[test]
    fn resolved_snapshot_ignores_later_input_changes() {
        let mut env = TestEnv::default().with("TOKEN", "snapshot-token");
        let ca = temp_file("ca.pem", b"ca-bytes");
        let body = client_toml("TOKEN");
        let mut config = file_config(&body);
        config.ca_cert = Some(ca.clone());
        let resolved = resolve_client_with(&config, &env.provider()).unwrap();
        assert_eq!(env.read_count("TOKEN"), 1);
        // Mutate every input after resolution: the snapshot must not move.
        env.vars.remove("TOKEN");
        fs::remove_file(&ca).unwrap();
        client_builder(resolved).validate().unwrap();
    }

    #[test]
    fn debug_and_json_never_carry_secret_values() {
        let secret = "super-secret-token-value-123";
        let proxy_value = "http://user:hunter2@proxy:3128";
        let env = TestEnv::default()
            .with("TOKEN", secret)
            .with("PROXY", proxy_value);
        let ca = temp_file("ca.pem", b"ca-bytes");
        let mut body = client_toml("TOKEN");
        body.push_str("outbound_proxy_env = \"PROXY\"\n");
        let mut config = file_config(&body);
        config.ca_cert = Some(ca.clone());
        let resolved = resolve_client_with(&config, &env.provider()).unwrap();
        for rendered in [
            format!("{resolved:?}"),
            serde_json::to_string(&check_report_ok(&config, "tcp_tls")).unwrap(),
            snapshot_event(&eggtunnel::Snapshot::default()).to_string(),
        ] {
            assert!(!rendered.contains(secret), "secret leaked: {rendered}");
            assert!(!rendered.contains("hunter2"), "proxy secret leaked");
        }
        fs::remove_file(ca).ok();
    }

    #[test]
    fn check_json_schema_is_small_stable_and_redacted() {
        let _env = TestEnv::default().with("TOKEN", "schema-token");
        let config = file_config(&client_toml("TOKEN"));
        let report = check_report_ok(&config, "tcp_tls");
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["schema"], "eggtunnel.check/v1");
        assert_eq!(value["ok"], true);
        assert_eq!(value["mode"], "client");
        assert_eq!(value["transport"], "tcp_tls");
        assert_eq!(value["services"], 1);
        assert_eq!(value["custom_ca"], false);
        assert_eq!(value["mtls"], false);
        assert_eq!(value["outbound_proxy"], false);
        assert!(value["error"].is_null());

        let error = CliError::new(ErrorCategory::BindValidation, "bad endpoint");
        let report = check_report_err(Some(&config), &error);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["category"], "bind_validation");
    }

    #[test]
    fn unknown_transport_and_mode_fail_with_stable_categories() {
        let env = TestEnv::default().with("TOKEN", "category-token");
        let mut config = file_config(&client_toml("TOKEN"));
        config.transport = "wss".to_owned();
        let error = resolve_client_with(&config, &env.provider()).unwrap_err();
        assert_eq!(error.category, ErrorCategory::Transport);
        config.transport = "tcp_tls".to_owned();
        config.mode = "daemon".to_owned();
        let error = resolve_client_with(&config, &env.provider()).unwrap_err();
        assert_eq!(error.category, ErrorCategory::ConfigResolution);
    }

    #[test]
    fn quic_with_custom_ca_is_rejected_by_the_library_validator() {
        let env = TestEnv::default().with("TOKEN", "quic-ca-token");
        let ca = temp_file("ca.pem", b"ca-bytes");
        let mut body = client_toml("TOKEN");
        body = body.replace("tcp_tls", "quic");
        body.push_str(&format!("ca_cert = {:?}\n", ca.to_string_lossy()));
        let mut config = file_config(&body);
        config.ca_cert = Some(ca.clone());
        let resolved = resolve_client_with(&config, &env.provider()).unwrap();
        let error = client_builder(resolved).validate().unwrap_err();
        let cli: CliError = error.into();
        assert_eq!(cli.category, ErrorCategory::ProfileValidation);
        fs::remove_file(ca).ok();
    }

    #[test]
    fn endpoint_parity_covers_ipv6_dns_and_rejections() {
        let env = TestEnv::default().with("TOKEN", "endpoint-token");
        for addr in ["[::1]:443", "localhost:443", "192.0.2.1:9443"] {
            let mut config = file_config(&client_toml("TOKEN"));
            config.server_addr = Some(addr.to_owned());
            let resolved = resolve_client_with(&config, &env.provider()).unwrap();
            assert_eq!(resolved.server_addr.as_str(), addr);
        }
        for addr in ["::1:443", "example.com:0", "exa mple.com:443"] {
            let mut config = file_config(&client_toml("TOKEN"));
            config.server_addr = Some(addr.to_owned());
            let error = resolve_client_with(&config, &env.provider()).unwrap_err();
            assert_eq!(error.category, ErrorCategory::BindValidation, "{addr}");
        }
    }

    #[test]
    fn snapshot_interval_has_a_minimum_and_requires_no_ambient_state() {
        assert!(validate_snapshot_interval(None).unwrap().is_none());
        assert_eq!(
            validate_snapshot_interval(Some(5)).unwrap(),
            Some(std::time::Duration::from_secs(5))
        );
        let error = validate_snapshot_interval(Some(1)).unwrap_err();
        assert_eq!(error.category, ErrorCategory::ConfigResolution);
    }

    #[test]
    fn missing_token_reference_is_a_stable_category() {
        let body = client_toml("EGGTUNNEL_TEST_DEFINITELY_UNSET_VAR");
        let env = TestEnv::default();
        let config = file_config(&body);
        let error = resolve_client_with(&config, &env.provider()).unwrap_err();
        assert_eq!(error.category, ErrorCategory::MissingSecretReference);
        assert!(!error.to_string().contains("super-secret"));
    }

    #[test]
    fn an_empty_token_reference_is_rejected_explicitly() {
        // Same explicit rejection as an empty `outbound_proxy_env`, rather than
        // falling through to an environment lookup of the empty name.
        let env = TestEnv::default();
        let config = file_config(&client_toml(""));
        let error = resolve_client_with(&config, &env.provider()).unwrap_err();
        assert_eq!(error.category, ErrorCategory::ConfigResolution);
        assert_eq!(error.message, "token_env must name an environment variable");
        assert!(env.reads.borrow().is_empty(), "no lookup should be issued");

        let env = TestEnv::default().with("TOKEN", "server-token");
        let config = file_config(
            r#"mode = "server"
transport = "tcp_tls"
listen_addr = "127.0.0.1:0"
token_env = ""
"#,
        );
        let error = resolve_server_with(&config, &env.provider()).unwrap_err();
        assert_eq!(error.category, ErrorCategory::ConfigResolution);
        assert_eq!(error.message, "token_env must name an environment variable");
        assert!(env.reads.borrow().is_empty(), "no lookup should be issued");
    }

    #[test]
    fn server_resolution_rejects_client_only_inputs() {
        let env = TestEnv::default().with("TOKEN", "server-token");
        let body = r#"mode = "server"
transport = "tcp_tls"
listen_addr = "127.0.0.1:0"
tls_cert = "/tmp/cert.pem"
tls_key = "/tmp/key.pem"
token_env = "TOKEN"
outbound_proxy_env = "SOME_PROXY"
"#;
        let config = file_config(body);
        let error = resolve_server_with(&config, &env.provider()).unwrap_err();
        assert_eq!(error.category, ErrorCategory::ConfigResolution);
    }

    #[test]
    fn snapshot_event_renders_effective_binds_from_the_snapshot() {
        let mut snapshot = eggtunnel::Snapshot {
            connected: true,
            registered_services: 1,
            ..Default::default()
        };
        snapshot.effective_binds.push((
            eggtunnel::proto::SessionId([7; 16]),
            eggtunnel::proto::ServiceId(9),
            eggtunnel::proto::EffectiveBind {
                address: std::net::Ipv6Addr::LOCALHOST.octets(),
                port: 31001,
            },
        ));
        let value = snapshot_event(&snapshot);
        assert_eq!(value["event"], "snapshot");
        assert_eq!(value["connected"], true);
        assert_eq!(value["registered_services"], 1);
        let binds = value["effective_binds"].as_array().unwrap();
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0]["service_id"], 9);
        assert_eq!(binds[0]["port"], 31001);
        assert_eq!(binds[0]["address"], "::1");
    }

    #[test]
    fn error_display_names_the_category_without_secret_values() {
        let error = CliError::new(ErrorCategory::TlsMaterial, "TLS key file must not be empty");
        assert_eq!(
            error.to_string(),
            "tls_material: TLS key file must not be empty"
        );
        for category in [
            ErrorCategory::ConfigParse,
            ErrorCategory::ConfigResolution,
            ErrorCategory::MissingSecretReference,
            ErrorCategory::TlsMaterial,
            ErrorCategory::ProfileValidation,
            ErrorCategory::BindValidation,
            ErrorCategory::RuntimeStart,
            ErrorCategory::Transport,
            ErrorCategory::Authentication,
        ] {
            assert!(!category.as_str().is_empty());
        }
    }
}
