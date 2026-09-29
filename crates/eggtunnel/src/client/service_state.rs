//! Session-local state for dynamic Service registration.
//!
//! Two wire modes share this owner:
//!
//! - legacy serial: the peer did not negotiate capability 1, so failures
//!   arrive as the generic `Error` message with no `ServiceId`. The
//!   protocol only permits one dynamic registration acknowledgement to be
//!   outstanding per Session;
//! - correlated bounded: capability 1 was negotiated, so failures arrive
//!   as `RegisterReject` carrying the `ServiceId`. Multiple transactions
//!   may be in flight, keyed by `ServiceId` and Session generation, up to
//!   a finite policy-derived ceiling.
//!
//! Desired state still changes only after an acknowledgement from the
//! current Session generation; stale, abandoned, and unknown responses
//! can never commit.
use std::collections::HashMap;

use eggtunnel_proto::{EffectiveBind, ServiceId};
use tokio::{sync::oneshot, time::Instant};

use crate::common::{ClientService, TunnelError};

pub(super) struct PendingRegistration {
    pub service: ClientService,
    pub generation: u64,
    pub reply: Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>,
    pub deadline: Option<Instant>,
}

pub(super) enum AckDisposition {
    Commit(
        ClientService,
        Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>,
    ),
    Abandoned,
    Stale(Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>),
    Unexpected,
}

pub(super) enum RejectDisposition {
    Reject(Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>),
    Stale(Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>),
    Unknown,
}

