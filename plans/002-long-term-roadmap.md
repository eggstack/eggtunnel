# Eggtunnel Long-Term Implementation Roadmap

Status: execution roadmap for plans/000-long-term-specification.md

Terminology: plans/001-terminology-and-domain-model.md

This roadmap orders the work required to reach the intended Eggtunnel architecture while preserving a small, reviewable implementation at every stage. It is dependency-ordered, not calendar-ordered.

Each phase MUST leave the repository in a coherent state and MUST include implementation plans, tests, documentation, and closure evidence before a dependent phase is treated as available.

## Cross-phase execution rules

Every phase MUST:

1. preserve the thin Eggtunnel/Eggress ownership boundary;
2. avoid introducing a second general proxy stack;
3. keep externally supplied lengths/counts bounded before allocation;
4. keep all production channels bounded;
5. give each spawned task explicit cancellation and ownership;
6. keep secrets redacted from diagnostics and Debug/Display output;
7. retain a client-only embedding profile without unnecessary server/CLI/optional transport dependencies;
8. add focused failure/cancellation/restart tests for newly introduced lifecycle state;
9. update architecture/protocol/security documentation with implementation;
10. record exact executable closure evidence.

## Phase 0 — Repository and protocol foundation

### Objective

Create the Rust workspace, public crate boundaries, protocol types/codecs, feature structure, dependency guardrails, and deterministic test foundation required by all later runtime work.

### Deliverables

- Create workspace members:
  - crates/eggtunnel-proto;
  - crates/eggtunnel;
  - crates/eggtunnel-cli.
- Set workspace MSRV to Rust 1.89 or newer and forbid Eggtunnel-owned unsafe code.
- Establish Cargo feature policy for client, server, tls, quic, websocket, outbound-proxy, and later mtls.
- Implement bounded protocol preface and frame codec.
- Define explicit stable wire IDs for initial messages.
- Define typed SessionId, ServiceId, ConnectionId, ServiceName, bind/target DTOs, capability set, and error taxonomy.
- Define exact maximums for frame length and all variable-length protocol fields.
- Add property/unit tests for encode/decode, exact consumption, truncation, oversize, unknown version, and unknown message.
- Add an initial fuzz target or fuzz-ready decoder boundary if fuzz infrastructure is not yet justified.
- Add dependency-direction/feature guards sufficient to catch accidental optional transport leakage into the minimal client.
- Add protocol and architecture documentation.

### Dependencies

None.

### Exit criteria

- Workspace builds on the declared MSRV.
- Protocol crate has no socket/runtime ownership.
- Every initial control/data message round-trips deterministically.
- Oversized input is rejected before proportional allocation.
- Numeric wire IDs are explicit and tested.
- A client-only feature slice compiles without QUIC/WebSocket/server/CLI dependencies.
- No current implementation decision contradicts ADR-0001.

### Required tests

- protocol round trips;
- boundary lengths at max-1/max/max+1;
- malformed/truncated input;
- exact-consumption decode;
- unknown version/message;
- ID entropy/format helpers where applicable;
- secret redaction types;
- feature-slice compilation.

## Phase 1 — TCP/TLS reverse-tunnel product

### Objective

Deliver the first complete end-to-end reverse TCP tunnel over authenticated TLS using one persistent control connection and short-lived dial-back data connections.

### Deliverables

- Implement Server control listener/session acceptance.
- Implement Client outbound control connection.
- Implement TLS-before-protocol semantics for non-loopback operation.
- Implement bearer/service-token authentication with constant-time comparison.
- Implement ClientHello/ServerHello/capability negotiation.
- Implement multi-service registration and server-authoritative EffectiveBind.
- Implement external listener lifecycle.
- Implement bounded Pending Connection registry.
- Implement cryptographically random single-use ConnectionId allocation.
- Implement Open and DataHello flow.
- Pair external and reverse data streams atomically.
- Relay opaque bytes through eggress-relay.
- Implement client Target connector for TCP.
- Implement reconnect with bounded jittered backoff and service re-registration.
- Implement Ping/Pong liveness and idle policy.
- Expose async ClientHandle/ServerHandle shutdown and bounded snapshots.
- Add CLI client/server/config-test/version surface.
- Add end-to-end loopback integration tests.

