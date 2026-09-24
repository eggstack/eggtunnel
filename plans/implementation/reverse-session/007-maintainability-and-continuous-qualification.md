# Reverse Session M007 — Maintainability and Continuous Qualification

Status: closed

Planning baseline: 2e2f3981a6e78596c32dfe5daf5b2585bfc37f1e

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#post-01-maintenance-and-evolution

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: polish / invariant

## 1. Objective

Reduce post-0.1 maintenance risk without changing the Eggtunnel wire protocol or supported transport semantics.

This milestone decomposes the oversized runtime/test topology, makes the declared Rust/feature support contract continuously executable in CI, removes the direct unmaintained PEM-parser dependency, and tightens the Eggtunnel/Eggress ownership boundary so later work does not grow a second generic reverse-proxy stack.

## 2. Why this milestone is dependency-ready

M001-M006 and C001 are closed. The current head passes hosted CI and has a published 0.1.0 release.

Current repository evidence at the planning baseline:

- `crates/eggtunnel/src/server.rs` is about 4,500 lines, with the inline test module beginning near line 1,306;
- most cross-transport integration tests live inside `server.rs`, which makes client-only behavior difficult to qualify independently;
- CI uses the current stable toolchain even though the workspace declares Rust 1.89 as MSRV;
- historical closure evidence exercised important feature slices that are not all continuously checked by CI;
- `rustls-pemfile 2.2.x` is a direct dependency and is reported as unmaintained by `cargo audit`;
- Eggress already owns a pproxy-compatible reverse/backward proxy implementation, while Eggtunnel owns the distinct persistent multi-Service Session model defined by ADR-0001.

No architecture redesign is required to address these findings.

## 3. Invariants that cannot regress

- Wire version, message IDs, DTO meanings, Session/Service/ConnectionId semantics, and transport behavior remain unchanged.
- No custom TCP multiplexer.
- No dependency on `egress-embed` or `eggress-protocol-reverse` is introduced for Eggtunnel-native sessions.
- Minimal `client,tls` builds remain free of server/QUIC/WebSocket/outbound-proxy code.
- No library-owned Tokio runtime or global tracing subscriber.
- Existing security/resource ceilings and secret-redaction behavior remain unchanged in this milestone.
- Existing public APIs remain source-compatible unless an unavoidable correctness issue is found; ergonomic API work belongs to M008.
- Closed M001-M006/C001 evidence is not rewritten.

## 4. In scope

- split the monolithic inline integration-test topology into focused integration/support modules;
- decompose server implementation responsibilities where this can be done mechanically without changing semantics;
- add continuous MSRV qualification;
- add continuous feature-slice qualification for supported library profiles;
- make rustdoc warnings a CI failure;
- replace the direct unmaintained PEM parser with a maintained, ownership-correct parsing path;
- preserve dependency isolation with explicit checks;
- clarify Eggtunnel vs Eggress reverse-session ownership in architecture/developer documentation;
- normalize stale planning/verification text discovered while making these changes.

## 5. Out of scope

- runtime-configurable resource/time policies;
- constructor/builder redesign;
- dynamic runtime Service registration;
- heartbeat RTT/health state;
- new protocol capabilities or wire-version behavior;
- Eggpack migration;
- new transports;
- performance optimization except measurements needed to prove no material regression.

## 6. Required production changes

### A. Test topology

Move transport/lifecycle integration tests out of `server.rs` into focused files or private test modules with a small shared test-support layer.

The split SHOULD separate at least:

- TCP/TLS session and lifecycle;
- authentication/bind/resource security;
- mTLS;
- QUIC;
- WebSocket;
- outbound proxy;
- shared certificates/echo targets/proxy fixtures.

Do not expose production-only internals merely to make tests compile. Test-only seams may remain `cfg(test)` and must not leak into release builds.

### B. Runtime module decomposition

Decompose `server.rs` only where a stable responsibility boundary already exists. Candidate boundaries include admission/authentication state, Session/pending state, transport accept loops, and service-listener lifecycle.

The implementation agent may choose exact module names after inspecting current ownership. The goal is reduced review surface, not arbitrary file-count growth.

### C. Continuous support-contract CI

Add a dedicated Rust 1.89 lane using the declared MSRV. It should compile the publishable library/protocol crates and representative minimal profiles without requiring optional transports that cannot build on that lane.

Continuously exercise at least:

```text
client,tls
client,server,tls
client,server,tls,mtls
client,server,tls,quic
client,server,tls,websocket
client,tls,outbound-proxy
client,server,tls,websocket,outbound-proxy
```

Use `--no-default-features` where appropriate. Full all-feature CI remains required.

Set `RUSTDOCFLAGS="-D warnings"` for documentation CI.

Add dependency evidence ensuring the minimal `client,tls` graph does not pull QUIC, WebSocket, outbound-proxy, server-only, or Eggress reverse-protocol crates.

### D. PEM parser maintenance

Replace direct `rustls-pemfile` usage with a maintained parser or a maintained API already available through the selected TLS stack.

Requirements:

- preserve accepted certificate/private-key forms unless intentionally documented;
- preserve secret redaction/zeroization expectations;
- add malformed/empty/multiple-key negative tests;
- remove the direct unmaintained dependency if no longer needed;
- do not vendor a parser.

If the only maintainable alternative materially enlarges the minimal dependency graph, stop and record the tradeoff rather than silently widening it.

### E. Eggress boundary guard

Document and test the distinction:

- Eggress reverse/backward protocol: compatibility/simple backward proxy behavior;
- Eggtunnel: native persistent authenticated Session, multi-Service registration, server-owned listeners, ConnectionId correlation, transport-neutral embedding.

Eggtunnel may continue using narrow Eggress relay/TLS/QUIC/WebSocket/outbound crates. It must not absorb Eggress compatibility behavior or depend on `eggress-protocol-reverse` merely to share control-plane code.

## 7. Failure/cancellation/restart semantics

This is intended to be behavior-preserving. Existing cancellation, reconnect, shutdown, pending-entry lifetime, permit release, and half-close tests must continue to pass after module/test movement.

Any newly exposed regression in teardown/resource accounting is a correctness defect and may be fixed narrowly inside M007. A larger semantic change requires a corrective or later milestone.

## 8. Required focused tests

- all moved tests execute under their intended feature gates;
- client-only validation/reconnect/connector tests exist where behavior can be tested without enabling `server`;
- PEM parser accepts the documented valid fixtures and rejects malformed/ambiguous material;
- minimal dependency graph excludes optional transports and `eggress-protocol-reverse`;
- each supported feature slice compiles/tests in CI;
- Rust 1.89 lane passes.

## 9. Required broad verification

At minimum:

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
cargo check --locked --manifest-path fixtures/embedder/Cargo.toml
cargo audit
cargo deny check licenses
```

Also run the supported feature-slice matrix and a Rust 1.89 check. Record exact commands in closure evidence.

## 10. Documentation updates

Update as needed:

- AGENTS.md;
- architecture/overview.md and affected deep dives;
- docs/ARCHITECTURE.md;
- docs/EMBEDDING.md;
- docs/DISTRIBUTION.md if the audit warning changes;
- plans/subsystems/reverse-session-roadmap.md;
- plans/registry.md.

Do not turn private module layout into a public compatibility promise.

## 11. Acceptance criteria

- production behavior and wire compatibility are unchanged;
- server/runtime responsibilities are materially easier to review;
- integration tests no longer make `server.rs` the single home for all transport tests;
- declared Rust 1.89 MSRV is continuously checked;
- supported feature slices are continuously checked;
- rustdoc warnings fail CI;
- the direct unmaintained PEM parser is removed or a documented stop condition explains why replacement is currently worse;
- the minimal dependency graph remains narrow;
- Eggtunnel/Eggress reverse ownership is explicit and no duplicate compatibility stack is introduced;
- no unresolved high/medium correctness/security finding remains.

## 12. Stop conditions

Stop and report rather than broadening the milestone if:

- module decomposition requires a public API redesign;
- replacing the PEM parser requires weakening certificate/key validation;
- the declared MSRV cannot support a currently published direct dependency;
- feature-slice CI reveals a semantic defect requiring wire/API redesign;
- avoiding Eggress reverse overlap would require changing ADR-0001.

## 13. Closure evidence required

Create `plans/closure/reverse-session/007-status.md` with:

- baseline, implementation commits, and final reviewed head;
- before/after module/test topology;
- exact CI/MSRV/feature-slice commands and hosted run IDs;
- dependency-tree evidence for minimal client;
- `cargo audit`/deny outcomes and PEM-parser disposition;
- requirement-to-test matrix;
- documentation/boundary evidence;
- residual findings by severity;
- disposition.
