use super::*;

pub(super) struct OpenContext {
    pub(super) transport: ClientDataTransport,
    pub(super) connector: Arc<dyn TargetConnector>,
    pub(super) session_id: eggtunnel_proto::SessionId,
    pub(super) cancel: CancellationToken,
    pub(super) out: mpsc::Sender<Message>,
    pub(super) counters: Counters,
}

pub(super) async fn handle_open(open: Open, service: ClientService, context: OpenContext) {
    let OpenContext {
        transport,
        connector,
        session_id,
        cancel,
        out,
        counters,
    } = context;
    let result = async {
        let target_context = TargetContext {
            session_id,
            connection_id: open.connection_id,
            cancellation: cancel.clone(),
        };
        let target = tokio::select! {
            _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
            result = timeout(counters.policy.timeouts.connect, connector.connect(service.clone(), target_context)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Target)?,
        };
        let mut data = match transport {
            ClientDataTransport::TcpTls { server_addr, server_name, tls, websocket, #[cfg(feature = "outbound-proxy")] outbound } => {
                let stream = tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = connect_server(&server_addr, counters.policy.timeouts.connect, #[cfg(feature = "outbound-proxy")] outbound.as_deref()) => result?,
                };
                #[allow(unused_mut)]
                let mut stream = tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = timeout(counters.policy.timeouts.handshake, tls_connect(stream, tls, &server_name)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
                };
                #[cfg(feature = "websocket")]
                if websocket {
                    let url = format!("wss://{server_addr}");
                    let ws_client = eggress_protocol_websocket::WebSocketTunnelClient::new(1024 * 1024);
                    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(1024 * 1024))
                        .max_frame_size(Some(1024 * 1024));
                    stream = tokio::select! {
                        _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                        result = timeout(counters.policy.timeouts.handshake, ws_client.connect_over_stream_with_config(&url, stream, ws_config)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
                    };
                }
                #[cfg(not(feature = "websocket"))]
                let _ = websocket;
                stream
            }
            #[cfg(feature = "quic")]
            ClientDataTransport::Quic(connection) => {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = timeout(counters.policy.timeouts.connect, connection.open_stream()) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Disconnected)?,
                }
            }
        };
        write_boxed(&mut data, &Message::DataHello(DataHello { session_id, service_id: service.id, connection_id: open.connection_id })).await?;
        match relay_with_options(target, data, RelayOptions::bounded(std::num::NonZeroUsize::new(16 * 1024).unwrap(), counters.policy.timeouts.relay_drain)).await {
            Ok(report) => {
                counters.bytes_upstream.fetch_add(report.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                counters.bytes_downstream.fetch_add(report.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
            }
            Err(failure) => {
                counters.bytes_upstream.fetch_add(failure.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                counters.bytes_downstream.fetch_add(failure.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok::<(), TunnelError>(())
    }.await;
    if let Err(error) = result {
        counters.record_termination(error.termination_category());
        tracing::debug!(service_id = service.id.0, termination = ?error.termination_category(), "client data Open ended");
        if !cancel.is_cancelled() {
            let _ = out.try_send(Message::OpenReject(OpenReject {
                connection_id: open.connection_id,
                code: 1,
            }));
        }
    }
}