### Dependencies

Phase 0 closed.

### Exit criteria

- One client registers multiple services.
- Concurrent external connections reach the correct local targets.
- Wrong-session, expired, duplicate, and replayed ConnectionIds fail closed.
- Local target refusal/timeout produces bounded deterministic errors and cleanup.
- Control reconnect creates a new Session and safely restores configured services.
- Half-close behavior is preserved by the relay path.
- Non-loopback configuration cannot silently run plaintext/unauthenticated.
- Client and server shutdown leave no owned pending/listener/task leaks in deterministic tests.

### Required tests

- authenticated handshake;
- invalid/missing token;
- multi-service registration;
- ephemeral and fixed external port policy;
- 1, N, and concurrent external connections;
- wrong-service/wrong-session DataHello;
- ConnectionId replay;
- pending expiry;
- target refusal/timeout;
- control drop during pending open;
- server restart;
- client restart;
- reconnect cancellation;
- half-close;
- graceful drain and forced shutdown.

## Phase 2 — Security, lifecycle, resource, and embedding hardening

### Objective

Turn the functional TCP/TLS product into a robust embeddable library with explicit resource ownership, mTLS option, application connectors, adversarial testing, and downstream-shaped integration.

### Deliverables

- Add explicit resource budget/admission model for sessions, services, pending connections, active relays, and transport tasks.
- Ensure admission uses owned permits/leases where practical.
- Add authentication throttling and bounded failure delay.
- Add optional mTLS identity profile.
- Add server policy object for bind addresses, port ranges, and per-principal ceilings.
- Add application Target connector boundary that can return an async duplex stream without a loopback TCP hop.
- Ensure public API does not require CLI/config files or expose transport implementation types unnecessarily.
- Add structured secret-free snapshots and termination reason taxonomy.
- Add adversarial cancellation tests at every handshake/pairing stage.
- Add repeated start/stop/reconnect leak checks.
- Add protocol fuzzing and/or corpus-based malformed input tests.
- Add downstream-shaped client-only compile/integration fixture modeled on CodeGG requirements.
- Add dependency-tree and binary-size informational evidence.

### Dependencies

Phase 1 closed.

### Exit criteria

- Capacity returns to baseline after every tested success/failure/cancel path.
- Saturation rejects new work rather than spawning or queueing without bound.
- mTLS and bearer-token profiles have explicit documented trust semantics.
- Public bind policy is fail-closed and independently tested from authentication.
- A downstream application can embed the client without a sidecar or process-global initialization.
- Direct application connector path works without a loopback TCP listener.
- No high/medium security or lifecycle finding remains unresolved.

### Required tests

- per-resource saturation and permit release;
- auth-rate limiting;
- bind-policy matrix;
- mTLS valid/invalid/revoked-or-untrusted certificate profile as supported;
- cancellation at accept/auth/register/open/data-pair/relay/drain stages;
- repeated reconnect/start-stop runs;
- downstream fixture;
- fuzz/corpus decoder tests;
- minimal feature dependency check.

## Phase 3 — Optional QUIC transport

### Objective

Add native QUIC multiplexing using Eggress's published QUIC transport without changing Eggtunnel service/session semantics.

### Deliverables

- Add quic feature with no default enablement.
- Adapt eggress-transport-quic to the Eggtunnel transport abstraction.
- Define/control one reserved QUIC bidirectional stream.
- Map each external TCP connection to one QUIC bidirectional stream.
- Carry bounded DataHello/preface before opaque relay bytes.
- Preserve Session/Service/Connection identity and authorization rules.
- Add QUIC transport-specific admission and stream ceilings.
- Implement QUIC reconnect/session replacement semantics.
- Verify certificate/SNI policy and prohibit insecure verification in production profiles.
- Add TCP/TLS-vs-QUIC protocol-equivalence tests.

### Dependencies

Phase 2 closed.

### Exit criteria

- Same service model and control semantics work over QUIC.
- Multiple concurrent tunneled TCP connections share one QUIC connection.
- Stream failure does not corrupt unrelated streams.
- QUIC connection replacement invalidates stale session correlations.
- Minimal TCP/TLS client remains free of QUIC dependencies when feature-disabled.
- No Eggtunnel custom stream multiplexer is introduced.

