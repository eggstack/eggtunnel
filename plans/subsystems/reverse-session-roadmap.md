# Reverse Session Subsystem Roadmap

Status: active — M002 active; M003-M006 dependency-blocked

Canonical references:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

## 1. Purpose and ownership boundary

This subsystem owns Eggtunnel's native reverse-session protocol and the client/server runtime that turns a private-side outbound connection into server-owned external TCP listeners.

It owns:

- protocol framing/version/capability negotiation;
- Session lifecycle;
- authentication handoff and Principal attachment;
- Service registration and effective bind assignment;
- Pending Connection correlation;
- ConnectionId generation/expiry/single-use consumption;
- Open/DataHello orchestration;
- target-connector boundary;
- relay orchestration;
- reconnect and registration restoration;
- admission/resource ceilings;
- shutdown/drain;
- transport adapters;
- secret-free snapshots/diagnostics;
- downstream embedding boundary.

It consumes generic networking primitives from Eggress where the supported published API fits.

It does not own:

- general forward proxy behavior;
- HTTP ingress routing;
- VPN/TUN/WireGuard;
- application authorization inside downstream services;
- remote execution;
- distributed scheduling;
- service discovery;
- ACME;
- mesh/federation;
- peer-to-peer NAT traversal.

## 2. Subsystem invariants

### Protocol

- Every frame length is bounded before allocation.
- Every variable-length field has a documented maximum.
- Wire message IDs are explicit and stable.
- Unknown versions/messages fail deterministically.
- Decoders consume exactly one intended frame and do not silently accept trailing protocol bytes.
- Secrets are never serialized into Debug/Display diagnostics.
- SessionId, ServiceId, ConnectionId, and Principal identity remain distinct.

### Correlation

- ConnectionId has at least 128 bits of unpredictable entropy.
- ConnectionId is server-generated.
- ConnectionId is bound to exactly one Session and Service.
- ConnectionId expires within a bounded interval.
- ConnectionId is atomically consumed at most once.
- Wrong-session, stale, duplicate, replayed, or unknown ConnectionIds fail closed.
- Pending state cannot outlive its Session.

### Listener and authorization

- Server is authoritative for EffectiveBind.
- Authentication success does not authorize arbitrary listener creation.
- Non-loopback exposure requires explicit secure policy.
- Listener count and active connection count are bounded.
- Session teardown releases session-owned listeners unless a later accepted policy explicitly changes ownership.

### Runtime

- No production unbounded channel.
- No detached ownerless production task.
- Cancellation interrupts connect/handshake/wait paths.
- Shutdown stops admission before draining.
- Capacity returns to baseline after failed/cancelled operations.
- Libraries do not install a runtime or global tracing subscriber.

### Embedding and dependencies

- Minimal client remains independent of CLI, server, QUIC, WebSocket, and outbound-proxy dependencies.
- Eggtunnel does not depend on eggress-embed for lower-level behavior.
- Synvoid and i2pr remain reference-only.
- Public API does not unnecessarily expose transport implementation types.

## 3. Current state

At the initial planning baseline the repository contained planning documents only. M001 is now closed with the Rust workspace and bounded protocol foundation. M002 is active: the TCP/TLS client/server runtime, CLI, and first product documentation are implemented in the working tree, with acceptance and closure evidence still in progress.

The current Eggress integration baseline is 1.0.8. The TLS, relay, and core stream APIs were inspected before adding dependencies.

## 4. Target architecture

Standalone or embedded client:

Client configuration / application owner
    |
    v
Eggtunnel Client
    |-- Session state
    |-- Service declarations
    |-- Target connectors
    |-- reconnect/backoff
    |
    +---- outbound secure transport ----> Eggtunnel Server
                                         |-- auth/policy
                                         |-- Session registry
                                         |-- Service registry
                                         |-- external listeners
                                         |-- Pending Connection table
                                         |
external peer -------------------------->+-- listener
                                            |
                                            +-- Open(ConnectionId)
                                            |
Client opens Data Path -------------------->+-- atomic pair
                                                |
                                                v
                                           eggress-relay
                                                |
                                                v
                                           opaque bytes

QUIC replaces separate transport connections with native independent streams but does not change the Session/Service model.

## 5. Dependency graph

M001 repository + protocol foundation
    |
    v
M002 TCP/TLS reverse-tunnel product
    |
    v
