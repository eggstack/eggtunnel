# Reverse Session M008 — Configurable Runtime Policy and API Composition

Status: active

Planning baseline: 2e2f3981a6e78596c32dfe5daf5b2585bfc37f1e

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#post-01-maintenance-and-evolution

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability / infrastructure

Hard dependency: M007 strict closure.

## 1. Objective

Replace hard-coded operational policy and constructor proliferation with validated, transport-neutral configuration surfaces while preserving current values and behavior as secure defaults.

The result should let embedders tune finite resource ceilings/timeouts and compose transport/identity/proxy choices without adding another constructor for every feature combination.

## 2. Why this milestone was blocked; readiness update

The work is technically feasible now, but M007 intentionally stabilizes module boundaries and continuous feature/MSRV qualification first. Performing the public/configuration refactor before that cleanup would mix structural movement with API semantics and make regression attribution harder.

M007 is strictly closed at `plans/closure/reverse-session/007-status.md`. Its hosted MSRV, feature-slice, minimal-dependency, and broad verification evidence is recorded there. The hard dependency is satisfied; this plan is ready without another architecture decision so long as work remains within ADR-0001.

## 3. Invariants

- Existing default behavior remains unchanged.
- Every production queue/task/resource remains finitely bounded.
- Configuration cannot silently disable TLS/authentication requirements or bind authorization.
- Unsupported transport combinations remain explicit errors.
- No transport-specific concrete type leaks into the general embedding API.
- Minimal client builds remain narrow.
- Existing convenience constructors remain as wrappers for at least the 0.1 compatibility line unless a closure-reviewed reason requires removal.
- No wire change.

## 4. In scope

- validated runtime resource-limit policy;
- validated timeout/lifecycle policy;
- client/server builders or equivalent typed composition surface;
- typed transport/profile selection;
- central validation of transport + mTLS + custom-CA + outbound-proxy combinations;
- CLI migration to the same programmatic validation/composition path;
- preservation of current constructors as thin compatibility wrappers;
- configuration/documentation updates and compile-contract tests.

## 5. Runtime policy model

Promote current hard-coded ceilings and timeouts into caller-supplied immutable policy objects with current values as defaults.

At minimum cover, where ownership exists today:

- sessions;
- services per Session;
- pending connections per Session;
- active connections/streams per Session;
- accepted unauthenticated handshakes;
- client Open tasks;
- control queue capacity;
- handshake timeout;
- control idle timeout;
- pending ConnectionId lifetime;
- relay drain;
- shutdown/drain grace;
- reconnect backoff bounds/heartbeat interval if already owned by the runtime.

Validation MUST reject zero/overflow/impossible combinations and must keep every value finite. Raising a default is an explicit caller decision, not an implicit environment-based behavior.

Do not make protocol frame/name/token bounds runtime configurable; those are wire/security invariants.

## 6. API composition

Introduce a typed composition surface such as builders/profiles; exact names are implementation-owned.

It should express:

- Client/Server role;
- TCP/TLS, QUIC, or WSS transport;
- CA/server-name policy where supported;
- optional mTLS identity where supported;
- optional outbound proxy where supported;
- TargetConnector;
- BindPolicy;
- runtime limits/timeouts;
- initial Services.

Unsupported combinations must fail through one canonical validator rather than scattered CLI-only branches.

Existing `Client::start_*` and `Server::bind_*` helpers should delegate to the canonical composition path where practical.

## 7. CLI/config consequence

The CLI remains a thin consumer of the library.

- TOML parsing may remain CLI-owned.
- Semantic validation of transport/profile compatibility should move to/reuse library policy.
- `eggtunnel check` and actual startup must call the same validator so a checked configuration cannot later fail because the CLI and runtime disagree.
- Secrets remain environment-sourced; do not add secret values to TOML.

## 8. Failure/cancellation/restart semantics

Changing limits/timeouts must not change ownership rules:

- rejected admission does not consume capacity;
- cancellation returns permits;
- timeout paths remain typed;
- shutdown stops admission before drain;
- reconnect uses the configured finite policy and remains cancellation-aware.

Tests should exercise non-default small limits to force deterministic saturation and cleanup.

## 9. Required focused tests

- default policy exactly matches pre-M008 effective values;
- zero/invalid/overflow policy rejection;
- small custom limits saturate and recover;
- custom handshake/pending/idle timeouts trigger the expected typed outcome;
- builder/profile matrix accepts every documented supported combination;
- builder/profile matrix rejects QUIC+custom CA, unsupported mTLS combinations, and unsupported proxy combinations exactly as documented;
- convenience constructors and builder paths are behavior-equivalent;
- CLI `check` and startup share validation results;
- minimal client feature/dependency isolation.

## 10. Required broad verification

Run the full M007 continuous matrix plus:

- compile-contract examples using builder/profile APIs;
- downstream embedder fixture using non-default policy;
- repeated small-limit lifecycle tests;
- dependency-tree comparison before/after.

## 11. Compatibility/migration

This is a pre-1.0 crate, but avoid needless churn.

- Existing config files should remain valid unless they depended on behavior already documented as unsupported.
- Existing convenience APIs should remain available as wrappers during this milestone.
- New policy types are additive.
- Any later removal/deprecation should be a separate release decision.

## 12. Documentation

Update:

- docs/API.md;
- docs/CONFIGURATION.md;
- docs/EMBEDDING.md;
- docs/OPERATIONS.md;
- docs/SECURITY.md where tunable limits affect threat modeling;
- architecture/common-core.md;
- architecture/client.md;
- architecture/server.md;
- architecture/cli-config-ops.md;
- AGENTS.md.

## 13. Acceptance criteria

- current hard-coded operational defaults are represented by explicit validated policy;
- embedders can choose finite non-default limits/timeouts programmatically;
- constructor growth is arrested by one typed composition path;
- CLI and library validation cannot drift for supported/unsupported profiles;
- security defaults remain unchanged;
- no wire change;
- no optional dependency leakage;
- no unresolved high/medium finding remains.

## 14. Stop conditions

Stop and require architecture review if:

- composition requires exposing Quinn/Tungstenite/Rustls concrete types in the general API;
- configurable limits create an unbounded mode;
- a requested profile requires changing transport/session architecture;
- preserving existing constructors makes the canonical path materially ambiguous rather than merely verbose.

## 15. Closure evidence required

Create `plans/closure/reverse-session/008-status.md` with:

- baseline/final head;
- public API and configuration diff summary;
- default-equivalence table;
- supported/rejected profile matrix;
- custom-limit/timeout test evidence;
- CLI/library validation equivalence evidence;
- feature/dependency evidence;
- exact verification commands and CI runs;
- residual findings/disposition.
