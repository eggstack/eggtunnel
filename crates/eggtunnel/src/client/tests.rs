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
        #[cfg(feature = "quic")]
        quic_client: Arc::new(std::sync::Mutex::new(None)),
    };
    let task_counters = counters.clone();
    let task_cancel = cancel.clone();
    let task_token = token.clone();
    let task = tokio::spawn(async move {
        let mut service_state = ServiceState::new(vec![service(1, "initial", 80)]);
        let mut command_rx = command_rx;
        let mut reconnect_delay = Duration::from_millis(500);
        run_session(
            Box::new(client_io),
            SessionRun {
                token: &task_token,
                transport: ClientDataTransport::TcpTls {
                    server_addr: "localhost:443".into(),
                    server_name: "localhost".into(),
                    tls: build_tls_config(None).unwrap(),
                    websocket: false,
                    #[cfg(feature = "outbound-proxy")]
                    outbound: None,
                },
                connector: Arc::new(super::config::TcpTargetConnector),
                cancel: &task_cancel,
                counters: &task_counters,
                reconnect_delay: &mut reconnect_delay,
                service_state: &mut service_state,
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

#[cfg(feature = "quic")]
#[test]
fn client_profile_validator_rejects_quic_custom_ca() {
    let mut config = config("localhost:443");
    config.ca_pem = Some(b"custom CA".to_vec());
    let builder = ClientBuilder::new(config).transport(ClientTransportProfile::Quic);
    assert!(builder.validate().is_err());
}

#[cfg(feature = "quic")]
#[test]
fn client_profile_validator_accepts_quic_defaults() {
    assert!(
        ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::Quic)
            .validate()
            .is_ok()
    );
}

#[cfg(feature = "websocket")]
#[test]
fn client_profile_validator_accepts_websocket_defaults() {
    assert!(
        ClientBuilder::new(config("localhost:443"))
            .transport(ClientTransportProfile::WebSocket)
            .validate()
            .is_ok()
    );
}

#[cfg(all(feature = "quic", feature = "outbound-proxy"))]
#[test]
fn client_profile_validator_rejects_quic_proxy() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .transport(ClientTransportProfile::Quic)
        .outbound_proxy("socks5://localhost:1080");
    assert!(builder.validate().is_err());
}

#[cfg(all(feature = "quic", feature = "mtls"))]
#[test]
fn client_profile_validator_rejects_quic_mtls() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .transport(ClientTransportProfile::Quic)
        .with_identity(ClientIdentity::new(b"cert".to_vec(), b"key".to_vec()));
    assert!(builder.validate().is_err());
}

#[cfg(all(feature = "websocket", feature = "outbound-proxy"))]
#[test]
fn client_profile_validator_accepts_websocket_proxy() {
    let builder = ClientBuilder::new(config("localhost:443"))
        .transport(ClientTransportProfile::WebSocket)
        .outbound_proxy("socks5://localhost:1080");
    assert!(builder.validate().is_ok());
}

#[cfg(all(feature = "websocket", feature = "mtls"))]
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
