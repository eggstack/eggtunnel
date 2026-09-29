//! Service listener accept, relay-task ownership, and per-connection guards.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use eggress_relay::{RelayOptions, relay_with_options};
use eggtunnel_proto::{EffectiveBind, Message, Open, ServiceId, ServiceName};
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
    task::JoinSet,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::common::Counters;

use super::pending::{PendingEntry, remove_service_pending};
use super::session::SessionContext;

/// Registered-Service bookkeeping owned by the control loop. Dropping the
/// `CancellationToken` alone does not stop the listener; the control loop
/// cancels it explicitly on unregister and on teardown.
pub(super) struct ServiceEntry {
    pub(super) name: ServiceName,
    pub(super) cancel: CancellationToken,
}

/// Hold an active-connection permit and keep the `active_connections` counter
/// balanced for the lifetime of the relay task.
pub(super) struct ActiveConnectionGuard {
    _permit: OwnedSemaphorePermit,
    counters: Counters,
}

impl ActiveConnectionGuard {
    pub(super) fn new(permit: OwnedSemaphorePermit, counters: Counters) -> Self {
        let active = counters
            .active_connections
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        counters
            .high_water_active_connections
            .fetch_max(active, std::sync::atomic::Ordering::Relaxed);
        Self {
            _permit: permit,
            counters,
        }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        self.counters
            .active_connections
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Accept loop for one registered Service.
///
/// For every accepted external connection the server issues a fresh
/// single-use `ConnectionId`, records a bounded pending entry, advertises
/// `Open` on the control channel, and hands the correlated client stream to
/// the Eggress relay. All three bounds (session connections, pending
/// correlations, relay tasks) are finite.
pub(super) async fn run_service(
    listener: TcpListener,
    service_id: ServiceId,
    session: Arc<SessionContext>,
    opens: mpsc::Sender<Message>,
    cancel: CancellationToken,
    counters: Counters,
) {
    let mut relays = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = session.cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((external, _remote)) = accepted else { continue; };
                let Ok(connection_permit) = session.connection_admission.clone().try_acquire_owned() else {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(crate::common::TerminationCategory::ResourceExhausted);
                    continue;
                };
                let active_guard = ActiveConnectionGuard::new(connection_permit, counters.clone());
                let connection_id = match eggtunnel_proto::ConnectionId::generate() {
                    Ok(id) => id,
                    Err(_) => { counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed); continue; }
                };
                let (data_tx, data_rx) = oneshot::channel();
                let mut pending = session.pending.lock().await;
                let pending_lifetime = counters.policy.timeouts.pending_connection;
                if pending.len() >= counters.policy.limits.pending_per_session {
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    counters.record_termination(crate::common::TerminationCategory::ResourceExhausted);
                    continue;
                }
                pending.insert(connection_id, PendingEntry { service_id, expires: Instant::now() + pending_lifetime, data_tx });
                drop(pending);
                let pending_connections = counters.pending.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                counters.high_water_pending.fetch_max(pending_connections, std::sync::atomic::Ordering::Relaxed);
                if opens.try_send(Message::Open(Open { service_id, connection_id })).is_err() {
                    if session.pending.lock().await.remove(&connection_id).is_some() { counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed); }
                    counters.rejected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    continue;
                }
                let session = session.clone();
                let counters = counters.clone();
                let relay_cancel = cancel.child_token();
                relays.spawn(async move {
                    let _active_guard = active_guard;
                    let outcome = tokio::select! {
                        _ = relay_cancel.cancelled() => None,
                        result = timeout(pending_lifetime, data_rx) => result.ok().and_then(Result::ok),
                    };
                    if session.pending.lock().await.remove(&connection_id).is_some() {
                        counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    if let Some(data) = outcome {
                        match relay_with_options(external, data, RelayOptions::bounded(std::num::NonZeroUsize::new(16 * 1024).unwrap(), counters.policy.timeouts.relay_drain)).await {
                            Ok(report) => {
                                tracing::debug!(service_id = service_id.0, termination = "clean", "relay completed");
                                counters.bytes_upstream.fetch_add(report.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                                counters.bytes_downstream.fetch_add(report.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
                            }
                            Err(failure) => {
                                tracing::debug!(service_id = service_id.0, termination = "transport", "relay ended");
                                counters.bytes_upstream.fetch_add(failure.bytes_upstream, std::sync::atomic::Ordering::Relaxed);
                                counters.bytes_downstream.fetch_add(failure.bytes_downstream, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
            Some(result) = relays.join_next(), if !relays.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    relays.abort_all();
    while relays.join_next().await.is_some() {}
    remove_service_pending(&session, service_id).await;
}

/// Project a bound socket into the protocol's address-family-independent
/// `EffectiveBind` (IPv4 addresses are carried as IPv4-mapped IPv6).
pub(super) fn socket_to_effective(addr: SocketAddr) -> EffectiveBind {
    let (ip, port) = match addr {
        SocketAddr::V4(addr) => (addr.ip().to_ipv6_mapped().octets(), addr.port()),
        SocketAddr::V6(addr) => (addr.ip().octets(), addr.port()),
    };
    EffectiveBind { address: ip, port }
}