/// What an acknowledgement-timeout sweep removed.
pub(super) enum Expiry {
    None,
    /// Legacy single transaction timed out; the caller ends the Session,
    /// preserving 1.0 behavior.
    Legacy,
    /// Correlated transactions timed out; the Session and unrelated
    /// transactions survive.
    Correlated(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RegistrationMode {
    LegacySerial,
    CorrelatedBounded { max_in_flight: usize },
}

pub(super) struct ServiceState {
    desired: Vec<ClientService>,
    active: HashMap<ServiceId, ClientService>,
    pending: Option<PendingRegistration>,
    in_flight: HashMap<ServiceId, PendingRegistration>,
    mode: RegistrationMode,
}

impl ServiceState {
    pub fn new(desired: Vec<ClientService>) -> Self {
        Self {
            desired,
            active: HashMap::new(),
            pending: None,
            in_flight: HashMap::new(),
            mode: RegistrationMode::LegacySerial,
        }
    }

    /// Select the wire mode negotiated for the Session that is about to
    /// run. Any transaction left over from a previous Session is failed
    /// closed first; in practice the previous Session already drained
    /// them through `finish_pending`.
    pub fn set_mode(&mut self, mode: RegistrationMode) {
        self.finish_pending(TunnelError::Disconnected);
        self.mode = mode;
    }

    pub fn mode(&self) -> RegistrationMode {
        self.mode
    }

    pub fn desired(&self) -> &[ClientService] {
        &self.desired
    }
    pub fn activate_initial(&mut self) {
        self.active = self
            .desired
            .iter()
            .cloned()
            .map(|service| (service.id, service))
            .collect();
    }
    pub fn active(&self) -> &HashMap<ServiceId, ClientService> {
        &self.active
    }
    #[cfg(test)]
    pub fn pending(&self) -> Option<&PendingRegistration> {
        self.pending.as_ref()
    }
    pub fn pending_mut(&mut self) -> Option<&mut PendingRegistration> {
        self.pending.as_mut()
    }
    /// Transactions without an acknowledgement, across both modes.
    pub fn unacknowledged(&self) -> usize {
        usize::from(self.pending.is_some()) + self.in_flight.len()
    }
    pub fn in_flight_mut(&mut self, id: ServiceId) -> Option<&mut PendingRegistration> {
        self.in_flight.get_mut(&id)
    }
    /// Earliest acknowledgement deadline across both modes.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.deadline)
            .into_iter()
            .chain(self.in_flight.values().filter_map(|txn| txn.deadline))
            .min()
    }
    /// Fail every transaction whose deadline has passed. Legacy expiry is
    /// reported separately so the caller can preserve 1.0 Session-level
    /// timeout behavior.
    pub fn expire_overdue(&mut self, now: Instant) -> Expiry {
        let mut correlated = 0;
        self.in_flight.retain(|_, txn| {
            let overdue = txn.deadline.is_some_and(|deadline| deadline <= now);
            if overdue {
                if let Some(reply) = txn.reply.take() {
                    let _ = reply.send(Err(TunnelError::Timeout));
                }
                correlated += 1;
            }
            !overdue
        });
        if let Some(pending) = self.pending.as_mut()
            && pending.deadline.is_some_and(|deadline| deadline <= now)
            && let Some(reply) = pending.reply.take()
        {
            let _ = reply.send(Err(TunnelError::Timeout));
            return Expiry::Legacy;
        }
        if correlated > 0 {
            Expiry::Correlated(correlated)
        } else {
            Expiry::None
        }
    }
    pub fn begin(
        &mut self,
        service: ClientService,
        generation: u64,
        reply: oneshot::Sender<Result<EffectiveBind, TunnelError>>,
    ) -> Result<
        (),
        (
            TunnelError,
            oneshot::Sender<Result<EffectiveBind, TunnelError>>,
        ),
    > {
        if self
            .desired
            .iter()
            .any(|existing| existing.id == service.id || existing.name == service.name)
            || self.pending.as_ref().is_some_and(|pending| {
                pending.service.id == service.id || pending.service.name == service.name
            })
            || self
                .in_flight
                .values()
                .any(|txn| txn.service.id == service.id || txn.service.name == service.name)
        {
            return Err((TunnelError::ServiceAlreadyExists, reply));
        }
        match self.mode {
            RegistrationMode::LegacySerial => {
                if self.pending.is_some() {
                    return Err((TunnelError::ResourceExhausted, reply));
                }
                self.pending = Some(PendingRegistration {
                    service,
                    generation,
                    reply: Some(reply),
                    deadline: None,
                });
            }
            RegistrationMode::CorrelatedBounded { max_in_flight } => {
                if self.in_flight.len() >= max_in_flight.max(1) {
                    return Err((TunnelError::ResourceExhausted, reply));
                }
                self.in_flight.insert(
                    service.id,
                    PendingRegistration {
                        service,
                        generation,
                        reply: Some(reply),
                        deadline: None,
                    },
                );
            }
        }
        Ok(())
    }
    fn take_transaction(&mut self, id: ServiceId) -> TransactionTake {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.service.id == id)
        {
            // `is_some_and` just proved this is `Some`.
            TransactionTake::Found(
                self.pending.take().expect("pending checked above"),
                TransactionHome::Legacy,
            )
        } else if self.pending.is_some() {
            // The legacy store holds a different Service: the response
            // cannot belong to it, so it stays for its own response.
            // (Unreachable in legacy mode — the peer never sends
            // `RegisterReject` there — but fail-closed regardless.)
            TransactionTake::Mismatched
        } else if let Some(txn) = self.in_flight.remove(&id) {
            TransactionTake::Found(txn, TransactionHome::Correlated)
        } else {
            TransactionTake::Missing
        }
    }
    pub fn take_ack(&mut self, id: ServiceId, generation: u64) -> AckDisposition {
        let (pending, _) = match self.take_transaction(id) {
            TransactionTake::Found(pending, home) => (pending, home),
            TransactionTake::Mismatched | TransactionTake::Missing => {
                return AckDisposition::Unexpected;
            }
        };
        if pending.generation != generation {
            return AckDisposition::Stale(pending.reply);
        }
        if pending
            .reply
            .as_ref()
            .is_none_or(oneshot::Sender::is_closed)
        {
            return AckDisposition::Abandoned;
        }
        AckDisposition::Commit(pending.service, pending.reply)
    }
    pub fn take_reject(&mut self, id: ServiceId, generation: u64) -> RejectDisposition {
        let (pending, _) = match self.take_transaction(id) {
            TransactionTake::Found(pending, home) => (pending, home),
            TransactionTake::Mismatched | TransactionTake::Missing => {
                return RejectDisposition::Unknown;
            }
        };
        if pending.generation != generation {
            return RejectDisposition::Stale(pending.reply);
        }
        RejectDisposition::Reject(pending.reply)
    }
    pub fn reject(
        &mut self,
    ) -> Option<Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>> {
        self.pending.take().map(|pending| pending.reply)
    }
    /// Abandon one correlated transaction (write timeout): fail its reply
    /// without touching the Session or unrelated transactions.
    pub fn abandon(&mut self, id: ServiceId) {
        if let Some(mut txn) = self.in_flight.remove(&id)
            && let Some(reply) = txn.reply.take()
        {
            let _ = reply.send(Err(TunnelError::Timeout));
        }
    }
    pub fn unregister(&mut self, id: ServiceId) -> bool {
        if let Some(pending) = self
            .pending
            .as_mut()
            .filter(|pending| pending.service.id == id)
            && let Some(reply) = pending.reply.take()
        {
            let _ = reply.send(Err(TunnelError::Cancelled));
        }
        if let Some(mut txn) = self.in_flight.remove(&id)
            && let Some(reply) = txn.reply.take()
        {
            let _ = reply.send(Err(TunnelError::Cancelled));
        }
        self.desired.retain(|service| service.id != id);
        self.active.remove(&id).is_some()
    }
    pub fn commit(&mut self, service: ClientService) {
        self.active.insert(service.id, service.clone());
        self.desired.push(service);
    }
    pub fn clear_active(&mut self) {
        self.active.clear();
    }
    pub fn finish_pending(&mut self, error: TunnelError) {
        if let Some(pending) = self.pending.take()
            && let Some(reply) = pending.reply
        {
            let _ = reply.send(Err(duplicate_error(&error)));
        }
        for (_, mut txn) in std::mem::take(&mut self.in_flight) {
            if let Some(reply) = txn.reply.take() {
                let _ = reply.send(Err(duplicate_error(&error)));
            }
        }
    }
}

