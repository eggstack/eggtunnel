# M002 Closure Status — TCP/TLS Reverse-Tunnel Product

Status: closed

Disposition: closed — the first authenticated TCP/TLS reverse-session product is implemented and locally qualified. Lifecycle fault injection and broader resource policy are carried into M003.

Implementation plan:

- plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md

## Baseline and implementation

- Planning baseline: `357480b942e95ef7087d86a043f26b7a3d175687` (accepted M001 head).
- Implementation commit: `13402200e51b46031a1a82240be1eb48027a09f4`.
- Final reviewed head: `13402200e51b46031a1a82240be1eb48027a09f4` (local source review and required verification completed).
- Eggress APIs were verified against version 1.0.8 before dependency selection. TLS and relay use narrow Eggress crates; `eggress-embed` is not used.

## Requirement evidence

| Requirement | Evidence |
|---|---|
| TLS control Session and token auth | Client/server end-to-end tests cover valid auth, rejected token, and wrong TLS server name. TLS handshake precedes bearer authentication. |
| Multiple services and routing | One authenticated Session registers two services and routes concurrent external connections to their configured TCP targets. |
| Bounded listener and connection state | Service, pending, active-connection, open-task, handshake, and control queues have explicit ceilings; service listeners default to loopback. |
| Pending correlation | Random ConnectionId is session/service-bound, expiring, consumed once; tests cover wrong Session, wrong service, expiration, and replay. |
| Relay semantics | `eggress-relay` carries opaque bytes; concurrent roundtrips verify half-close response drain and byte counters. |
| Failure cleanup | Refused target and active relay shutdown tests verify external closure and pending/active counters return to zero. |
| Reconnect | Server restart test observes a new SessionId and restored service registration. |
| Library/CLI/docs | Caller-owned Tokio runtime API, TOML client/server examples, `check` and `version`, configuration/security/operations/embedding docs. |
| Optional dependency boundary | Client+TLS, server+TLS, no-default, and CLI feature checks pass; filtered client-only dependency tree has no QUIC, WebSocket, SOCKS, or proxy crates. |

## Verification

Toolchain: rustc 1.98.1, cargo 1.98.1, aarch64-apple-darwin host.

- `rtk cargo fmt --all -- --check` — passed.
- `rtk cargo check --locked --workspace --all-targets` — passed.
- `rtk cargo test --locked --workspace --all-targets` — passed, 15 tests across 3 packages (8 runtime/server tests and 7 protocol tests).
- `rtk cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `RUSTDOCFLAGS=-Dwarnings rtk cargo doc --locked --workspace --no-deps` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features --features client,tls` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features --features server,tls` — passed.
- `rtk cargo check --locked -p eggtunnel-cli` — passed.
- `rtk cargo tree --locked -p eggtunnel --no-default-features --features client,tls` — reviewed; optional transports absent.
- CLI smoke: `eggtunnel version` and `eggtunnel check examples/client.toml` with `EGGTUNNEL_TOKEN` — passed.

## Limitations carried forward

- Cancellation injection is not exhaustive for every individual dial, TLS, registration, and control-send await point. M003 owns the expanded lifecycle probes and repeated start/stop/pending-expiry stress qualification.
- M002 uses bounded global concurrent handshake admission and a fixed session ceiling; source-based auth throttling, richer typed bind policy, optional mTLS, and explicit resource-budget reporting belong to M003.
- The TLS dependency may install the Rustls ring provider as process default. The library does not create a runtime or install tracing state; embedding implications are documented in `docs/SECURITY.md`.
- No hosted CI result or independent security review is claimed.

## Handoff

M002 is closed at `13402200e51b46031a1a82240be1eb48027a09f4`. M003 may begin from this baseline and must promote the lifecycle qualification gaps listed above.
