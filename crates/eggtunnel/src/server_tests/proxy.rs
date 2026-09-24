use super::*;

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_http_connect_keeps_eggtunnel_tls_end_to_end() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"proxy-profile-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();

        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    assert!(request.starts_with(b"CONNECT "));
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
                        .await
                        .unwrap();
                });
            }
        });

        let client = Client::start_with_outbound_proxy_and_connector(
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
            &format!("http://{proxy_addr}"),
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
            roundtrip(addr, b"tls-through-connect").await,
            b"tls-through-connect"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_socks5_keeps_eggtunnel_tls_end_to_end() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"socks-proxy-profile-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    downstream.read_exact(&mut greeting).await.unwrap();
                    let mut methods = vec![0; greeting[1] as usize];
                    downstream.read_exact(&mut methods).await.unwrap();
                    downstream.write_all(&[5, 0]).await.unwrap();
                    let mut request = [0u8; 4];
                    downstream.read_exact(&mut request).await.unwrap();
                    assert_eq!(request[0], 5);
                    assert_eq!(request[1], 1);
                    match request[3] {
                        1 => {
                            let mut tail = [0u8; 6];
                            downstream.read_exact(&mut tail).await.unwrap();
                        }
                        3 => {
                            let length = downstream.read_u8().await.unwrap() as usize;
                            let mut tail = vec![0; length + 2];
                            downstream.read_exact(&mut tail).await.unwrap();
                        }
                        atyp => panic!("unexpected SOCKS address type: {atyp}"),
                    }
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
                        .await
                        .unwrap();
                });
            }
        });
        let client = Client::start_with_outbound_proxy_and_connector(
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
            &format!("socks5://{proxy_addr}"),
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
            roundtrip(addr, b"tls-through-socks5").await,
            b"tls-through-socks5"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_proxy_refused_endpoint_terminates_without_secret_in_diagnostic() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"refused-proxy-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        // Bind a listener only to immediately drop it, leaving a deterministic
        // unbound loopback address with credentials embedded in the URI.
        let unbound = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let refused_addr = unbound.local_addr().unwrap();
        drop(unbound);
        let secret_marker = "REDACTED-PROXY-CREDENTIAL";
        let uri = format!("http://user:{secret_marker}@{refused_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token: SecretToken::new(b"refused-proxy-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snap = client.handle().snapshot();
                if snap.reconnects > 0 || snap.last_termination.is_some() {
                    break snap;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            snapshot.rejected_connections > 0
                || matches!(
                    snapshot.last_termination,
                    Some(TerminationCategory::Transport)
                        | Some(TerminationCategory::Authorization)
                        | Some(TerminationCategory::Authentication)
                        | Some(TerminationCategory::Timeout)
                ),
            "refused proxy must produce a bounded termination, got {snapshot:?}"
        );
        client.shutdown().await;
        server.shutdown().await;
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_proxy_handshake_timeout_tears_down_bounded() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"timeout-proxy-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        // Proxy fixture accepts the TCP connection but never sends the HTTP CONNECT
        // 200 response, so the client-side handshake must time out.
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    // Hold the TCP connection open without sending a CONNECT
                    // response; the client side eventually hits its connect timeout.
                    let mut buf = [0u8; 1024];
                    loop {
                        match downstream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(_) => continue,
                        }
                    }
                });
            }
        });
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token: SecretToken::new(b"timeout-proxy-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &format!("http://{proxy_addr}"),
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        // Wait for the client to record a timeout termination.
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if client.handle().snapshot().last_termination == Some(TerminationCategory::Timeout)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_proxy_cancellation_terminates_in_progress_handshake() {
        let (cert, key) = certificate();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: SecretToken::new(b"cancel-proxy-secret".to_vec()).unwrap(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    // Drain bytes without ever responding so cancellation has an
                    // in-progress handshake to interrupt.
                    let mut buf = [0u8; 1024];
                    while downstream.read(&mut buf).await.is_ok_and(|n| n > 0) {}
                });
            }
        });
        let client = Client::start_with_outbound_proxy_and_connector(
            ClientConfig {
                server_addr: server.local_addr().to_string(),
                tls_server_name: "localhost".into(),
                ca_pem: Some(cert.into_bytes()),
                token: SecretToken::new(b"cancel-proxy-secret".to_vec()).unwrap(),
                services: vec![ClientService::new(
                    ServiceId(1),
                    ServiceName::new("direct-echo").unwrap(),
                    RequestedBind::Loopback { port: 0 },
                    TcpTarget::new("127.0.0.1", 9).unwrap(),
                )],
            },
            &format!("http://{proxy_addr}"),
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        // Cancel the client promptly. The in-progress proxy handshake task must
        // observe the cancellation and tear down without leaking resources.
        tokio::time::sleep(Duration::from_millis(50)).await;
        client.shutdown().await;
        // Verify the server side never observed an authenticated session: the
        // proxy canceled before any Eggtunnel registration completed.
        assert_eq!(server.handle().snapshot().active_sessions, 0);
        assert_eq!(server.handle().snapshot().registered_services, 0);
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_http_connect_auth_success_routes_through_proxy() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"http-auth-ok-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    assert!(request.starts_with(b"CONNECT "));
                    // Require Basic auth; reject otherwise.
                    let header = std::str::from_utf8(&request).unwrap_or_default();
                    let auth_line = header
                        .split("\r\n")
                        .find(|l| l.to_ascii_lowercase().starts_with("proxy-authorization:"))
                        .unwrap_or("");
                    let value = auth_line
                        .trim_start_matches("Proxy-Authorization:")
                        .trim_start_matches("proxy-authorization:")
                        .trim();
                    if value != "Basic YWxpY2U6czNjcmV0" {
                        downstream
                            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                            .await
                            .unwrap();
                        return;
                    }
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
                        .await
                        .unwrap();
                });
            }
        });
        let uri = format!("http://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
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
            &uri,
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
            roundtrip(addr, b"tls-through-connect-auth").await,
            b"tls-through-connect-auth"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_http_connect_auth_failure_rejects_without_secret_leak() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"http-auth-fail-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    downstream
                        .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                        .await
                        .unwrap();
                });
            }
        });
        let uri = format!("http://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
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
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        // The client should record an authorization failure and never register a
        // service. We poll for either a reconnection or a bounded termination; the
        // URI/credential must not appear in any client-side diagnostic.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client.handle().snapshot().reconnects > 0
                    || client.handle().snapshot().last_termination.is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 0);
        let snapshot_text = format!("{:?}", client.handle().snapshot());
        assert!(
            !snapshot_text.contains("s3cret"),
            "client snapshot must not contain proxy credentials: {snapshot_text}"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_socks5_auth_success_routes_through_proxy() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"socks5-auth-ok-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let tunnel_addr = server.local_addr();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    let _ = downstream.read_exact(&mut greeting).await;
                    let mut methods = vec![0; greeting[1] as usize];
                    let _ = downstream.read_exact(&mut methods).await;
                    // Accept username/password auth (0x02) only.
                    assert!(
                        methods.contains(&0x02),
                        "expected SOCKS5 username/password auth method, got {methods:?}"
                    );
                    downstream.write_all(&[5, 0x02]).await.unwrap();
                    // Username/password sub-negotiation: version, username length, username,
                    // password length, password.
                    let version = downstream.read_u8().await.unwrap();
                    assert_eq!(version, 1);
                    let user_len = downstream.read_u8().await.unwrap() as usize;
                    let mut user = vec![0; user_len];
                    downstream.read_exact(&mut user).await.unwrap();
                    let pass_len = downstream.read_u8().await.unwrap() as usize;
                    let mut pass = vec![0; pass_len];
                    downstream.read_exact(&mut pass).await.unwrap();
                    if user != b"alice" || pass != b"s3cret" {
                        // 0x01 = version, 0x01 = failure
                        downstream.write_all(&[1, 1]).await.unwrap();
                        return;
                    }
                    // 0x01 = version, 0x00 = success. Send it before reading the
                    // CONNECT request so the client unblocks.
                    downstream.write_all(&[1, 0]).await.unwrap();
                    let mut request = [0u8; 4];
                    let _ = downstream.read_exact(&mut request).await;
                    assert_eq!(request[0], 5);
                    assert_eq!(request[1], 1);
                    match request[3] {
                        1 => {
                            let mut tail = [0u8; 6];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        3 => {
                            let length = downstream.read_u8().await.unwrap() as usize;
                            let mut tail = vec![0; length + 2];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        atyp => panic!("unexpected SOCKS address type: {atyp}"),
                    }
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });
        let uri = format!("socks5://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
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
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snap = client.handle().snapshot();
                if snap.registered_services == 1
                    || snap.reconnects > 0
                    || snap.last_termination.is_some()
                {
                    break snap;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            snapshot.registered_services, 1,
            "service registration must succeed through SOCKS5 auth"
        );
        let bind = snapshot.effective_binds.first().cloned().unwrap().2;
        let addr = SocketAddr::V6(std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::from(bind.address),
            bind.port,
            0,
            0,
        ));
        assert_eq!(
            roundtrip(addr, b"tls-through-socks5-auth").await,
            b"tls-through-socks5-auth"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_socks5_auth_failure_rejects_without_secret_leak() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"socks5-auth-fail-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = proxy_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    downstream.read_exact(&mut greeting).await.unwrap();
                    let mut methods = vec![0; greeting[1] as usize];
                    downstream.read_exact(&mut methods).await.unwrap();
                    downstream.write_all(&[5, 0x02]).await.unwrap();
                    let _ = downstream.read_u8().await.unwrap();
                    let user_len = downstream.read_u8().await.unwrap() as usize;
                    let mut _user = vec![0; user_len];
                    downstream.read_exact(&mut _user).await.unwrap();
                    let pass_len = downstream.read_u8().await.unwrap() as usize;
                    let mut _pass = vec![0; pass_len];
                    downstream.read_exact(&mut _pass).await.unwrap();
                    // Reject auth: version 1, status 0x01 (failure).
                    downstream.write_all(&[1, 1]).await.unwrap();
                });
            }
        });
        let uri = format!("socks5://alice:s3cret@{proxy_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
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
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if client.handle().snapshot().reconnects > 0
                    || client.handle().snapshot().last_termination.is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(server.handle().snapshot().registered_services, 0);
        let snapshot_text = format!("{:?}", client.handle().snapshot());
        assert!(
            !snapshot_text.contains("s3cret"),
            "client snapshot must not contain proxy credentials: {snapshot_text}"
        );
        client.shutdown().await;
        server.shutdown().await;
        proxy_task.abort();
    }

    #[cfg(feature = "outbound-proxy")]
    #[tokio::test]
    async fn outbound_two_hop_socks5_then_http_connect_routes_end_to_end() {
        let (cert, key) = certificate();
        let token = SecretToken::new(b"two-hop-secret".to_vec()).unwrap();
        let server = Server::bind(ServerConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: cert.as_bytes().to_vec(),
            private_key_pem: key.as_bytes().to_vec(),
            token: token.clone(),
            allow_public_service_binds: false,
        })
        .await
        .unwrap();
        let tunnel_addr = server.local_addr();
        // Second hop: HTTP CONNECT proxy listening on its own port.
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let http_proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = http_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let byte = downstream.read_u8().await.unwrap();
                        request.push(byte);
                        assert!(request.len() <= 4096, "CONNECT request too large");
                    }
                    assert!(request.starts_with(b"CONNECT "));
                    let mut upstream = TcpStream::connect(tunnel_addr).await.unwrap();
                    downstream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });
        // First hop: SOCKS5 proxy that performs a SOCKS5 CONNECT to the second hop
        // (HTTP CONNECT proxy) and then bridges bytes. The chain executor
        // delivers the SOCKS5 CONNECT bytes for the second hop's endpoint.
        let socks5_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks5_addr = socks5_listener.local_addr().unwrap();
        let http_target_addr = http_addr;
        let socks5_proxy_task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = socks5_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 2];
                    let _ = downstream.read_exact(&mut greeting).await;
                    let mut methods = vec![0; greeting[1] as usize];
                    let _ = downstream.read_exact(&mut methods).await;
                    // No-auth greeting only.
                    assert!(methods.contains(&0x00), "expected SOCKS5 no-auth method");
                    downstream.write_all(&[5, 0x00]).await.unwrap();
                    // Read SOCKS5 CONNECT request.
                    let mut request = [0u8; 4];
                    let _ = downstream.read_exact(&mut request).await;
                    assert_eq!(request[0], 5);
                    assert_eq!(request[1], 1);
                    // Consume the address payload (we do not need the host/port here
                    // because the SOCKS5 proxy simply opens a TCP connection to the
                    // configured second-hop endpoint).
                    match request[3] {
                        1 => {
                            let mut tail = [0u8; 6];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        3 => {
                            let length = downstream.read_u8().await.unwrap() as usize;
                            let mut tail = vec![0; length + 2];
                            let _ = downstream.read_exact(&mut tail).await;
                        }
                        atyp => panic!("unexpected SOCKS address type: {atyp}"),
                    }
                    let mut upstream = TcpStream::connect(http_target_addr).await.unwrap();
                    // Reply to the client with SOCKS5 success; the HTTP CONNECT
                    // bytes will arrive over the bridged stream.
                    downstream
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });
        let uri = format!("socks5://{socks5_addr}__http://{http_addr}");
        let client = Client::start_with_outbound_proxy_and_connector(
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
            &uri,
            Arc::new(DuplexEchoConnector),
        )
        .await
        .unwrap();
        let bind = tokio::time::timeout(Duration::from_secs(10), async {
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
            roundtrip(addr, b"tls-through-two-hop-chain").await,
            b"tls-through-two-hop-chain",
            "two-hop SOCKS5+HTTP CONNECT chain must deliver TLS end-to-end"
        );
        client.shutdown().await;
        server.shutdown().await;
        socks5_proxy_task.abort();
        http_proxy_task.abort();
    }