/// Reproduce a session-end error per abandoned reply. All call sites pass
/// unit variants; `Io`/`Protocol` payloads cannot occur here and map to
/// `Disconnected` fail-closed rather than dropping replies silently.
fn duplicate_error(error: &TunnelError) -> TunnelError {
    match error {
        TunnelError::Configuration(message) => TunnelError::Configuration(message),
        TunnelError::Io(_) | TunnelError::Protocol(_) => TunnelError::Disconnected,
        TunnelError::Tls => TunnelError::Tls,
        TunnelError::Authentication => TunnelError::Authentication,
        TunnelError::Authorization => TunnelError::Authorization,
        TunnelError::Disconnected => TunnelError::Disconnected,
        TunnelError::Cancelled => TunnelError::Cancelled,
        TunnelError::Timeout => TunnelError::Timeout,
        TunnelError::Target => TunnelError::Target,
        TunnelError::ResourceExhausted => TunnelError::ResourceExhausted,
        TunnelError::ServiceAlreadyExists => TunnelError::ServiceAlreadyExists,
        TunnelError::PeerClosed => TunnelError::PeerClosed,
    }
}

enum TransactionHome {
    Legacy,
    Correlated,
}

enum TransactionTake {
    Found(PendingRegistration, TransactionHome),
    /// A legacy transaction for a different Service is outstanding; it is
    /// left untouched and the response matches nothing.
    Mismatched,
    Missing,
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggtunnel_proto::{RequestedBind, ServiceName, TcpTarget};

    fn service(id: u64, name: &str) -> ClientService {
        ClientService::new(
            ServiceId(id),
            ServiceName::new(name).unwrap(),
            RequestedBind::Loopback { port: 0 },
            TcpTarget::new("127.0.0.1", 80).unwrap(),
        )
    }

