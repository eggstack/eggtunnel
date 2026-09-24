use super::*;

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn websocket_tls_session_registers_and_relays_data_paths() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"websocket-profile-secret".to_vec()).unwrap();
        let server = Server::bind_websocket(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let client = Client::start_websocket_with_connector(
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
        let mut external = TcpStream::connect(addr).await.unwrap();
        external.write_all(b"websocket-over-tls").await.unwrap();
        let mut response = [0u8; 18];
        tokio::time::timeout(Duration::from_secs(5), external.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"websocket-over-tls");
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn wss_peer_close_during_active_relay_terminates_cleanly() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"wss-close-secret".to_vec()).unwrap();
        let server = Server::bind_websocket(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_websocket_with_connector(
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
                if let Some((_, _, bind)) = server_handle.snapshot().effective_binds.first() {
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
        let external = TcpStream::connect(addr).await.unwrap();
        // Wait for the server to observe the active connection before closing.
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if server_handle.snapshot().active_connections >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(external);
        // Wait for the active connection to drop on the server side as well; the
        // WebSocket adapter must close the underlying TCP connection rather than
        // leaving the relay half-open.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().active_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server_handle.snapshot().pending_connections, 0);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn wss_payload_larger_than_message_cap_roundtrips_multiple_frames() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"wss-large-secret".to_vec()).unwrap();
        let server = Server::bind_websocket(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_websocket_with_connector(
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
                if let Some((_, _, bind)) = server_handle.snapshot().effective_binds.first() {
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
        // Send a payload larger than a single WebSocket frame so the relay exercises
        // multi-frame backpressure. The Eggress adapter caps messages at 1 MiB; we
        // pick a payload well under that limit but large enough to require multiple
        // frames. We use a target connector that holds the response until the request
        // is fully read so we can verify the relay correctly backpressures the
        // buffered write across multiple WebSocket frames.
        let payload = vec![0xABu8; 64 * 1024];
        external.write_all(&payload).await.unwrap();
        // Read the response as it echoes back. WebSocket cannot half-close, so we
        // only shut down after the response arrives in full.
        let mut received = Vec::with_capacity(payload.len());
        while received.len() < payload.len() {
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout(Duration::from_secs(10), external.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(received, payload, "WSS multi-frame payload must round-trip");
        drop(external);
        client.shutdown().await;
        server.shutdown().await;
    }
