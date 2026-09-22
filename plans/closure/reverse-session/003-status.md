# M003 Closure Status — Security, Lifecycle, Resource, and Embedding Hardening

Status: closed

Disposition: closed — the TCP/TLS product now has bounded observable resource classes, source-based auth throttling, typed bind policy, optional mutual TLS, direct application connectors, and lifecycle/hostile-input qualification.

Implementation plan:

- plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md

## Baseline and implementation

- Planning baseline: `13402200e51b46031a1a82240be1eb48027a09f4` (accepted M002 head).
- Implementation commits: `736fc8c2e918ca4cdf234f504bd8d41dbe766ec1`, `45bb5cc6e6549d81e0f5e9dfab274889ed7f7a0f`, and `31458e83e543304d6b271898575bf2f6e98c7352`.
- Final reviewed head: `31458e83e543304d6b271898575bf2f6e98c7352` (local source review and required verification completed).

## Requirement evidence

| Requirement | Evidence |
|---|---|
| Resource ceilings and telemetry | `ResourceLimits` documents ceilings for 128 sessions, 64 services/session, 128 pending/session, 128 active connections/session, 64 unauthenticated handshakes, 128 client opens, and 128 queued control messages. Snapshots expose current/high-water counts and last typed termination. |
| RAII admission and recovery | Owned semaphore permits and pending-map removal release on normal completion, target refusal, cancellation, and shutdown. Tests qualify the 64/65 unauthenticated-handshake boundary and verify admission returns to zero. |
| Authentication throttling | Sliding 60-second source-IP window, 10 failures per source, 1,024-entry cap, 100 ms failed-token delay. Deterministic unit test covers threshold, bounded table, and expiry. |
| Bind authorization | `BindPolicy` checks public addresses, explicit address allowlists, port ranges, ephemeral ports, and service count before bind. Matrix test covers allowed loopback, disallowed port/address, and ephemeral denial. |
| Optional mTLS | Feature-gated client/server APIs require a trusted client certificate plus the existing bearer token. Leaf DER SHA-256 is the Principal identity and must match on Data Connections. Tests cover valid, absent, untrusted, wrong-server-name, identity mismatch, and key redaction. |
| Direct connector and embedding | `TargetConnector` receives client-owned service context and cancellation, returning a transport-neutral stream. End-to-end echo and cancellation tests pass. `fixtures/embedder` compiles with default features disabled and no CLI dependency. |
| Typed lifecycle state | `TerminationCategory` is exposed in snapshots; owned child panics increment a bounded counter and record `Internal`. Tests inject a child panic and cover pending/active cleanup. |
| Hostile-input harness | Protocol test exercises 10,000 deterministic pseudo-random inputs of lengths up to 4,096 bytes through the frame decoder with no crash. Existing malformed frame, oversized field, and replay tests remain passing. |
| Dependency/footprint evidence | Client+TLS and server+TLS profiles compile independently, with and without mTLS. The filtered client+TLS tree contains no QUIC, WebSocket, SOCKS, or proxy transport packages. Release CLI binary measured 4.4 MiB on the local host. |

## Verification

Toolchain: rustc 1.98.1, cargo 1.98.1, aarch64-apple-darwin host.

- `rtk cargo fmt --all -- --check` — passed.
- `rtk cargo check --locked --workspace --all-targets --all-features` — passed.
- `rtk cargo test --locked --workspace --all-targets --all-features` — passed, 25 tests across 3 workspace packages.
- `rtk cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `RUSTDOCFLAGS=-Dwarnings rtk cargo doc --locked --workspace --no-deps --all-features` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features --features client,tls` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features --features server,tls` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features --features client,tls,mtls` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features --features server,tls,mtls` — passed.
- `rtk cargo check --locked --manifest-path fixtures/embedder/Cargo.toml` — passed.
- `rtk cargo build --locked --release -p eggtunnel-cli` — passed; binary 4.4 MiB.
- `rtk cargo test --locked --workspace --all-targets --all-features` run twice consecutively — passed both times, 23 tests at that point. The final suite passed 25 tests after the additional admission-boundary and handshake-cancellation cases.
- Repeated lifecycle integration test performs 3 start/stop cycles per invocation and returns current resource counts to zero.

## Limitations and residual risk

- The arbitrary-input harness is deterministic and time-bounded by the test suite; it is not a libFuzzer campaign and does not claim exhaustive state-space coverage.
- The auth throttle is process-local and keyed by the TCP source IP; deployments behind shared NAT may need a higher-level policy.
- mTLS uses explicitly provisioned CA roots and certificate fingerprints; enrollment, rotation, and revocation services remain out of scope.
- TLS configuration may install the Rustls ring provider as process default. Eggtunnel still does not create a runtime or install a tracing subscriber.
- No hosted CI run or independent security review is claimed.

## Handoff

M003 is closed at `31458e83e543304d6b271898575bf2f6e98c7352`. M004 and M005 may execute against this baseline; M006 remains blocked until selected transport profiles are closed.
