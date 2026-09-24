use super::service_state::{AckDisposition, ServiceState};
use crate::{ClientService, TunnelError};
use eggtunnel_proto::{RequestedBind, ServiceId, ServiceName, TcpTarget};
use tokio::sync::oneshot;

fn service(id: u64) -> ClientService {
    ClientService::new(
        ServiceId(id),
        ServiceName::new(format!("sequence-{id}")).unwrap(),
        RequestedBind::Loopback { port: 0 },
        TcpTarget::new("127.0.0.1", 80).unwrap(),
    )
}

#[tokio::test]
async fn deterministic_service_state_sequence_preserves_invariants_for_10000_steps() {
    const SEED: u64 = 0x4e4f_574d_414e_3031;
    let mut rng = SEED;
    let mut next_id = 1;
    let mut state = ServiceState::new(Vec::new());
    state.activate_initial();
    let mut waiter = None;

    for step in 0..10_000 {
        rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        match (rng >> 32) % 6 {
            0 | 1 if state.pending().is_none() => {
                let id = next_id;
                next_id += 1;
                let (reply, response) = oneshot::channel();
                waiter = Some(response);
                state
                    .begin(service(id), step as u64 % 7 + 1, reply)
                    .unwrap();
            }
            2 => {
                if let Some((id, generation)) = state
                    .pending()
                    .map(|pending| (pending.service.id, pending.generation))
                {
                    match state.take_ack(id, generation) {
                        AckDisposition::Commit(service, reply) => {
                            drop(reply);
                            state.commit(service);
                        }
                        AckDisposition::Abandoned => {}
                        AckDisposition::Stale(reply) => drop(reply),
                        AckDisposition::Unexpected => {
                            panic!("pending acknowledgement lost correlation")
                        }
                    }
                    waiter = None;
                }
            }
            3 => {
                if let Some(reply) = state.reject() {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err(TunnelError::Authorization));
                    }
                    waiter = None;
                }
            }
            4 => {
                let id = if next_id > 1 {
                    (rng % next_id).max(1)
                } else {
                    1
                };
                state.unregister(ServiceId(id));
            }
            5 => {
                state.finish_pending(TunnelError::Disconnected);
                waiter = None;
                state.clear_active();
                state.activate_initial();
            }
            _ => {}
        }

        let desired_ids: Vec<_> = state.desired().iter().map(|item| item.id).collect();
        for (index, id) in desired_ids.iter().enumerate() {
            assert!(
                !desired_ids[..index].contains(id),
                "duplicate desired id at step {step}"
            );
        }
        assert_eq!(
            state
                .active()
                .keys()
                .copied()
                .collect::<std::collections::HashSet<_>>(),
            desired_ids.iter().copied().collect(),
            "active snapshot diverged from desired state at step {step}"
        );
        assert!(state.pending().is_none_or(|pending| {
            !desired_ids.contains(&pending.service.id)
                && !state
                    .desired()
                    .iter()
                    .any(|item| item.name == pending.service.name)
        }));
        assert_eq!(
            waiter.is_some(),
            state.pending().is_some(),
            "waiter and in-flight transaction diverged at step {step}"
        );
    }
    eprintln!("deterministic Service state sequence: seed={SEED:#x}, steps=10000, outcome=pass");
}
