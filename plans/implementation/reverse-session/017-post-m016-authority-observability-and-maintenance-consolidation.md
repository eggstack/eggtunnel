# Reverse Session M017 — Post-M016 Authority, Observability, and Maintenance Consolidation

Status: ready

Planning baseline: ece46fd223265b7b0609e3640b0caa9efadd1535

Source roadmap:

- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md

Applicable ADRs:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md
- plans/adrs/ADR-0002-capability-negotiated-minor-protocol-evolution.md

Primary class: polish / invariant

Hard dependency: M016 strict closure and C001 optional-transport corrective closure.

## 1. Objective

Resolve the concrete post-M016 ownership and maintenance defects found by the repository audit without adding a new tunnel capability, changing wire semantics, or broadening Eggtunnel into a general proxy/runtime platform.

The milestone has four outcomes:

1. make server policy authority explicit instead of maintaining mirrored configuration state by convention;
2. make the standalone operational JSON surface a faithful, versioned projection of the bounded library Snapshot;
3. reduce the remaining CLI/shared-control maintenance concentration along already-established ownership seams;
4. remove small production invariant traps and repeated data-plane constants that weaken the otherwise fail-closed runtime posture.

This is the cleanup gate before additional standalone policy exposure or transport-boundary work.

## 2. Why this milestone is dependency-ready

M001-M016 and C001 are closed. The repository is not missing a functional reverse-tunnel core; the remaining findings are concrete maintenance defects in the current implementation:

- `ServerConfig.allow_public_service_binds` and `BindPolicy.allow_public_addresses` represent the same coarse public-bind choice. `ServerBuilder::bind_policy()` mutates the former to keep both values synchronized and `validate_server_profile()` rejects disagreement, leaving two mutable representations of one authority.
- Service admission uses `min(BindPolicy.max_services_per_session, RuntimePolicy.limits.services_per_session)` without clearly naming the distinct policy-vs-resource semantics of those ceilings.
- the library `Snapshot` contains current/high-water resource counts, resource limits, heartbeat RTT/last-Pong state, bytes, termination category and effective binds, while the CLI `eggtunnel.events/v1` snapshot renderer omits part of that bounded state despite documentation describing it as the same snapshot surface.
- the CLI maps both `TunnelError::Authentication` and `TunnelError::Authorization` to the machine-readable `authentication` category even though the runtime deliberately distinguishes identity failure from Service/bind authorization failure.
- `crates/eggtunnel-cli/src/main.rs` is approximately 1.5 kLOC and owns parsing, overrides, resolution, builder lowering, output DTOs, runtime loops and tests in one module.
- `common.rs` remains a broad shared owner, and `server/control.rs` has accumulated enough per-Session state that `register_service` requires a `too_many_arguments` exception.
- production paths retain a small number of logic-invariant `expect`/`unreachable!` sites, and client/server relay paths duplicate the same fixed 16 KiB `RelayOptions` construction.
- public `ServiceSpec` remains reserved future vocabulary and is load-bearing nowhere; its compatibility status should be made explicit without a breaking removal.

These changes can be implemented against the current public/wire contract with no new ADR.

## 3. Invariants that cannot regress

- Wire protocol remains v1.1 with v1.0 fallback, message IDs 1-15 unchanged, and ADR-0002 capability semantics unchanged.
- Existing public Rust constructors/builders/handles and existing TOML/CLI command forms remain source-compatible.
- `ServerConfig.allow_public_service_binds` remains accepted as the compatibility coarse public-bind seed; existing callers must not gain public exposure they did not request.
- `BindPolicy` remains the server authorization object and `RuntimePolicy` remains finite runtime resource/lifecycle policy.
- Authentication and authorization remain distinct runtime concepts.
- No new remote control plane, hot reload, daemon API, persistence layer, user database, or Principal-provider system is introduced.
- Generic relay remains owned by `eggress-relay`; no local byte-relay implementation is added.
- Every production queue/task/session/Service/pending correlation/connection remains bounded.
- Minimal/client-only feature slices remain isolated exactly as qualified by current CI.
- Secret values and private-key/proxy material remain absent from Debug, JSON events, diagnostics and tracing.

## 4. In scope

- one canonical server public-bind policy authority after builder construction;
- explicit semantics and one helper/path for effective Service admission when authorization and runtime ceilings both apply;
- versioned CLI operational DTO conversion from `Snapshot` with complete bounded state coverage;
- authentication-vs-authorization CLI error-category correction;
- private CLI module decomposition;
- targeted shared-core/control-loop decomposition where it removes duplicated ownership rather than merely moving lines;
- production invariant-panic cleanup;
- central relay options/buffer ownership;
- `ServiceSpec` compatibility/disposition documentation;
- architecture/operations/API documentation reconciliation;
- exact-head qualification.

