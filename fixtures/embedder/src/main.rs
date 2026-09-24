use std::sync::Arc;

use eggtunnel::{
    ClientBuilder, ClientConfig, ClientService, RuntimePolicy, SecretToken, TargetConnector,
    TargetContext, TargetFuture, TargetStream,
    proto::{RequestedBind, ServiceId, ServiceName, TcpTarget},
};

struct InProcessEcho;

impl TargetConnector for InProcessEcho {
    fn connect(&self, _service: ClientService, _context: TargetContext) -> TargetFuture {
        Box::pin(async {
            let (application, peer) = tokio::io::duplex(16 * 1024);
            tokio::spawn(async move {
                let (mut read, mut write) = tokio::io::split(peer);
                let _ = tokio::io::copy(&mut read, &mut write).await;
                let _ = tokio::io::AsyncWriteExt::shutdown(&mut write).await;
            });
            Ok(Box::new(application) as TargetStream)
        })
    }
}

fn client_config() -> Result<ClientConfig, Box<dyn std::error::Error>> {
    Ok(ClientConfig {
        server_addr: "127.0.0.1:7443".into(),
        tls_server_name: "localhost".into(),
        ca_pem: None,
        token: SecretToken::new(b"provided-out-of-band".to_vec())?,
        services: vec![ClientService::new(
            ServiceId(1),
            ServiceName::new("embedded-service")?,
            RequestedBind::Loopback { port: 0 },
            // The direct connector uses the application stream; this value
            // remains trusted, client-owned service metadata.
            TcpTarget::new("127.0.0.1", 8080)?,
        )],
    })
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut policy = RuntimePolicy::default();
    policy.limits.services_per_session = 8;
    let client = ClientBuilder::new(client_config()?)
        .with_connector(Arc::new(InProcessEcho))
        .runtime_policy(policy)
        .start()
        .await?;
    tracing::info!("embedder owns logging and runtime policy");
    client.shutdown().await;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}
