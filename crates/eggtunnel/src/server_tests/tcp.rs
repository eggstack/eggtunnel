use super::*;

    #[tokio::test]
    async fn application_target_connector_relays_without_loopback_target() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"direct-connector-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_with_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"application-stream").await,
            b"application-stream"
        );
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn client_cancellation_releases_pending_direct_connector_and_external_peer() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"pending-connector-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_with_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("pending-connector").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            Arc::new(PendingConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if server.handle().snapshot().pending_connections == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await;
        let mut drained = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), external.read_to_end(&mut drained))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(server.handle().snapshot().pending_connections, 0);
        assert_eq!(server.handle().snapshot().active_connections, 0);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn custom_runtime_pending_limit_saturates_and_recovers() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"custom-pending-limit".to_vec()).unwrap();
        let mut policy = crate::RuntimePolicy::default();
        policy.limits.pending_per_session = 1;
        policy.limits.active_connections_per_session = 2;
        let server = crate::ServerBuilder::new(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .runtime_policy(policy)
        .bind()
        .await
        .unwrap();
        let client = crate::ClientBuilder::new(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.into_bytes()),
            token,
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("limited-pending").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .with_connector(Arc::new(PendingConnector))
        .runtime_policy(policy)
        .start()
        .await
        .unwrap();
        let handle = server.handle();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = handle.snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let first = TcpStream::connect(addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.snapshot().pending_connections == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let second = TcpStream::connect(addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.snapshot().rejected_connections > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(handle.snapshot().pending_connections, 1);
        drop((first, second));
        client.shutdown().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = handle.snapshot();
                if snapshot.pending_connections == 0 && snapshot.active_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(handle.snapshot().resource_limits.pending_per_session, 1);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn custom_pending_timeout_expires_pending_connection_and_releases_capacity() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"custom-pending-timeout".to_vec()).unwrap();
        let mut policy = crate::RuntimePolicy::default();
        policy.timeouts.pending_connection = Duration::from_millis(250);
        let server = crate::ServerBuilder::new(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .runtime_policy(policy)
        .bind()
        .await
        .unwrap();
        let client = crate::ClientBuilder::new(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.into_bytes()),
            token,
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("pending-timeout").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .with_connector(Arc::new(PendingConnector))
        .runtime_policy(policy)
        .start()
        .await
        .unwrap();
        let handle = server.handle();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = handle.snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.snapshot().pending_connections == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = handle.snapshot();
                if snapshot.pending_connections == 0 && snapshot.active_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut drained = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), external.read_to_end(&mut drained))
            .await
            .unwrap()
            .unwrap();
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn custom_handshake_timeout_is_reported_as_timeout() {
        let (cert, key) = certificate();
        let mut policy = crate::RuntimePolicy::default();
        policy.timeouts.handshake = Duration::from_millis(250);
        let server = crate::ServerBuilder::new(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"custom-handshake-timeout".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .runtime_policy(policy)
        .bind()
        .await
        .unwrap();
        let handle = server.handle();
        let peer = TcpStream::connect(server.local_addr()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if handle.snapshot().active_handshakes == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = handle.snapshot();
                if snapshot.active_handshakes == 0
                    && snapshot.last_termination == Some(crate::TerminationCategory::Timeout)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        drop(peer);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn custom_control_idle_timeout_is_reported_as_timeout() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"custom-control-idle-timeout".to_vec()).unwrap();
        let mut server_policy = crate::RuntimePolicy::default();
        server_policy.timeouts.control_idle = Duration::from_millis(400);
        server_policy.timeouts.heartbeat_interval = Duration::from_millis(100);
        let server = crate::ServerBuilder::new(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .runtime_policy(server_policy)
        .bind()
        .await
        .unwrap();
        let mut client_policy = crate::RuntimePolicy::default();
        client_policy.timeouts.control_idle = Duration::from_secs(3);
        client_policy.timeouts.heartbeat_interval = Duration::from_secs(2);
        let client = crate::ClientBuilder::new(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.into_bytes()),
            token,
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("idle-timeout").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .runtime_policy(client_policy)
        .start()
        .await
        .unwrap();
        let handle = server.handle();
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if handle.snapshot().last_termination
                    == Some(crate::TerminationCategory::Timeout)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn tcp_tls_reverse_session_registers_and_relays_data() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"test-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();

        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            while let Ok((stream, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.into_split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                    let _ = write.shutdown().await;
                });
            }
        });

        let service = ClientService::new(
            ServiceId(1),
            ServiceName::new("echo").unwrap(),
            eggtunnel_proto::RequestedBind::Loopback { port: 0 },
            TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
        );
        let second_service = ClientService::new(
            ServiceId(2),
            ServiceName::new("echo-two").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
        );
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token,
            services: vec![service, second_service],
        })
        .await
        .unwrap();

        let bound = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server.handle().snapshot();
                if snapshot.effective_binds.len() == 2 {
                    break snapshot
                        .effective_binds
                        .iter()
                        .map(|(_, _, effective)| {
                            SocketAddr::V6(std::net::SocketAddrV6::new(
                                std::net::Ipv6Addr::from(effective.address),
                                effective.port,
                                0,
                                0,
                            ))
                        })
                        .collect::<Vec<_>>();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (first, second) = tokio::join!(
            roundtrip(bound[0], b"first-connection"),
            roundtrip(bound[1], b"second-connection")
        );
        assert_eq!(first, b"first-connection");
        assert_eq!(second, b"second-connection");
        assert!(server.handle().snapshot().bytes_upstream > 0);
        assert!(server.handle().snapshot().bytes_downstream > 0);
        assert!(client.handle().snapshot().bytes_upstream > 0);
        assert!(client.handle().snapshot().bytes_downstream > 0);
        let server_snapshot = server.handle().snapshot();
        let client_snapshot = client.handle().snapshot();
        assert_eq!(server_snapshot.resource_limits.sessions, MAX_SESSIONS);
        assert_eq!(server_snapshot.high_water_services, 2);
        assert!(server_snapshot.high_water_active_connections >= 1);
        assert!(server_snapshot.high_water_pending_connections >= 1);
        assert!(server_snapshot.high_water_handshakes >= 1);
        assert!(client_snapshot.high_water_client_open_tasks >= 1);

        let mut active_external = TcpStream::connect(bound[1]).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().active_connections > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        client
            .handle()
            .unregister_service(ServiceId(1))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().effective_binds.len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        client.shutdown().await;
        let mut drained = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            active_external.read_to_end(&mut drained),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(server.handle().snapshot().active_connections, 0);
        assert_eq!(server.handle().snapshot().pending_connections, 0);
        server.shutdown().await;
        echo_task.abort();
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn production_quic_profile_rejects_untrusted_server_certificate() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-trust-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_quic(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-trust-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("must-not-register").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if client.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(server.handle().snapshot().registered_services, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn bad_token_does_not_create_a_registered_session() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"incorrect-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("echo").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server.handle().snapshot();
                if snapshot.rejected_connections > 0
                    && snapshot.last_termination == Some(TerminationCategory::Authentication)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(client.handle().snapshot().reconnects, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn wrong_tls_server_name_is_rejected_before_authentication() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "wrong.example".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("echo").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if client.handle().snapshot().reconnects > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(server.handle().snapshot().registered_services, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn public_service_bind_is_denied_without_explicit_policy() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("public").unwrap(),
                RequestedBind::Ip {
                    address: "2001:db8::1"
                        .parse::<std::net::Ipv6Addr>()
                        .unwrap()
                        .octets(),
                    port: 9000,
                },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().rejected_connections > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 0);
        assert!(server.handle().snapshot().effective_binds.is_empty());
        assert_eq!(client.handle().snapshot().reconnects, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn refused_target_rejects_external_connection_and_releases_pending_capacity() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let unavailable = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let client = Client::start(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: SecretToken::new(b"correct-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("refused").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new(unavailable.ip().to_string(), unavailable.port()).unwrap(),
            )],
        })
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((_, _, bind)) = server.handle().snapshot().effective_binds.first() {
                    break bind.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        let mut external = TcpStream::connect(addr).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), external.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.is_empty());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server.handle().snapshot().pending_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn client_reconnects_and_restores_services_in_a_new_session_generation() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"restart-secret".to_vec()).unwrap();
        let first_server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let endpoint = first_server.local_addr();
        let service = ClientService::new(
            ServiceId(1),
            ServiceName::new("restored").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", 9).unwrap(),
        );
        let client = Client::start(ClientConfig {
            server_addr: endpoint.to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: Some(cert.as_bytes().to_vec()),
            token: token.clone(),
            services: vec![service],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if first_server.handle().snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let first_session = first_server.handle().snapshot().effective_binds[0].0;
        first_server.shutdown().await;

        let second_server = Server::bind(ServerConfig {
            listen_addr: endpoint,
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token,
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if second_server.handle().snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let second_session = second_server.handle().snapshot().effective_binds[0].0;
        assert_ne!(first_session, second_session);
        assert!(client.handle().snapshot().reconnects > 0);
        client.shutdown().await;
        second_server.shutdown().await;
    }

    #[tokio::test]
    async fn data_hello_is_session_service_bound_and_single_use() {
        let session_id = SessionId([1; 16]);
        let (session, sessions, counters) = test_session(session_id).await;
        let connection_id = eggtunnel_proto::ConnectionId([3; 16]);
        let (data_tx, data_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            connection_id,
            PendingEntry {
                service_id: ServiceId(5),
                expires: Instant::now() + Duration::from_secs(5),
                data_tx,
            },
        );
        counters
            .pending
            .store(1, std::sync::atomic::Ordering::Relaxed);

        let wrong_session = DataHello {
            session_id: SessionId([2; 16]),
            service_id: ServiceId(5),
            connection_id,
        };
        assert!(matches!(
            accept_data_hello(
                test_data_stream(),
                wrong_session,
                None,
                &sessions,
                &counters
            )
            .await,
            Err(TunnelError::Authentication)
        ));
        assert_eq!(session.pending.lock().await.len(), 1);

        let correct = DataHello {
            session_id,
            service_id: ServiceId(5),
            connection_id,
        };
        accept_data_hello(
            test_data_stream(),
            correct.clone(),
            None,
            &sessions,
            &counters,
        )
        .await
        .unwrap();
        assert!(data_rx.await.is_ok());
        assert_eq!(
            counters.pending.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(matches!(
            accept_data_hello(test_data_stream(), correct, None, &sessions, &counters).await,
            Err(TunnelError::Authorization)
        ));
    }

    #[tokio::test]
    async fn wrong_service_and_expired_data_hellos_consume_and_reject_pending_state() {
        let session_id = SessionId([8; 16]);
        let (session, sessions, counters) = test_session(session_id).await;
        let wrong_service_id = eggtunnel_proto::ConnectionId([9; 16]);
        let (wrong_tx, _wrong_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            wrong_service_id,
            PendingEntry {
                service_id: ServiceId(1),
                expires: Instant::now() + Duration::from_secs(5),
                data_tx: wrong_tx,
            },
        );
        counters
            .pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(
            accept_data_hello(
                test_data_stream(),
                DataHello {
                    session_id,
                    service_id: ServiceId(2),
                    connection_id: wrong_service_id
                },
                None,
                &sessions,
                &counters
            )
            .await
            .is_err()
        );

        let expired_id = eggtunnel_proto::ConnectionId([10; 16]);
        let (expired_tx, _expired_rx) = oneshot::channel();
        session.pending.lock().await.insert(
            expired_id,
            PendingEntry {
                service_id: ServiceId(1),
                expires: Instant::now() - Duration::from_millis(1),
                data_tx: expired_tx,
            },
        );
        counters
            .pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(
            accept_data_hello(
                test_data_stream(),
                DataHello {
                    session_id,
                    service_id: ServiceId(1),
                    connection_id: expired_id
                },
                None,
                &sessions,
                &counters
            )
            .await
            .is_err()
        );
        assert_eq!(session.pending.lock().await.len(), 0);
        assert_eq!(
            counters.pending.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn repeated_client_server_start_stop_returns_runtime_counts_to_zero() {
        for cycle in 0..3u64 {
            let (cert, key) = certificate();
            let token = SecretToken::new(format!("cycle-secret-{cycle}").into_bytes()).unwrap();
            let server = Server::bind(ServerConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                certificate_pem: cert.as_bytes().to_vec(),
                private_key_pem: key.as_bytes().to_vec(),
                token: token.clone(),
                allow_public_service_binds: false,
            })
            .await
            .unwrap();
            let server_handle = server.handle();
            let client = Client::start(ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token,
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("cycle-service").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if server_handle.snapshot().registered_services == 1 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            client.shutdown().await;
            server.shutdown().await;
            let snapshot = server_handle.snapshot();
            assert_eq!(snapshot.active_sessions, 0);
            assert_eq!(snapshot.registered_services, 0);
            assert_eq!(snapshot.pending_connections, 0);
            assert_eq!(snapshot.active_connections, 0);
            assert_eq!(snapshot.active_handshakes, 0);
        }
    }

    #[tokio::test]
    async fn owned_task_panic_is_counted_as_internal_termination() {
        let counters = Counters::default();
        let result = tokio::spawn(async { panic!("injected child panic") }).await;
        counters.record_join_result(&result);
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.task_panics, 1);
        assert_eq!(
            snapshot.last_termination,
            Some(TerminationCategory::Internal)
        );
    }

    #[tokio::test]
    async fn server_shutdown_cancels_incomplete_tls_and_authentication_handshakes() {
        use eggress_transport_tls::{TlsClientConfigBuilder, tls_connect};

        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"handshake-cancel-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let handle = server.handle();
        let incomplete_tls = TcpStream::connect(server.local_addr()).await.unwrap();
        let tls_config = TlsClientConfigBuilder::new()
            .with_custom_ca_pem(cert.as_bytes())
            .unwrap()
            .build()
            .unwrap();
        let control_tcp = TcpStream::connect(server.local_addr()).await.unwrap();
        let mut control = tls_connect(Box::new(control_tcp), tls_config, "localhost")
            .await
            .unwrap();
        write_boxed(
            &mut control,
            &Message::ClientHello(ClientHello {
                version: ProtocolVersion::CURRENT,
                capabilities: Capabilities::default(),
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            read_boxed(&mut control).await.unwrap(),
            Message::ServerHello(_)
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.snapshot().active_handshakes == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server.shutdown().await;
        assert_eq!(handle.snapshot().active_handshakes, 0);
        assert_eq!(handle.snapshot().active_sessions, 0);
        drop(control);
        drop(incomplete_tls);
    }

    #[tokio::test]
    async fn unauthenticated_handshake_admission_caps_at_limit_and_recovers() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"admission-cap-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let handle = server.handle();
        let mut peers = Vec::new();
        for _ in 0..(MAX_HANDSHAKES + 1) {
            peers.push(TcpStream::connect(server.local_addr()).await.unwrap());
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = handle.snapshot();
                if snapshot.active_handshakes == MAX_HANDSHAKES && snapshot.rejected_connections > 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(handle.snapshot().active_handshakes, MAX_HANDSHAKES);
        drop(peers);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if handle.snapshot().active_handshakes == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        server.shutdown().await;
    }

    #[test]
    fn auth_failure_limiter_is_per_source_bounded_and_expires() {
        let limiter = AuthFailureLimiter::new(2, Duration::from_secs(5), 1);
        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "192.0.2.2".parse().unwrap();
        let start = Instant::now();

        assert!(!limiter.is_blocked_at(first, start));
        limiter.record_failure_at(first, start);
        assert!(!limiter.is_blocked_at(first, start + Duration::from_secs(1)));
        limiter.record_failure_at(first, start + Duration::from_secs(1));
        assert!(limiter.is_blocked_at(first, start + Duration::from_secs(2)));
        assert!(limiter.is_blocked_at(second, start + Duration::from_secs(2)));
        assert!(!limiter.is_blocked_at(first, start + Duration::from_secs(6)));
        assert_eq!(
            limiter
                .failures
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len(),
            0
        );
    }

    #[test]
    fn bind_policy_enforces_address_port_and_ephemeral_rules() {
        let policy = BindPolicy {
            allow_public_addresses: true,
            allowed_addresses: vec![std::net::Ipv6Addr::LOCALHOST.octets()],
            allowed_port_ranges: vec![(8000, 8100)],
            allow_ephemeral_ports: false,
            max_services_per_session: 4,
        };
        assert!(policy.validate().is_ok());
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 8080
                },
                &policy
            )
            .is_ok()
        );
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 0
                },
                &policy
            )
            .is_err()
        );
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 9000
                },
                &policy
            )
            .is_err()
        );
        assert!(
            bind_to_socket(
                &RequestedBind::Ip {
                    address: "2001:db8::1"
                        .parse::<std::net::Ipv6Addr>()
                        .unwrap()
                        .octets(),
                    port: 8080
                },
                &policy
            )
            .is_err()
        );
    }

    #[cfg(feature = "quic")]
    pub(super) struct CapturingBlockingConnector {
        pub(super) captured: std::sync::Arc<std::sync::Mutex<Option<eggtunnel_proto::ConnectionId>>>,
    }

    #[cfg(feature = "quic")]
    impl TargetConnector for CapturingBlockingConnector {
        fn connect(&self, _service: ClientService, context: TargetContext) -> TargetFuture {
            let captured = self.captured.clone();
            Box::pin(async move {
                *captured.lock().unwrap_or_else(|p| p.into_inner()) = Some(context.connection_id);
                std::future::pending::<Result<TargetStream, TargetError>>().await
            })
        }
    }

    #[cfg(feature = "quic")]
    pub(super) async fn wait_for_quic_pending(server_handle: &crate::ServerHandle, expected: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().pending_connections == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(feature = "quic")]
    pub(super) async fn write_quic_data_hello(
        stream: &mut eggress_core::BoxStream,
        hello: eggtunnel_proto::DataHello,
    ) {
        use crate::wire_io::write_boxed;
        use eggtunnel_proto::Message;
        write_boxed(stream, &Message::DataHello(hello))
            .await
            .unwrap();
    }
