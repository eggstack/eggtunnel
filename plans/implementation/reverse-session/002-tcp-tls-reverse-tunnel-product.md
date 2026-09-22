# Reverse Session M002 — TCP/TLS Reverse-Tunnel Product

Status: blocked — requires accepted M001 closure and baseline refresh before execution

Planning baseline: 82fe121c208a6dd4c06acaaf1ab3b5ba03d7a847

Before this plan becomes ready, replace the planning baseline with the accepted M001 reviewed head and reconcile every current-implementation statement against the repository.

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#7-milestone-m002--tcptls-reverse-tunnel-product

Long-term requirements:

- plans/000-long-term-specification.md#5-canonical-deployment-model
- plans/000-long-term-specification.md#6-canonical-connection-model
- plans/000-long-term-specification.md#11-service-model
- plans/000-long-term-specification.md#12-authentication-and-authorization
- plans/000-long-term-specification.md#14-reconnect-and-recovery
- plans/000-long-term-specification.md#15-cancellation-and-shutdown
- plans/000-long-term-specification.md#16-egress-integration-boundary

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability

## 1. Objective

Deliver the first complete functional Eggtunnel product: an authenticated private-side Client establishes a persistent TLS control Session to a reachable Server, registers multiple TCP Services, and external peers connect through server-owned listeners to client-side TCP Targets.

The data plane uses one short-lived outbound TLS Data Connection per accepted external connection and eggress-relay for protocol-neutral byte copying.

## 2. Dependency readiness

Hard dependency: M001 must be strictly closed.

Before implementation:

- read M001 closure;
- replace this plan's baseline with M001 final reviewed head;
- verify protocol types/message IDs/features as actually implemented;
- verify current published Eggress crate versions/API;
- amend only implementation details necessary to fit the closed M001 contract.

Do not execute this plan against the planning-only baseline.

## 3. Invariants that must not regress

- TLS completes before any bearer credential is transmitted on non-loopback transport.
- Server is authoritative for EffectiveBind.
- One Pending Connection maps to at most one Data Path.
- ConnectionId is single-use, session-bound, service-bound, and expiring.
- Wrong-session/stale/replayed DataHello cannot attach to a new Session.
- No unbounded production channel or pending table.
- Application bytes remain opaque after DataHello.
- Relay semantics use eggress-relay rather than a duplicate copy loop unless an upstream defect is proven and documented.
- No custom stream multiplexing.
- Client library remains embeddable and does not initialize global runtime/logging.
- QUIC/WebSocket/outbound proxy remain out of scope.

## 4. In scope

- TcpTls transport implementation;
- client/server lifecycle;
- TLS configuration;
- token authentication;
- capability negotiation using M001 protocol;
- service registration/unregistration;
- server bind policy foundation;
- external listener ownership;
- pending correlation table;
- Open/OpenReject/DataHello;
- local TCP Target connector;
- eggress-relay integration;
- reconnect/backoff;
- re-registration;
- Ping/Pong/idle liveness;
- bounded current-state snapshots;
- async shutdown/drain;
- initial CLI client/server/config-test/version;
- end-to-end tests;
- initial user/operator docs.

## 5. Out of scope

- mTLS;
- complex per-principal account store;
- QUIC;
- WebSocket/WSS;
- outbound proxy chains;
- UDP;
- persistence/database;
- direct in-process application Target connector;
- release packaging;
- custom mux.

## 6. Required production architecture

### Server

Implement an owning Server/ServerHandle split.

Server owns:

- control listener;
- authenticated Session registry;
- per-session Service registry;
- external listeners;
- Pending Connection registry;
- data-connection accept path;
- relay tasks;
- cancellation/drain state.

Handle exposes bounded status/snapshot and shutdown.

### Client

Implement Client/ClientHandle or equivalent.

Client owns:

- server endpoint/TLS/auth configuration;
- configured Services;
- current Session generation;
- control reader/writer ownership;
- reconnect controller;
- per-Open target/data tasks;
- cancellation/drain state.

