//! Session-local state for dynamic Service registration.
//!
//! The wire Error message has no ServiceId, so the protocol only permits one
//! dynamic registration acknowledgement to be outstanding per Session. Keep
//! that constraint explicit here rather than relying on map iteration order.
use std::collections::HashMap;

use eggtunnel_proto::{EffectiveBind, ServiceId};
use tokio::sync::oneshot;

use crate::common::{ClientService, TunnelError};

pub(super) struct PendingRegistration {
    pub service: ClientService,
    pub generation: u64,
    pub reply: Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>,
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

pub(super) struct ServiceState {
    desired: Vec<ClientService>,
    active: HashMap<ServiceId, ClientService>,
    pending: Option<PendingRegistration>,
}

impl ServiceState {
    pub fn new(desired: Vec<ClientService>) -> Self {
        Self {
            desired,
            active: HashMap::new(),
            pending: None,
        }
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
    pub fn pending(&self) -> Option<&PendingRegistration> {
        self.pending.as_ref()
    }
    pub fn pending_mut(&mut self) -> Option<&mut PendingRegistration> {
        self.pending.as_mut()
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
        {
            return Err((TunnelError::ServiceAlreadyExists, reply));
        }
        if self.pending.is_some() {
            return Err((TunnelError::ResourceExhausted, reply));
        }
        self.pending = Some(PendingRegistration {
            service,
            generation,
            reply: Some(reply),
        });
        Ok(())
    }
    pub fn take_ack(&mut self, id: ServiceId, generation: u64) -> AckDisposition {
        let Some(pending) = self.pending.take() else {
            return AckDisposition::Unexpected;
        };
        if pending.service.id != id {
            self.pending = Some(pending);
            return AckDisposition::Unexpected;
        }
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
    pub fn reject(
        &mut self,
    ) -> Option<Option<oneshot::Sender<Result<EffectiveBind, TunnelError>>>> {
        self.pending.take().map(|pending| pending.reply)
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
            let _ = reply.send(Err(error));
        }
    }
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
}
