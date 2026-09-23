# Reverse Session Post-Closure Corrective C001 — Optional-Transport Qualification and Planning Reconciliation

Status: closed

Repository baseline:

- fc19fe57a32d0c45be339ff3dd80044c4a8bc069

Closure record:

- plans/closure/reverse-session-post-closure-corrective/001-status.md

Source corrective roadmap:

- plans/subsystems/reverse-session-post-closure-corrective-addendum.md#5-corrective-milestone-c001

Parent subsystem:

- plans/subsystems/reverse-session-roadmap.md

Original milestones and closure records:

- M004 plan: plans/implementation/reverse-session/004-quic-transport.md
- M004 closure: plans/closure/reverse-session/004-status.md
- M005 plan: plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md
- M005 closure: plans/closure/reverse-session/005-status.md

Long-term requirements:

- plans/000-long-term-specification.md#7-quic-model
- plans/000-long-term-specification.md#8-websocket-and-outbound-proxy-traversal
- plans/000-long-term-specification.md#13-resource-model
- plans/000-long-term-specification.md#14-reconnect-and-recovery
- plans/000-long-term-specification.md#15-cancellation-and-shutdown
- plans/000-long-term-specification.md#24-testing-and-evidence
- plans/000-long-term-specification.md#28-system-invariants

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: corrective / invariant / evidence

## 1. Objective

Close the specific optional-transport qualification gaps left by M004 and M005 and restore a single truthful planning state across the registry, subsystem roadmap, closure history, support documentation, and active M006 release work.

This is a narrow post-closure corrective. It MUST preserve the implemented Session/Service architecture and avoid feature expansion.

## 2. Why this corrective is ready

M004 and M005 are implemented and have accepted historical closure records.

Their closure records themselves identify the unresolved evidence:

M004:
- no QUIC-specific wrong/stale DataHello integration test;
- no QUIC-specific half-close integration test;
- no QUIC stream-saturation integration test;
- Eggtunnel does not independently gate the adapter's UDP/TLS handshake processing before the application Session admission point.

M005:
- proxy authentication is not integration-tested;
- multi-hop chains are not integration-tested;
- timeout/refusal paths are not integration-tested;
- cancellation during individual proxy hops is not integration-tested;
- WebSocket close semantics are not TCP half-close equivalent.

The repository planning state also drifted:
- registry still describes M004 active, M005 ready, and M006 blocked;
- parent roadmap header/status table describe M004/M005 closed and M006 active, but several individual milestone sections still use older status text;
- M006 implementation has already landed a release-qualification foundation and CI is green on the current head.

No architecture decision is missing. The work is evidence and status convergence.

## 3. Current repository evidence

At baseline fc19fe57:

- M001-M003 are closed;
- M004 QUIC implementation is at commit 47c37f4abf962748a7fcd6a860e1d5766035d694;
- M005 WSS/outbound proxy implementation is at commit 448c6159696c7f2792e563622bc525c35028d24b;
- M006 release-qualification foundation is at fc19fe57a32d0c45be339ff3dd80044c4a8bc069;
- GitHub Actions Rust run 35795541560 passed at fc19fe57;
- current optional transports use Eggress 1.0.8 public crates;
- minimal client+TLS dependency evidence recorded by M004/M005 excludes QUIC/WebSocket/outbound-proxy crates;
- current M006 release work remains intentionally unclosed.

Implementation MUST inspect the actual head before changing tests or status files and preserve any user changes after this baseline.

## 4. Invariants that must not regress

- M001-M003 production contracts remain closed.
- Protocol wire IDs/version meaning do not change.
- SessionId, ServiceId, and ConnectionId remain distinct.
- ConnectionId remains random, Session-bound, Service-bound, expiring, and single-use.
- QUIC uses one native bidirectional stream per logical tunneled TCP connection.
- No custom stream mux is introduced.
- WebSocket remains a stream adapter with its documented full-close limitation.
- Proxy traversal remains listener-free through eggress-outbound.
- No direct fallback after a configured proxy failure unless explicitly configured by a later plan.
- Application payloads remain opaque.
- client+tls minimal builds remain free of optional transport dependencies.
- No eggress-embed, Synvoid, or i2pr production dependency is added.
- No production unbounded channel or ownerless task is introduced.
- M006 release work stays separate from transport correctness.

## 5. Explicit non-goals

C001 MUST NOT:

