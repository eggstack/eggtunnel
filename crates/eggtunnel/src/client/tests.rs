use super::*;
use eggtunnel_proto::{RequestedBind, ServiceName, TcpTarget};

fn config(server_addr: &str) -> ClientConfig {
    ClientConfig {
        server_addr: server_addr.to_owned(),
        tls_server_name: "localhost".to_owned(),
        ca_pem: None,
        token: SecretToken::new(b"test-token".to_vec()).unwrap(),
        services: vec![ClientService::new(
            ServiceId(1),
            ServiceName::new("one").unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", 80).unwrap(),
        )],
    }
}

#[test]
fn client_validation_rejects_invalid_endpoint_without_server_feature() {
    assert!(validate_config(&config("missing-port"), &Default::default()).is_err());
    assert!(validate_config(&config("localhost:0"), &Default::default()).is_err());
    assert!(validate_config(&config("localhost:443"), &Default::default()).is_ok());
}

#[test]
fn client_validation_rejects_duplicate_service_identity() {
    let mut config = config("localhost:443");
    config.services.push(config.services[0].clone());
    assert!(validate_config(&config, &Default::default()).is_err());
}

#[test]
fn client_config_debug_redacts_bearer_token_for_tracing_callers() {
    let marker = "client-token-must-not-appear";
    let mut config = config("localhost:443");
    config.token = SecretToken::new(marker.as_bytes().to_vec()).unwrap();
    let formatted = format!("{config:?}");
    assert!(!formatted.contains(marker));
    assert!(formatted.contains("REDACTED"));
}

fn service(id: u64, name: &str, target_port: u16) -> ClientService {
    ClientService::new(
        ServiceId(id),
        ServiceName::new(name).unwrap(),
        RequestedBind::Loopback { port: 0 },
        TcpTarget::new("127.0.0.1", target_port).unwrap(),
    )
}

async fn fake_connected_client() -> (
    ClientHandle,
    Counters,
    JoinHandle<Result<(), TunnelError>>,
    BoxStream,
) {
    fake_connected_client_with_policy(crate::RuntimePolicy::default()).await
}

