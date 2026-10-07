#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;
    use crate::server::auth::AuthFailureLimiter;
    use crate::server::pending::{PendingEntry, accept_data_hello};
    use crate::server::session::SessionContext;
    use crate::{
        BindPolicy, RuntimePolicy, SecretToken, ServerBuilder, ServerConfig,
        ServerTransportProfile, TerminationCategory,
    };
    #[cfg(feature = "mtls")]
    use crate::ClientIdentity;
    use crate::{
        Client, ClientConfig, ClientService, TargetConnector, TargetContext, TargetError,
        TargetFuture, TargetStream,
    };
    use crate::common::{bind_to_socket, verify_token};
    use crate::wire_io::{read_boxed, write_boxed};
    use eggtunnel_proto::{
        Capabilities, ClientHello, DataHello, Message, ProtocolVersion, RequestedBind, ServiceId,
        ServiceName, SessionId, TcpTarget,
    };
    use eggress_core::BoxStream;
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::{Mutex, Semaphore, oneshot},
    };

    /// Test-only reference ceilings matching the default `RuntimePolicy`.
    const MAX_SESSIONS: usize = 128;
    const MAX_HANDSHAKES: usize = 64;

    async fn test_session(
        session_id: SessionId,
    ) -> (
        Arc<SessionContext>,
        crate::server::session::SessionRegistry,
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
        let sessions = crate::server::session::new_session_registry();
        sessions
            .entries()
            .insert(session_id, Arc::downgrade(&context));
        (context, sessions, counters)
    }

    fn test_data_stream() -> BoxStream {
        let (stream, _peer) = tokio::io::duplex(32);
        Box::new(stream)
    }

    fn builder() -> ServerBuilder {
        ServerBuilder::new(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: b"cert".to_vec(),
            private_key_pem: b"key".to_vec(),
            token: SecretToken::new(b"test-token".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .bind_policy(BindPolicy::default())
        .runtime_policy(RuntimePolicy::default())
    }

    #[test]
    fn server_builder_accepts_tcp_tls_default_profile() {
        assert!(builder().validate().is_ok());
    }

    #[test]
    fn server_builder_keeps_public_bind_setting_in_sync_with_bind_policy() {
        let policy = BindPolicy {
            allow_public_addresses: true,
            ..BindPolicy::default()
        };
        assert!(builder().bind_policy(policy).validate().is_ok());
    }

    #[test]
    fn server_builder_rejects_empty_and_oversize_tls_material() {
        let mut config = ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: Vec::new(),
            private_key_pem: b"key".to_vec(),
            token: SecretToken::new(b"test-token".to_vec()).unwrap(),
            allow_public_service_binds: false,
        };
        assert!(ServerBuilder::new(config.clone()).validate().is_err());
        config.certificate_pem = b"cert".to_vec();
        config.private_key_pem = vec![0; eggtunnel_proto::MAX_FRAME_BYTES + 1];
        assert!(ServerBuilder::new(config).validate().is_err());
    }

    #[cfg(feature = "quic-server")]
    #[test]
    fn server_builder_accepts_quic() {
        assert!(builder()
            .transport(ServerTransportProfile::Quic)
            .validate()
            .is_ok());
    }

    #[cfg(feature = "websocket-server")]
    #[test]
    fn server_builder_accepts_websocket() {
        assert!(builder()
            .transport(ServerTransportProfile::WebSocket)
            .validate()
            .is_ok());
    }

    #[cfg(feature = "mtls")]
    #[test]
    fn server_builder_accepts_tcp_mtls() {
        assert!(builder().client_ca_pem(b"client CA".to_vec()).validate().is_ok());
    }

    #[cfg(all(feature = "mtls", feature = "quic-server"))]
    #[test]
    fn server_builder_rejects_quic_mtls() {
        assert!(builder()
            .transport(ServerTransportProfile::Quic)
            .client_ca_pem(b"client CA".to_vec())
            .validate()
            .is_err());
    }

    #[cfg(all(feature = "mtls", feature = "websocket-server"))]
    #[test]
    fn server_builder_rejects_websocket_mtls() {
        assert!(builder()
            .transport(ServerTransportProfile::WebSocket)
            .client_ca_pem(b"client CA".to_vec())
            .validate()
            .is_err());
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

    #[test]
    fn token_comparison_is_exact_across_lengths_and_contents() {
        let expected = SecretToken::new(b"correct-horse-battery-staple".to_vec()).unwrap();
        assert!(verify_token(&expected, b"correct-horse-battery-staple"));
        // Same length, different content.
        assert!(!verify_token(&expected, b"correct-horse-battery-stapl3"));
        // Different lengths, including the empty token and the ceiling.
        assert!(!verify_token(&expected, b""));
        assert!(!verify_token(&expected, b"correct-horse-battery-staple "));
        assert!(!verify_token(
            &expected,
            b"correct-horse-battery-stapl3-correct-horse-battery-staple"
        ));
        // A single flipped bit anywhere in the token is rejected.
        let mut flipped = b"correct-horse-battery-staple".to_vec();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(!verify_token(&expected, &flipped));
        // The comparison is fixed width, so a padded token of the same value
        // is still rejected on content.
        let mut padded = b"correct-horse-battery-staple".to_vec();
        padded.push(0);
        assert!(!verify_token(&expected, &padded));
    }

    #[tokio::test]
    async fn session_guard_removes_its_registry_entry_on_every_path() {
        let registry = crate::server::session::new_session_registry();
        let live_counters;
        let live = {
            let (context, _sessions, counters) = test_session(SessionId([3; 16])).await;
            live_counters = counters.clone();
            SessionContext::register(&context, &registry, MAX_SESSIONS)
                .unwrap();
            let _guard = crate::server::session::SessionGuard::new(context.clone(), registry.clone());
            assert_eq!(registry.entries().len(), 1);
            context
        };
        // The guard is gone, and the registry entry it owned is gone with it.
        drop(live);
        assert!(
            registry.entries().is_empty(),
            "a dropped Session must not leave a registry entry behind"
        );
        assert_eq!(live_counters.sessions.load(std::sync::atomic::Ordering::Relaxed), 0);
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
    // Transport integration suites exercise both roles, so they need both
    // halves of the role-specific slice.
    #[cfg(all(feature = "quic-client", feature = "quic-server"))]
    #[path = "../server_tests/quic.rs"]
    mod quic;
    #[cfg(all(feature = "websocket-client", feature = "websocket-server"))]
    #[path = "../server_tests/websocket.rs"]
    mod websocket;
    #[cfg(feature = "outbound-proxy")]
    #[path = "../server_tests/proxy.rs"]
    mod proxy;
}