## 5. Out of scope

- exposing all `RuntimePolicy` or `BindPolicy` fields in TOML/CLI;
- dynamic standalone Service administration;
- multi-tenant Principal/auth-provider policy;
- Eggress version changes or transport dependency changes;
- QUIC trust-profile expansion;
- Windows/musl/armv7 release expansion;
- protocol changes;
- removal of existing public fields/types before a separately planned breaking release.

## 6. Required production changes

### A. Canonicalize public-bind policy ownership

Treat `ServerConfig.allow_public_service_binds` as compatibility input used to seed the default `BindPolicy` in `ServerBuilder::new`.

After a builder owns a `BindPolicy`:

- `BindPolicy` is the single runtime authorization authority;
- `ServerBuilder::bind_policy()` must not need to mutate a second field merely to maintain equality;
- validation must not depend on a synchronized mirror after policy construction;
- existing direct `Server::bind(config)` behavior remains identical because its builder is seeded from the legacy field;
- documentation must state the compatibility role of the legacy coarse flag and the authoritative role of `BindPolicy`.

Do not remove or rename the public field in the 0.2 line.

### B. Make Service ceilings semantically explicit

Preserve both existing public fields without pretending they mean the same thing:

- `BindPolicy.max_services_per_session` is the authorization/policy ceiling;
- `RuntimePolicy.limits.services_per_session` is the runtime resource ceiling.

Centralize their intersection in one named helper or admission owner so the `min(...)` rule is not re-derived at call sites. Tests must cover asymmetric values in both directions and prove the lower ceiling wins for the intended reason.

If implementation evidence shows `BindPolicy.max_services_per_session` cannot be given an independent authorization meaning, stop and record a public-API migration proposal rather than silently ignoring one field.

### C. Canonical versioned operational Snapshot DTO

Create one CLI-owned conversion from `eggtunnel::Snapshot` to the `eggtunnel.events/v1` snapshot event. It must include the bounded state already exposed by the library, including at least:

- current resource counts;
- high-water resource counts;
- `ResourceLimits`;
- reconnect/rejection/byte counters;
- last termination category;
- heartbeat generation, last-Pong age, latest RTT and missed count;
- effective binds.

The JSON DTO must remain independent of direct `Serialize` on the public Rust `Snapshot`; the external event schema is an explicit CLI contract, not an accidental serialization of Rust layout.

Additive v1 fields are permitted. Any incompatible JSON change requires a schema version change.

### D. Preserve authn/authz distinction at the CLI boundary

Give `TunnelError::Authorization` a distinct stable machine-readable category from `Authentication`. Review related `ServiceAlreadyExists`, bind validation and runtime-start mappings so each category reflects the layer that actually rejected the operation.

Human diagnostics remain secret-safe and need not expose additional internal detail.

### E. Decompose the CLI by responsibility

Split the current single-file implementation into private modules with no behavior change. The exact names are implementation-owned, but the resulting boundaries should separately own:

- syntax/override types;
- single-read resolution and secret/file access;
- lowering into library builders;
- check/runtime event DTOs and rendering;
- client/server command loops;
- focused tests near the responsibility they qualify.

Keep `main.rs` as dispatch/composition rather than a second policy engine.

Do not move reusable runtime policy into the CLI crate merely to make the split convenient.

### F. Tighten shared/control ownership only where evidence supports it

Perform a focused decomposition of `common.rs` and/or `server/control.rs` only for already-distinct responsibilities.

In particular, prefer a private Session-control context/state owner over passing policy, capability, writer budget, service/name tables and child-task ownership independently into `register_service`.

Do not introduce a general proxy trait, generic dependency injection framework or public control-session API.

### G. Remove invariant traps and duplicated relay constants

Replace production `expect`/`unreachable!` sites that depend on mutable runtime invariants with typed/fail-closed handling where practical. Test-only assertions may remain.

Define the Eggtunnel relay buffer/options policy once and use it from both client and server relay paths. The current effective 16 KiB buffer and configured `relay_drain` semantics are compatibility constraints for this milestone; this is not a performance-tuning pass.

### H. Clarify orphaned public vocabulary

Audit `ServiceSpec` against docs and public usage.

Because it is already public, do not remove it in M017. Either:

- retain it as explicitly reserved future server-policy vocabulary with no runtime authority; or
- mark it deprecated with a migration note if repository/downstream evidence shows that is the safer path.

No future multi-tenant semantics may be smuggled into M017 through this cleanup.

## 7. Ordered work packages