- redesign QUIC authentication or replace the native Eggtunnel protocol;
- add QUIC datagrams/UDP;
- add custom CA/mTLS support to QUIC by bypassing the current Eggress public API;
- implement TCP half-close emulation inside WebSocket;
- implement a new proxy stack;
- add new proxy protocols;
- publish crates;
- create a GitHub release;
- claim Windows/musl/armv7/SBC support;
- change release-version policy;
- add a general benchmark framework.

## 6. Work package A — QUIC correlation and generation correctness

### Intent

Exercise the same correlation security invariants through the actual QUIC stream path rather than relying on shared protocol/unit coverage.

### Required cases

1. Wrong SessionId DataHello:
   - establish a valid QUIC Session;
   - create a real pending external connection;
   - open a QUIC data stream with a different SessionId and the pending ConnectionId;
   - assert typed rejection;
   - assert the pending entry is not consumed by the wrong stream;
   - assert a subsequent valid stream can still consume it if still within deadline.

2. Stale old-generation DataHello:
   - establish Session A;
   - force connection replacement/reconnect to Session B;
   - create/retain a deterministic old-session correlation scenario using test seams;
   - submit old Session A data identity after Session B is authoritative;
   - assert it cannot attach to Session B state.

3. Duplicate/replay:
   - complete one valid pairing;
   - replay the same DataHello/ConnectionId on another QUIC stream;
   - assert deterministic rejection and no second target/external attachment.

Tests MUST use the production QUIC adapter path rather than only the shared codec or table helper.

## 7. Work package B — QUIC stream saturation and isolation

### Intent

Directly prove the M003 resource budget plus QUIC transport limits produce bounded behavior.

### Required test shape

- configure a deliberately small active-connection or stream ceiling;
- hold enough real QUIC data streams open to reach the ceiling;
- attempt one or more additional Opens/data streams;
- observe explicit bounded rejection, backpressure, or timeout according to the production contract;
- assert no unbounded handler/task growth;
- close/release one admitted stream;
- assert capacity returns and a subsequent connection succeeds;
- assert an unrelated admitted stream remains functional throughout.

If the Eggress/Quinn stream limit itself prevents the extra stream from opening before Eggtunnel receives it, record that exact mechanism. The test must still prove bounded task/resource behavior and recovery.

Do not create an artificial production-only queue solely to make the test easier.

## 8. Work package C — QUIC half-close semantics

### Intent

Determine and document the actual TCP <-> QUIC relay half-close behavior.

Required test:

- external TCP peer sends request then shuts down its write half;
- tunneled local target receives EOF on request side;
- target sends a response after observing EOF;
- external peer receives the response before final close.

If the actual Eggress QUIC BoxStream or relay path cannot preserve that semantic:
- do not add fragile emulation;
- capture the observed transport behavior;
- narrow the QUIC support claim in docs;
- classify the residual impact;
- do not describe QUIC as TCP-half-close equivalent.

The preferred outcome is preserving generic relay half-close semantics, but evidence controls the claim.

## 9. Work package D — QUIC pre-session handshake admission review

### Intent

Resolve the M004 recorded limitation that QUIC UDP/TLS handshake work occurs inside the Eggress adapter before Eggtunnel's authenticated Session semaphore is acquired.

Required steps:

1. inspect the current eggress-transport-quic 1.0.8 public listener configuration and Quinn-exposed limits;
2. identify whether connection/handshake concurrency can be bounded through supported public configuration without replacing the adapter;
3. if yes, apply the smallest bound consistent with M003 resource policy and add saturation evidence;
4. if no:
   - retain the Eggress adapter;
   - document the exact boundary as transport-layer work outside Eggtunnel application admission;
   - confirm finite underlying QUIC stream/connection settings actually available;
   - classify the residual risk in C001 closure;
   - do not claim Eggtunnel directly accounts pre-session handshake work.

Stop and report if fixing this would require vendoring/replacing the Eggress adapter or introducing a second QUIC implementation.

## 10. Work package E — WSS close, flush, and backpressure qualification

### Intent

Qualify the actual stream semantics Eggtunnel supports over WebSocket without pretending to emulate TCP half-close.

Required coverage:

- request/response round trip under normal flush;
- enough payload/backpressure to exercise multiple WebSocket frames/messages or buffered writes within configured caps;
- peer Close/EOF while an application relay is active;
- deterministic task/session cleanup;
- no unbounded outbound message accumulation;
- another independent Session/data path remains unaffected where practical.

Document that WebSocket close is full-connection close unless the production implementation proves a stronger property.

