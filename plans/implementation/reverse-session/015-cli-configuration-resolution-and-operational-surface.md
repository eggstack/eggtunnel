# Reverse Session M015 — CLI Configuration Resolution and Operational Surface

Status: closed

Planning baseline: ae35859c00089784254b6a078a5a519420de1994

Source roadmap:

- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability / polish

Hard dependency: M014 strict closure.

## 1. Objective

Bring the standalone Eggtunnel CLI up to the operational contract already implied by the long-term specification and the library's 0.2 observability surface, without adding a daemon control plane or moving application policy into the CLI.

The milestone should make configuration resolution deterministic and single-pass, add machine-readable validation/startup output, and provide explicit non-secret command-line overrides suitable for service managers and automation.

## 2. Why this milestone exists

At the 0.2.0 baseline:

- `eggtunnel check` performs CLI-specific structural validation and then calls library builder validation;
- startup calls `check_config` and then resolves/builds the configuration again;
- token environment variables, proxy environment values, certificate/key/CA files may therefore be read multiple times between validation and use;
- endpoint validation logic overlaps library validation;
- CLI output is human-readable only;
- the long-term specification requires machine-readable JSON for state/check output where useful and recommends explicit command-line overrides;
- the library already exposes bounded `Snapshot` state, tracing events, heartbeat health, effective binds, byte counters, rejection counters, and resource ceilings, but the standalone CLI exposes only a small subset.

M015 should improve the standalone operator experience without creating a second configuration semantics layer.

## 3. Invariants that cannot regress

- Library builders remain the authoritative semantic validators for transport/profile/runtime-policy combinations.
- Secrets are not accepted as plaintext CLI arguments by default and never appear in JSON/human diagnostics.
- Configuration/environment/file resolution occurs before runtime tasks are spawned.
- Client/server library APIs remain process-neutral and do not gain CLI/config-file dependencies.
- Existing TOML configuration remains accepted.
- Existing `version`, `check`, `client`, and `server` command forms remain usable.
- No persistent admin socket, HTTP API, Prometheus exporter, database, service manager, or self-update engine is introduced.
- No wire change.
- Runtime policy defaults and transport support matrix remain unchanged unless explicitly overridden through already-supported library policy.

## 4. In scope

- one CLI resolution pipeline from file + environment + overrides to a resolved client/server launch configuration;
- single-read secret/certificate/proxy resolution;
- structured validation diagnostics;
- JSON output for `check` and startup/status events where useful;
- explicit non-secret CLI overrides;
- optional rendering of bounded library snapshots during standalone operation without an external control protocol;
- tests for precedence, redaction, environment/file races, and machine-readable output;
- operator documentation/examples.

## 5. Out of scope

- changing the library wire protocol;
- remote status/control API;
- dynamic config file watching/reload;
- account management or Principal authorization;
- persisting tokens;
- accepting token values directly on command lines;
- service-manager installation;
- Eggup self-update;
- ACME/certificate enrollment;
- logging subscriber policy beyond a minimal CLI-owned subscriber if one already exists or is deliberately added as CLI-only behavior.

## 6. Required production changes

### A. Parse, resolve, validate, launch pipeline

Replace the current repeated `read_config -> check_config -> builder` resolution with explicit stages:

1. parse TOML into syntax-level `FileConfig`;
2. apply command-line overrides;
3. resolve environment references and file contents exactly once into a redacted in-memory resolved config;
4. construct `ClientConfig`/`ServerConfig`, builders, policy and transport profile;
5. invoke the canonical library `validate()`;
6. launch using the already-resolved values.

The resolved config must not derive `Debug`/serialization that can reveal token bytes, private keys, proxy credentials, or full secret-bearing URIs.

If an input changes after resolution, the running process must continue using the resolved snapshot rather than silently rereading it during launch.

### B. Override semantics

Add explicit command-line overrides for operationally useful non-secret fields. At minimum consider:

- mode-specific listen/server endpoint;
- TLS server name;
- transport profile;
- config-selected certificate/CA/key paths;
- service bind port(s) only if a deterministic service selector is defined;
- public-bind coarse switch;
- environment-variable *name* used for token/proxy lookup.

Do not add `--token <secret>` or proxy-password flags that predictably leak to process listings/shell history.

Precedence must be documented and tested as:

CLI override > TOML field/default > built-in default.

Reject overrides that are meaningless for the selected mode/profile through the library validator where possible.

### C. Machine-readable check output

Add a stable JSON mode for `eggtunnel check`, for example `eggtunnel check --json <config>` or an equivalent conventional flag.

The schema should be intentionally small and versionable, containing fields such as:

- `ok`;
- mode;
- selected transport profile;
- number of configured Services;
- whether custom CA/mTLS/proxy is configured as booleans only;
- validation error category/code when invalid.

Do not serialize raw configuration or secret-bearing paths/values unless the field is explicitly assessed as non-sensitive.