1. Capture exact pre-change behavior for public-bind and dual Service ceilings.
2. Canonicalize builder-time bind-policy ownership and add asymmetric ceiling tests.
3. Add the canonical `Snapshot -> events/v1` DTO conversion and authn/authz error mapping.
4. Split CLI parsing/resolution/output/runtime responsibilities without semantic changes.
5. Introduce a private Session-control state/context only as needed to remove argument/ownership duplication.
6. Centralize relay options and remove production invariant panics.
7. Reconcile `ServiceSpec` documentation/status.
8. Update architecture, API, configuration and operations docs.
9. Run focused, broad, feature-slice, MSRV, dependency, audit/license and hosted CI qualification.
10. Create closure evidence.

## 8. Failure, cancellation, and restart semantics

M017 must preserve existing behavior:

- invalid public-bind policy fails before listener creation;
- Service saturation fails without leaking listeners/tasks/counters;
- authorization rejection stays terminal where currently terminal and is not reclassified as authentication internally;
- JSON rendering cannot mutate runtime state;
- broken output does not bypass joined shutdown semantics;
- cancellation while registering/opening/relaying remains bounded by current policy;
- reconnect restores only acknowledged desired Services;
- all Session-scoped binds/counters are cleared on Session replacement.

Refactors must be regression-proven rather than inferred from unchanged signatures.

## 9. Required focused tests

At minimum:

- `ServerBuilder::new` seeds `BindPolicy` from the legacy config flag;
- an explicit `bind_policy()` is authoritative without mirrored-state drift;
- loopback/public address matrix remains fail-closed;
- authorization ceiling < runtime ceiling and runtime ceiling < authorization ceiling both reject at the expected effective count;
- default effective Service ceiling remains 64;
- JSON snapshot contains every intended v1 bounded field and no secret-bearing field;
- high-water/resource-limit/RTT/last-Pong fields are present;
- authentication and authorization produce distinct stable categories;
- CLI module split preserves parse/override/single-read behavior;
- invariant-error branches do not panic;
- both client/server relay paths use the same fixed Eggtunnel relay policy.

## 10. Broad verification

Run the repository gate in AGENTS.md, including:

- fmt/check/test/clippy/rustdoc;
- embedder fixture;
- all 14 feature slices;
- MSRV 1.89 slices;
- minimal-dependency guards;
- `cargo audit`;
- `cargo deny check licenses`.

Also run CLI integration tests covering human and JSON `check`, client and server output, and the existing deterministic Service-state/selected lifecycle qualification relevant to moved code.

Hosted CI must pass on the exact candidate head before closure.

## 11. Compatibility and migration effects

No wire migration and no TOML migration.

Existing `ServerConfig` callers retain the same coarse public-bind behavior. Existing explicit `BindPolicy` callers retain or gain clearer precedence semantics without additional exposure.

Operational JSON gains missing bounded fields additively under `eggtunnel.events/v1`; consumers must continue to tolerate additive fields.

No public Rust item is removed.

## 12. Documentation updates

Update at minimum:

- `docs/API.md`;
- `docs/CONFIGURATION.md`;
- `docs/OPERATIONS.md`;
- `docs/SECURITY.md` where policy authority is described;
- `architecture/common-core.md`;
- `architecture/server.md`;
- `architecture/cli-config-ops.md`;
- `architecture/overview.md` if module topology changes;
- `AGENTS.md` path/gotcha references.

## 13. Acceptance criteria

M017 may close only when:

- server bind authorization has one runtime authority after builder construction;
- dual Service ceilings have explicit non-duplicative semantics and one effective-admission path;
- CLI snapshot JSON faithfully exposes the intended bounded Snapshot state;
- authentication and authorization are not collapsed in machine-readable errors;
- CLI/control/shared ownership is materially easier to review without public/wire behavior change;
- no production invariant panic remains in the touched paths where a typed fail-closed path is practical;
- relay options are defined once;
- `ServiceSpec` compatibility status is explicit;
- all standard/feature/MSRV/security/license/downstream gates and exact-head hosted CI pass;
- no unresolved high/medium correctness, security or compatibility finding remains.

## 14. Stop conditions

Stop and write a separate plan/ADR if implementation requires:

- removing/renaming a published public field or type;
- changing authentication trust or Principal authorization;
- changing wire behavior;
- introducing config hot reload or remote control;
- changing relay buffer/drain behavior for performance reasons;
- moving generic proxy/transport ownership from Eggress into Eggtunnel.

## 15. Closure evidence required

Create `plans/closure/reverse-session/017-status.md` with:

- baseline/final head;
- before/after policy-authority diagram;
- asymmetric Service-ceiling evidence;
- JSON schema/field coverage and redaction evidence;
- authn/authz category evidence;
- module-topology summary;
- focused regression commands;
- broad local/hosted gate results;
- compatibility notes and unresolved findings.

## 16. Handoff notes

Prefer deleting synchronization obligations over adding more validation between mirrors. Preserve published compatibility fields as input shims where necessary, but make runtime authority singular and obvious.