Do not add synthetic half-close messages to the Eggtunnel protocol in this corrective.

## 11. Work package F — Proxy refusal, timeout, and cancellation

### Intent

Exercise production failure/reconnect behavior around the listener-free Eggress outbound connector.

Use deterministic local fixtures only.

Required cases:

1. Refused/unreachable proxy endpoint:
   - verify bounded typed termination/reconnect classification;
   - no secret-bearing URI appears in error output.

2. Proxy handshake timeout:
   - local fixture accepts but deliberately withholds required response;
   - configured timeout fires;
   - connector task tears down;
   - reconnect/backoff remains bounded.

3. Cancellation during proxy establishment:
   - hold the proxy connection in-progress;
   - cancel Client/shutdown or owning Session;
   - verify prompt task termination;
   - verify no lingering reconnect or connection resource;
   - verify capacity counters return to baseline where observable.

These cases should cover both the outbound connector and Eggtunnel owner lifecycle, not merely the fixture parser.

## 12. Work package G — Proxy authentication capability qualification

### Intent

Match support claims to what eggress-outbound 1.0.8 actually exposes.

Required procedure:

- inspect supported URI/config credential forms for HTTP CONNECT and SOCKS5;
- inspect current Eggtunnel parser/config pass-through behavior;
- never log the supplied proxy URI or credential.

If HTTP CONNECT authentication is supported:
- add deterministic Basic/other supported auth success test;
- add invalid-credential failure test.

If SOCKS5 username/password authentication is supported:
- add success/failure tests.

If a profile is not supported through the public connector:
- make configuration reject or documentation explicitly exclude it;
- do not add an Eggtunnel-specific authentication implementation.

Closure evidence must distinguish "unsupported" from "supported but untested."

## 13. Work package H — Multi-hop claim reconciliation

### Intent

Resolve the difference between Eggtunnel accepting proxy chain text and the absence of a direct multi-hop integration test.

Inspect docs/config/API claims.

If Eggtunnel currently advertises supported multi-hop chains:
- create one deterministic two-hop local chain using supported Eggress schemes;
- tunnel a complete authenticated Eggtunnel Session and one data path through it;
- verify end-to-end TLS still authenticates Eggtunnel Server;
- verify failure of one hop does not fall back direct.

If the public surface cannot reliably qualify a chain:
- narrow M005/M006 support docs to the actually tested single-hop HTTP CONNECT and SOCKS5 profiles;
- retain parser support only if clearly labeled unqualified/internal;
- do not call multi-hop a supported release profile.

## 14. Work package I — Planning and evidence reconciliation

### Registry

Update plans/registry.md to show:

- M001-M005 closed;
- C001 ready/active/closing as execution progresses;
- M006 active;
- M006 strict closure blocked on C001 plus M006's own hosted release, advisory/license, and publication/downstream gates.

Remove stale claims that the repository is in planning/bootstrap state.

### Parent roadmap

Update plans/subsystems/reverse-session-roadmap.md:

- M002 section -> closed;
- M004 section -> closed;
- M005 section -> closed;
- M006 section -> active;
- current-state text -> M006 active, not merely ready;
- add a post-closure corrective subsection/reference;
- status table -> include C001 or link to the corrective addendum;
- preserve historical milestone sequence.

### Closure history

Do not rewrite M004/M005 closure records to hide their original limitations.

Optionally add a brief supplemental pointer in each historical closure record after C001 closes, but the authoritative corrective evidence belongs at:

- plans/closure/reverse-session-post-closure-corrective/001-status.md

### M006

M006 may continue implementation in parallel, but its closure record MUST NOT be accepted before C001 closes.

## 15. Failure, cancellation, and resource semantics

Every new negative-path test must prove more than an error string.

Where applicable assert:

- pending ConnectionId remains/does not remain according to the state transition;
- permits/counters return to baseline;
- child tasks terminate;
- reconnect controller does not spin;
- unrelated Session/data path remains operational;
- secrets are redacted;
- no direct proxy fallback occurs;
- cancellation beats timeout where cancellation is triggered first.

Use bounded timeouts in tests to detect hangs, but do not use elapsed-time-only assertions as proof of correct cleanup if a direct completion/counter observation is available.

## 16. Required focused tests

Names may match repository conventions, but closure must map each case explicitly.

