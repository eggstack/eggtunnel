# Reverse Session Subsystem Roadmap

Status: active — M001-M010 and C001 closed; M011 ready

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
- downstream embedding boundary;
- post-0.1 runtime policy, dynamic Service lifecycle, and bounded operational observability.

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

At the initial planning baseline the repository contained planning documents only. M001 is closed with the Rust workspace and bounded protocol foundation. M002 is closed with the authenticated TCP/TLS product. M003 is closed with resource accounting, auth and bind hardening, optional mTLS, direct application connectors, and lifecycle qualification. M004 and M005 are historically closed. C001 is closed and supplies supplemental transport-specific evidence for QUIC wrong/stale/replay/saturation/half-close, WSS close-during-relay and bounded backpressure, proxy refusal/timeout/cancellation, HTTP CONNECT and SOCKS5 authentication success/failure, and a two-hop SOCKS5+HTTP CONNECT chain. M006 is closed with the 0.1.0 distribution (hosted 4-target release, crates.io publication, downstream registry-consumption, advisory/license review).

The current Eggress integration baseline is 1.0.8. The TLS, relay, and core stream APIs were inspected before adding dependencies.

A post-0.1 repository review at baseline `2e2f3981a6e78596c32dfe5daf5b2585bfc37f1e` found no open correctness blocker, but identified maintenance/evolution work that should precede additional breadth: concentrated server/test topology, lack of continuous MSRV and supported feature-slice qualification, a direct unmaintained PEM parser, hard-coded operational policy, constructor/profile proliferation, no dynamic runtime Service registration, and a documented tracing/heartbeat observability contract that is only partially implemented. These findings are sequenced as M007-M009 below.

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
                    |
                    v
       M007 maintainability/continuous qualification
                    |
                    v
       M008 runtime policy/API composition
                    |
                    v
       M009 dynamic Service lifecycle/observability

M004 and M005 have a hard dependency on M003 but only soft/interface dependencies on each other.

M006 depends on M003 plus whichever optional transport profiles are declared part of the first published support matrix.

M007 starts the post-0.1 maintenance line. M008 had a hard dependency on M007 so public/configuration refactoring was not mixed with structural/test movement. M009 had a hard dependency on M008 so dynamic Service state and heartbeat/tracing policy are built on the canonical composition/configuration surface. M008 is strictly closed and M009 is now closed under ADR-0001 with existing RegisterService/UnregisterService and Ping/Pong semantics preserved.

A future negotiated-protocol milestone requires a concrete extension plus an accepted ADR. A future Eggpack distribution cutover requires stable Eggpack build/qualification, bootstrap-installer, and generated-CI interfaces. Neither is dependency-ready today.

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

Status: closed

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

Status: closed

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

Status: closed — historical closure supplemented by post-closure C001

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

Status: closed — historical closure supplemented by post-closure C001

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

Status: closed

Implementation plan:

- plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md

Closure record:

- plans/closure/reverse-session/006-status.md

Primary class: capability / polish

### Objective

Finalize public documentation, release artifacts, supported target matrix, crates publication, and real downstream-shaped qualification.

### Exit conditions

- public crate API and support matrix are truthful;
- release targets pass install/smoke evidence;
- CodeGG-shaped embedding consumes published public APIs;
- installer/updater duplication is avoided;
- semver/protocol support policy is documented.

## Post-closure corrective workstream

The M004/M005 historical closure records intentionally retain the limitations observed when those milestones closed. C001 is closed and supplies supplemental transport-specific evidence for QUIC correlation/generation, stream saturation, half-close semantics, WSS close/backpressure, outbound-proxy failure/cancellation, proxy authentication success/failure, and a multi-hop chain qualification. It also owns planning-control-surface reconciliation.

- roadmap: plans/subsystems/reverse-session-post-closure-corrective-addendum.md
- C001 plan: plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md
- C001 status: closed (historical; closure record at plans/closure/reverse-session-post-closure-corrective/001-status.md)

M006 is closed: the hosted 4-target release, crates.io publication, downstream registry-consumption, and advisory/license review all have recorded evidence.

## Post-0.1 maintenance and evolution

### Milestone M007 — Maintainability and continuous qualification

Status: closed

Implementation plan:

