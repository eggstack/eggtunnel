# Reverse Session M003 — Security, Lifecycle, Resource, and Embedding Hardening

Status: active

Planning baseline: 13402200e51b46031a1a82240be1eb48027a09f4

M002 is closed at the baseline above. M002 closure findings promoted into this milestone: exhaustive cancellation injection and repeated lifecycle/pending-expiry qualification.

The public API, feature graph, runtime ownership, and client/server resource ceilings were reconciled against M002 before execution.

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#8-milestone-m003--security-lifecycle-resource-and-embedding-hardening

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: invariant / capability

## 1. Objective

Harden the closed TCP/TLS product so its resource accounting, failure/cancellation behavior, authorization policy, optional mTLS, public embedding API, and hostile-input evidence are strong enough to serve as the stable base for optional transport expansion and downstream consumption.

M003 should not broaden the product with QUIC/WSS/proxy traversal. It reduces risk and stabilizes ownership.

## 2. Dependency readiness

Hard dependency: M002 strict closure.

Before execution:

- update baseline to M002 final reviewed head;
- read M001/M002 closure findings;
- identify any conditional evidence that must be promoted into this milestone;
- inspect actual public API and feature dependency graph.

If M002 requires a corrective pass, M003 remains blocked.

## 3. Invariants

- Existing M002 end-to-end behavior remains unchanged for valid configurations.
- Resource exhaustion rejects or backpressures; it never creates unbounded queues/tasks.
- Capacity is released exactly once.
- Authentication and bind authorization remain separate.
- Client-side Target remains client-owned; Server cannot request arbitrary local destinations.
- Shutdown/cancellation owns and joins all spawned work.
- Secret-bearing types remain redacted.
- Public embedding API remains process-neutral.
- Optional hardening features do not force broad dependencies into client-only TLS builds.
- No QUIC/WSS/custom mux work.

## 4. In scope

- explicit resource budget and RAII permits;
- per-principal/session/service ceilings;
- auth failure throttling;
- optional mTLS profile;
- stronger bind/port policy object;
- direct application Target connector;
- transport-neutral public stream boundary;
- typed termination/error classification;
- bounded snapshots;
- lifecycle and leak instrumentation for tests;
- fuzzing/corpus hardening;
- downstream-shaped embedding fixture;
- dependency-tree/footprint evidence;
- security documentation refinement.

## 5. Resource model

Define explicit resource classes at least for:

- active Sessions;
- Services;
- external listeners;
- Pending Connections;
- active Relays;
- client Open tasks;
- accepted but unauthenticated connections;
- queued control messages.

A ResourceBudget or equivalent should:

- hold immutable configured limits;
- atomically admit or reject;
- return owned permits/leases;
- expose used/limit/high-water/denied counters;
- prevent release underflow;
- support bundled admission where partial acquisition would leak.

Do not import i2pr code. Independently implement only the small concepts required.

## 6. Task and queue ownership

Audit every spawn/channel in M002.

Requirements:

- bounded channels only;
- channel capacity constants documented;
- every task belongs to a Session, Service, connection scope, or top-level runtime owner;
- cancellation propagates before sibling joins;
- shutdown has a bounded join/drain policy;
- panicking child task produces an observable typed termination and does not strand siblings;
- no task holds a permit indefinitely after owner cancellation.

Add test-only counters/probes if needed, but avoid production global registries solely for tests.

## 7. Authentication hardening

Add:

- bounded concurrent unauthenticated handshakes;
- per-source or globally bounded auth failure throttling appropriate to the server model;
- fixed/bounded failure delay or sliding-window limiter;
- generic failure diagnostics that do not reveal token validity details;
- token length/format caps;
- secret zeroization where practical.

Avoid expensive password hashing unless the credential model actually uses human passwords. Bearer service tokens should remain high-entropy credentials.

## 8. mTLS profile

Add optional mTLS only if it can remain a clean feature/profile.

Requirements:

- server certificate verification remains mandatory;
- client certificate trust roots configured explicitly;
- certificate identity maps to a Principal through documented rules;
- bearer token may remain an independent profile or be combined only with explicit semantics;
- certificate/key material never logs;
- invalid/expired/untrusted client certificates fail before Service registration;
- no home-grown PKI/enrollment service in Eggtunnel.

CodeGG node enrollment/rotation/revocation remains CodeGG-owned if CodeGG later uses mTLS identities.

## 9. Bind authorization policy

Promote M002 checks into a typed server policy.

Policy should be able to express:

- allowed listener IPs/interfaces;
- allowed/denied port ranges;
- ephemeral-port permission;
- max Services per Principal/Session;
- max active/pending connections;
- optional service-name restrictions if justified.

Non-loopback binds should require explicit authorization and secure transport.

Policy evaluation must occur before bind.

## 10. Direct application Target connector

Add a generic embedding path so a downstream application can satisfy an Open request with an async duplex stream without a loopback TCP hop.

Desired semantics:

- connector receives only trusted local Service identity/config and bounded connection context;
- Server-provided data cannot redirect the connector to an arbitrary local target;
- connector returns an Eggtunnel-owned/re-exported boxed async stream or equivalent transport-neutral type;
- connector cancellation is propagated;
- connector failure maps to OpenReject/typed termination.

Do not expose Quinn/Tungstenite types through this boundary.

A simple TCP connector remains the built-in default.

## 11. Public API qualification

Review the library surface for:

- ClientConfig / Client;
- ClientHandle;
- ServerConfig / Server;
- ServerHandle;
- ServiceSpec;
- Target/TargetConnector;
- snapshots;
- shutdown;
- errors.