QUIC:
- wrong-session DataHello;
- stale-generation DataHello after replacement;
- replay/duplicate DataHello;
- active stream saturation + capacity recovery;
- unrelated stream isolation under saturation/reset;
- half-close request/response;
- pre-session handshake admission test if a supported bound is added.

WSS:
- close during active relay;
- flush/backpressure multi-frame or bounded-buffer behavior;
- task/resource cleanup.

Proxy:
- refused endpoint;
- handshake timeout;
- cancellation during in-progress handshake;
- HTTP CONNECT auth success/failure if supported;
- SOCKS5 auth success/failure if supported;
- two-hop chain if claimed supported.

Static/config:
- unsupported proxy auth/chain combinations fail or are absent from support claims;
- secret-bearing proxy URI is not formatted into diagnostics;
- minimal client+TLS cargo tree remains free of optional transport dependencies.

## 17. Required broad verification

At minimum:

cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
cargo check --locked --manifest-path fixtures/embedder/Cargo.toml

Feature slices:

cargo test --locked -p eggtunnel --no-default-features --features client,tls
cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,quic
cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,websocket
cargo test --locked -p eggtunnel --no-default-features --features client,tls,outbound-proxy
cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,websocket,outbound-proxy

Dependency isolation:

cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal

Confirm no QUIC/WebSocket/outbound-proxy packages appear in that slice.

Run the current repository CI workflow if triggered by the corrective commit and record its run ID/conclusion. Hosted release-target workflow evidence remains M006, not C001.

## 18. Documentation updates

Review/update as needed:

- docs/SUPPORT.md;
- docs/SECURITY.md;
- docs/OPERATIONS.md;
- docs/CONFIGURATION.md;
- docs/ARCHITECTURE.md;
- docs/API.md if support claims affect public API expectations;
- plans/subsystems/reverse-session-roadmap.md;
- plans/subsystems/reverse-session-post-closure-corrective-addendum.md;
- plans/registry.md.

Do not broaden support claims beyond direct evidence.

## 19. Acceptance criteria

C001 is complete only when:

- QUIC wrong-session, stale-generation, and replay paths have transport-specific integration evidence;
- QUIC saturation/capacity recovery has transport-specific evidence;
- QUIC half-close behavior is either demonstrated or accurately narrowed in documentation;
- pre-session QUIC handshake admission is either bounded through the supported adapter or explicitly classified/documented as a residual adapter-level risk;
- WSS close/backpressure behavior has direct integration evidence;
- proxy refusal, timeout, and cancellation have direct integration evidence;
- proxy authentication is either qualified where supported or explicitly excluded where unsupported;
- multi-hop is either directly qualified or removed from supported-release claims;
- no optional-transport secret leaks into diagnostics;
- full verification passes;
- minimal client feature isolation still holds;
- registry/roadmap/M006 gating are internally consistent;
- no unresolved high/medium finding remains.

## 20. Stop conditions

Stop and report rather than improvising if:

- closing a QUIC gap requires replacing or forking eggress-transport-quic;
- proxy-auth qualification requires implementing a second proxy protocol stack;
- a required test exposes a production correctness/security defect larger than a narrow corrective fix;
- multi-hop behavior is ambiguous in the current Eggress public API;
- WebSocket correctness would require changing the native Eggtunnel wire protocol;
- M006 release support claims would need to contradict actual transport evidence.

If a substantial product defect is found, create a new corrective milestone rather than expanding C001 silently.

## 21. Closure evidence required

Create:

- plans/closure/reverse-session-post-closure-corrective/001-status.md

It MUST record:

- baseline and final reviewed head;
- implementation commits;
- requirement-to-evidence matrix for every C001 acceptance criterion;
- exact focused test names/results;
- repeated runs for race/saturation tests where relevant;
- resource/task/capacity observations;
- Eggress adapter capabilities/limitations actually observed;
- support-claim changes;
- cargo feature/dependency evidence;
- full verification commands/results;
- CI run ID/conclusion if available;
- registry/roadmap reconciliation;
- unresolved findings classified by severity;
- disposition: closed, conditionally closed, corrective pass required, or blocked.

## 22. Handoff notes

Keep this pass evidence-driven. The optional transports already work; the goal is to prove the edge semantics that were named in the original plans and ensure the repository tells one consistent story.

Do not optimize throughput or add features while working this plan. If a test reveals a real transport defect, fix the smallest ownership-correct path and add the regression test. If a limitation belongs to the upstream Eggress adapter and cannot be corrected through its supported public API, narrow the claim and record the boundary rather than building a parallel implementation.
