# Reverse Session M011 — Sustained Robustness and Performance Qualification

Status: closing

Planning baseline: 0c8830e76d92c209485eaf955cbdb51bdc953413

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#post-01-maintenance-and-evolution

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: invariant / polish

Hard dependency: M010 strict closure.

## 1. Objective

Add repeatable sustained evidence around the mature 0.x runtime: protocol fuzzing, lifecycle/reconnect soak, connection churn, resource convergence, and host-specific performance/footprint baselines.

This milestone measures and stresses the existing product. It does not optimize the data plane, change the protocol, or make noisy performance numbers release correctness gates.

## 2. Planning-time dependency rationale

At planning time, M010 first needed to stabilize the client runtime and desired-Service state boundaries. M010 is now strictly closed at `plans/closure/reverse-session/010-status.md`, so this dependency is satisfied and M011 is ready to execute.

## 3. Planning-time ecosystem state

At this planning baseline:

- Eggbench has a mature local-runner/evidence foundation, but its network/external-driver and Eggstack integration layers are still being implemented;
- Eggchaos has completed a deep pre-tag qualification line, but owner tagging/publication is still a separate action;
- neither is a stable hard dependency for Eggtunnel qualification today.

Therefore M011 must be self-contained and lightweight. At execution time, stable Eggbench/Eggchaos interfaces MAY contribute supplemental evidence, but closure must not depend on unreleased sibling APIs and must not vendor their functionality.

## 4. Invariants

- Fuzz/soak tooling is development/qualification-only and does not enter the production dependency graph.
- No unsafe code is introduced into Eggtunnel production crates.
- Wire semantics and public API remain unchanged.
- Security/resource limits are not raised merely to make stress tests pass.
- A performance regression is investigated, not automatically “fixed” by weakening correctness/backpressure.
- Host-specific throughput/timing values are informational unless a later accepted plan establishes a calibrated threshold.
- Every soak has a bounded duration and deterministic teardown.
- Secrets never appear in generated corpus, logs, benchmark output, or artifacts.

## 5. Protocol fuzzing

Add a maintained fuzz target for the public bounded decoder, centered on `eggtunnel_proto::decode_frame`.

Requirements:

- raw arbitrary bytes;
- no panic, abort, UB, or pathological unbounded allocation;
- respect the protocol's existing size guards;
- seed corpus from valid minimal frames plus malformed/truncated/oversized/version/message-ID cases already covered by unit tests;
- keep fuzz workspace/tooling outside normal production dependency resolution where practical.

A second fuzz target MAY cover encode/decode round-trip mutation or bounded public constructors if it adds meaningful coverage without exposing private runtime internals.

Closure must state engine/version, target, seed/corpus count, run duration, and outcome. Do not describe a short fuzz run as exhaustive proof.

The existing deterministic 10,000-input hostile decoder test remains a fast CI guard and must not be removed merely because a fuzzer exists.

## 6. Stateful deterministic stress harness

Add a deterministic sequence/stress harness for client Service/session lifecycle using the private state boundary established by M010 or public fake-session seams.

Exercise long command sequences including:

- register;
- acknowledge/reject;
- unregister;
- cancel waiter;
- disconnect;
- reconnect/generation replacement;
- heartbeat miss/recovery;
- Open admission/release where practical.

Use a fixed/recorded seed and bounded step count. The harness should assert state invariants after each step or phase, not just “did not crash.”

Avoid adding a heavyweight property-testing framework if a small deterministic harness is sufficient.

## 7. Lifecycle and connection soak

Add an explicit long-running qualification mode, separate from ordinary fast unit tests.

Minimum scenarios:

### TCP/TLS lifecycle soak

Repeatedly:

- start client/server;
- establish Session;
- register/unregister dynamic Services;
- relay multiple connections;
- force server/client disconnect;
- reconnect;
- shutdown.

Assert after cycles:

- current counts converge to zero/expected steady state;
- no monotonic growth in Sessions, pending connections, Open tasks, handshakes, or registered Services outside intended high-water counters;
- no task panic;
- reconnect backoff remains bounded/cancellation-aware.

### Connection churn

Exercise many short-lived relays under bounded concurrency with at least small and medium payloads.

Record:

- completed connections;
- bytes relayed;
- failures by typed category;
- high-water resource counters;
- elapsed time as informational evidence.

### Optional transports

Run bounded sustained cases for QUIC and WSS when feature-enabled. Outbound-proxy soak may use existing local fixtures.

Do not claim TCP half-close equivalence for WebSocket.