- plans/implementation/reverse-session/007-maintainability-and-continuous-qualification.md

Primary class: polish / invariant

Objective:

- reduce review/maintenance concentration without semantic change;
- move the cross-transport integration suite out of the monolithic server source;
- add continuous Rust 1.89 MSRV and supported feature-slice qualification;
- make rustdoc warnings fail CI;
- replace the direct unmaintained PEM parser through a maintained ownership-correct path;
- continuously guard the narrow Eggtunnel/Eggress boundary, including absence of `eggress-protocol-reverse` from the native Session dependency graph.

Exit conditions:

- behavior/wire compatibility unchanged;
- feature/MSRV support claims continuously executable;
- direct PEM-parser maintenance debt resolved or explicitly stopped on a documented dependency tradeoff;
- minimal client graph remains narrow;
- no high/medium finding remains.

### Milestone M008 — Configurable runtime policy and API composition

Status: closed

Implementation plan:

- plans/implementation/reverse-session/008-configurable-runtime-policy-and-api-composition.md

Primary class: capability / infrastructure

Objective:

- promote hard-coded finite resource/time policy into validated caller-configurable policy with identical secure defaults;
- replace transport/identity/proxy constructor multiplication with one typed composition/validation path;
- make CLI `check` and runtime startup share the same semantic validator.

Exit conditions:

- defaults are behavior-equivalent to the 0.1 runtime;
- non-default finite limits/timeouts are supported;
- unsupported profile combinations fail through one canonical validator;
- convenience constructors remain compatibility wrappers;
- no wire change or optional dependency leakage.

### Milestone M009 — Dynamic Service lifecycle and operational observability

Status: closed — closure recorded at `plans/closure/reverse-session/009-status.md`.

Implementation plan:

- plans/implementation/reverse-session/009-dynamic-service-lifecycle-and-operational-observability.md

Primary class: capability

Objective:

- add bounded runtime Service registration complementary to existing unregistration;
- make acknowledged dynamic Services reconnect-stable;
- emit structured secret-safe tracing without installing a subscriber;
- turn existing Ping/Pong into bounded RTT/missed-heartbeat state exposed through snapshots.

Exit conditions:

- an embedder can add/remove Services without restarting the Client;
- stale/unacknowledged registration cannot enter desired state;
- reconnect restores acknowledged dynamic Services;
- tracing and heartbeat health are bounded and secret-safe;
- no wire change.

### Milestone M010 — Client runtime modularization and state-machine hardening

Status: closed — closure recorded at `plans/closure/reverse-session/010-status.md`

Implementation plan:

- plans/implementation/reverse-session/010-client-runtime-modularization-and-state-machine-hardening.md

Primary class: polish / invariant

Objective:

- split the now-concentrated client runtime along stable private responsibility boundaries;
- give desired-Service and dynamic-registration transitions one explicit state owner;
- extract client tests from the production runtime file;
- preserve all M009 public/wire semantics, including the deliberate one-registration-in-flight constraint.

Exit conditions:

- no public API or wire change;
- reconnect/register/unregister/heartbeat/Open behavior remains qualified;
- no production dependency growth;
- client runtime and tests are materially easier to review;
- hosted continuous qualification passes.

### Milestone M011 — Sustained robustness and performance qualification

Status: ready — M010 strict closure recorded at `plans/closure/reverse-session/010-status.md`

Implementation plan:

- plans/implementation/reverse-session/011-sustained-robustness-and-performance-qualification.md

Primary class: invariant / polish

Objective:

- add a sustained protocol fuzz target while retaining the fast deterministic hostile-input guard;
- add deterministic client state-sequence stress;
- add bounded reconnect/dynamic-Service/connection-churn soak;
- record reproducible host-scoped throughput/churn/footprint baselines without turning noisy timings into hard CI correctness gates.

Exit conditions:

- fuzz and soak evidence are recorded with exact duration/configuration;
- resource/task counts converge after sustained failure/reconnect cycles;
- performance/footprint evidence is reproducible and informational;
- no production dependency or protocol/public-API change.

### Milestone M012 — 0.2.0 release qualification and publication gate

Status: blocked on M011 strict closure

Implementation plan:

- plans/implementation/reverse-session/012-0.2.0-release-qualification-and-publication-gate.md