### Required tests

- multi-stream concurrency;
- stream reset isolation;
- connection loss/reconnect;
- stale stream/session rejection;
- stream-limit saturation;
- certificate/SNI verification;
- feature-off dependency/compile checks.

## Phase 4 — Restricted-network transports and outbound proxy traversal

### Objective

Add optional WebSocket/WSS and Eggress outbound-proxy traversal for environments where direct TCP/TLS or QUIC cannot be used.

### Deliverables

- Add websocket feature using Eggress's WebSocket stream adapter.
- Define whether WebSocket transports the control/data TCP-dialback model or a documented stream profile; avoid inventing an unbounded message mux.
- Add outbound-proxy feature using eggress-outbound.
- Support direct, HTTP CONNECT, SOCKS, and supported Eggress chains according to the published API.
- Preserve TLS/auth/session semantics end to end.
- Add configuration validation preventing ambiguous double-TLS or insecure downgrade combinations.
- Add proxy failure classification and reconnect policy.
- Document firewall/proxy deployment tradeoffs.

### Dependencies

Phase 2 closed. May proceed in parallel with Phase 3 after Phase 2 if adapters remain independent.

### Exit criteria

- Client can establish a tunnel through a supported outbound proxy without starting a local proxy listener.
- WSS profile authenticates and tunnels services with the same application model.
- Unsupported proxy/transport combinations fail explicitly.
- Optional features do not alter the minimal dependency slice.

### Required tests

- local HTTP CONNECT proxy fixture;
- local SOCKS5 proxy fixture;
- WSS control/data flow;
- proxy authentication failure;
- proxy disconnect/reconnect;
- feature-off dependency checks.

## Phase 5 — Distribution and downstream qualification

### Objective

Prepare Eggtunnel for dependable consumption by Eggstack applications and standalone deployment on desktops, servers, and SBC-class systems.

### Deliverables

- Finalize README, security model, protocol overview, embed guide, operations guide, and support matrix.
- Publish crates in dependency order when release policy is approved.
- Add release archives for supported targets.
- Reuse Eggstack shared installer/updater infrastructure when ready rather than duplicating updater logic.
- Qualify Linux x86_64/aarch64 and macOS x86_64/aarch64 as primary release targets.
- Add armv7/musl/Windows targets as dependency/toolchain evidence permits.
- Exercise Raspberry Pi / Le Potato-class Linux deployment assumptions.
- Add CodeGG-shaped client embedding qualification against the published crate surface.
- Add Eggchaos/Eggbench fault/performance regression integration when those tools have stable consumable interfaces.
- Establish semver/support policy for public Rust API and protocol compatibility.

### Dependencies

Phases 2 and the selected production transports intended for 0.1.

### Exit criteria

- Published/library surface matches documentation.
- Standalone binary install and client/server smoke tests work on supported release targets.
- Client-only downstream profile has a documented dependency/feature footprint.
- CodeGG-shaped embedding fixture uses public APIs only.
- Release artifacts carry provenance/checksum evidence according to Eggstack conventions.
- No documentation claims unsupported platform/transport behavior.

## Deferred roadmap candidates

The following do not belong in Phases 0-5 and require new evidence plus an ADR/subsystem roadmap:

- UDP/datagram services;
- listener persistence across disconnected clients;
- multi-server relay federation;
- dynamic DNS or hostname allocation;
- ACME/certificate automation;
- browser-facing ingress;
- service discovery;
- peer-to-peer NAT traversal/hole punching;
- bandwidth accounting/billing;
- pluggable authorization backends;
- custom multiplexing over TCP;
- persistent session database;
- HA/consensus.

## Dependency graph

Phase 0 protocol/workspace
    |
    v
Phase 1 TCP/TLS product
    |
    v
Phase 2 hardening/embedding
    |               |
    v               v
Phase 3 QUIC     Phase 4 WSS/proxy
    \               /
     \             /
      v           v
        Phase 5 distribution/downstream qualification

## Initial planning decomposition

The initial subsystem roadmap is:

- plans/subsystems/reverse-session-roadmap.md

Initial handoff plans are expected under:

- plans/implementation/reverse-session/

The planning registry determines which milestone is dependency-ready.