## 8. Performance and footprint baselines

Create a reproducible development benchmark/qualification harness that records at least:

- steady-state loopback relay throughput for TCP/TLS;
- connection-establishment/churn rate;
- QUIC connection/stream churn comparison where available;
- control-path registration/reconnect timing as informational data;
- release CLI binary size;
- minimal `client,tls` dependency tree/package count.

The harness may use a dev-only benchmark crate/tool if justified, but it must not affect downstream library dependencies.

For every recorded result capture:

- commit SHA;
- host OS/architecture;
- Rust version/profile;
- transport;
- payload/concurrency parameters;
- duration/sample count.

Do not add hard CI pass/fail thresholds from a single hosted runner. If a clear regression is found relative to repeated same-host baselines, stop and create a targeted corrective/performance plan.

## 9. CI and workflow behavior

Fast CI remains fast.

Required routine CI:

- existing unit/integration suite;
- deterministic hostile-input/corpus tests;
- feature/MSRV/minimal dependency lanes.

Long fuzz/soak/performance work should be manually invokable or otherwise isolated from every-push latency. A `workflow_dispatch` qualification workflow MAY be added for bounded fuzz/soak evidence if it can reuse repository commands without duplicating release machinery.

Do not schedule recurring external network load.

## 10. Fault-injection boundary

Use existing in-process/fake peer fixtures for deterministic network failures.

If a stable released Eggchaos interface exists at execution time, a supplemental local experiment MAY demonstrate latency/reset/churn behavior through Eggtunnel. Do not add Eggchaos as a production dependency or block M011 on its publication.

Similarly, if Eggbench has closed the driver/integration seams needed for Eggtunnel, M011 MAY emit evidence that Eggbench can ingest. Do not reimplement Eggbench's experiment-bundle/statistics model locally.

## 11. Required focused evidence

At minimum:

- sustained `decode_frame` fuzz run with recorded duration;
- deterministic state-sequence run over many transitions;
- repeated TCP/TLS reconnect/dynamic-Service soak;
- bounded connection-churn run;
- QUIC and WSS sustained runs where supported;
- resource convergence after every soak;
- benchmark/footprint record on at least one named host;
- all existing negative transport/security cases remain passing.

## 12. Required broad verification

Run the complete M010 verification matrix after adding qualification tooling.

Additionally record the exact commands for:

- fuzzer build/run;
- corpus regression run;
- lifecycle soak;
- connection churn;
- performance baseline;
- optional transport soak.

Hosted CI on the implementation head is required. Long-running evidence may be local/manual where hosted timing would be misleading, but its host/config/duration must be explicit.

## 13. Documentation

Update:

- `docs/OPERATIONS.md` with developer qualification commands only if appropriate;
- `docs/SECURITY.md` with fuzz/hostile-input evidence boundaries;
- `docs/DISTRIBUTION.md` with footprint evidence if release-relevant;
- architecture testing/ops documentation;
- `AGENTS.md` / verify skill if new qualification commands should be discoverable;
- roadmap/registry.

Keep performance numbers out of user-facing marketing claims unless they are measured and scoped.

## 14. Acceptance criteria

- a real sustained protocol fuzz target exists and has closure evidence;
- the fast deterministic malformed/arbitrary-input guard remains;
- lifecycle/reconnect/dynamic-Service soak completes with resource convergence;
- connection churn is bounded and repeatable;
- at least one reproducible performance/footprint baseline is recorded with host/config metadata;
- optional transport sustained evidence is truthful about limitations;
- no production dependency/feature leakage is introduced;
- no protocol/public API change is required;
- no unresolved high/medium correctness/security finding remains.

## 15. Stop conditions

Stop and create a corrective plan if:

- fuzzing finds a decoder panic, unbounded allocation, or protocol invariant violation;
- soak reveals task/resource growth that does not converge;
- connection churn exposes correlation/replay failure;
- fixing a finding requires protocol versioning or capability negotiation;
- meaningful benchmarking requires adding production data-plane abstractions or optimizations;
- sibling-tool integration would require depending on unstable/unpublished APIs.

## 16. Closure evidence required

Create `plans/closure/reverse-session/011-status.md` with:

- baseline/final head;
- fuzz engine/target/corpus/duration/results;
- deterministic sequence seed/steps/invariant results;
- soak scenarios, cycle counts/durations, and resource before/after tables;
- performance/footprint measurements with host/config metadata;
- optional Eggbench/Eggchaos evidence, clearly supplemental;
- full verification and hosted CI;
- residual findings/disposition.
