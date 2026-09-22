#![forbid(unsafe_code)]

use std::{env, fs, net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};
use eggtunnel::{
    Client, ClientConfig, ClientIdentity, ClientService, SecretToken, Server, ServerConfig,
    proto::{RequestedBind, ServiceId, ServiceName, TcpTarget},
};
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "eggtunnel", about = "Authenticated TCP reverse tunnel")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Version,
    Check { config: PathBuf },
    Client { config: PathBuf },
    Server { config: PathBuf },
}

#[derive(Deserialize)]
struct FileConfig {
    mode: String,
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
    client_cert: Option<PathBuf>,
    #[serde(default)]
    client_key: Option<PathBuf>,
    #[serde(default)]
    client_ca: Option<PathBuf>,
    #[serde(default)]
    services: Vec<FileService>,
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

fn read_config(path: &PathBuf) -> Result<FileConfig, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    Ok(toml::from_str(&text)?)
}

fn load_token(env_name: &str) -> Result<SecretToken, Box<dyn std::error::Error>> {
    let value = env::var(env_name)
        .map_err(|_| format!("required environment variable {env_name} is not set"))?;
    Ok(SecretToken::new(value.into_bytes())?)
}

fn client_services(config: &FileConfig) -> Result<Vec<ClientService>, Box<dyn std::error::Error>> {
    config
        .services
        .iter()
        .map(|service| {
            Ok(ClientService::new(
                ServiceId(service.id),
                ServiceName::new(service.name.clone())?,
                RequestedBind::Loopback {
                    port: service.bind_port,
                },
                TcpTarget::new(service.target_host.clone(), service.target_port)?,
            ))
        })
        .collect()
}

fn checked_addr(value: &str, key: &'static str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    value
        .parse()
        .map_err(|_| format!("{key} must be a socket address such as 127.0.0.1:443").into())
}

fn checked_endpoint(value: &str) -> Result<String, Box<dyn std::error::Error>> {
    let (host, port) = if value.starts_with('[') {
        let end = value
            .find(']')
            .ok_or("IPv6 endpoint must use [address]:port syntax")?;
        if value.as_bytes().get(end + 1) != Some(&b':') {
            return Err("endpoint must end with :port".into());
        }
        (&value[1..end], &value[end + 2..])
    } else {
        let (host, port) = value
            .rsplit_once(':')
            .ok_or("endpoint must use host:port syntax")?;
        (host, port)
    };
    if host.is_empty()
        || host.chars().any(char::is_whitespace)
        || !port.parse::<u16>().is_ok_and(|port| port != 0)
    {
        return Err("endpoint must contain a host and numeric port".into());
    }
    Ok(value.to_owned())
}

fn check_config(config: &FileConfig) -> Result<(), Box<dyn std::error::Error>> {
    let _token = load_token(&config.token_env)?;
    match config.mode.as_str() {
        "client" => {
            let addr = config
                .server_addr
                .as_deref()
                .ok_or("client config requires server_addr")?;
            checked_endpoint(addr)?;
            if config.tls_server_name.as_deref().is_none_or(str::is_empty) {
                return Err("client config requires tls_server_name".into());
            }
            if config.services.is_empty() {
                return Err("client config requires at least one service".into());
            }
            let _ = client_services(config)?;
            if let Some(ca) = &config.ca_cert {
                let _ = fs::read(ca)?;
            }
            if config.client_cert.is_some() != config.client_key.is_some() {
                return Err("client_cert and client_key must be configured together".into());
            }
            if let (Some(cert), Some(key)) = (&config.client_cert, &config.client_key)
                && (fs::read(cert)?.is_empty() || fs::read(key)?.is_empty())
            {
                return Err("client certificate and key files must not be empty".into());
            }
        }
        "server" => {
            let listen = config
                .listen_addr
                .as_deref()
                .ok_or("server config requires listen_addr")?;
            checked_addr(listen, "listen_addr")?;
            let cert = config
                .tls_cert
                .as_ref()
                .ok_or("server config requires tls_cert")?;
            let key = config
                .tls_key
                .as_ref()
                .ok_or("server config requires tls_key")?;
            if fs::read(cert)?.is_empty() || fs::read(key)?.is_empty() {
                return Err("TLS certificate and key files must not be empty".into());
            }
            if let Some(client_ca) = &config.client_ca
                && fs::read(client_ca)?.is_empty()
            {
                return Err("client CA file must not be empty".into());
            }
        }
        _ => return Err("mode must be 'client' or 'server'".into()),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().command {
        Command::Version => println!("eggtunnel {}", env!("CARGO_PKG_VERSION")),
        Command::Check { config: path } => {
            check_config(&read_config(&path)?)?;
            println!("configuration is structurally valid");
        }
        Command::Server { config: path } => {
            let config = read_config(&path)?;
            check_config(&config)?;
            let token = load_token(&config.token_env)?;
            let server_config = ServerConfig {
                listen_addr: checked_addr(
                    config.listen_addr.as_deref().ok_or("missing listen_addr")?,
                    "listen_addr",
                )?,
                certificate_pem: fs::read(config.tls_cert.as_ref().ok_or("missing tls_cert")?)?,
                private_key_pem: fs::read(config.tls_key.as_ref().ok_or("missing tls_key")?)?,
                token,
                allow_public_service_binds: config.allow_public_service_binds,
            };
            let server = if let Some(client_ca) = &config.client_ca {
                Server::bind_mtls(server_config, fs::read(client_ca)?).await?
            } else {
                Server::bind(server_config).await?
            };
            println!("server listening on {}", server.local_addr());
            let handle = server.handle();
            let mut printed = std::collections::HashSet::new();
            let mut refresh = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = refresh.tick() => {
                        for (session, service, bind) in handle.snapshot().effective_binds {
                            let key = (session, service, bind.address, bind.port);
                            if printed.insert(key) {
                                println!("service {} session {:?} listening on [{}]:{}", service.0, session, std::net::Ipv6Addr::from(bind.address), bind.port);
                            }
                        }
                    }
                }
            }
            server.shutdown().await;
        }
        Command::Client { config: path } => {
            let config = read_config(&path)?;
            check_config(&config)?;
            let services = client_services(&config)?;
            let tls_server_name = config
                .tls_server_name
                .clone()
                .ok_or("missing tls_server_name")?;
            let client_config = ClientConfig {
                server_addr: checked_endpoint(
                    config.server_addr.as_deref().ok_or("missing server_addr")?,
                )?,
                tls_server_name,
                ca_pem: config.ca_cert.as_ref().map(fs::read).transpose()?,
                token: load_token(&config.token_env)?,
                services,
            };
            let client = if let (Some(client_cert), Some(client_key)) =
                (&config.client_cert, &config.client_key)
            {
                Client::start_with_mtls(
                    client_config,
                    ClientIdentity::new(fs::read(client_cert)?, fs::read(client_key)?),
                )
                .await?
            } else {
                Client::start(client_config).await?
            };
            println!("client started; waiting for authenticated session");
            tokio::signal::ctrl_c().await?;
            client.shutdown().await;
        }
    }
    Ok(())
}