async fn fake_connected_client_with_policy(
    policy: crate::RuntimePolicy,
) -> (
    ClientHandle,
    Counters,
    JoinHandle<Result<(), TunnelError>>,
    BoxStream,
) {
    let token = SecretToken::new(b"dynamic-registration-test".to_vec()).unwrap();
    let counters = Counters::with_policy(policy);
    let cancel = CancellationToken::new();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (commands, command_rx) = mpsc::channel(32);
    let handle = ClientHandle {
        cancel: cancel.clone(),
        counters: counters.clone(),
        commands,
        #[cfg(feature = "quic-client")]
        quic_client: Arc::new(std::sync::Mutex::new(None)),
    };
    let task_counters = counters.clone();
    let task_cancel = cancel.clone();
    let task_token = token.clone();
    let task = tokio::spawn(async move {
        let mut supervisor =
            ReconnectSupervisor::new(vec![service(1, "initial", 80)], Duration::from_millis(500));
        let mut command_rx = command_rx;
        run_session(
            Box::new(client_io),
            SessionRun {
                token: &task_token,
                transport: ClientDataTransport::TcpTls {
                    endpoint: crate::Endpoint::parse("localhost:443").unwrap(),
                    server_name: "localhost".into(),
                    tls: build_tls_config(None).unwrap(),
                    websocket: false,
                    #[cfg(feature = "outbound-proxy")]
                    outbound: None,
                },
                connector: Arc::new(super::config::TcpTargetConnector),
                cancel: &task_cancel,
                counters: &task_counters,
                reconnect_delay: &mut supervisor.reconnect_delay,
                service_state: &mut supervisor.services,
                commands: &mut command_rx,
            },
        )
        .await
    });
    let mut peer: BoxStream = Box::new(server_io);
    assert!(matches!(
        read_boxed(&mut peer).await.unwrap(),
        Message::ClientHello(_)
    ));
    write_boxed(
        &mut peer,
        &Message::ServerHello(ServerHello {
            version: ProtocolVersion::CURRENT,
            capabilities: Capabilities::default(),
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_boxed(&mut peer).await.unwrap(),
        Message::Auth(_)
    ));
    write_boxed(
        &mut peer,
        &Message::AuthOk(AuthOk {
            session_id: eggtunnel_proto::SessionId::generate().unwrap(),
        }),
    )
    .await
    .unwrap();
    let Message::RegisterService(initial) = read_boxed(&mut peer).await.unwrap() else {
        panic!("expected initial registration")
    };
    write_boxed(
        &mut peer,
        &Message::RegisterAck(eggtunnel_proto::RegisterAck {
            service_id: initial.service_id,
            effective_bind: EffectiveBind {
                address: std::net::Ipv6Addr::LOCALHOST.octets(),
                port: 31001,
            },
        }),
    )
    .await
    .unwrap();
    (handle, counters, task, peer)
}

/// Session driven by a scripted peer speaking the given (minor,
/// capabilities) as the server. Completes the initial handshake and
/// returns the running session plus the peer stream.
async fn fake_session_with_hello(
    minor: u16,
    caps: Capabilities,
    policy: crate::RuntimePolicy,
) -> (
    ClientHandle,
    Counters,
    JoinHandle<Result<(), TunnelError>>,
    BoxStream,
) {
    let token = SecretToken::new(b"capability-negotiation-test".to_vec()).unwrap();
    let counters = Counters::with_policy(policy);
    let cancel = CancellationToken::new();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (commands, command_rx) = mpsc::channel(32);
    let handle = ClientHandle {
        cancel: cancel.clone(),
        counters: counters.clone(),
        commands,
        #[cfg(feature = "quic-client")]
        quic_client: Arc::new(std::sync::Mutex::new(None)),
    };
    let task_counters = counters.clone();
    let task_cancel = cancel.clone();
    let task_token = token.clone();
    let task = tokio::spawn(async move {
        let mut supervisor =
            ReconnectSupervisor::new(vec![service(1, "initial", 80)], Duration::from_millis(500));
        let mut command_rx = command_rx;
        run_session(
            Box::new(client_io),
            SessionRun {
                token: &task_token,
                transport: ClientDataTransport::TcpTls {
                    endpoint: crate::Endpoint::parse("localhost:443").unwrap(),
                    server_name: "localhost".into(),
                    tls: build_tls_config(None).unwrap(),
                    websocket: false,
                    #[cfg(feature = "outbound-proxy")]
                    outbound: None,
                },
                connector: Arc::new(super::config::TcpTargetConnector),
                cancel: &task_cancel,
                counters: &task_counters,
                reconnect_delay: &mut supervisor.reconnect_delay,
                service_state: &mut supervisor.services,
                commands: &mut command_rx,
            },
        )
        .await
    });
    let mut peer: BoxStream = Box::new(server_io);
    // The client must always advertise the full supported set at 1.1.
    let Message::ClientHello(hello) = read_boxed(&mut peer).await.unwrap() else {
        panic!("expected ClientHello");
    };
    assert_eq!(hello.version.major, 1);
    assert_eq!(hello.version.minor, eggtunnel_proto::PROTOCOL_MINOR);
    assert_eq!(hello.capabilities, Capabilities::supported());
    write_boxed(
        &mut peer,
        &Message::ServerHello(ServerHello {
            version: ProtocolVersion { major: 1, minor },
            capabilities: caps,
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        read_boxed(&mut peer).await.unwrap(),
        Message::Auth(_)
    ));
    write_boxed(
        &mut peer,
        &Message::AuthOk(AuthOk {
            session_id: eggtunnel_proto::SessionId::generate().unwrap(),
        }),
    )
    .await
    .unwrap();
    let Message::RegisterService(initial) = read_boxed(&mut peer).await.unwrap() else {
        panic!("expected initial registration")
    };
    write_boxed(
        &mut peer,
        &Message::RegisterAck(eggtunnel_proto::RegisterAck {
            service_id: initial.service_id,
            effective_bind: EffectiveBind {
                address: std::net::Ipv6Addr::LOCALHOST.octets(),
                port: 31001,
            },
        }),
    )
    .await
    .unwrap();
    (handle, counters, task, peer)
}

async fn read_register(peer: &mut BoxStream) -> ServiceId {
    let Message::RegisterService(register) =
        tokio::time::timeout(Duration::from_secs(2), read_boxed(peer))
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected RegisterService");
    };
    register.service_id
}

async fn ack_service(peer: &mut BoxStream, id: ServiceId, port: u16) {
    write_boxed(
        peer,
        &Message::RegisterAck(eggtunnel_proto::RegisterAck {
            service_id: id,
            effective_bind: EffectiveBind {
                address: std::net::Ipv6Addr::LOCALHOST.octets(),
                port,
            },
        }),
    )
    .await
    .unwrap();
}

async fn reject_service(peer: &mut BoxStream, id: ServiceId, code: u16) {
    write_boxed(
        peer,
        &Message::RegisterReject(eggtunnel_proto::RegisterReject {
            service_id: id,
            code,
            diagnostic: eggtunnel_proto::BoundedDiagnostic::new("rejected").unwrap(),
        }),
    )
    .await
    .unwrap();
}

fn full_capabilities() -> Capabilities {
    Capabilities::new(vec![
        eggtunnel_proto::CAPABILITY_REGISTER_REJECT,
        eggtunnel_proto::CAPABILITY_DRAIN_DEADLINE,
    ])
    .unwrap()
}

#[tokio::test]
async fn a_ready_session_resets_the_shared_reconnect_backoff_independent_of_transport() {
    // Backoff ownership lives in the supervisor, not in a transport adapter,
    // so a Session that reaches the ready state resets the same delay for
    // TCP/TLS, WebSocket, and QUIC.
    for label in ["tcp_tls", "websocket_tls", "quic"] {
        let policy = crate::RuntimePolicy::default();
        let counters = Counters::with_policy(policy);
        let session_counters = counters.clone();
        let cancel = CancellationToken::new();
        let token = SecretToken::new(format!("backoff-reset-{label}").into_bytes()).unwrap();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (_command_tx, mut command_rx) = mpsc::channel(1);
        let mut supervisor =
            ReconnectSupervisor::new(vec![service(1, "initial", 80)], Duration::from_secs(9));
        let session = tokio::spawn(async move {
            run_session(
                Box::new(client_io),
                SessionRun {
                    token: &token,
                    transport: ClientDataTransport::TcpTls {
                        endpoint: crate::Endpoint::parse("localhost:443").unwrap(),
                        server_name: "localhost".into(),
                        tls: build_tls_config(None).unwrap(),
                        websocket: false,
                        #[cfg(feature = "outbound-proxy")]
                        outbound: None,
                    },
                    connector: Arc::new(super::config::TcpTargetConnector),
                    cancel: &cancel,
                    counters: &session_counters,
                    reconnect_delay: &mut supervisor.reconnect_delay,
                    service_state: &mut supervisor.services,
                    commands: &mut command_rx,
                },
            )
            .await
        });
        let mut peer: BoxStream = Box::new(server_io);
        assert!(matches!(
            read_boxed(&mut peer).await.unwrap(),
            Message::ClientHello(_)
        ));
        write_boxed(
            &mut peer,
            &Message::ServerHello(ServerHello {
                version: ProtocolVersion::CURRENT,
                capabilities: Capabilities::default(),
            }),
        )
        .await
        .unwrap();
        assert!(matches!(
            read_boxed(&mut peer).await.unwrap(),
            Message::Auth(_)
        ));
        write_boxed(
            &mut peer,
            &Message::AuthOk(AuthOk {
                session_id: eggtunnel_proto::SessionId::generate().unwrap(),
            }),
        )
        .await
        .unwrap();
        let Message::RegisterService(initial) = read_boxed(&mut peer).await.unwrap() else {
            panic!("{label}: expected initial registration")
        };
        write_boxed(
            &mut peer,
            &Message::RegisterAck(eggtunnel_proto::RegisterAck {
                service_id: initial.service_id,
                effective_bind: EffectiveBind {
                    address: std::net::Ipv6Addr::LOCALHOST.octets(),
                    port: 31000,
                },
            }),
        )
        .await
        .unwrap();
        // The Session is ready: the supervisor's delay is back to the policy
        // initial value, not the pre-seeded 9 s value.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = counters.snapshot();
            if snapshot.connected {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{label}: Session never became ready"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(peer);
        let _ = session.await;
    }
}

#[tokio::test]
async fn dynamic_registration_returns_disconnected_if_ack_is_lost() {
    let (handle, _, task, mut peer) = fake_connected_client().await;

    let register_handle = handle.clone();
    let register = tokio::spawn(async move {
        register_handle
            .register_service(service(2, "dynamic", 81))
            .await
    });
    let dynamic = tokio::time::timeout(Duration::from_secs(2), read_boxed(&mut peer))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(dynamic, Message::RegisterService(_)));
    drop(peer);
    assert!(matches!(
        register.await.unwrap(),
        Err(TunnelError::Disconnected)
    ));
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn cancelled_registration_is_unregistered_and_never_becomes_desired_state() {
    let (handle, counters, task, mut peer) = fake_connected_client().await;
    let register_handle = handle.clone();
    let register = tokio::spawn(async move {
        register_handle
            .register_service(service(2, "cancelled", 81))
            .await
    });
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), read_boxed(&mut peer))
            .await
            .unwrap()
            .unwrap(),
        Message::RegisterService(_)
    ));
    register.abort();
    write_boxed(
        &mut peer,
        &Message::RegisterAck(eggtunnel_proto::RegisterAck {
            service_id: ServiceId(2),
            effective_bind: EffectiveBind {
                address: std::net::Ipv6Addr::LOCALHOST.octets(),
                port: 31002,
            },
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), read_boxed(&mut peer))
            .await
            .unwrap()
            .unwrap(),
        Message::UnregisterService(eggtunnel_proto::UnregisterService {
            service_id: ServiceId(2)
        })
    ));
    assert_eq!(counters.snapshot().registered_services, 1);
    assert!(
        counters
            .snapshot()
            .effective_binds
            .iter()
            .all(|(_, service_id, _)| *service_id != ServiceId(2))
    );
    drop(peer);
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn command_from_an_older_session_generation_cannot_register() {
    let (handle, counters, task, mut peer) = fake_connected_client().await;
    let (reply, response) = oneshot::channel();
    let generation = counters
        .session_generation
        .load(std::sync::atomic::Ordering::Relaxed);
    handle
        .commands
        .send(ClientCommand::Register {
            service: service(2, "stale-generation", 82),
            generation: generation.saturating_sub(1),
            reply,
        })
        .await
        .unwrap();
    assert!(matches!(
        response.await.unwrap(),
        Err(TunnelError::Disconnected)
    ));
    assert_eq!(counters.snapshot().registered_services, 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), read_boxed(&mut peer))
            .await
            .is_err()
    );
    handle.shutdown();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn dynamic_registration_ack_timeout_is_typed_and_closes_session() {
    let mut policy = crate::RuntimePolicy::default();
    policy.timeouts.handshake = Duration::from_millis(100);
    let (handle, _, task, mut peer) = fake_connected_client_with_policy(policy).await;
    let register_handle = handle.clone();
    let register = tokio::spawn(async move {
        register_handle
            .register_service(service(2, "no-ack", 82))
            .await
    });
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), read_boxed(&mut peer))
            .await
            .unwrap()
            .unwrap(),
        Message::RegisterService(_)
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), register)
            .await
            .unwrap()
            .unwrap(),
        Err(TunnelError::Timeout)
    ));
    assert!(matches!(task.await.unwrap(), Err(TunnelError::Timeout)));
}

