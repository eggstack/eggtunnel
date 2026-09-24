use super::*;

    #[cfg(feature = "mtls")]
    #[tokio::test]
    async fn mtls_requires_trusted_client_certificate_and_keeps_server_name_validation() {
        let (
            ca_pem,
            server_cert,
            server_key,
            trusted_identity,
            trusted_identity_wrong_name,
            rogue_identity,
        ) = mtls_certificates();
        let server = Server::bind_mtls(
            ServerConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                certificate_pem: server_cert.as_bytes().to_vec(),
                private_key_pem: server_key.as_bytes().to_vec(),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                allow_public_service_binds: false,
            },
            ca_pem.as_bytes().to_vec(),
        )
        .await
        .unwrap();
        let service = ClientService::new(
            ServiceId(1),
            ServiceName::new("mtls-service").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", 9).unwrap(),
        );
        let trusted = Client::start_with_mtls(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(ca_pem.as_bytes().to_vec()),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                services: vec![service.clone()],
            },
            trusted_identity,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(server.handle().snapshot().high_water_sessions >= 1);

        let identity_debug = format!("{trusted_identity_wrong_name:?}");
        assert!(identity_debug.contains("REDACTED"));
        assert!(!identity_debug.contains("PRIVATE KEY"));
        let wrong_name = Client::start_with_mtls(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "wrong.example".into(),
                ca_pem: Some(ca_pem.as_bytes().to_vec()),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                services: vec![service.clone()],
            },
            trusted_identity_wrong_name,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if wrong_name.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 1);

        let rejected = Client::start_with_mtls(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(ca_pem.as_bytes().to_vec()),
                token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
                services: vec![service.clone()],
            },
            rogue_identity,
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if rejected.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 1);

        let missing = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(ca_pem.as_bytes().to_vec()),
            token: SecretToken::new(b"mtls-secret".to_vec()).unwrap(),
            services: vec![service],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if missing.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 1);

        trusted.shutdown().await;
        wrong_name.shutdown().await;
        rejected.shutdown().await;
        missing.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "mtls")]
    #[tokio::test]
    async fn mtls_principal_mismatch_cannot_attach_data_stream() {
        let session_id = SessionId([11; 16]);
        let counters = Counters::default();
        let session = Arc::new(SessionContext {
            id: session_id,
            principal: Some([1; 32]),
            cancel: CancellationToken::new(),
            pending: Mutex::new(HashMap::new()),
            connection_admission: Arc::new(Semaphore::new(1)),
            control_tx: Mutex::new(None),
            counters: counters.clone(),
        });
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        sessions
            .lock()
            .await
            .insert(session_id, Arc::downgrade(&session));
        let connection_id = eggtunnel_proto::ConnectionId([12; 16]);
        let (data_tx, _data_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            connection_id,
            PendingEntry {
                service_id: ServiceId(4),
                expires: Instant::now() + Duration::from_secs(5),
                data_tx,
            },
        );
        counters
            .pending
            .store(1, std::sync::atomic::Ordering::Relaxed);
        assert!(
            accept_data_hello(
                test_data_stream(),
                DataHello {
                    session_id,
                    service_id: ServiceId(4),
                    connection_id,
                },
                Some([2; 32]),
                &sessions,
                &counters,
            )
            .await
            .is_err()
        );
        assert_eq!(session.pending.lock().await.len(), 1);
        assert_eq!(
            counters.pending.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
