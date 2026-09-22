# ADR-0001 — Reverse Session Architecture and Eggress Boundary

Status: accepted

Date: 2026-09-22

## Context

Eggtunnel is intended to provide a lightweight reverse-tunnel capability that can run standalone or be embedded by downstream applications such as CodeGG.

The Eggstack ecosystem already contains Eggress, whose published Rust crates provide generic bidirectional relay, boxed async streams, outbound proxy chaining, TLS composition, WebSocket tunneling, QUIC connections/streams, and a pproxy-compatible reverse-proxy implementation.

Synvoid contains a broader QUIC/WireGuard/TUN/mesh tunnel subsystem with useful session, admission, reconnect, and health patterns, but its internal crates are not a supported external dependency surface and its scope is substantially larger than Eggtunnel requires.

i2pr contains strong runtime-boundary, resource-budget, bounded-channel, and lifecycle design patterns, but its transport machinery is I2P-specific and the repository currently has no selected license.

The principal architecture question is whether Eggtunnel should:

1. reuse Eggress narrowly and own only reverse-session semantics;
2. wrap Eggress's pproxy-compatible reverse protocol as the product;
3. build an independent proxy/tunnel stack;
4. import Synvoid/i2pr tunnel/runtime components.

A second question is whether the baseline should implement a custom multiplexing protocol over one TCP connection or instead use a persistent control connection plus separately established reverse data connections.

## Forces and constraints

- The library must remain lightweight enough for downstream embedding.
- Generic networking machinery should not be duplicated across Eggstack.
- The first product must be easy to reason about and harden.
- Public listener ownership and connection correlation require semantics not provided by a generic relay alone.
- Eggress's current pproxy-compatible reverse protocol uses one control connection per forwarded session and therefore does not directly model one persistent multi-service tunnel session.
- QUIC already provides native independent bidirectional streams, making a second application-level mux questionable.
- Downstream consumers should not be forced to compile QUIC/WebSocket/server/CLI code when they need only a client over TLS.
- Non-loopback use must be encrypted and authenticated by default.

## Decision

Eggtunnel will be a thin reverse-session layer over narrowly selected published Eggress primitives.

Eggtunnel owns:

- native Eggtunnel protocol/version negotiation;
- authenticated Session lifecycle;
- multi-Service registration;
- server-authoritative external listener allocation;
- Pending Connection registry;
- ConnectionId generation, expiry, and single-use correlation;
- Open/DataHello orchestration;
- reconnect and registration restoration;
- service/bind/admission policy;
- bounded snapshots and termination diagnostics;
- embedding API and process-neutral lifecycle.

Eggress owns or supplies, where its public API is suitable:

- protocol-neutral byte relay through eggress-relay;
- common async stream representation where needed through eggress-core;
- TLS stream composition through eggress-transport-tls;
- optional QUIC connections/streams through eggress-transport-quic;
- optional WebSocket stream adaptation through eggress-protocol-websocket;
- optional outbound proxy traversal through eggress-outbound.

Eggtunnel will not depend on eggress-embed merely to access lower-level primitives.

Eggress's pproxy-compatible reverse protocol is reference/compatibility material, not Eggtunnel's native protocol.

## Baseline TCP/TLS data architecture

The initial transport uses:

- one persistent outbound TLS control connection per Session;
- one independent outbound TLS Data Connection per accepted external TCP connection.

Flow:

1. Client establishes TLS and authenticates a control Session.
2. Client registers Services.
3. Server accepts an External Connection.
4. Server creates a random short-lived ConnectionId and Pending Connection entry.
5. Server sends Open(service_id, connection_id).
6. Client connects to the local Target.
7. Client opens a new outbound TLS Data Connection.
8. Client sends DataHello(session_id, connection_id).
9. Server atomically consumes and pairs the pending external stream with the Data Connection.
10. eggress-relay copies opaque bytes bidirectionally.

This architecture deliberately avoids a custom TCP stream-multiplexing layer.

## QUIC architecture

QUIC is optional and additive.

When enabled:

- one QUIC connection represents the Session transport;
- one bidirectional stream carries control messages;
- each external TCP connection maps to one independent QUIC bidirectional stream;
- each data stream carries a bounded Eggtunnel data preface before opaque relay bytes.