M003 security/lifecycle/resource/embedding hardening
    |                       |
    v                       v
M004 QUIC transport     M005 WSS/outbound-proxy
    \                       /
     \                     /
      v                   v
       M006 distribution/downstream qualification

M004 and M005 have a hard dependency on M003 but only soft/interface dependencies on each other.

M006 depends on M003 plus whichever optional transport profiles are declared part of the first published support matrix.

## 6. Milestone M001 — Repository and protocol foundation

Status: closed

Implementation plan:

- plans/implementation/reverse-session/001-repository-and-protocol-foundation.md

Primary class: infrastructure / invariant

### Objective

Create the compilable workspace and bounded runtime-neutral native protocol on which all later session/runtime behavior depends.

### Deliverables

- Cargo workspace/crate skeleton;
- MSRV/edition/license/lint policy;
- feature matrix skeleton;
- protocol preface/frame codec;
- explicit message IDs;
- typed IDs and bounded names/specs;
- initial message set;
- exact error taxonomy;
- deterministic tests;
- dependency/feature guards;
- architecture/protocol docs.

### Exit conditions

- M001 implementation plan acceptance criteria pass;
- closure record accepted;
- no unresolved high/medium finding;
- M002 can implement runtime behavior without redesigning protocol identity/framing.

## 7. Milestone M002 — TCP/TLS reverse-tunnel product

Status: active

Implementation plan:

- plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md

Primary class: capability

### Objective

Deliver the complete functional TCP/TLS reverse tunnel from external peer to private-side TCP Target.

### Deliverables

- client/server runtime;
- TLS-first control/data transport;
- authentication;
- service registration;
- listener binding;
- pending correlation;
- Open/DataHello;
- eggress-relay integration;
- reconnect/re-registration;
- liveness;
- CLI baseline;
- end-to-end tests.

### Exit conditions

- multiple services and concurrent connections work;
- correlation/replay invariants hold;
- reconnect and shutdown are deterministic;
- public exposure fails closed when insecure;
- M003 can harden rather than redesign the product.

## 8. Milestone M003 — Security, lifecycle, resource, and embedding hardening

Status: blocked on M002 closure

Implementation plan:

- plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md

Primary class: invariant / capability

### Objective

Close resource, security, cancellation, lifecycle, and downstream-embedding gaps before optional transports widen the state space.

### Deliverables

- owned resource budgets/permits;
- per-principal bind/connection policy;
- auth throttling;
- optional mTLS;
- direct application Target connector;
- structured snapshots/termination reasons;
- adversarial lifecycle tests;
- fuzzing/corpus hardening;
- downstream-shaped client-only fixture;
- dependency/footprint evidence.

### Exit conditions

- capacity/leak invariants pass repeated failure/cancel paths;
- embedding API is stable enough for transport adapters/downstream use;
- no high/medium security finding remains;
- M004/M005 may add transport adapters without reopening session ownership.

## 9. Milestone M004 — QUIC transport

Status: blocked on M003 closure

Implementation plan:

- plans/implementation/reverse-session/004-quic-transport.md

Primary class: capability

### Objective

Map the closed Eggtunnel Session/Service model onto Eggress QUIC connections and independent bidirectional streams.

### Exit conditions

- protocol-equivalent behavior;
- concurrent stream isolation;
- reconnect/session-generation correctness;
- certificate verification;
- no custom mux;
- QUIC stays absent from minimal builds.

## 10. Milestone M005 — Restricted-network transports and outbound proxy traversal

Status: blocked on M003 closure

Implementation plan:

- plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md

Primary class: capability

### Objective

Add WSS and Eggress outbound-chain traversal without changing session semantics or dependency defaults.

### Exit conditions

- supported proxy fixtures work;
- WSS product path works;
- unsupported/downgrade combinations fail explicitly;
- minimal build remains clean.

## 11. Milestone M006 — Distribution and downstream qualification

Status: blocked on M003 and selected transport closure

Implementation plan:

- plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md

Primary class: capability / polish

### Objective

Finalize public documentation, release artifacts, supported target matrix, crates publication, and real downstream-shaped qualification.

### Exit conditions

- public crate API and support matrix are truthful;
- release targets pass install/smoke evidence;
- CodeGG-shaped embedding consumes published public APIs;
- installer/updater duplication is avoided;
- semver/protocol support policy is documented.