#[tokio::test]
async fn heartbeat_tracks_rtt_misses_and_recovery_with_one_probe() {
    let mut policy = crate::RuntimePolicy::default();
    policy.timeouts.control_idle = Duration::from_secs(2);
    policy.timeouts.heartbeat_interval = Duration::from_millis(50);
    let (handle, counters, task, mut peer) = fake_connected_client_with_policy(policy).await;
    let Message::Ping(first) = tokio::time::timeout(Duration::from_secs(1), read_boxed(&mut peer))
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected heartbeat Ping")
    };
    write_boxed(
        &mut peer,
        &Message::Pong(eggtunnel_proto::Pong { nonce: first.nonce }),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if counters.snapshot().heartbeat.latest_rtt_ms.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let Message::Ping(unanswered) = read_boxed(&mut peer).await.unwrap() else {
        panic!("expected next heartbeat Ping")
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if counters.snapshot().heartbeat.missed_heartbeats > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    write_boxed(
        &mut peer,
        &Message::Pong(eggtunnel_proto::Pong {
            nonce: unanswered.nonce,
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if counters.snapshot().heartbeat.missed_heartbeats == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(counters.snapshot().heartbeat.last_pong_age_ms.is_some());
    handle.shutdown();
    drop(peer);
    let _ = task.await;
}

#[test]
fn client_builder_applies_custom_service_ceiling() {
    let mut config = config("localhost:443");
    config.services.push(ClientService::new(
        ServiceId(2),
        ServiceName::new("two").unwrap(),
        RequestedBind::Loopback { port: 0 },
        TcpTarget::new("127.0.0.1", 81).unwrap(),
    ));
    let mut policy = crate::common::RuntimePolicy::default();
    policy.limits.services_per_session = 1;
    let builder = ClientBuilder::new(config).runtime_policy(policy);
    assert!(builder.validate().is_err());
}

#[test]
fn client_builder_accepts_tcp_tls_and_default_policy() {
    assert!(
        ClientBuilder::new(config("localhost:443"))
            .validate()
            .is_ok()
    );
    let mut dynamic_only = config("localhost:443");
    dynamic_only.services.clear();
    assert!(ClientBuilder::new(dynamic_only).validate().is_ok());
    let mut custom_ca = config("localhost:443");
    custom_ca.ca_pem = Some(b"custom CA".to_vec());
    assert!(ClientBuilder::new(custom_ca).validate().is_ok());
}

#[cfg(feature = "mtls")]
#[test]
fn client_builder_accepts_tcp_mtls() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()));
    assert!(builder.validate().is_ok());
}

#[cfg(feature = "quic-client")]
#[test]
fn client_profile_validator_rejects_quic_custom_ca() {
    let mut config = config("localhost:443");
    config.ca_pem = Some(b"custom CA".to_vec());
    let builder = ClientBuilder::new(config).transport(ClientTransportProfile::Quic);
    assert!(builder.validate().is_err());
}

#[cfg(feature = "quic-client")]
#[test]
fn client_profile_validator_accepts_quic_defaults() {
    assert!(
        ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::Quic)
            .validate()
            .is_ok()
    );
}

