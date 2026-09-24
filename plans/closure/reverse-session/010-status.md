# M010 Closure Status — Client Runtime Modularization and State-Machine Hardening

Status: closed

Implementation plan: `plans/implementation/reverse-session/010-client-runtime-modularization-and-state-machine-hardening.md`.

## Baseline and implementation

- Planning baseline recorded in the plan: `0c8830e76d92c209485eaf955cbdb51bdc953413`.
- Execution baseline: `8a21cb8c58b001f168d0ede5588fb507319b8957`.
- Implementation commit and hosted-qualified head: `fdd3fac06fbab029f48a6cf36b74f35a29f093fa` (`refactor: modularize client runtime state`).
- Hosted GitHub Actions Rust run: [36000201025](https://github.com/eggstack/eggtunnel/actions/runs/36000201025), success on the exact implementation SHA.
- Closure evidence is a documentation-only follow-up commit; the code head above is the hosted-qualified head.

## Requirement-to-evidence review

| Requirement | Evidence / disposition |
|---|---|
| Separate client responsibilities without changing public or wire behavior | Configuration/builders/connectors moved to `client/config.rs`; Session-local desired/active/pending Service ownership to `client/service_state.rs`; bounded heartbeat state to `client/heartbeat.rs`; Open/data path to `client/open.rs`; client tests to `client/tests.rs`. Existing crate-level exports and wire DTOs/messages are unchanged. |
| Make desired-Service registration transitions explicit | `ServiceState` owns deterministic desired order, active registrations, and one optional pending transaction. Matching current-generation acknowledgement commits; rejection/disconnect do not; cancellation/unregister retains the pending tombstone so late success is unregistered; stale generation is rejected. |
| Represent one in-flight registration constraint | `ServiceState::begin` rejects a second pending transaction. Its module documentation records why generic `Error` without ServiceId requires this invariant. |
| Cover transitions and reconnect/cancellation edges | New focused state tests cover initial ordering, duplicate ID/name, pending capacity, acknowledgement commit, rejection, stale generation, disconnect, unregister tombstone, and late acknowledgement cleanup. Existing client tests retain timeout, stale command, and cancellation behavior. |
| Preserve heartbeat generation bounds | `HeartbeatState` stores at most one probe and is constructed per Session; unit test verifies matching-Pong behavior and fresh-state reset. Existing integration-style fake-session heartbeat test verifies RTT, miss, and recovery. |
| Preserve bounded queues, Open admission, drain, task cleanup, and tracing behavior | Runtime queue/semaphore/JoinSet paths remain in the Session coordinator and existing lifecycle tests; full all-feature suite and hosted CI pass. No logging fields or secret handling changed. |
| Keep dependency and minimal feature boundaries | No production dependency or Cargo feature changes. Hosted `minimal-dependencies` guard passes for `client,tls`; tree excludes QUIC, WebSocket, outbound proxy, reverse protocol, and tokio-tungstenite. |
| Document ownership and protocol limitation | `architecture/client.md` and `docs/EMBEDDING.md` describe private ownership and the one-in-flight limitation. No private module name is made a public compatibility promise. |

## Verification evidence

Local exact commands passed:

```text
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets --all-features (90 passed)
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
cargo check --locked --manifest-path fixtures/embedder/Cargo.toml
cargo audit
cargo deny check licenses
```

All seven CI feature slices passed locally and in hosted run 36000201025:

- `client,tls`
- `client,server,tls`
- `client,server,tls,mtls`
- `client,server,tls,quic`
- `client,server,tls,websocket`
- `client,tls,outbound-proxy`
- `client,server,tls,websocket,outbound-proxy`

The Rust 1.89 checks passed for `eggtunnel-proto`, `eggtunnel` with `client,tls`, and `eggtunnel` with `client,server,tls`. The M009 dynamic registration/reconnect/unregister integration test passed three consecutive repeated local runs. Hosted CI passed all workspace gates, all feature slices, MSRV, and the minimal dependency guard on the exact implementation head.

`cargo audit` reported zero vulnerabilities and the existing allowed `atomic-polyfill 1.0.3` unmaintained warning (RUSTSEC-2023-0089). `cargo deny check licenses` passed.

## Security, compatibility, and residual findings

- No protocol, public API, runtime policy default, or dependency graph change was made.
- Hostile-input bounds, authentication/authorization boundaries, task and queue limits, timeout paths, cancellation, reconnect generation checks, and secret-safe diagnostics remain unchanged and covered by the pre-existing suite.
- No unresolved high or medium correctness/security finding remains.
- The one-registration-in-flight limit remains an intentional protocol constraint until an accepted wire change correlates generic registration errors.

Disposition: **closed**.
