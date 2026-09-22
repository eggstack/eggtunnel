# Reverse Session M004 — QUIC Transport

Status: closed

Planning baseline: 31458e83e543304d6b271898575bf2f6e98c7352

M003 is closed at the baseline above. The public target stream and resource lifecycle boundaries were reconciled before starting QUIC work.

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#9-milestone-m004--quic-transport

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability

## 1. Objective

Add an optional QUIC transport that carries the already-closed Eggtunnel Session/Service model over one QUIC connection using transport-native independent bidirectional streams.

This milestone MUST NOT redesign Service registration, authorization, ConnectionId semantics, Target connectors, resource ownership, or public application API.

## 2. Dependency readiness

Hard dependency: M003 strict closure.

Before execution:

- update baseline to M003 final reviewed head;
- inspect M003 public transport abstraction;
- verify the current published eggress-transport-quic version/API;
- confirm client-only TLS feature slice and its dependency baseline;
- record any transport-interface mismatch before changing code.

## 3. Invariants

- QUIC remains opt-in.
- Minimal client+TLS builds do not compile/link QUIC dependencies.
- Session/Service/Principal semantics are identical across TCP/TLS and QUIC.
- One external TCP connection maps to one independent QUIC bidirectional stream.
- No Eggtunnel custom stream mux is introduced.
- Server certificate verification is enabled in production.
- Insecure QUIC verification, if available for tests, is compile-time/test-only and cannot become a normal CLI default.
- Stream/task admission remains bounded.
- Failure of one data stream does not corrupt unrelated streams.
- Connection replacement creates a new Session generation and rejects stale stream work.

## 4. In scope

- quic Cargo feature;
- Eggress QUIC adapter;
- client/server QUIC endpoint configuration;
- one control stream;
- per-connection data streams;
- DataHello/preface over data stream;
- stream/connection cancellation;
- stream admission limits;
- QUIC TLS/server-name configuration;
- reconnect/session replacement;
- protocol-equivalence tests;
- docs/config support.

## 5. Out of scope

- HTTP/3 application routing;
- QUIC datagram/UDP tunneling;
- 0-RTT application semantics unless later separately reviewed;
- connection migration policy beyond what the underlying transport safely provides;
- custom congestion control;
- custom certificate verifier;
- custom mux;
- WebSocket/proxy work;
- changes to downstream authorization.

## 6. Eggress QUIC integration

Use eggress-transport-quic if its supported public surface still provides the required shape.

Expected useful primitives at planning time:

- QuicClient;
- QuicConnection;
- open_stream;
- accept_stream;
- QuicListener;
- server/client config with certificate verification;
- bounded stream limits.

Do not reach into Eggress private modules or vendor Quinn merely to bypass the public boundary.

If the upstream public adapter lacks a necessary stable operation, document the gap and choose the smallest solution:
1. propose/use an Eggress public API improvement; or
2. use Quinn directly behind Eggtunnel's adapter only if doing so is lower-risk and does not duplicate substantial Eggress logic.

Do not pull in eggress-embed.

## 7. QUIC session shape

One established QUIC connection represents one transport connection for a new Session.

After transport establishment:

1. client opens the designated control stream;
2. normal Eggtunnel hello/auth/session negotiation runs;
3. Services register exactly as in TCP/TLS;
4. Server accepts external TCP connection and creates Pending Connection;
5. Server sends Open on control stream;
6. Client opens a new QUIC bidirectional stream;
7. Client sends DataHello/session correlation;
8. Server validates/consumes pending entry;
9. eggress-relay joins external TCP stream to QUIC stream;
10. stream completion affects only that connection.

The exact direction for opening control/data streams must be fixed and documented to prevent ambiguous simultaneous-role behavior.

## 8. Flow control and admission

Map M003 resource budgets onto QUIC:

- max concurrent Session connections;
- max bidi data streams per Session;
- max pending Opens;
- max stream-handler tasks.

Underlying QUIC max_concurrent_bidi_streams is defense-in-depth, not the only Eggtunnel policy.

Do not respond to stream exhaustion by unbounded task/queue accumulation.

## 9. TLS and identity

QUIC's TLS configuration must align with M003 trust semantics:

- SNI/server name verified;
- configured roots or platform verifier documented;
- optional mTLS mapped consistently if supported by the adapter;
- certificate/key data redacted;
- no insecure verification in production defaults.

If mTLS cannot be expressed through the Eggress adapter without exposing internals, stop and document rather than silently weakening identity semantics.

## 10. Reconnect and failure

QUIC connection failure terminates the Session generation.

Client reconnect follows the existing bounded reconnect controller.

On replacement:

- old Services/listeners/pending correlations clean according to existing Session ownership;
- new SessionId is created;
- configured Services re-register;
- late streams from old connection cannot attach to new pending state.

Stream reset/close affects only its Data Path unless the failure is explicitly connection-level.

## 11. Ordered work packages

A. Feature and adapter boundary
- add quic feature;
- integrate Eggress QUIC;
- preserve feature-off dependency tree.

B. Control Session over QUIC
- connection setup;
- control stream role;
- existing handshake/auth/registration.

C. Data streams
- Open -> open_bi;
- DataHello;
- pairing;
- relay;
- stream reset isolation.

D. Lifecycle/limits
- connection replacement;
- cancellation;
- stream limits;
- repeated tests.

E. Docs/equivalence
- transport config;
- support matrix;
- TCP/TLS-vs-QUIC behavior tests.

## 12. Required tests

- QUIC handshake and service registration;
- two Services over one QUIC Session;
- many concurrent data streams;
- independent stream reset;
- wrong/stale DataHello;
- pending expiry;
- external half-close;
- connection loss during active streams;
- reconnect/re-registration;
- old-connection late stream rejection;
- stream-limit saturation;
- wrong server name/untrusted certificate;
- mTLS equivalence if supported;
- cancellation during connection/control/data stream open;
- feature-off dependency check.

Protocol-equivalence tests should run the same logical behavior suite against TCP/TLS and QUIC where practical.

## 13. Verification

Run full workspace verification plus:

- quic feature build/test;
- client+tls no-quic cargo tree;
- quic-only relevant feature slice;
- repeated multi-stream integration test;
- certificate negative tests.

Record exact commands and repetitions.

## 14. Documentation

Update:

- docs/ARCHITECTURE.md;
- docs/PROTOCOL.md only for transport preface details if needed;
- docs/SECURITY.md;
- docs/CONFIGURATION.md;
- docs/EMBEDDING.md;
- support matrix;
- subsystem roadmap/registry.

## 15. Acceptance criteria

- QUIC is an optional transport for the existing product model.
- Multiple tunneled TCP connections share one QUIC connection.
- Data streams are isolated.
- Session replacement/reconnect is generation-safe.
- Certificate verification is production-safe.
- stream/task limits are bounded.
- no custom mux exists.
- client+TLS feature slice remains QUIC-free.
- all required tests/verification pass.

## 16. Stop conditions

Stop and report if:

- M003 transport abstraction cannot represent QUIC without changing public Service/Target semantics;
- Eggress QUIC API requires insecure verification for production;
- mTLS semantics would silently diverge from TCP/TLS;
- implementing QUIC requires a custom multiplexing layer;
- a broad Eggress runtime dependency would leak into all builds.

## 17. Closure evidence required

- refreshed baseline/final head;
- Eggress QUIC version/API used;
- dependency-tree feature-off evidence;
- logical TCP/TLS-vs-QUIC equivalence matrix;
- stream-isolation/reconnect evidence;
- TLS verification evidence;
- resource/saturation evidence;
- exact verification commands/results;
- residual limitations/disposition.
