# M008 Closure Status — Configurable Runtime Policy and API Composition

Status: closed

Implementation plan: `plans/implementation/reverse-session/008-configurable-runtime-policy-and-api-composition.md`.

## Baseline and implementation

- Planning baseline: `2e2f3981a6e78596c32dfe5daf5b2585bfc37f1e`.
- Implementation commit: `d9c7cda` (`feat: complete M008 runtime policy and builders`).
- Follow-up correctness commit: `645d2a3` (`fix: report configured control idle timeout`).
- Hosted-qualified head: `645d2a36bfac0700c33ef551354956fbe7121708`.
- This record and the M009 readiness transition are planning-only; no production code changed after hosted qualification.

## Public API and configuration changes

- Added public `RuntimePolicy`, `ResourceLimits`, and `TimeoutPolicy`, all finite and validated. Configured capacities are constrained to `1..=65536`; durations are positive and at most 24 hours; reconnect bounds and heartbeat/control-idle ordering are checked.
- Added `ClientBuilder` / `ClientTransportProfile` and `ServerBuilder` / `ServerTransportProfile`. The builders compose typed transport selection, connector/bind policy, runtime policy, and optional mTLS/proxy settings without exposing concrete Eggress adapter types.
- Existing `Client::start_*` and `Server::bind_*` entry points delegate through the builders. The CLI translates TOML to those same builders; `check_config` and startup both call `validate()`.
- The embedder fixture now configures a non-default runtime service limit through the public builder API.
- CLI TOML stays on the existing defaults; no secret values or new TOML secret fields were added.
- No Cargo dependency or lockfile change; no protocol DTO/message change.

## Default-equivalence evidence

| Runtime policy | Default | Pre-M008 effective value / source |
|---|---:|---|
| Sessions | 128 | Server session admission ceiling |
| Services per Session | 64 | Client config, server `BindPolicy` default, and registration ceiling |
| Pending Connections per Session | 128 | Pending ConnectionId admission |
| Active Connections per Session | 128 | TCP connection and QUIC stream admission |
| Accepted handshakes | 64 | Pre-authentication admission |
| Client Open tasks | 128 | Client Open semaphore |
| Protocol control queue | 128 | Client outbound and server Open queues |
| Client command queue | 32 | `ClientHandle` command channel |
| Connect / handshake | 10 s / 10 s | Existing connect and TLS/protocol bounds |
| Control idle / pending lifetime | 90 s / 30 s | Existing control idle and pending ConnectionId expiry |
| Relay drain / shutdown grace | 15 s / 1 s | Existing relay and drain behavior |
| Reconnect initial / maximum | 500 ms / 30 s | Existing bounded retry policy |
| Heartbeat interval | 20 s | Existing client Ping cadence |

Unit tests pin the defaults and reject zero, excessive, and inconsistent policy values. The configured control-idle expiry now exits as `TunnelError::Timeout` and records `TerminationCategory::Timeout`; the 400 ms integration case verifies it.

## Supported and rejected profile matrix

| Client profile | Supported options | Rejected combinations |
|---|---|---|
| TCP/TLS | System or custom CA; optional mTLS identity; optional outbound proxy; custom `TargetConnector` | Outbound proxy + mTLS |
| QUIC | Platform roots; custom `TargetConnector` | Custom CA, mTLS, outbound proxy |
| WebSocket/TLS | System or custom CA; optional outbound proxy; custom `TargetConnector` | mTLS |

| Server profile | Supported options | Rejected combinations |
|---|---|---|
| TCP/TLS | Standard bearer auth; optional trusted client CA/mTLS; `BindPolicy` | Empty client CA |
| QUIC | Standard bearer auth; `BindPolicy` | mTLS/client CA |
| WebSocket/TLS | Standard bearer auth; `BindPolicy` | mTLS/client CA |

Builder tests exercise each supported base profile and the unsupported combinations. CLI profile construction calls the exact public builder validators used again by `start()` / `bind()`; the former CLI-only transport compatibility checks were removed.

## Limit, timeout, and lifecycle evidence

- Custom pending limit saturates at one pending ConnectionId, rejects the next admission, and recovers capacity after cleanup.
- Custom pending expiry removes the pending entry and releases capacity.
- Custom handshake timeout is reported as the typed timeout category.
- Custom control-idle timeout terminates the established session with a typed timeout result.
- Those custom-policy tests passed three repeated serial runs before the idle test was added; the idle test then passed directly, and the final workspace test suite passed 74 tests.
- Tests confirm default legacy constructors use the default builder policy path; application connector, server bind, and client registration/relay cases remain in the all-feature suite.

## Feature, dependency, and hosted qualification

- Local Rust 1.89 checks passed for `eggtunnel-proto`, minimal `client,tls`, and `client,server,tls`.
- Local minimal `client,tls` dependency tree contains none of QUIC, WebSocket, outbound-proxy, `eggress-protocol-reverse`, or `tokio-tungstenite`. `Cargo.toml` and `Cargo.lock` are unchanged.
- Hosted GitHub Actions Rust run `35959854494` completed successfully on the qualified head. Main workspace checks, the seven feature slices, the Rust 1.89 job, and the minimal-dependency guard all passed.
- Final local gates passed: `cargo fmt --all -- --check`; `cargo check --locked --workspace --all-targets --all-features`; `cargo test --locked --workspace --all-targets --all-features` (74 passed); `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`; `cargo doc --locked --workspace --all-features --no-deps`; `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml`; the three Rust 1.89 checks; `cargo audit`; and `cargo deny check licenses`.
- The seven serial feature-slice tests passed locally on the implementation before the idle-timeout result-classification correction. The final all-feature and hosted seven-slice gates include that correction.
- `cargo audit` reported zero vulnerabilities and one allowed transitive unmaintained warning: `atomic-polyfill 1.0.3` (RUSTSEC-2023-0089). `cargo deny check licenses` passed.

## Documentation, findings, and disposition

Updated `docs/API.md`, `docs/CONFIGURATION.md`, `docs/EMBEDDING.md`, `docs/OPERATIONS.md`, `docs/SECURITY.md`, architecture dives for common/client/server/CLI, and `AGENTS.md`. Runtime resource ceilings and lifecycle timeouts are documented as finite policy; wire/name/token bounds remain fixed security/protocol invariants.

- No unresolved high or medium correctness/security finding remains.
- The existing transitive `atomic-polyfill 1.0.3` unmaintained warning remains as an informational dependency finding; it is not a known vulnerability.
- No stop condition was triggered. No wire change or optional dependency leakage was introduced.

Disposition: **closed**.

## Handoff

M008's hard dependency is satisfied. M009 is moved from blocked to ready: its stated existing-wire-semantics scope fits ADR-0001, and no additional architecture decision is needed unless implementation discovers that dynamic registration or heartbeat health requires wire-semantic changes.
