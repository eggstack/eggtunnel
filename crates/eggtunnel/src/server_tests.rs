#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;
    #[cfg(feature = "mtls")]
    use crate::ClientIdentity;
    use crate::{
        Client, ClientConfig, ClientService, TargetConnector, TargetContext, TargetError,
        TargetFuture, TargetStream,
    };
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
            principal: None,
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

    #[cfg(feature = "mtls")]
    fn mtls_certificates() -> (
        String,
        String,
        String,
        ClientIdentity,
        ClientIdentity,
        ClientIdentity,
    ) {
        use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};

        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let ca_pem = ca_cert.pem();

        let mut server_params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().unwrap();
        let server_cert = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .unwrap();

        let client_identity = |common_name: &str| {
            let mut params = CertificateParams::new(vec![common_name.to_owned()]).unwrap();
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
            let key = KeyPair::generate().unwrap();
            let certificate = params.signed_by(&key, &ca_cert, &ca_key).unwrap();
            ClientIdentity::new(
                certificate.pem().into_bytes(),
                key.serialize_pem().into_bytes(),
            )
        };
        let trusted_identity = client_identity("trusted-client");
        let trusted_identity_wrong_name = client_identity("trusted-client-wrong-name");

        let mut rogue_ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        rogue_ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let rogue_ca_key = KeyPair::generate().unwrap();
        let rogue_ca_cert = rogue_ca_params.self_signed(&rogue_ca_key).unwrap();
        let rogue_identity = {
            let mut params = CertificateParams::new(vec!["rogue-client".to_owned()]).unwrap();
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
            let key = KeyPair::generate().unwrap();
            let certificate = params
                .signed_by(&key, &rogue_ca_cert, &rogue_ca_key)
                .unwrap();
            ClientIdentity::new(
                certificate.pem().into_bytes(),
                key.serialize_pem().into_bytes(),
            )
        };

        (
            ca_pem,
            server_cert.pem(),
            server_key.serialize_pem(),
            trusted_identity,
            trusted_identity_wrong_name,
            rogue_identity,
        )
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

    struct DuplexEchoConnector;

    impl TargetConnector for DuplexEchoConnector {
        fn connect(&self, service: ClientService, _context: TargetContext) -> TargetFuture {
            Box::pin(async move {
                if service.name.as_str() != "direct-echo" {
                    return Err(TargetError::Refused);
                }
                let (application, peer) = tokio::io::duplex(64 * 1024);
                tokio::spawn(async move {
                    let (mut read, mut write) = tokio::io::split(peer);
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                    let _ = write.shutdown().await;
                });
                Ok(Box::new(application) as TargetStream)
            })
        }
    }

    struct PendingConnector;

    impl TargetConnector for PendingConnector {
        fn connect(&self, _service: ClientService, _context: TargetContext) -> TargetFuture {
            Box::pin(std::future::pending())
        }
    }

    #[path = "../server_tests/tcp.rs"]
    mod tcp;
    #[cfg(feature = "mtls")]
    #[path = "../server_tests/mtls.rs"]
    mod mtls;
    #[cfg(feature = "quic")]
    #[path = "../server_tests/quic.rs"]
    mod quic;
    #[cfg(feature = "websocket")]
    #[path = "../server_tests/websocket.rs"]
    mod websocket;
    #[cfg(feature = "outbound-proxy")]
    #[path = "../server_tests/proxy.rs"]
    mod proxy;
}