## 12. Security considerations across milestones

M001:
- hostile input bounds;
- secret-safe types;
- protocol downgrade/unknown-version behavior.

M002:
- TLS-before-auth;
- token comparison;
- bind policy;
- correlation replay;
- pending exhaustion;
- target SSRF/local target policy is client-owned and explicit.

M003:
- authentication throttling;
- mTLS;
- admission/permit correctness;
- cancellation/resource release;
- fuzzing;
- downstream authority boundary.

M004:
- certificate verification/SNI;
- stream admission;
- connection replacement.

M005:
- proxy credential redaction;
- WSS origin/non-browser semantics;
- downgrade/double-TLS validation.

M006:
- artifact provenance;
- release dependency/audit evidence;
- support-claim truthfulness.

## 13. Protocol and compatibility concerns

M001 freezes the first draft wire identifiers and versioning model.

Before 0.1, breaking protocol changes are allowed but MUST be explicit and tested.

After the first published compatibility commitment:

- existing wire IDs are never silently reassigned;
- incompatible semantic changes require protocol version/capability behavior;
- compatibility documentation names supported peer versions.

No compatibility with third-party reverse-tunnel protocols is implied.

## 14. Storage and persistence

The initial product SHOULD avoid a database.

Configuration is process/application owned.

Session, pending, listener, and active-connection state is in memory and generation-scoped.

If durable server-side registrations or account state become necessary, that is a separate subsystem/ADR because it changes recovery and authority semantics.

## 15. Observability

Snapshots should expose only bounded current state and counters.

Do not accumulate unbounded event history in the runtime.

The CLI may render snapshots as human-readable text and JSON.

Libraries emit tracing events without installing subscribers.

## 16. Performance and footprint

Performance work is secondary to correctness until M003.

Useful informational measurements:

- steady-state relay throughput;
- per-connection allocation/task count;
- control-frame overhead;
- client-only dependency tree;
- release binary size;
- QUIC vs TCP/TLS connection-churn behavior.

Do not add a buffer pool, zero-copy abstraction, custom allocator, or bespoke scheduler without measurement.

## 17. Known risks

### Dependency leakage

Using broad Eggress facades could pull unwanted protocols/transports into downstream builds.

Mitigation: depend on narrow crates, disable defaults where appropriate, and add feature-slice evidence in M001 onward.

### TLS abstraction mismatch

eggress-transport-tls may not expose the exact client/server composition shape Eggtunnel needs.

Mitigation: M002 may use direct rustls/tokio-rustls if the narrow Eggress public API cannot provide the required semantics without layering problems. Such a decision must be documented; do not import eggress-embed merely for uniformity.

### TCP data-connection churn

One TLS connection per external connection costs handshakes.

Mitigation: accept as the simple baseline; QUIC M004 provides multiplexed streams. Do not preemptively add a custom mux.

### Stale session races

Reconnect plus delayed DataHello/Open can accidentally bind old work to a new session if identity is weak.

Mitigation: SessionId generation binding plus single-use ConnectionId tests from M002.

### Downstream API overfitting

Designing solely around CodeGG could leak CodeGG concepts into Eggtunnel.

Mitigation: keep Target connector generic and CodeGG only as a qualification fixture.

## 18. Deferred work

Explicitly deferred until after M006 or a new ADR:

- UDP;
- custom TCP mux;
- relay clustering/federation;
- persistent account database;
- hostname service;
- ingress HTTP routing;
- ACME;
- WireGuard/TUN;
- P2P hole punching;
- traffic inspection;
- remote execution semantics;
- bandwidth billing.

## 19. Status table

| Milestone | Status | Plan | Closure | Blocker |
|---|---|---|---|---|
| M001 repository/protocol foundation | closed | plans/implementation/reverse-session/001-repository-and-protocol-foundation.md | plans/closure/reverse-session/001-status.md | — |
| M002 TCP/TLS product | active | plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md | — | Implementation and closure evidence in progress |
| M003 hardening/embedding | blocked | plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md | — | M002 closure |
| M004 QUIC | blocked | plans/implementation/reverse-session/004-quic-transport.md | — | M003 closure |
| M005 WSS/proxy traversal | blocked | plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md | — | M003 closure |
| M006 distribution/downstream | blocked | plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md | — | M003 + selected transport closures |