### Task ownership

Every accept loop, control reader/writer, listener task, target/data connector task, relay task, and reconnect task must have explicit ownership.

Prefer JoinSet or equivalent scoped task ownership. Do not spawn detached tasks with no join/cancel route.

## 7. TLS

Verify eggress-transport-tls public API against the actual version.

Prefer reuse if it provides:

- client connect over an established TCP stream;
- server accept over an established TCP stream;
- explicit rustls configs;
- cancellation can wrap the handshake;
- no full Eggress runtime dependency.

If that public surface is unsuitable, using rustls/tokio-rustls directly is allowed. Record why in closure. Do not adopt eggress-embed merely for TLS.

Requirements:

- platform/WebPKI verification policy documented;
- SNI/server-name validation;
- no production insecure-verifier default;
- certificate/key material redacted;
- TLS handshake deadline.

## 8. Authentication

Initial profile: bearer/service token inside the encrypted control stream.

Requirements:

- both sides validate configuration before binding/dialing;
- constant-time token comparison;
- bounded token length;
- tokens never appear in Debug/Display/tracing/snapshot;
- missing/invalid credentials fail closed;
- authentication failure produces bounded generic wire error;
- no service registration before authentication.

M003 owns advanced rate limiting/mTLS; M002 should still avoid obvious brute-force amplification and unlimited auth tasks.

## 9. Service registration

RegisterService carries the M001 service name/requested bind/target-facing metadata required by the protocol.

Server validates:

- authenticated Session;
- service count ceiling;
- unique service name/ID within Session;
- bind address/port policy;
- listener availability.

Server binds and returns EffectiveBind.

Port zero is supported for ephemeral assignment when policy allows.

If registration fails, no orphan listener/service record remains.

UnregisterService closes/removes the service listener and its pending work according to documented semantics.

## 10. Pending correlation

Use a bounded map keyed by ConnectionId.

On external accept:

1. acquire pending/active admission;
2. create random ConnectionId;
3. store Pending Connection with SessionId, ServiceId, deadline, external stream, owned permits;
4. send Open;
5. wait for matching DataHello or expiry/cancellation.

On DataHello:

- validate SessionId;
- locate ConnectionId;
- compare/bind to expected Service;
- atomically remove/consume entry;
- reject duplicate/replay;
- pair streams exactly once.

Expiry closes external stream and releases all owned capacity.

A failed Open send removes the pending entry immediately.

## 11. Relay

Use eggress-relay as the canonical relay engine.

Choose and document half-close policy. Default SHOULD preserve drain/half-close semantics appropriate for generic TCP.

Record RelayReport/byte counts into bounded counters if the upstream API provides them.

Relay failure must classify direction/termination without logging payload bytes.

## 12. Client Open handling

For each valid Open:

- confirm Session generation is current;
- enforce local concurrent-open ceiling;
- resolve configured Service/Target;
- connect local TcpTarget with timeout;
- open outbound TCP data connection;
- complete TLS;
- send DataHello;
- enter relay between local target and server data stream.

If target connect fails, send OpenReject where protocol/state permits and cleanly release resources.

Never open arbitrary client-side targets specified by the Server. Target comes from client-owned service configuration/registration state.

## 13. Reconnect

Implement bounded exponential backoff with jitter and cancellation.

Requirements:

- reset delay after a stable successful Session according to documented rule;
- cap maximum delay;
- cancellation interrupts sleep/dial/TLS;
- reconnect creates a new SessionId;
- configured Services re-register only after successful auth;
- old Pending Connection state is not restored;
- stale Open from old Session cannot launch a target connection.

## 14. Liveness

Implement bounded Ping/Pong or equivalent control liveness.

Distinguish:

- transport idle;
- protocol liveness;
- service Target health.

M002 does not need active probing of local services.

Avoid a complex health score.

## 15. CLI

Implement at least:

- eggtunnel server --config <file>;
- eggtunnel client --config <file>;
- eggtunnel check --config <file>;
- eggtunnel version.

Exact flag spelling may follow existing Eggstack conventions.

Configuration:

- TOML;
- secrets may reference environment/file rather than requiring plaintext;
- config validation must not bind sockets for check mode unless explicitly documented;
- JSON diagnostic output should be supported for check/state where practical.

## 16. Ordered work packages

A. Transport/auth/session skeleton
- TCP dial/listen;
- TLS;
- hello/version/capability;
- auth;
- Session ownership.

B. Service/listener lifecycle
- registration;
- bind approval;
- external listeners;
- unregister/session cleanup.

C. Pending correlation/data path
- ConnectionId;
- Open/DataHello;
- atomic consume;
- eggress-relay.

D. Client reconnect/liveness
- backoff+jitter;
- re-registration;
- Ping/Pong;
- stale generation rejection.

E. CLI/docs/end-to-end qualification
- config;
- commands;
- loopback integration tests;
- docs.

## 17. Required failure/cancellation cases

Test cancellation/failure during:

- TCP control dial;
- TLS handshake;
- auth;
- registration bind;
- control send;
- external accept pending wait;
- target connect;
- data TCP dial;
- data TLS handshake;
- DataHello pairing;
- active relay;
- reconnect backoff;
- drain.

Every path must release owned table entries, listener/task ownership, and permits.

## 18. Required tests

Focused:
- config validation;
- TLS policy;
- auth success/failure/redaction;
- service duplicate/limit/bind denial;
- ConnectionId allocation/expiry/replay;
- wrong SessionId/DataHello;
- reconnect backoff;
- snapshot bounds.

End-to-end:
- one service echo;
- two services routed correctly;
- concurrent external connections;
- external half-close with response drain;
- local target refusal;
- target timeout;
- control disconnect while pending;
- data disconnect before pair;
- client reconnect/re-register;
- server restart/client reconnect;
- graceful shutdown;
- forced shutdown.

Repeated:
- start/stop cycles;
- reconnect cycles;
- pending expiry cycles;
- concurrent connection burst within configured limits.

## 19. Required verification

Use repository-equivalent commands, including at minimum:

cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

Also verify:

- client-only TLS feature slice;
- server+TLS slice;
- CLI slice;
- cargo tree proving QUIC/WebSocket/outbound-proxy are absent from client-only TLS.

## 20. Documentation updates

- README quick start;
- docs/ARCHITECTURE.md;
- docs/PROTOCOL.md;
- docs/SECURITY.md;
- docs/CONFIGURATION.md;
- docs/EMBEDDING.md;
- subsystem roadmap/registry status.

## 21. Acceptance criteria

- authenticated TLS Session works end to end;
- multiple Services register under one Session;
- external peer reaches correct private Target;
- concurrent connections are isolated;
- ConnectionId replay/stale/wrong-session cases fail closed;
- target errors clean up deterministically;
- reconnect creates new Session and restores configured Services;
- half-close semantics are correct;
- shutdown leaves no known owned runtime state behind;
- minimal client does not pull optional transports;
- required tests/verification pass.

## 22. Stop conditions

Stop and report if:

- M001 protocol cannot express the required flow without a semantic redesign;
- Eggress relay/TLS public APIs require broad runtime adoption or incompatible semantics;
- secure listener policy needs a persistent identity/account database;
- implementation begins requiring a custom mux;
- a high-severity race in Session/Pending ownership cannot be resolved within this milestone.

## 23. Closure evidence required

- refreshed execution baseline;
- implementation/final reviewed commits;
- end-to-end topology and test matrix;
- ConnectionId replay/expiry evidence;
- reconnect/session-generation evidence;
- task/pending/listener cleanup evidence;
- cargo feature/dependency evidence;
- TLS/auth/bind-policy review;
- exact verification commands/results;
- known limitations;
- closure disposition.
