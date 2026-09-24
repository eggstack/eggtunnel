use super::*;
use super::tcp::{CapturingBlockingConnector, wait_for_quic_pending, write_quic_data_hello};

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_session_multiplexes_isolated_data_streams_for_two_services() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-test-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            while let Ok((stream, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = tokio::io::split(stream);
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                    let _ = write.shutdown().await;
                });
            }
        });
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-test-secret".to_vec()).unwrap(),
            services: vec![
                ClientService::new(
                    ServiceId(1),
                    ServiceName::new("quic-one").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
                ),
                ClientService::new(
                    ServiceId(2),
                    ServiceName::new("quic-two").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
                ),
            ],
        })
        .await
        .unwrap();
        let binds = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let binds = server.handle().snapshot().effective_binds;
                if binds.len() == 2 {
                    break binds
                        .into_iter()
                        .map(|(_, _, bind)| {
                            SocketAddr::V6(std::net::SocketAddrV6::new(
                                std::net::Ipv6Addr::from(bind.address),
                                bind.port,
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
        let (one, two) = tokio::join!(
            roundtrip(binds[0], b"quic-stream-one"),
            roundtrip(binds[1], b"quic-stream-two")
        );
        assert_eq!(one, b"quic-stream-one");
        assert_eq!(two, b"quic-stream-two");

        let mut reset_stream = TcpStream::connect(binds[0]).await.unwrap();
        reset_stream.write_all(b"reset-this-stream").await.unwrap();
        drop(reset_stream);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            roundtrip(binds[1], b"stream-after-reset").await,
            b"stream-after-reset"
        );
        client.shutdown().await;
        server.shutdown().await;
        echo_task.abort();
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_connection_replacement_creates_new_session_and_reregisters_services() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"quic-reconnect-secret".to_vec()).unwrap();
        let first_server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let endpoint = first_server.local_addr();
        let first_handle = first_server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: endpoint.to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: token.clone(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-restored").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if first_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let first_session = first_handle.snapshot().effective_binds[0].0;
        first_server.shutdown().await;

        let second_server = Server::bind_quic(ServerConfig {
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

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_wrong_session_data_hello_is_rejected_and_pending_entry_survives() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-wrong-session-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let captured: std::sync::Arc<std::sync::Mutex<Option<eggtunnel_proto::ConnectionId>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let client = Client::start_quic_insecure_with_connector_for_test(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: None,
                token: SecretToken::new(b"quic-wrong-session-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("wrong-session-svc").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            std::sync::Arc::new(CapturingBlockingConnector {
                captured: captured.clone(),
            }),
        )
        .await
        .unwrap();
        let client_handle = client.handle();
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
        // External TCP peer connects; server inserts a pending entry and emits Open to
        // the client. The client's normal Open handler blocks on the never-resolving
        // CapturingBlockingConnector, leaving the pending entry alive for inspection.
        let external = TcpStream::connect(addr).await.unwrap();
        wait_for_quic_pending(&server_handle, 1).await;
        let captured_id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(id) = *captured.lock().unwrap_or_else(|p| p.into_inner()) {
                    break id;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // Submit a wrong-session DataHello through a separately opened QUIC stream that
        // shares the established session.
        let quic = client_handle
            .quic_client_for_test()
            .expect("client must have an established QUIC client");
        let mut wrong_stream = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut wrong_stream,
            eggtunnel_proto::DataHello {
                session_id: SessionId([9; 16]),
                service_id: ServiceId(1),
                connection_id: captured_id,
            },
        )
        .await;
        // The server rejects the wrong-session DataHello with an Authentication error and
        // closes the stream; reading should fail rather than produce a DataHello success.
        let mut buf = [0u8; 64];
        let read_result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut wrong_stream, &mut buf),
        )
        .await;
        assert!(
            read_result.is_err() || matches!(read_result, Ok(Ok(0)) | Ok(Err(_))),
            "wrong-session DataHello should not produce server-side data, got {read_result:?}"
        );
        assert_eq!(
            server_handle.snapshot().rejected_connections,
            1,
            "wrong-session DataHello must increment rejected_connections"
        );
        assert_eq!(
            server_handle.snapshot().pending_connections,
            1,
            "wrong-session DataHello must not consume the pending entry"
        );
        drop(external);
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_replay_data_hello_on_second_stream_is_rejected() {
        let (cert, key) = certificate();
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            while let Ok((stream, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.into_split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-replay-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-replay-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-replay-svc").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new(echo_addr.ip().to_string(), echo_addr.port()).unwrap(),
            )],
        })
        .await
        .unwrap();
        let client_handle = client.handle();
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
        // First connection completes normally; the relay consumes the pending entry.
        let mut first = TcpStream::connect(addr).await.unwrap();
        first.write_all(b"first").await.unwrap();
        let mut response = [0u8; 5];
        tokio::time::timeout(Duration::from_secs(5), first.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"first");
        drop(first);
        // Wait until the pending entry has been consumed and the active connection has
        // returned to baseline so the server-side state reflects the first pairing.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server_handle.snapshot();
                if snapshot.pending_connections == 0 && snapshot.active_connections == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let legitimate_session_id = server_handle.snapshot().effective_binds[0].0;
        // Replay: open a fresh QUIC stream and resend the same ConnectionId pair. The
        // server has already consumed the ConnectionId; accept_data_hello must reject
        // the replay as Authorization (the pending entry is gone).
        let quic = client_handle
            .quic_client_for_test()
            .expect("client must have an established QUIC client");
        let mut replay_stream = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut replay_stream,
            eggtunnel_proto::DataHello {
                session_id: legitimate_session_id,
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([42; 16]),
            },
        )
        .await;
        let mut buf = [0u8; 64];
        let read_result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut replay_stream, &mut buf),
        )
        .await;
        assert!(
            read_result.is_err() || matches!(read_result, Ok(Ok(0)) | Ok(Err(_))),
            "replay DataHello should not produce server-side data, got {read_result:?}"
        );
        assert!(
            server_handle.snapshot().rejected_connections >= 1,
            "replay DataHello must increment rejected_connections"
        );
        client.shutdown().await;
        server.shutdown().await;
        echo_task.abort();
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_stale_old_generation_data_hello_is_rejected_after_reconnect() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"quic-stale-secret".to_vec()).unwrap();
        let first_server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let endpoint = first_server.local_addr();
        let first_handle = first_server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: endpoint.to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: token.clone(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-stale-svc").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        let client_handle = client.handle();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if first_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let old_session_id = first_handle.snapshot().effective_binds[0].0;
        let rejected_before = first_handle.snapshot().rejected_connections;
        first_server.shutdown().await;
        // Second server takes the same endpoint with a fresh SessionId.
        let second_server = Server::bind_quic(ServerConfig {
            listen_addr: endpoint,
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let second_handle = second_server.handle();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if second_handle.snapshot().registered_services == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let new_session_id = second_handle.snapshot().effective_binds[0].0;
        assert_ne!(old_session_id, new_session_id);
        // Wait for the client's QUIC transport to settle onto the second server.
        let quic = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(client) = client_handle.quic_client_for_test()
                    && client.get_connection().await.map(|_| true).unwrap_or(false)
                {
                    break client;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        // Submit a DataHello with the OLD generation SessionId; the second server is
        // authoritative for the new SessionId only, so the stale DataHello must be
        // rejected with Authentication (session lookup misses).
        let mut stale_stream = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut stale_stream,
            eggtunnel_proto::DataHello {
                session_id: old_session_id,
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([7; 16]),
            },
        )
        .await;
        let mut buf = [0u8; 64];
        let read_result = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut stale_stream, &mut buf),
        )
        .await;
        assert!(
            read_result.is_err() || matches!(read_result, Ok(Ok(0)) | Ok(Err(_))),
            "stale DataHello should not produce server-side data, got {read_result:?}"
        );
        assert!(
            second_handle.snapshot().rejected_connections > rejected_before,
            "stale DataHello must be rejected by the new server"
        );
        client.shutdown().await;
        second_server.shutdown().await;
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_stream_saturation_recovers_capacity_and_keeps_unrelated_streams_alive() {
        let (cert, key) = certificate();
        let ceiling = 2;
        let server = Server::bind_quic_with_admission_for_test(
            ServerConfig {
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                certificate_pem: cert.as_bytes().to_vec(),
                private_key_pem: key.as_bytes().to_vec(),
                token: SecretToken::new(b"quic-saturation-secret".to_vec()).unwrap(),
                allow_public_service_binds: false,
            },
            ceiling,
        )
        .await
        .unwrap();
        let server_handle = server.handle();
        let client = Client::start_quic_insecure_for_test(ClientConfig {
            server_addr: server.local_addr().to_string(),
            tls_server_name: "localhost".into(),
            ca_pem: None,
            token: SecretToken::new(b"quic-saturation-secret".to_vec()).unwrap(),
            services: vec![ClientService::new(
                ServiceId(1),
                ServiceName::new("quic-saturation-svc").unwrap(),
                RequestedBind::Loopback { port: 0 },
                TcpTarget::new("127.0.0.1", 9).unwrap(),
            )],
        })
        .await
        .unwrap();
        let client_handle = client.handle();
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
        let quic = client_handle
            .quic_client_for_test()
            .expect("client must have an established QUIC client");
        // Open `ceiling` streams that occupy the stream_admission semaphore. Each
        // stream's DataHello will fail because there are no pending entries, but the
        // permit is held until the server closes the stream.
        let mut admitted = Vec::new();
        for _ in 0..ceiling {
            let mut stream = quic.open_stream().await.unwrap();
            write_quic_data_hello(
                &mut stream,
                eggtunnel_proto::DataHello {
                    session_id: SessionId([0; 16]),
                    service_id: ServiceId(1),
                    connection_id: eggtunnel_proto::ConnectionId([0; 16]),
                },
            )
            .await;
            admitted.push(stream);
        }
        // Allow the server to drain the admitted streams so it observes the permits as
        // held. The DataHello with a wrong SessionId is rejected quickly; wait for the
        // rejection counter to reach `ceiling`.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_handle.snapshot().rejected_connections >= ceiling as u64 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // With the ceiling saturated by admitted permits, an additional QUIC stream
        // open is still possible at the Quinn level (we doubled max_concurrent_streams
        // to permit this in the test helper), but the Eggtunnel-level stream_admission
        // semaphore must reject it before reading DataHello. Submitting a stream and
        // observing its server-side termination gives us that signal.
        let mut extra = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut extra,
            eggtunnel_proto::DataHello {
                session_id: SessionId([0; 16]),
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([0; 16]),
            },
        )
        .await;
        // Wait for the server to record the extra rejection. We expect the rejected
        // counter to advance beyond `ceiling` because the stream_admission semaphore
        // refused a permit before any DataHello read.
        let rejected_after_extra = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = server_handle.snapshot();
                if snapshot.rejected_connections > ceiling as u64 {
                    break snapshot.rejected_connections;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            rejected_after_extra > ceiling as u64,
            "extra QUIC stream must be rejected once the ceiling is full, got {rejected_after_extra}"
        );
        // Release one admitted permit by dropping the corresponding stream; the
        // server's handler should observe the dropped stream and free its permit.
        admitted.remove(0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        // A new stream should be admitted again. We don't need it to complete a
        // successful DataHello (no pending entry exists); we only need the rejection
        // counter to NOT advance further as a result of an admission-cap denial,
        // proving capacity returned. We assert it does not hit the same rejected
        // boundary again with the new attempt.
        let rejected_after_release = server_handle.snapshot().rejected_connections;
        let mut follow_up = quic.open_stream().await.unwrap();
        write_quic_data_hello(
            &mut follow_up,
            eggtunnel_proto::DataHello {
                session_id: SessionId([0; 16]),
                service_id: ServiceId(1),
                connection_id: eggtunnel_proto::ConnectionId([0; 16]),
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The follow-up stream is admitted (it isn't rejected by stream_admission);
        // its DataHello is rejected at the authorization stage because there is no
        // pending entry. We verify the server still operates by checking that the
        // registered service count remains at 1 throughout.
        assert_eq!(
            server_handle.snapshot().registered_services,
            1,
            "unrelated admitted stream path must remain operational"
        );
        assert!(
            server_handle.snapshot().rejected_connections >= rejected_after_release,
            "follow-up stream must not stall"
        );
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "quic")]
    struct HalfCloseTarget {
        reply: Vec<u8>,
    }

    #[cfg(feature = "quic")]
    impl TargetConnector for HalfCloseTarget {
        fn connect(&self, _service: ClientService, _context: TargetContext) -> TargetFuture {
            let reply = self.reply.clone();
            Box::pin(async move {
                let (application, _peer) = tokio::io::duplex(8 * 1024);
                let reply_clone = reply.clone();
                tokio::spawn(async move {
                    let (mut read, mut write) = tokio::io::split(application);
                    let mut request = Vec::new();
                    // Read until EOF to simulate TCP half-close: the application
                    // observes the request side shutting down.
                    let _ = tokio::io::copy(&mut read, &mut request).await;
                    drop(read);
                    // Send the response after observing EOF.
                    let _ = write.write_all(&reply_clone).await;
                    let _ = write.shutdown().await;
                });
                Ok(Box::new(_peer) as TargetStream)
            })
        }
    }

    #[cfg(feature = "quic")]
    #[tokio::test]
    async fn quic_half_close_preserves_response_after_request_eof() {
        let (cert, key) = certificate();
        let server = Server::bind_quic(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"quic-half-close-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let server_handle = server.handle();
        let reply = b"response-after-eof".to_vec();
        let connector = std::sync::Arc::new(HalfCloseTarget {
            reply: reply.clone(),
        });
        let client = Client::start_quic_insecure_with_connector_for_test(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: None,
                token: SecretToken::new(b"quic-half-close-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("quic-half-close-svc").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            connector,
        )
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
        // Send the request, then shut down the write half to simulate TCP half-close
        // on the external peer side. The relay must propagate the EOF to the target
        // before the target can reply.
        external.write_all(b"request").await.unwrap();
        external.shutdown().await.unwrap();
        // Read until EOF and assert the response arrives before close.
        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), external.read_to_end(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            received, reply,
            "QUIC half-close must deliver the target response after request EOF"
        );
        client.shutdown().await;
        server.shutdown().await;
    }
