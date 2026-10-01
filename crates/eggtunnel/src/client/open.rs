use super::reconnect::connect_tcp;
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
            result = timeout(counters.policy.timeouts.connect, connector.connect(service.clone(), target_context)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|error| match error {
                super::TargetError::Refused => TunnelError::Target,
                super::TargetError::Failed => TunnelError::Io(std::io::Error::other("application target failed")),
            })?,
        };
        let mut data = match transport {
            ClientDataTransport::TcpTls { endpoint, server_name, tls, websocket, #[cfg(feature = "outbound-proxy")] outbound } => {
                let stream = tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = connect_tcp(&endpoint, counters.policy.timeouts.connect, #[cfg(feature = "outbound-proxy")] outbound.as_deref()) => result?,
                };
                #[allow(unused_mut)]
                let mut stream = tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = timeout(counters.policy.timeouts.handshake, tls_connect(stream, tls, &server_name)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
                };
                #[cfg(feature = "websocket-client")]
                if websocket {
                    let url = endpoint.websocket_url();
                    let ws_client = eggress_protocol_websocket::WebSocketTunnelClient::new(crate::common::MAX_WEBSOCKET_FRAME_SIZE);
                    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(crate::common::MAX_WEBSOCKET_FRAME_SIZE))
                        .max_frame_size(Some(crate::common::MAX_WEBSOCKET_FRAME_SIZE));
                    stream = tokio::select! {
                        _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                        result = timeout(counters.policy.timeouts.handshake, ws_client.connect_over_stream_with_config(&url, stream, ws_config)) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Tls)?,
                    };
                }
                #[cfg(not(feature = "websocket-client"))]
                let _ = websocket;
                stream
            }
            #[cfg(feature = "quic-client")]
            ClientDataTransport::Quic(connection) => {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(TunnelError::Cancelled),
                    result = timeout(counters.policy.timeouts.connect, connection.open_stream()) => result.map_err(|_| TunnelError::Timeout)?.map_err(|_| TunnelError::Disconnected)?,
                }
            }
        };
        timeout(counters.policy.timeouts.handshake, write_boxed(&mut data, &Message::DataHello(DataHello { session_id, service_id: service.id, connection_id: open.connection_id })))
            .await.map_err(|_| TunnelError::Timeout)??;
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
        tracing::debug!(service_id = service.id.0, termination = ?error.termination_category(), "client data Open ended");
        if !cancel.is_cancelled() {
            let _ = out.try_send(Message::OpenReject(OpenReject {
                connection_id: open.connection_id,
                code: 1,
            }));
        }
    }
}