    #[tokio::test]
    async fn only_one_registration_can_be_in_flight_and_ack_commits_desired_state() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        let (reply, _rx) = oneshot::channel();
        state.begin(service(2, "dynamic"), 9, reply).unwrap();
        let (second, _second_rx) = oneshot::channel();
        assert!(matches!(
            state.begin(service(3, "other"), 9, second),
            Err((TunnelError::ResourceExhausted, _))
        ));
        let committed = match state.take_ack(ServiceId(2), 9) {
            AckDisposition::Commit(s, _) => s,
            _ => panic!("matching current-generation ack should commit"),
        };
        state.commit(committed);
        assert_eq!(state.desired().len(), 2);
        assert!(matches!(
            state.take_ack(ServiceId(2), 9),
            AckDisposition::Unexpected
        ));
    }

    #[tokio::test]
    async fn disconnect_fails_pending_and_reconnect_snapshot_excludes_it() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        let (reply, rx) = oneshot::channel();
        state.begin(service(2, "pending"), 4, reply).unwrap();
        state.finish_pending(TunnelError::Disconnected);
        assert!(matches!(rx.await.unwrap(), Err(TunnelError::Disconnected)));
        assert_eq!(
            state.desired().iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![ServiceId(1)]
        );
    }

    #[tokio::test]
    async fn unregister_tombstones_pending_ack_and_removes_desired_service() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        let (reply, rx) = oneshot::channel();
        state.begin(service(2, "pending"), 4, reply).unwrap();
        state.unregister(ServiceId(2));
        assert!(matches!(rx.await.unwrap(), Err(TunnelError::Cancelled)));
        assert!(matches!(
            state.take_ack(ServiceId(2), 4),
            AckDisposition::Abandoned
        ));
        assert_eq!(state.desired().len(), 1);
    }

    #[test]
    fn initial_snapshot_is_ordered_and_duplicate_id_or_name_is_rejected() {
        let mut state = ServiceState::new(vec![service(2, "second"), service(1, "first")]);
        assert_eq!(
            state.desired().iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![ServiceId(2), ServiceId(1)]
        );
        let (reply, _rx) = oneshot::channel();
        assert!(matches!(
            state.begin(service(2, "new-name"), 1, reply),
            Err((TunnelError::ServiceAlreadyExists, _))
        ));
        let (reply, _rx) = oneshot::channel();
        assert!(matches!(
            state.begin(service(3, "first"), 1, reply),
            Err((TunnelError::ServiceAlreadyExists, _))
        ));
    }

    #[tokio::test]
    async fn rejected_or_stale_acknowledgement_never_enters_desired_state() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        let (reply, rx) = oneshot::channel();
        state.begin(service(2, "rejected"), 7, reply).unwrap();
        let reply = state.reject().unwrap().unwrap();
        let _ = reply.send(Err(TunnelError::Authorization));
        assert!(matches!(rx.await.unwrap(), Err(TunnelError::Authorization)));

        let (reply, rx) = oneshot::channel();
        state.begin(service(3, "stale"), 7, reply).unwrap();
        let AckDisposition::Stale(reply) = state.take_ack(ServiceId(3), 8) else {
            panic!("old generation must be rejected")
        };
        let _ = reply.unwrap().send(Err(TunnelError::Disconnected));
        assert!(matches!(rx.await.unwrap(), Err(TunnelError::Disconnected)));
        assert_eq!(
            state.desired().iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![ServiceId(1)]
        );
    }

    #[tokio::test]
    async fn correlated_mode_allows_bounded_concurrent_registrations() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        state.set_mode(RegistrationMode::CorrelatedBounded { max_in_flight: 2 });
        let (first, first_rx) = oneshot::channel();
        let (second, second_rx) = oneshot::channel();
        state.begin(service(2, "two"), 9, first).unwrap();
        state.begin(service(3, "three"), 9, second).unwrap();
        assert_eq!(state.unacknowledged(), 2);
        // The ceiling fails closed without disturbing live transactions.
        let (third, _) = oneshot::channel();
        assert!(matches!(
            state.begin(service(4, "four"), 9, third),
            Err((TunnelError::ResourceExhausted, _))
        ));
        // Out-of-order reject then ack both correlate by ServiceId.
        let RejectDisposition::Reject(reply) = state.take_reject(ServiceId(3), 9) else {
            panic!("reject must correlate")
        };
        let _ = reply.unwrap().send(Err(TunnelError::Authorization));
        assert!(matches!(
            second_rx.await.unwrap(),
            Err(TunnelError::Authorization)
        ));
        let committed = match state.take_ack(ServiceId(2), 9) {
            AckDisposition::Commit(s, _) => s,
            _ => panic!("ack must correlate"),
        };
        state.commit(committed);
        drop(first_rx);
        assert_eq!(state.unacknowledged(), 0);
        assert_eq!(state.desired().len(), 2);
    }

    #[tokio::test]
    async fn correlated_unknown_and_stale_rejects_never_commit() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        state.set_mode(RegistrationMode::CorrelatedBounded { max_in_flight: 4 });
        assert!(matches!(
            state.take_reject(ServiceId(99), 9),
            RejectDisposition::Unknown
        ));
        let (reply, rx) = oneshot::channel();
        state.begin(service(2, "two"), 9, reply).unwrap();
        let RejectDisposition::Stale(reply) = state.take_reject(ServiceId(2), 10) else {
            panic!("old generation must be stale")
        };
        let _ = reply.unwrap().send(Err(TunnelError::Disconnected));
        assert!(matches!(rx.await.unwrap(), Err(TunnelError::Disconnected)));
        assert_eq!(state.unacknowledged(), 0);
        assert_eq!(state.desired().len(), 1);
    }

    #[tokio::test]
    async fn correlated_overdue_transactions_fail_without_touching_others() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        state.set_mode(RegistrationMode::CorrelatedBounded { max_in_flight: 4 });
        let (first, first_rx) = oneshot::channel();
        let (second, _second_rx) = oneshot::channel();
        state.begin(service(2, "two"), 9, first).unwrap();
        state.begin(service(3, "three"), 9, second).unwrap();
        state.in_flight_mut(ServiceId(2)).unwrap().deadline =
            Some(Instant::now() - std::time::Duration::from_secs(1));
        state.in_flight_mut(ServiceId(3)).unwrap().deadline =
            Some(Instant::now() + std::time::Duration::from_secs(60));
        assert!(state.next_deadline().unwrap() <= Instant::now());
        assert!(matches!(
            state.expire_overdue(Instant::now()),
            Expiry::Correlated(1)
        ));
        assert!(matches!(first_rx.await.unwrap(), Err(TunnelError::Timeout)));
        assert_eq!(state.unacknowledged(), 1);
    }

    #[tokio::test]
    async fn unregister_cancels_correlated_transactions() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        state.set_mode(RegistrationMode::CorrelatedBounded { max_in_flight: 4 });
        let (reply, rx) = oneshot::channel();
        state.begin(service(2, "two"), 9, reply).unwrap();
        state.unregister(ServiceId(2));
        assert!(matches!(rx.await.unwrap(), Err(TunnelError::Cancelled)));
        assert!(matches!(
            state.take_ack(ServiceId(2), 9),
            AckDisposition::Unexpected
        ));
    }

    #[tokio::test]
    async fn mode_switch_fails_leftover_transactions_closed() {
        let mut state = ServiceState::new(vec![service(1, "initial")]);
        let (reply, rx) = oneshot::channel();
        state.begin(service(2, "two"), 9, reply).unwrap();
        state.set_mode(RegistrationMode::CorrelatedBounded { max_in_flight: 4 });
        assert!(matches!(rx.await.unwrap(), Err(TunnelError::Disconnected)));
        assert_eq!(state.unacknowledged(), 0);
    }
}
