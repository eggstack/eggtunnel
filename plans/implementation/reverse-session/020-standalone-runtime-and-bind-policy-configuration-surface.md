# Reverse Session M020 — Standalone Runtime and Bind Policy Configuration Surface

Status: blocked — hard dependency M017 is not yet closed

Planning baseline: ece46fd223265b7b0609e3640b0caa9efadd1535

Source roadmap:

- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability / polish

Hard dependency: M017 strict closure.

Soft dependency: M018/M019 may proceed independently; this milestone must not depend on transport-version work unless current repository evidence changes.

## 1. Objective

Expose the already-supported finite library `RuntimePolicy` and server `BindPolicy` through the standalone TOML configuration surface without creating a second policy engine, changing secure defaults, or adding runtime hot reload.

The standalone CLI currently always installs `RuntimePolicy::default()` and reduces `BindPolicy` to the legacy `allow_public_service_binds` boolean. M020 lets operators configure the same bounded ceilings, timeouts, address allowlist, port ranges and ephemeral-port policy already available to embedders.

## 2. Why this milestone is gated on M017

M017 establishes the canonical authority semantics between:

- the compatibility `ServerConfig.allow_public_service_binds` input;
- `BindPolicy.allow_public_addresses`;
- `BindPolicy.max_services_per_session`;
- `RuntimePolicy.limits.services_per_session`.

The CLI must not expose richer policy until those semantics are singular and documented. Otherwise configuration would fossilize the current mirrored-state ambiguity.

No new library capability is required after M017; M020 is a standalone configuration/lowering capability.

## 3. Invariants that cannot regress

- absence of new policy sections produces behavior identical to current standalone defaults;
- loopback-only service binds remain the secure default;
- non-loopback exposure remains explicit and fail-closed;
- all configurable limits/timeouts remain finite and are validated by the canonical library `RuntimePolicy::validate` / `BindPolicy::validate`;
- CLI parse/resolution code does not reimplement runtime authorization semantics;
- secrets remain environment/file referenced as today;
- invalid policy fails before bind/dial/task startup;
- existing TOML files remain valid;
- no remote control, file watching, hot reload or persistent policy database is introduced;
- no wire change.

## 4. In scope

- optional TOML sections for runtime resource limits;
- optional TOML fields for lifecycle/reconnect/heartbeat timeouts;
- optional server bind-policy section for public-address permission, address allowlist, port ranges, ephemeral-port permission and authorization Service ceiling;
- deterministic parsing of IP addresses/ranges into existing library types;
- explicit compatibility behavior for the legacy `allow_public_service_binds` field;
- resolved-config and builder lowering through existing library validators;
- redacted/machine-readable `check --json` reporting of non-secret effective policy metadata where useful;
- examples/configuration/operations docs;
- parser/resolution/integration tests.

## 5. Out of scope

- runtime policy mutation after process start;
- dynamic Service add/remove through the standalone CLI;
- per-Principal or per-Service authorization rules;
- user/account/provider configuration;
- bandwidth/rate quotas not represented by current library policy;
- transport-specific policy additions;
- changing library default values;
- changing authentication throttling, which remains fixed security policy outside `RuntimePolicy`;
- service-manager generation.

## 6. Required configuration design

### A. Runtime resource limits

Add an optional structured TOML surface that maps one-to-one onto the existing `ResourceLimits` fields:

- sessions;
- services per Session;
- pending per Session;
- active connections per Session;
- accepted handshakes;
- client Open tasks;
- control queue;
- client command queue.

The exact TOML naming should be conventional and stable. Missing fields inherit `ResourceLimits::default()`; do not duplicate default numbers in multiple parsing branches when the Rust default can be used directly.

Server-only/client-only applicability must be documented, but a shared runtime section may contain the complete object if unused fields remain harmless bounded metadata. Prefer explicit rejection of clearly meaningless mode-specific fields only when that improves correctness without creating another semantic validator.

### B. Timeout/retry policy

Expose the existing `TimeoutPolicy` fields with explicit units in key names or schema documentation. Avoid an additional human-duration parser dependency unless repository evidence justifies it.

Fields:

- connect;
- handshake;
- control idle;
- pending connection;
- relay drain;
- shutdown grace;
- reconnect initial;
- reconnect max;
- heartbeat interval.

Missing values inherit `TimeoutPolicy::default()`.

The library validator remains authoritative for:

- nonzero/maximum duration bounds;
- reconnect initial <= reconnect max;
- heartbeat interval < control idle.

### C. Server BindPolicy

Add an optional server-only bind-policy section mapping to:

- `allow_public_addresses`;
- `allowed_addresses`;
- `allowed_port_ranges`;
- `allow_ephemeral_ports`;
- `max_services_per_session`.

Parse address strings using standard IP parsing and convert IPv4 addresses to the same canonical 16-byte representation expected by the library. Do not implement a separate allow/deny matcher in the CLI.

Represent port ranges structurally, not through a regex mini-language, unless current config conventions provide a stronger established form.

### D. Legacy coarse public-bind compatibility

Preserve existing TOML files using `allow_public_service_binds`.

After M017 defines canonical authority, choose one documented lowering rule that cannot create two live policy authorities. Preferred shape:

- when the richer bind-policy section is absent, the legacy boolean seeds the default BindPolicy exactly as today;
- when the richer section is present, it is the explicit BindPolicy source and contradictory simultaneous legacy configuration is either rejected or resolved by one documented precedence rule.

Do not silently OR two independently supplied public-exposure settings.

### E. Resolved configuration remains single-read and typed

Extend the M015 parse -> override -> resolve -> builder pipeline. Policy values are non-secret and require no rereads.

Lower directly into `ClientBuilder::runtime_policy`, `ServerBuilder::runtime_policy` and `ServerBuilder::bind_policy`, then call the canonical builder validator.

## 7. CLI override policy

Do not create dozens of command-line flags merely because fields exist in TOML.

M020 may add a very small set of high-value non-secret overrides only if operator evidence warrants them. The TOML surface is sufficient for closure.

Existing `--allow-public-service-binds` compatibility behavior must remain documented and consistent with the selected legacy/rich-policy precedence rule.

## 8. Ordered work packages

1. Start from M017's finalized policy-authority semantics and write the exact TOML schema/tests before runtime edits.
2. Add syntax-level optional runtime/bind-policy configuration types with defaults.
3. Lower them into library `RuntimePolicy`/`BindPolicy` without duplicating semantic validation.
4. Implement canonical IPv4/IPv6 address and structured port-range conversion.
5. Resolve the legacy public-bind flag interaction explicitly and test conflicts.
6. Extend `check` human/JSON diagnostics with bounded non-secret effective-policy information only where useful.
7. Add examples and CONFIGURATION/OPERATIONS documentation.
8. Run focused CLI/config tests, full feature/MSRV/security gates and hosted CI.
9. Create closure evidence.

## 9. Failure, cancellation, and restart semantics

- malformed policy TOML fails at parse/resolution before runtime startup;
- library-invalid limits/timeouts fail through the canonical validator before bind/dial;
- malformed address/range entries fail rather than being ignored;
- a public-bind configuration that is ambiguous or contradictory fails closed;
- restart is required for policy changes; running policy is immutable for the process lifetime;
- existing shutdown/reconnect/cancellation semantics remain unchanged.

## 10. Required focused tests

- legacy config with no new sections lowers to exact current defaults;
- partial runtime section inherits omitted default fields;
- minimum/maximum/zero/excessive resource values are rejected by library validation;
- timeout consistency rules are enforced by the library;
- IPv4, IPv6 and loopback allowlist conversion matches `BindPolicy` behavior;
- allowed port ranges and ephemeral-port switch enforce expected binds;
- richer bind policy cannot accidentally enable public exposure through legacy flag ambiguity;
- client/server mode-specific config rejects inappropriate bind-policy use;
- `check` and startup lower the same resolved policy object;
- secret redaction behavior is unchanged.

## 11. Broad verification

Run the full AGENTS.md gate, all feature slices, MSRV, minimal dependency guards, audit/license checks and exact-head hosted CI.

Run CLI integration tests using both old configuration examples and new non-default policy fixtures.

No new dependency should be required; if one is introduced solely for configuration convenience, record and justify it.

## 12. Compatibility and migration effects

Existing TOML remains valid and retains current defaults.

The new sections are additive. Operators do not need to migrate unless they want non-default policy.

No Rust API or wire change is required.

If contradictory legacy/new public-bind settings are rejected, document the conflict rule prominently; this is preferable to silent exposure.

## 13. Documentation updates

Update at minimum:

- `docs/CONFIGURATION.md`;
- `docs/OPERATIONS.md`;
- `docs/SECURITY.md`;
- `README.md` examples only if a concise non-default example is useful;
- `examples/server.toml` / `examples/client.toml` or additional policy examples;
- `architecture/cli-config-ops.md`;
- `architecture/common-core.md` for policy ownership references;
- `AGENTS.md` config gotchas.

## 14. Acceptance criteria

M020 may close only when:

- standalone config can express the existing finite RuntimePolicy without library/API duplication;
- server TOML can express the existing BindPolicy safely;
- absence of new fields is behavior-identical to current defaults;
- legacy public-bind configuration remains compatible with one unambiguous authority rule;
- invalid/unsafe combinations fail before network startup;
- check/startup share one lowering/validation path;
- no hot reload/admin/auth-provider scope is added;
- exact-head full/MSRV/feature/security/license/hosted gates pass;
- no unresolved high/medium configuration/security finding remains.

## 15. Stop conditions

Stop and require a separate plan/ADR if:

- richer policy requires per-Principal trust semantics;
- a public API change is required to represent configuration safely;
- policy must become runtime-mutable;
- a generalized remote administration protocol becomes necessary;
- a new persistent policy store is proposed.

## 16. Closure evidence required

Create `plans/closure/reverse-session/020-status.md` with:

- baseline/final head;
- old/new TOML schema and default-equivalence evidence;
- legacy/rich public-bind precedence matrix;
- non-default RuntimePolicy/BindPolicy integration tests;
- fail-closed invalid-policy cases;
- full local/hosted gate results;
- compatibility notes and unresolved findings.

## 17. Handoff notes

This milestone should expose existing library policy, not invent a new standalone policy model. Prefer direct typed lowering and library validation over CLI-side semantic checks.