#[cfg(feature = "websocket-client")]
#[test]
fn client_profile_validator_accepts_websocket_defaults() {
    assert!(
        ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::WebSocket)
            .validate()
            .is_ok()
    );
}

#[cfg(all(feature = "quic-client", feature = "outbound-proxy"))]
#[test]
fn client_profile_validator_rejects_quic_proxy() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .transport(ClientTransportProfile::Quic)
        .outbound_proxy("socks5://localhost:1080");
    assert!(builder.validate().is_err());
}

#[cfg(all(feature = "quic-client", feature = "mtls"))]
#[test]
fn client_profile_validator_rejects_quic_mtls() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .transport(ClientTransportProfile::Quic)
        .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()));
    assert!(builder.validate().is_err());
}

#[cfg(all(feature = "websocket-client", feature = "outbound-proxy"))]
#[test]
fn client_profile_validator_accepts_websocket_proxy() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .transport(ClientTransportProfile::WebSocket)
        .outbound_proxy("socks5://localhost:1080");
    assert!(builder.validate().is_ok());
}

#[cfg(all(feature = "websocket-client", feature = "mtls"))]
#[test]
fn client_profile_validator_rejects_websocket_mtls() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .transport(ClientTransportProfile::WebSocket)
        .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()));
    assert!(builder.validate().is_err());
}