Goals:

- no mandatory config file;
- no global mutable singleton;
- no process exit;
- no hidden runtime creation;
- caller controls tracing subscriber;
- caller can provide cancellation/shutdown;
- feature-gated types disappear cleanly when features are off;
- errors are typed enough for downstream policy.

Add compile-contract tests for representative imports and construction.

## 12. Snapshot and termination model

Snapshots should include bounded current values such as:

- session state;
- registered service count/list within configured cap;
- effective binds;
- pending/active counts;
- reconnect count;
- byte counters;
- last error category/code without secrets.

Define typed termination categories, for example:

- Clean;
- Cancelled;
- Timeout;
- Authentication;
- Authorization;
- Protocol;
- Transport;
- Target;
- ResourceExhausted;
- PeerClosed;
- Internal.

Exact naming may differ; avoid relying on string parsing.

## 13. Fuzzing and malformed-input hardening

Add a protocol fuzz target or equivalent sustained arbitrary-byte decoder harness.

Targets:

- frame decoder;
- message decoder;
- service/bind parsing;
- DataHello/preface;
- state-machine sequence validator if deterministic and cheap.

Seed corpus with boundary/known-invalid cases.

Fuzzing evidence can be time-bounded locally; closure must report duration/engine rather than claiming exhaustive proof.

## 14. Downstream-shaped embedding fixture

Create a small test/example shaped like CodeGG's needs:

- library dependency only;
- default features disabled;
- client + TLS enabled;
- caller-owned Tokio runtime;
- caller-owned tracing;
- no CLI/config file;
- programmatic Service;
- loopback Target and direct connector variants;
- clean startup/shutdown.

Do not add CodeGG as a dependency.

## 15. Dependency and footprint evidence

Record:

- cargo tree for eggtunnel client+tls;
- cargo tree for full default library profile;
- feature graph;
- whether QUIC/WebSocket/proxy crates are absent from client+tls;
- release binary size for CLI as informational evidence if CLI already exists;
- release library/dependency notes.

Do not set brittle binary-size thresholds unless a measured regression problem exists.

## 16. Ordered work packages

A. Resource/admission ownership
- budget types;
- permits;
- saturation tests;
- task/channel audit.

B. Authentication and bind-policy hardening
- throttling;
- policy object;
- negative matrix.

C. mTLS
- optional profile;
- cert identity;
- negative tests.

D. Embedding API
- TargetConnector;
- public API cleanup;
- compile contracts;
- downstream fixture.

E. Lifecycle/fuzz/footprint closure
- cancellation races;
- repeated convergence;
- fuzz corpus;
- dependency evidence;
- docs.

## 17. Required tests

Resource:
- each ceiling at limit and limit+1;
- bundled partial-acquisition rollback;
- permit release on success/error/cancel/panic;
- repeated saturation returns to baseline.

Auth/policy:
- invalid token burst bounded;
- auth delay/limiter does not create unbounded tasks;
- loopback/non-loopback matrix;
- port range allow/deny;
- auth success + bind denial remains denial.

mTLS:
- valid client cert;
- absent cert when required;
- untrusted cert;
- wrong server name;
- redaction.

Embedding:
- direct connector success;
- connector refusal/timeout/cancel;
- client-only compile surface;
- no global runtime/tracing side effects.

Lifecycle:
- cancellation during every M002 stage;
- server shutdown with active/pending connections;
- client shutdown while reconnecting;
- panic injection where practical;
- repeated start/stop/reconnect without count growth.

Security:
- malformed frames;
- replay;
- oversized names/diagnostics;
- fuzz target no-crash run.

## 18. Verification

Run the repository's full required suite and focused feature slices.

At minimum include:

cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

Additionally:

- client+tls no-default-features build/test;
- server+tls build/test;
- mtls slice;
- cargo tree evidence;
- fuzz/corpus command;
- repeated lifecycle integration run counts recorded exactly.

## 19. Documentation

Update:

- docs/ARCHITECTURE.md;
- docs/SECURITY.md;
- docs/EMBEDDING.md;
- docs/PROTOCOL.md for auth/cert semantics only if wire behavior changes;
- docs/CONFIGURATION.md;
- public crate docs;
- subsystem roadmap and registry.

## 20. Acceptance criteria

- every production resource class is bounded;
- saturation has deterministic typed outcomes;
- repeated error/cancel paths return resource/task counts to baseline;
- bind authorization is explicit and separate from auth;
- optional mTLS works without inventing a PKI service;
- downstream direct connector works;
- public client-only API is programmatic/process-neutral;
- protocol fuzz/corpus evidence is recorded;
- optional future transports remain absent from minimal dependency graph;
- no unresolved high/medium finding remains.

## 21. Stop conditions

Stop and report if:

- resource correctness requires a general scheduler/framework rather than a small local budget;
- mTLS requires an identity database/enrollment service;
- direct connector would require exposing transport-specific concrete types;
- M002 lifecycle defects are large enough to require a separate corrective milestone first;
- hardening requires changing ADR-0001 transport/session ownership.

## 22. Closure evidence required

- refreshed baseline/final reviewed head;
- resource-class/limit table;
- requirement-to-test matrix;
- leak/convergence repeated-run evidence;
- auth/bind/mTLS security evidence;
- downstream compile/integration evidence;
- fuzz/corpus evidence;
- cargo tree/feature evidence;
- exact commands/results;
- residual findings and disposition.
