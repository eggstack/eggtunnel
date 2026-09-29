//! Authenticated control-loop Session driver.
//!
//! Owns the lifetime of one authenticated Session: the Service registration
//! table, Service listener spawn/cancel, `Open`/`OpenReject` correlation,
//! heartbeat response, and drain. All mutations are bounded by the validated
//! `RuntimePolicy` and `BindPolicy`.

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
};

use eggtunnel_proto::{
    AuthOk, BoundedDiagnostic, CAPABILITY_DRAIN_DEADLINE, CAPABILITY_REGISTER_REJECT, Capabilities,
    ClientHello, ErrorMessage, Message, Ping, Pong, ProtocolVersion, RegisterAck, RegisterReject,
    RegisterService, ServerHello, ServiceId, SessionId, UnregisterService,
};
use tokio::{
    io::AsyncWrite,
    net::TcpListener,
    sync::{Mutex, Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::common::{BindPolicy, Counters, SecretToken, TunnelError, bind_to_socket, verify_token};

use super::auth::{AuthFailureLimiter, reject_authentication};
use super::pending::{remove_all_pending, remove_service_pending};
use super::service::{ServiceEntry, run_service, socket_to_effective};
use super::session::{
    HandshakeGuard, Principal, SessionContext, SessionGuard, SessionRegistry, record_saturation,
};
use crate::wire_io::{read_boxed, read_message, write_boxed, write_message};

/// Generic `Error` codes used when the peer has not negotiated correlated
/// registration rejection. The same numeric vocabulary is reused by
/// `RegisterReject` so a negotiated peer keeps identical categories.
pub(super) const REGISTRATION_ERROR_DUPLICATE: u16 = 1;
pub(super) const REGISTRATION_ERROR_BIND: u16 = 2;
pub(super) const REGISTRATION_ERROR_LISTENER: u16 = 3;
pub(super) const REGISTRATION_ERROR_ADMISSION: u16 = 5;

/// One accepted client control connection, carried from the transport accept
/// path through authentication into Session establishment.
pub(super) struct ControlAdmission {
    pub(super) stream: eggress_core::BoxStream,
    pub(super) hello: ClientHello,
    pub(super) token: SecretToken,
    pub(super) source: IpAddr,
    pub(super) sessions: SessionRegistry,
    pub(super) counters: Counters,
    pub(super) auth_failures: Arc<AuthFailureLimiter>,
    pub(super) bind_policy: BindPolicy,
    pub(super) cancel: CancellationToken,
    pub(super) principal: Principal,
    pub(super) admission: Option<tokio::sync::OwnedSemaphorePermit>,
    pub(super) handshake_guard: Option<HandshakeGuard>,
}

/// Run the authenticated control Session for one client.
///
/// Ordering is load-bearing: the `ServerHello` is written before the bearer
/// token is read, the admission and handshake slots are released only after
/// authentication succeeds, and the Session is registered before `AuthOk` is
/// written so a client never observes a Session the server will not serve.
pub(super) async fn serve_control(admission: ControlAdmission) -> Result<(), TunnelError> {
    let ControlAdmission {
        mut stream,
        hello,
        token,
        source,
        sessions,
        counters,
        auth_failures,
        bind_policy,
        cancel,
        principal,
        mut admission,
        mut handshake_guard,
    } = admission;

    if auth_failures.is_blocked(source) {
        return Err(TunnelError::Authentication);
    }
    if hello.version.major != ProtocolVersion::CURRENT.major {
        return Err(TunnelError::Protocol(
            eggtunnel_proto::ProtocolError::UnsupportedVersion(
                hello.version.major,
                hello.version.minor,
            ),
        ));
    }
    // Capability intersection (ADR-0002): only capabilities both peers
    // support are negotiated. A 1.0 peer advertises an empty list and gets
    // baseline behavior; minor inequality alone never gates anything.
    let negotiated = Capabilities::supported().intersect(&hello.capabilities);
    write_boxed(
        &mut stream,
        &Message::ServerHello(ServerHello {
            version: ProtocolVersion::CURRENT,
            capabilities: negotiated.clone(),
        }),
    )
    .await?;
    let auth =
        match tokio::time::timeout(counters.policy.timeouts.handshake, read_boxed(&mut stream))
            .await
            .map_err(|_| TunnelError::Timeout)??
        {
            Message::Auth(auth) => auth,
            _ => return Err(TunnelError::Authentication),
        };
    if !verify_token(&token, auth.token()) {
        return reject_authentication(&mut stream, source, &auth_failures, &counters, || {
            drop(handshake_guard.take());
            drop(admission.take());
        })
        .await;
    }
    drop(handshake_guard.take());
    drop(admission.take());
    let session_id = SessionId::generate()
        .map_err(|_| TunnelError::Configuration("operating system randomness unavailable"))?;
    if cancel.is_cancelled() {
        return Err(TunnelError::Cancelled);
    }
    tracing::info!(session_id = ?session_id, "authenticated server Session established");
    let context = Arc::new(SessionContext {
        id: session_id,
        principal,
        cancel: CancellationToken::new(),
        pending: Mutex::new(HashMap::new()),
        connection_admission: Arc::new(Semaphore::new(
            counters.policy.limits.active_connections_per_session,
        )),
        control_tx: Mutex::new(None),
        counters: counters.clone(),
    });
    if let Err(error) =
        SessionContext::register(&context, &sessions, counters.policy.limits.sessions).await
    {
        record_saturation(&counters);
        return Err(error);
    }
    let _session_guard = SessionGuard::new(context.clone(), sessions);
    write_boxed(&mut stream, &Message::AuthOk(AuthOk { session_id })).await?;
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (open_tx, mut open_rx) = mpsc::channel(counters.policy.limits.control_queue);
    *context.control_tx.lock().await = Some(open_tx.clone());
    let mut services = HashMap::<ServiceId, ServiceEntry>::new();
    let mut names: HashSet<String> = HashSet::new();
    let mut children = JoinSet::new();
    let idle_timeout = counters.policy.timeouts.control_idle;
    let idle = tokio::time::sleep(idle_timeout);
    tokio::pin!(idle);
    let mut idle_expired = false;
    let mut peer_drain_deadline: Option<u32> = None;
    let negotiated_drain = negotiated.has(CAPABILITY_DRAIN_DEADLINE);
    loop {
        tokio::select! {
            _ = context.cancel.cancelled() => break,
            _ = &mut idle => {
                idle_expired = true;
                tracing::debug!(category = "control_idle_timeout", "server Session idle timeout");
                break;
            },
            incoming = read_message(&mut reader) => {
                match incoming {
                    Ok(Message::RegisterService(register)) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        register_service(
                            register,
                            &context,
                            &open_tx,
                            &bind_policy,
                            &counters,
                            &negotiated,
                            &mut writer,
                            &mut services,
                            &mut names,
                            &mut children,
                        ).await?;
                    }
                    Ok(Message::UnregisterService(UnregisterService { service_id })) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        if let Some(entry) = services.remove(&service_id) {
                            entry.cancel.cancel();
                            names.remove(entry.name.as_str());
                            counters.binds.lock().unwrap_or_else(|p| p.into_inner()).retain(|(sid, id, _)| *sid != session_id || *id != service_id);
                            counters.services.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            remove_service_pending(&context, service_id).await;
                            tracing::info!(service_id = service_id.0, "Service listener removed");
                        }
                    }
                    Ok(Message::OpenReject(reject)) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        if let Some(entry) = context.pending.lock().await.remove(&reject.connection_id) {
                            counters.pending.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            drop(entry);
                        }
                    }
                    Ok(Message::Ping(Ping { nonce })) => {
                        idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        write_message(&mut writer, &Message::Pong(Pong { nonce })).await?;
                    }
                    Ok(Message::Drain(drain)) => {
                        peer_drain_deadline = Some(drain.deadline_ms);
                        break;
                    },
                    Ok(_) => return Err(TunnelError::Protocol(eggtunnel_proto::ProtocolError::UnexpectedMessage)),
                    Err(error) => return Err(error.into()),
                }
            }
            Some(message) = open_rx.recv() => {
                let draining = matches!(&message, Message::Drain(_));
                write_message(&mut writer, &message).await?;
                if draining { break; }
            }
            Some(result) = children.join_next(), if !children.is_empty() => {
                counters.record_join_result(&result);
            }
        }
    }
    // Negotiated Drain deadline (capability 2, ADR-0002): when the peer
    // asked us to drain, let owned service/relay tasks run up to
    // min(peer, local shutdown ceiling) before forced cancellation, so
    // in-flight relays can finish naturally. The peer value can only
    // shorten the local maximum, never extend it. Without the capability
    // (1.0 behavior) teardown stays immediate.
    if negotiated_drain && let Some(peer_ms) = peer_drain_deadline {
        let effective = std::time::Duration::from_millis(peer_ms as u64)
            .min(counters.policy.timeouts.shutdown_grace);
        if !effective.is_zero() {
            let _ = tokio::time::timeout(effective, async {
                while children.join_next().await.is_some() {}
            })
            .await;
        }
    }
    for entry in services.values() {
        entry.cancel.cancel();
    }
    children.abort_all();
    while children.join_next().await.is_some() {}
    remove_all_pending(&context).await;
    if idle_expired {
        Err(TunnelError::Timeout)
    } else {
        Ok(())
    }
}

