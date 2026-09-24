# M009 Closure Status — Dynamic Service Lifecycle and Operational Observability

Status: closed

Implementation plan: `plans/implementation/reverse-session/009-dynamic-service-lifecycle-and-operational-observability.md`.

## Baseline and implementation

- Planning baseline: `2e2f3981a6e78596c32dfe5daf5b2585bfc37f1e`.
- M008 strict closure: `plans/closure/reverse-session/008-status.md`.
- Implementation commit: `4c4a89c` (`feat: add dynamic service lifecycle observability`).
- Hosted-qualified implementation head: `4c4a89cd6c73c75e6de616327c9c8943b3e2cb05`.
- Hosted GitHub Actions Rust run: [35965470764](https://github.com/eggstack/eggtunnel/actions/runs/35965470764), success on the implementation SHA.
- This planning closure is a separate documentation-only commit after hosted qualification.

## Dynamic Service lifecycle

- `ClientHandle::register_service` accepts a client-owned `ClientService`, validates it against the active policy, sends the existing RegisterService message, waits within the configured handshake timeout for the matching response, and returns server-authoritative `EffectiveBind`.
- Registration while disconnected, stale-generation queue work, server denial, server capacity rejection, and duplicate local ID/name produce explicit typed outcomes. Only acknowledged registrations enter reconnect desired state.
- The registration command/reply path is bounded and cancellation-aware. A late acknowledgement after caller cancellation is explicitly unregistered. Since existing Error has no ServiceId, only one dynamic registration is in flight per Session; no protocol extension was introduced.
- `unregister_service` is idempotent, bounded through reconnect, removes desired state, and handles an unregister racing with a pending registration using a tombstone until the matching response arrives.
- Programmatic builders may start with no configured Services for dynamic-only clients; the CLI keeps its existing requirement for one TOML service.

| Invariant / outcome | Evidence |
|---|---|
| Dynamic registration returns effective bind and restores after reconnect | `acknowledged_dynamic_service_survives_reconnect_and_unregister_persists` integration test |
| Unregister remains absent after reconnect | Same integration test; includes concurrent unregister and server shutdown/reconnect |
| Duplicate ID/name and local policy validation | Client desired-state checks and builder validation tests |
| Server capacity rejection maps to ResourceExhausted | `dynamic_registration_maps_server_service_limit_rejection` |
| Disconnect before ack returns Disconnected | Fake-session client test |
| Cancellation before ack never creates desired state | Fake-session test verifies late RegisterAck is followed by Unregister |
| Old generation command cannot write registration | Stale-generation fake-session test |
| Ack timeout is typed and closes uncertain Session | Registration timeout test |
| Dynamic-only programmatic configuration validates | `client_builder_accepts_tcp_tls_and_default_policy` covers empty initial service list |

The multi-generation reconnect/unregister integration case passed three consecutive repeated runs during qualification and passed again in the final focused run.

## Heartbeat and tracing

- `Snapshot` includes a fixed-size heartbeat view: current Session generation, age of last matching Pong, latest RTT in milliseconds, and consecutive missed intervals. At most one Ping is outstanding; matching Pong updates RTT and recovers the missed counter. Values reset at the next Session and do not accumulate history.
- Heartbeat tests verify RTT measurement, a missed response, and recovery on the matching Pong.
- Structured `tracing` events cover connection/transport/auth/session/service lifecycle, EffectiveBind, rejection category, DataHello correlation, relay termination, and shutdown. The library does not install a subscriber or global runtime state.
- Fields avoid tokens, proxy URIs/credentials, private keys, certificates, or whole config objects. Debug-level attacker-controlled rejection events limit log amplification; a config Debug redaction test covers token material.
- Dependency delta: `tracing` is now a direct optional dependency enabled for client/server features and is already present transitively in the Eggress graph. No new resolved package was added; root and fixture lockfiles record the direct edge.

## Embedding and documentation

The separate `fixtures/embedder` consumer uses `default-features = false`, a custom connector and runtime policy, and registers a Service through the public handle without restarting the Client. Its locked manifest check passes. Public API, embedding, operations, security and protocol docs plus client/common/server/protocol architecture dives describe the new contracts.

## Qualification evidence

- Final workspace: `cargo fmt --all -- --check`; `cargo check --locked --workspace --all-targets`; `cargo test --locked --workspace --all-targets --all-features` (84 passed); `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`; `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps`; embedder locked check; `cargo audit`; `cargo deny check licenses`.
- Rust 1.89 local checks passed for `eggtunnel-proto`, `eggtunnel` with `client,tls`, and `client,server,tls`.
- The seven hosted feature slices passed: `client,tls`; `client,server,tls`; `client,server,tls,mtls`; `client,server,tls,quic`; `client,server,tls,websocket`; `client,tls,outbound-proxy`; `client,server,tls,websocket,outbound-proxy`.
- Hosted CI also passed MSRV, workspace gates, and the minimal dependency guard on the exact implementation head.
- The minimal `client,tls` dependency tree contains none of QUIC, WebSocket, outbound-proxy, `eggress-protocol-reverse`, or `tokio-tungstenite`.

`cargo audit` found zero vulnerabilities and one allowed transitive unmaintained warning, `atomic-polyfill 1.0.3` (RUSTSEC-2023-0089). `cargo deny check licenses` passed. No unresolved high or medium correctness/security finding remains.

## Future-plan readiness

Reviewed the reverse-session roadmap and registry after M009. There is no concrete M010 implementation plan to unblock. The roadmap's later negotiated-protocol work still requires a concrete extension and accepted ADR; Eggpack release-workflow cutover still requires stable producer, bootstrap-installer, and generated-CI interfaces. These are the roadmap's explicit gates, not newly discovered M009 blockers, so their statuses remain deferred and no status promotion is appropriate. Existing wire semantics remained unchanged, satisfying the M009 stop conditions.

Disposition: **closed**.