Eggtunnel will use transport-native QUIC multiplexing rather than maintaining a second custom mux.

## WebSocket architecture

WebSocket/WSS is optional and exists for restricted-network compatibility.

It must preserve Eggtunnel Session/Service semantics and reuse Eggress's stream adapter where practical.

No unbounded message-oriented multiplexing layer is authorized by this ADR.

## Synvoid disposition

Synvoid is an architecture reference only.

Reusable concepts include:

- Hello/HelloAck negotiation;
- bounded length-prefixed binary control frames;
- per-session registries;
- semaphore-based admission;
- authentication throttling;
- keepalive/RTT observation;
- jittered reconnect;
- one QUIC stream per logical connection.

Do not import:

- TUN/TAP;
- WireGuard;
- mesh/distributed state;
- VPN routing;
- broad health scoring;
- unsupported internal Synvoid crates.

## i2pr disposition

i2pr is an architecture reference only.

Reusable concepts include:

- protocol/policy separation from runtime I/O;
- explicit task ownership;
- cancellation-first teardown;
- bounded channels;
- resource budgets/owned leases;
- typed lifecycle/termination;
- exact-consumption hostile-input handling.

No i2pr production dependency or copied implementation is authorized by this ADR.

## Public API consequence

The Eggtunnel library should expose transport-neutral Client/Server/Session/Service concepts.

Quinn, Rustls, Tungstenite, and similar concrete implementation types should remain internal unless a later ADR deliberately promotes them.

The embedding API must not require:

- global runtime installation;
- global tracing setup;
- CLI parsing;
- config files;
- sidecar processes.

## Dependency consequence

The minimal client feature slice should compile without:

- server listener code;
- eggtunnel-cli;
- QUIC;
- WebSocket;
- outbound proxy support;
- full eggress runtime/embed machinery.

Every milestone that changes dependencies must verify that this remains true.

## Security consequence

- TLS is required for ordinary non-loopback baseline operation.
- Authentication occurs only after transport security is established.
- Authentication does not imply arbitrary bind authorization.
- ConnectionId is a short-lived correlation capability, not a long-lived credential.
- Public bind/listener policy remains server-authoritative.
- Insecure certificate verification may exist only in explicit test/development-only configuration and must not be a normal production path.

## Alternatives rejected

### Use Eggress reverse protocol directly as Eggtunnel

Rejected because the current pproxy-compatible model is connection-oriented rather than a persistent multi-service Session protocol. Adapting it into the product would either constrain Eggtunnel's model or progressively fork compatibility code into a second semantic role.

### Build a fully independent proxy/tunnel stack

Rejected because it duplicates generic relay, TLS, QUIC, WebSocket, and proxy-chain machinery already maintained in Eggress and directly conflicts with the lightweight objective.

### Depend on Synvoid tunnel crates

Rejected because the subsystem is substantially broader than required and Synvoid currently documents most internal crates as unsupported for external consumption.

### Depend on i2pr runtime/resource crates

Rejected because the product domain is unrelated, most machinery is I2P-specific, and the repository has no selected license. Architecture patterns may be independently implemented.

### Custom TCP multiplexing from the first release

Rejected because separate reverse data connections are simpler and easier to harden, while QUIC provides native multiplexing for the transport profile that benefits from it.

## Compatibility implications

This ADR defines Eggtunnel's native protocol as distinct from pproxy reverse compatibility.

No promise of wire compatibility with pproxy, rathole, bore, FRP, Chisel, Synvoid, or i2pr is made.

Future wire compatibility changes require version/capability handling and may require a superseding ADR.

## Affected planning documents

- plans/000-long-term-specification.md
- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md
- all reverse-session implementation plans

## Review trigger

Revisit this ADR only if concrete implementation evidence shows one of the following:

- Eggress published primitives cannot support the intended boundary without unacceptable dependency weight or API leakage;
- separate baseline Data Connections create an operational problem large enough to justify a custom mux;
- a supported external Synvoid/i2pr crate boundary appears and materially reduces ownership;
- downstream consumers require a transport model incompatible with the current Session abstraction.

Any such change requires a new ADR; do not silently rewrite this decision.