#[cfg(all(feature = "mtls", feature = "outbound-proxy"))]
#[test]
fn client_profile_validator_rejects_mtls_proxy() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()))
        .outbound_proxy("socks5://localhost:1080");
    assert!(builder.validate().is_err());
}

#[test]
fn negotiate_capabilities_returns_the_intersection_only() {
    use eggtunnel_proto::{CAPABILITY_DRAIN_DEADLINE, CAPABILITY_REGISTER_REJECT};
    let offered = Capabilities::supported();
    // Server-claimed extras outside our advertisement are ignored.
    let claimed = Capabilities::new(vec![CAPABILITY_REGISTER_REJECT, 99]).unwrap();
    let negotiated = negotiate_capabilities(&offered, &claimed);
    assert_eq!(negotiated.as_slice(), &[CAPABILITY_REGISTER_REJECT]);
    // Empty (1.0) advertisement negotiates nothing.
    assert!(
        negotiate_capabilities(&offered, &Capabilities::default())
            .as_slice()
            .is_empty()
    );
    // Drain-only peer negotiates drain alone.
    let drain_only = Capabilities::new(vec![CAPABILITY_DRAIN_DEADLINE]).unwrap();
    assert_eq!(
        negotiate_capabilities(&offered, &drain_only).as_slice(),
        &[CAPABILITY_DRAIN_DEADLINE]
    );
}