Primary class: capability / polish

Objective:

- freeze and qualify M007-M011 as the next public 0.2.0 line;
- review public API compatibility against published 0.1.0;
- version/package/inspect the proto and library crates;
- preserve the evidence-backed four-target binary release contract;
- re-evaluate Eggpack at execution time without silently migrating release infrastructure;
- stop before tag/crates/GitHub publication until explicit owner authorization.

Exit conditions:

- exact candidate passes full, feature, MSRV, security/license, sustained, packaging, and installer evidence;
- protocol remains truthfully wire 1.0;
- 0.2.0 public API/release notes are coherent;
- authorized tag/release/publication and clean registry-consumer evidence complete before published-release closure.

### Later gated work

Protocol capability negotiation is intentionally not assigned an executable milestone yet. The current protocol exchanges an empty capability set and treats minor versions as informational. The first change to that compatibility meaning must have a concrete extension and an accepted ADR before an implementation plan is registered.

Eggpack release/bootstrap/CI adoption is also intentionally not assigned an executable Eggtunnel milestone yet. Eggtunnel's current release workflow remains authoritative until Eggpack exposes stable build/qualification, bootstrap-installer, and generated-CI contracts capable of preserving the closed M006 release evidence.

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

Libraries emit tracing events without installing subscribers. This intended contract was incomplete at the M007 planning baseline; M009 implements bounded tracing and heartbeat health after M008 stabilized runtime policy.

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
- bandwidth billing;
- negotiated capability/version semantics until a concrete extension and ADR exist;
- Eggpack release-workflow cutover until stable producer interfaces are available.

## 19. Status table

| Milestone | Status | Plan | Closure | Blocker |
|---|---|---|---|---|
| M001 repository/protocol foundation | closed | plans/implementation/reverse-session/001-repository-and-protocol-foundation.md | plans/closure/reverse-session/001-status.md | — |
| M002 TCP/TLS product | closed | plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md | plans/closure/reverse-session/002-status.md | — |
| M003 hardening/embedding | closed | plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md | plans/closure/reverse-session/003-status.md | — |
| M004 QUIC | closed | plans/implementation/reverse-session/004-quic-transport.md | plans/closure/reverse-session/004-status.md | M003 closed at `31458e83e543304d6b271898575bf2f6e98c7352`; supplemental evidence at plans/closure/reverse-session-post-closure-corrective/001-status.md |
| M005 WSS/proxy traversal | closed | plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md | plans/closure/reverse-session/005-status.md | M003/M004 closed; supplemental evidence at plans/closure/reverse-session-post-closure-corrective/001-status.md |
| C001 optional-transport corrective | closed | plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md | plans/closure/reverse-session-post-closure-corrective/001-status.md | M004/M005 historical closures + post-closure corrective workstream |
| M006 distribution/downstream | closed | plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md | plans/closure/reverse-session/006-status.md | none |
| M007 maintainability/continuous qualification | closed | plans/implementation/reverse-session/007-maintainability-and-continuous-qualification.md | plans/closure/reverse-session/007-status.md | Rust 1.89, feature slices, dependency guard, PEM parser, and hosted CI passed |
| M008 configurable runtime policy/API composition | closed | plans/implementation/reverse-session/008-configurable-runtime-policy-and-api-composition.md | plans/closure/reverse-session/008-status.md | M007 strict closure |
| M009 dynamic Service lifecycle/observability | closed | plans/implementation/reverse-session/009-dynamic-service-lifecycle-and-operational-observability.md | plans/closure/reverse-session/009-status.md | M008 strict closure; hosted CI passed |
| M010 client runtime modularization/state-machine hardening | closed | plans/implementation/reverse-session/010-client-runtime-modularization-and-state-machine-hardening.md | plans/closure/reverse-session/010-status.md | M009 strict closure; hosted CI passed |
| M011 sustained robustness/performance qualification | ready | plans/implementation/reverse-session/011-sustained-robustness-and-performance-qualification.md | — | M010 strict closure |
| M012 0.2.0 release qualification/publication gate | blocked | plans/implementation/reverse-session/012-0.2.0-release-qualification-and-publication-gate.md | — | M011 strict closure; publication requires explicit authorization |
