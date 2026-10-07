//! Pending Connection / `DataHello` correlation.
//!
//! A `ConnectionId` is issued by the service listener, advertised through
//! `Open`, and claimed exactly once by a `DataHello` on a separate Data
//! Connection. The pending table is bounded per Session and every removal
//! path returns the `pending_connections` counter to zero.

use std::time::Instant;

use eggtunnel_proto::{DataHello, ServiceId};

use crate::common::{Counters, TunnelError};

use super::session::{SessionContext, SessionRegistry};

/// A service-side Open awaiting the matching client DataHello.
pub(super) struct PendingEntry {
    pub(super) service_id: ServiceId,
    pub(super) expires: Instant,
    pub(super) data_tx: tokio::sync::oneshot::Sender<eggress_core::BoxStream>,
}

/// Resolve a client `DataHello` against the live Session registry.
///
/// A DataHello is accepted only when the Session exists, the Data
/// Connection presents the same Principal as the control Session, the
/// ConnectionId is still pending, and the entry is for the requested Service
/// and has not expired. Every other outcome is a counted rejection.
pub(super) async fn accept_data_hello(
    stream: eggress_core::BoxStream,
    hello: DataHello,
    principal: Option<[u8; 32]>,
    sessions: &SessionRegistry,
    counters: &Counters,
) -> Result<(), TunnelError> {
    let session = sessions
        .entries()
        .get(&hello.session_id)
        .and_then(std::sync::Weak::upgrade);
    let Some(session) = session else {
        tracing::warn!(
            category = "data_hello_session_mismatch",
            "DataHello rejected"
        );
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authentication);
    };
    if session.principal != principal {
        tracing::warn!(
            category = "data_hello_principal_mismatch",
            "DataHello rejected"
        );
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authentication);
    }
    let pending = {
        let mut entries = session.pending.lock().await;
        let entry = entries.remove(&hello.connection_id);
        if entry.is_some() {
            session
                .counters
                .pending
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        entry
    };
    let Some(pending) = pending else {
        tracing::warn!(
            category = "data_hello_unknown_connection",
            "DataHello rejected"
        );
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authorization);
    };
    if pending.service_id != hello.service_id || pending.expires <= Instant::now() {
        tracing::warn!(
            category = "data_hello_service_or_expiry_mismatch",
            "DataHello rejected"
        );
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Err(TunnelError::Authorization);
    }
    tracing::debug!(
        service_id = hello.service_id.0,
        "DataHello correlated to pending ConnectionId"
    );
    pending
        .data_tx
        .send(stream)
        .map_err(|_| TunnelError::Cancelled)
}

/// Drop every pending entry belonging to one Service (listener removal or
/// Service unregistration).
pub(super) async fn remove_service_pending(session: &SessionContext, service: ServiceId) {
    let mut pending = session.pending.lock().await;
    let before = pending.len();
    pending.retain(|_, entry| entry.service_id != service);
    let removed = before - pending.len();
    session
        .counters
        .pending
        .fetch_sub(removed, std::sync::atomic::Ordering::Relaxed);
}

/// Drop every pending entry for the Session (Session teardown).
pub(super) async fn remove_all_pending(session: &SessionContext) {
    {
        let mut pending = session.pending.lock().await;
        let len = pending.len();
        pending.clear();
        session
            .counters
            .pending
            .fetch_sub(len, std::sync::atomic::Ordering::Relaxed);
    }
}