#[tokio::test]
async fn legacy_peer_keeps_exactly_one_registration_in_flight() {
    let (handle, _counters, task, mut peer) =
        fake_session_with_hello(0, Capabilities::default(), crate::RuntimePolicy::default()).await;
    let register = handle.clone();
    let first =
        tokio::spawn(async move { register.register_service(service(2, "dynamic", 81)).await });
    assert_eq!(read_register(&mut peer).await, ServiceId(2));
    // A second dynamic registration fails immediately: serial fallback.
    assert!(matches!(
        handle.register_service(service(3, "other", 82)).await,
        Err(TunnelError::ResourceExhausted)
    ));
    // No second frame was written for the rejected registration.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), read_boxed(&mut peer))
            .await
            .is_err()
    );
    ack_service(&mut peer, ServiceId(2), 31002).await;
    assert!(first.await.unwrap().is_ok());
    handle.shutdown();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn correlated_peers_correlate_out_of_order_ack_and_reject() {
    let (handle, counters, task, mut peer) =
        fake_session_with_hello(1, full_capabilities(), crate::RuntimePolicy::default()).await;
    let first_handle = handle.clone();
    let first = tokio::spawn(async move {
        first_handle
            .register_service(service(2, "dynamic", 81))
            .await
    });
    let second_handle = handle.clone();
    let second = tokio::spawn(async move {
        second_handle
            .register_service(service(3, "other", 82))
            .await
    });
    // Both registrations are in flight concurrently.
    assert_eq!(read_register(&mut peer).await, ServiceId(2));
    assert_eq!(read_register(&mut peer).await, ServiceId(3));
    // Reject arrives first (out of order) with the shared code vocabulary.
    reject_service(&mut peer, ServiceId(3), 5).await;
    assert!(matches!(
        second.await.unwrap(),
        Err(TunnelError::ResourceExhausted)
    ));
    ack_service(&mut peer, ServiceId(2), 31002).await;
    let bind = first.await.unwrap().unwrap();
    assert_eq!(bind.port, 31002);
    assert_eq!(counters.snapshot().registered_services, 2);
    handle.shutdown();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn duplicate_simultaneous_registrations_fail_before_any_frame() {
    let (handle, _counters, task, mut peer) =
        fake_session_with_hello(1, full_capabilities(), crate::RuntimePolicy::default()).await;
    let first_handle = handle.clone();
    let first = tokio::spawn(async move {
        first_handle
            .register_service(service(2, "dynamic", 81))
            .await
    });
    assert_eq!(read_register(&mut peer).await, ServiceId(2));
    // Same id and same name both fail locally without a second frame.
    assert!(matches!(
        handle.register_service(service(2, "clash", 82)).await,
        Err(TunnelError::ServiceAlreadyExists)
    ));
    assert!(matches!(
        handle.register_service(service(4, "dynamic", 82)).await,
        Err(TunnelError::ServiceAlreadyExists)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), read_boxed(&mut peer))
            .await
            .is_err()
    );
    ack_service(&mut peer, ServiceId(2), 31002).await;
    assert!(first.await.unwrap().is_ok());
    handle.shutdown();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn unnegotiated_register_reject_fails_the_session_closed() {
    let (handle, _counters, task, mut peer) =
        fake_session_with_hello(0, Capabilities::default(), crate::RuntimePolicy::default()).await;
    let register = handle.clone();
    let pending =
        tokio::spawn(async move { register.register_service(service(2, "dynamic", 81)).await });
    assert_eq!(read_register(&mut peer).await, ServiceId(2));
    // A 1.0 peer never sends ID 15: treat it as a protocol violation.
    reject_service(&mut peer, ServiceId(2), 1).await;
    assert!(task.await.unwrap().is_err());
    assert!(matches!(
        pending.await.unwrap(),
        Err(TunnelError::Disconnected)
    ));
}

#[tokio::test]
async fn partial_capability_reject_without_negotiation_is_a_violation() {
    use eggtunnel_proto::CAPABILITY_DRAIN_DEADLINE;
    let caps = Capabilities::new(vec![CAPABILITY_DRAIN_DEADLINE]).unwrap();
    let (handle, _counters, task, mut peer) =
        fake_session_with_hello(1, caps, crate::RuntimePolicy::default()).await;
    let register = handle.clone();
    let pending =
        tokio::spawn(async move { register.register_service(service(2, "dynamic", 81)).await });
    assert_eq!(read_register(&mut peer).await, ServiceId(2));
    // Capability 1 was not negotiated: the reject fails the Session.
    reject_service(&mut peer, ServiceId(2), 1).await;
    assert!(task.await.unwrap().is_err());
    assert!(matches!(
        pending.await.unwrap(),
        Err(TunnelError::Disconnected)
    ));
}

#[tokio::test]
async fn disconnect_with_pending_correlated_registrations_fails_all() {
    let (handle, _counters, task, mut peer) =
        fake_session_with_hello(1, full_capabilities(), crate::RuntimePolicy::default()).await;
    let first_handle = handle.clone();
    let first =
        tokio::spawn(async move { first_handle.register_service(service(2, "two", 81)).await });
    let second_handle = handle.clone();
    let second = tokio::spawn(async move {
        second_handle
            .register_service(service(3, "three", 82))
            .await
    });
    assert_eq!(read_register(&mut peer).await, ServiceId(2));
    assert_eq!(read_register(&mut peer).await, ServiceId(3));
    drop(peer);
    assert!(matches!(
        first.await.unwrap(),
        Err(TunnelError::Disconnected)
    ));
    assert!(matches!(
        second.await.unwrap(),
        Err(TunnelError::Disconnected)
    ));
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn negotiated_peer_drain_deadline_cannot_extend_local_shutdown() {
    let mut policy = crate::RuntimePolicy::default();
    policy.timeouts.shutdown_grace = Duration::from_millis(100);
    let (_handle, _counters, task, mut peer) =
        fake_session_with_hello(1, full_capabilities(), policy).await;
    // A 30 s peer deadline must collapse to the 100 ms local ceiling.
    write_boxed(
        &mut peer,
        &Message::Drain(eggtunnel_proto::Drain {
            deadline_ms: 30_000,
        }),
    )
    .await
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("peer drain must not extend local shutdown");
    assert!(outcome.unwrap().is_ok());
}

#[tokio::test]
async fn absent_capability_drain_keeps_local_only_shutdown_timing() {
    let mut policy = crate::RuntimePolicy::default();
    policy.timeouts.shutdown_grace = Duration::from_millis(100);
    let (_handle, _counters, task, mut peer) =
        fake_session_with_hello(0, Capabilities::default(), policy).await;
    // Without capability 2 the field is tolerated and local timing rules.
    write_boxed(
        &mut peer,
        &Message::Drain(eggtunnel_proto::Drain {
            deadline_ms: 30_000,
        }),
    )
    .await
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("1.0 drain must keep local shutdown timing");
    assert!(outcome.unwrap().is_ok());
}

#[tokio::test]
async fn correlated_cancelled_registration_is_unregistered_and_never_commits() {
    let (handle, counters, task, mut peer) =
        fake_session_with_hello(1, full_capabilities(), crate::RuntimePolicy::default()).await;
    let register_handle = handle.clone();
    let register = tokio::spawn(async move {
        register_handle
            .register_service(service(2, "cancelled", 81))
            .await
    });
    assert_eq!(read_register(&mut peer).await, ServiceId(2));
    register.abort();
    // A late Ack for the abandoned transaction triggers cleanup with
    // UnregisterService and can never enter desired state.
    ack_service(&mut peer, ServiceId(2), 31002).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), read_boxed(&mut peer))
            .await
            .unwrap()
            .unwrap(),
        Message::UnregisterService(eggtunnel_proto::UnregisterService {
            service_id: ServiceId(2)
        })
    ));
    assert_eq!(counters.snapshot().registered_services, 1);
    drop(peer);
    assert!(task.await.unwrap().is_err());
}

/// A peer that never accepts another byte: the exact shape of a server that
/// stops reading and used to wedge the whole client `select!` loop.
struct StalledWriter;

impl tokio::io::AsyncWrite for StalledWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Pending
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
}

#[tokio::test]
async fn control_writes_are_bounded_by_their_budget() {
    let mut stalled = StalledWriter;
    let error = super::write_control(
        &mut stalled,
        &Message::Ping(eggtunnel_proto::Ping { nonce: 1 }),
        Duration::from_millis(50),
    )
    .await
    .expect_err("a stalled server must not hold the client loop");
    assert!(matches!(error, TunnelError::Timeout));
    assert_eq!(
        error.termination_category(),
        crate::common::TerminationCategory::Timeout
    );
}