/// Admit one `RegisterService` request: bound check, duplicate check, bind
/// authorization, listener bind, relay-task spawn, then `RegisterAck`.
///
/// The client `target` descriptor is client-owned bounded metadata. The server
/// never dials it, never selects it, and never rewrites the connector the
/// client actually uses.
#[allow(clippy::too_many_arguments)]
async fn register_service<W>(
    register: RegisterService,
    context: &Arc<SessionContext>,
    open_tx: &mpsc::Sender<Message>,
    bind_policy: &BindPolicy,
    counters: &Counters,
    negotiated: &Capabilities,
    writer: &mut W,
    services: &mut HashMap<ServiceId, ServiceEntry>,
    names: &mut HashSet<String>,
    children: &mut JoinSet<()>,
) -> Result<(), TunnelError>
where
    W: AsyncWrite + Unpin,
{
    let service_id = register.service_id;
    let max_services = bind_policy
        .max_services_per_session
        .min(counters.policy.limits.services_per_session);
    if services.len() >= max_services {
        record_saturation(counters);
        tracing::debug!(
            category = "service_admission",
            "Service registration rejected"
        );
        return write_registration_response(
            writer,
            service_id,
            REGISTRATION_ERROR_ADMISSION,
            negotiated,
        )
        .await;
    }
    if services.contains_key(&service_id) || names.contains(register.name.as_str()) {
        counters
            .rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::debug!(
            category = "duplicate_service",
            service_id = service_id.0,
            "Service registration rejected"
        );
        return write_registration_response(
            writer,
            service_id,
            REGISTRATION_ERROR_DUPLICATE,
            negotiated,
        )
        .await;
    }
    // The target descriptor is client-owned. The server uses it only as bounded registration metadata.
    let bind_addr = match bind_to_socket(&register.requested_bind, bind_policy) {
        Ok(addr) => addr,
        Err(_) => {
            counters
                .rejected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                category = "bind_authorization",
                service_id = service_id.0,
                "Service bind rejected"
            );
            return write_registration_response(
                writer,
                service_id,
                REGISTRATION_ERROR_BIND,
                negotiated,
            )
            .await;
        }
    };
    let listener = match TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(_) => {
            counters
                .rejected
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                category = "listener_bind",
                service_id = service_id.0,
                "Service listener bind failed"
            );
            return write_registration_response(
                writer,
                service_id,
                REGISTRATION_ERROR_LISTENER,
                negotiated,
            )
            .await;
        }
    };
    let effective = socket_to_effective(listener.local_addr()?);
    tracing::info!(service_id = service_id.0, service_name = register.name.as_str(), effective_address = %std::net::Ipv6Addr::from(effective.address), effective_port = effective.port, "Service listener bound");
    counters
        .binds
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((context.id, service_id, effective.clone()));
    let service_cancel = context.cancel.child_token();
    let name = register.name.clone();
    children.spawn(run_service(
        listener,
        service_id,
        context.clone(),
        open_tx.clone(),
        service_cancel.clone(),
        counters.clone(),
    ));
    names.insert(name.as_str().to_owned());
    services.insert(
        service_id,
        ServiceEntry {
            name,
            cancel: service_cancel,
        },
    );
    let registered_services = counters
        .services
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    counters
        .high_water_services
        .fetch_max(registered_services, std::sync::atomic::Ordering::Relaxed);
    write_message(
        writer,
        &Message::RegisterAck(RegisterAck {
            service_id,
            effective_bind: effective,
        }),
    )
    .await?;
    Ok(())
}

/// Answer one failed `RegisterService` request. With capability 1
/// negotiated the server sends the correlated `RegisterReject`
/// (ADR-0002); otherwise it keeps the legacy generic `Error` with the
/// same numeric code vocabulary.
async fn write_registration_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    service_id: ServiceId,
    code: u16,
    negotiated: &Capabilities,
) -> Result<(), TunnelError> {
    let diagnostic = BoundedDiagnostic::new("service registration rejected")?;
    if negotiated.has(CAPABILITY_REGISTER_REJECT) {
        write_message(
            writer,
            &Message::RegisterReject(RegisterReject {
                service_id,
                code,
                diagnostic,
            }),
        )
        .await?;
    } else {
        write_message(writer, &Message::Error(ErrorMessage { code, diagnostic })).await?;
    }
    Ok(())
}