Human output remains the default for compatibility.

### D. Runtime/startup operational output

Provide a JSON output mode usable by automation for standalone client/server processes.

At minimum emit bounded structured events for:

- process/version/config validated;
- server ingress listener address;
- Service effective bind announcements;
- client authenticated Session ready/disconnected/reconnect state where exposed by snapshots;
- shutdown/termination category.

Avoid an unbounded in-memory event history. Output should be streamed to stdout/stderr or derived periodically from bounded `Snapshot`.

If periodic snapshot output is added, require an explicit interval flag and enforce a sane minimum interval to avoid accidental log amplification.

### E. Error taxonomy at the CLI boundary

Map parse/resolution/library errors into stable coarse categories suitable for JSON without exposing secret-bearing underlying strings.

Examples:

- config_parse;
- config_resolution;
- missing_secret_reference;
- tls_material;
- profile_validation;
- bind_validation;
- runtime_start;
- transport;
- authentication/authorization where known.

Exit status should remain nonzero for invalid configuration/start failure.

## 7. Ordered work packages

1. After M014, inspect the canonical library validation/endpoint helpers and define the CLI resolved-config types.
2. Refactor config resolution so environment/files are read once.
3. Add override precedence with redaction-safe diagnostics.
4. Add `check` JSON schema and tests.
5. Add runtime/startup JSON event rendering using existing handles/snapshots.
6. Remove redundant CLI validation now owned by builders while retaining syntax/mode-specific parse errors that builders cannot express.
7. Update CONFIGURATION/OPERATIONS/README examples.
8. Run full workspace and CLI integration qualification and create closure evidence.

## 8. Failure and cancellation semantics

- A failed environment/file resolution prevents runtime startup.
- Missing token/proxy environment references are deterministic config-resolution errors.
- A Ctrl-C during client/server operation retains current graceful shutdown behavior.
- JSON rendering failure must not corrupt tunnel lifecycle state; ordinary stdout broken-pipe behavior may terminate the CLI cleanly.
- Invalid override combinations fail before socket bind/connect.
- The CLI must never fall back to direct networking when configured proxy resolution/validation fails.
- Validation must not mutate persistent state.

## 9. Required focused tests

- TOML-only config produces the same library builder profile as before M015;
- each override wins over TOML and does not affect unrelated fields;
- token/proxy environment names are resolved once and secret values are absent from Debug/JSON/error output;
- cert/key/CA files are read once per resolution and empty/malformed material fails deterministically through the appropriate layer;
- check JSON is valid, stable, and redacted on both success and representative failures;
- human check output remains available;
- runtime JSON effective-bind events correspond to `Snapshot.effective_binds`;
- invalid mode/profile combinations are rejected by the same library validator used for runtime startup;
- IPv6/DNS endpoint behavior matches M014 library semantics.

## 10. Broad verification

Run M014's full workspace/MSRV/feature/security/license gates plus:

- CLI command integration tests for `version`, human/JSON `check`, `client`, and `server`;
- golden or schema-focused tests for JSON output that avoid brittle incidental ordering;
- secret-redaction scans over representative CLI stdout/stderr;
- installer smoke to ensure command-line compatibility of the shipped binary;
- hosted CI on the exact candidate head.

## 11. Compatibility and migration effects

- Existing TOML files and command names remain valid.
- New JSON/override flags are additive.
- The CLI may produce more structured output only when explicitly requested.
- No library or wire compatibility change.
- If a previously accepted but semantically invalid configuration was only passing because CLI and library validation drifted, rejection after unification is a correctness fix and must be documented.

## 12. Acceptance criteria

M015 may close only when:

- config/env/file resolution has one execution path and one resolved snapshot;
- canonical library validation owns transport/profile semantics;
- useful check/startup state is available as redacted machine-readable JSON;
- explicit non-secret overrides have documented/tested precedence;
- existing human/TOML workflows remain compatible;
- no secret is exposed in process arguments, JSON, Debug, or diagnostics by the new surface;
- no new process-global or remote-control dependency is introduced;
- full exact-head CI passes with no unresolved high/medium finding.

## 13. Stop conditions

Stop and create a separate plan/ADR if the work requires:

- remote runtime control/status protocol;
- config hot reload with transactional Service mutation;
- authentication/account management;
- self-update/service manager ownership;
- changing library public ownership boundaries;
- changing wire semantics.

## 14. Closure evidence required

Create `plans/closure/reverse-session/015-status.md` with:

- baseline/final head;
- before/after config-resolution flow;
- override precedence matrix;
- JSON schema/examples with secrets demonstrably absent;
- CLI integration test outcomes;
- full workspace/hosted verification;
- compatibility notes and unresolved findings.

## 15. Handoff notes

Treat the CLI as an adapter over the library, not a second policy engine. Prefer resolving inputs into existing typed library objects and deleting redundant semantic checks rather than reproducing builder validation.
