# M005 Closure Status — Restricted-Network Transports and Proxy Traversal

Status: closed with adapter limitations recorded

Implementation plan: `plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md`.

## Baseline and implementation

- Planning baseline: `31458e83e543304d6b271898575bf2f6e98c7352` (accepted M003 head); executed after M004.
- WebSocket adapter: `eggress-protocol-websocket = 1.0.8`, used as a stream adapter after Eggtunnel-owned TLS. Each TCP/TLS control and data connection gets an independent WebSocket upgrade. Binary messages and frames are capped at 1 MiB.
- Proxy adapter: `eggress-outbound = 1.0.8` with `pproxy-compat`, using listener-free `OutboundConnector` execution. The connector opens the upstream path before Eggtunnel TLS; there is no direct fallback.
- CLI selects `transport = "websocket_tls"` and reads proxy URI/chain text from the environment variable named by `outbound_proxy_env`. The library provides connector-aware proxy startup methods.

## Requirement evidence

| Requirement | Evidence |
|---|---|
| Optional feature/dependency boundary | Separate `websocket` and `outbound-proxy` Cargo features. Minimal `client,tls` normal dependency tree contains no QUIC, WebSocket, or outbound packages. |
| WSS protocol equivalence | Local integration test establishes verified WSS control, registers a service, opens a WSS data path, and roundtrips bytes through a direct application connector. |
| WebSocket bounds | Both Tungstenite endpoints configure 1 MiB maximum message and frame sizes; the Eggress stream adapter also enforces the message cap. |
| HTTP CONNECT proxy | Local deterministic HTTP CONNECT fixture forwards the TLS handshake and protocol; the end-to-end session and data path pass. |
| SOCKS5 proxy | Local deterministic SOCKS5 fixture forwards the TLS handshake and protocol; the end-to-end session and data path pass. |
| End-to-end TLS through proxy | Both proxy integration tests use a self-signed server certificate trusted only by the client's configured CA bundle and verify the Eggtunnel TLS session. |
| Credential/config boundary | CLI TOML stores only the proxy environment-variable name. Proxy URI parsing errors exposed by Eggtunnel are generic; Eggress 1.0.8 typed diagnostics omit credential-bearing URIs. |
| Incompatible profiles | CLI rejects QUIC+proxy, proxy+mTLS, and WebSocket+mTLS; WSS is always TLS and there is no plaintext WebSocket profile. |

## Verification

Toolchain: rustc 1.98.1, cargo 1.98.1, aarch64-apple-darwin host.

- `rtk cargo fmt --all -- --check` — passed.
- `rtk cargo test --locked --workspace --all-targets --all-features` — passed, 31 tests across 3 suites.
- `rtk cargo check --locked --workspace --all-targets --all-features` — passed.
- `rtk cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `rtk cargo doc --locked --workspace --all-features --no-deps` — passed.
- Focused WSS, HTTP CONNECT, and SOCKS5 end-to-end tests — passed.
- Feature slices `client,tls`; `server,tls`; `client,tls,server,websocket`; `client,tls,outbound-proxy`; and `client,tls,server,websocket,outbound-proxy` — passed.
- `rtk cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal` filtered for `quic|websocket|outbound` — no matches.

## Limitations and residual risk

- WebSocket close closes the whole WebSocket connection. It does not preserve TCP half-close semantics; the end-to-end WSS test keeps the application write side open while reading its reply. Do not claim transparent half-close behavior.
- HTTP CONNECT and unauthenticated SOCKS5 were exercised with local fixtures. Proxy authentication, multi-hop chains, timeout/refusal cases, and cancellation during individual hops do not have Eggtunnel integration tests.
- Eggress proxy failure kind is mapped to the existing bounded Eggtunnel termination categories. Detailed proxy hop/stage facts are not currently surfaced through `Snapshot`.
- mTLS cannot be combined with WebSocket or outbound proxy in the current public API/CLI profile. QUIC proxy traversal remains unsupported.
- No hosted CI, cross-platform run, or independent security review is claimed.

## Handoff

M005 is closed. M006 may proceed. For an initial release, the support matrix should distinguish the fully qualified TCP/TLS profile from optional profiles with recorded integration gaps.
