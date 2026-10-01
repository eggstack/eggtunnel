//! Authenticated Session registry and lifetime ownership.
//!
//! The registry holds only `Weak` references so a live `SessionContext` is
//! owned exclusively by the control loop that created it. `SessionGuard`
//! performs the counter/bind/listener cleanup that must happen exactly once
//! when a Session ends for any reason.

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use eggtunnel_proto::SessionId;
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::common::{Counters, TerminationCategory, TunnelError};

use super::pending::PendingEntry;

/// Opaque authenticated peer identity attached by the mTLS handshake. `None`
/// means the transport proved no peer identity (plain TCP/TLS or WSS).
pub(super) type Principal = Option<[u8; 32]>;

/// Weak-reference map of live Sessions. Kept as one bounded map so shutdown can
/// enumerate Sessions for drain without extending any lifetime.
///
/// The map is guarded by a standard mutex: every critical section is a
/// non-awaiting map operation, and `SessionGuard::drop` must be able to remove
/// its entry deterministically.
pub(super) type SessionRegistry = Arc<std::sync::Mutex<HashMap<SessionId, Weak<SessionContext>>>>;

pub(super) fn new_session_registry() -> SessionRegistry {
    Arc::new(std::sync::Mutex::new(HashMap::new()))
}

/// Per-Session ownership record. One instance exists per authenticated control
/// Session; the control loop holds the only strong reference.
pub(super) struct SessionContext {
    pub(super) id: SessionId,
    pub(super) principal: Principal,
    pub(super) cancel: CancellationToken,
    pub(super) pending: Mutex<HashMap<eggtunnel_proto::ConnectionId, PendingEntry>>,
    pub(super) connection_admission: Arc<Semaphore>,
    pub(super) control_tx: Mutex<Option<mpsc::Sender<eggtunnel_proto::Message>>>,
    pub(super) counters: Counters,
}

impl Drop for SessionContext {
    fn drop(&mut self) {
        let removed = self.pending.get_mut().len();
        self.counters
            .pending
            .fetch_sub(removed, std::sync::atomic::Ordering::Relaxed);
    }
}

impl SessionContext {
    /// Insert a new Session, failing closed when the bounded Session ceiling is
    /// reached. Dead weak entries are pruned first so the ceiling reflects live
    /// Sessions rather than accumulated history.
    pub(super) fn register(
        context: &Arc<SessionContext>,
        registry: &SessionRegistry,
        max_sessions: usize,
    ) -> Result<(), TunnelError> {
        let mut active = registry.lock().unwrap_or_else(|p| p.into_inner());
        active.retain(|_, weak| weak.strong_count() > 0);
        if active.len() >= max_sessions {
            return Err(TunnelError::ResourceExhausted);
        }
        active.insert(context.id, Arc::downgrade(context));
        Ok(())
    }

    /// Live Sessions with a strong reference, used by server shutdown drain.
    pub(super) fn live(registry: &SessionRegistry) -> Vec<Arc<SessionContext>> {
        registry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

/// Decrements Session/bind/service accounting and removes the registry entry
/// exactly once, on any control-loop exit path.
pub(super) struct SessionGuard {
    context: Arc<SessionContext>,
    sessions: SessionRegistry,
}

impl SessionGuard {
    pub(super) fn new(context: Arc<SessionContext>, sessions: SessionRegistry) -> Self {
        let active = context
            .counters
            .sessions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        context
            .counters
            .high_water_sessions
            .fetch_max(active, std::sync::atomic::Ordering::Relaxed);
        Self { context, sessions }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.context.cancel.cancel();
        self.context
            .counters
            .sessions
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        let removed = {
            let mut binds = self
                .context
                .counters
                .binds
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let before = binds.len();
            binds.retain(|(sid, _, _)| *sid != self.context.id);
            before - binds.len()
        };
        self.context
            .counters
            .services
            .fetch_sub(removed, std::sync::atomic::Ordering::Relaxed);
        // The registry entry is removed here, on every exit path: a contended
        // drop must not leave a stale entry behind.
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.context.id);
    }
}

/// Track an in-flight handshake so the `active_handshakes` counter converges to
/// zero on success, failure, saturation, cancellation, and shutdown.
pub(super) struct HandshakeGuard(Counters);

impl HandshakeGuard {
    pub(super) fn new(counters: Counters) -> Self {
        let active = counters
            .handshakes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        counters
            .high_water_handshakes
            .fetch_max(active, std::sync::atomic::Ordering::Relaxed);
        Self(counters)
    }
}

impl Drop for HandshakeGuard {
    fn drop(&mut self) {
        self.0
            .handshakes
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(super) fn record_saturation(counters: &Counters) {
    counters
        .rejected
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    counters.record_termination(TerminationCategory::ResourceExhausted);
}
