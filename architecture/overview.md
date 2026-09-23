# Eggtunnel — Architecture Overview

Eggtunnel is a Rust TCP/TLS reverse-tunnel library and CLI. A process behind
NAT connects outward to a reachable server; the server exposes approved local
services through server-owned listeners. Each external connection opens a
separate TLS data connection back through the client to a client-owned target.

This document is the birds-eye view and the index for discrete deep dives in
this directory. Each section below summarizes one module/component and links
to its dedicated review file.

## Module / component index

| # | Component | Crate / path | Deep dive |
|---|-----------|--------------|-----------|
| 1 | Wire protocol (`eggtunnel-proto`) | `crates/eggtunnel-proto/src/lib.rs` | [proto-wire-protocol.md](proto-wire-protocol.md) |
| 2 | Shared core (`common.rs`) | `crates/eggtunnel/src/common.rs` | [common-core.md](common-core.md) |
| 3 | Reverse-session client | `crates/eggtunnel/src/client.rs` | [client.md](client.md) |
| 4 | Reverse-session server | `crates/eggtunnel/src/server.rs` | [server.md](server.md) |
| 5 | Wire I/O + transports (TLS / QUIC / WSS / outbound-proxy, Eggress) | `crates/eggtunnel/src/wire_io.rs`, feature gates in `crates/eggtunnel/Cargo.toml` | [transports-wire-io.md](transports-wire-io.md) |
| 6 | CLI + config + embedding API | `crates/eggtunnel-cli/src/main.rs`, `crates/eggtunnel/src/lib.rs`, `examples/`, `fixtures/embedder`, `docs/CONFIGURATION.md` | [cli-config-ops.md](cli-config-ops.md) |
| 7 | Ops, tooling, distribution, and process docs | `scripts/`, `.github/workflows/`, `install.sh`, `docs/`, `plans/`, `deny.toml` | [ops-tooling-distribution.md](ops-tooling-distribution.md) |

Cross-cutting security, auth, resource limits, and observability are covered
inline in each dive and summarized in §8 below.

## 1. Wire protocol — `eggtunnel-proto` ([deep dive](proto-wire-protocol.md))

Runtime-neutral, `forbid(unsafe_code)`, no socket/async/timer/task dependencies.
Owns bounded wire DTOs, framing (`ETUN` magic + major/minor u16 BE + message-ID
u16 BE + payload-len u32 BE + one postcard payload), and `encode_frame` /
`decode_frame` (exactly-one-frame, concatenated-frame friendly).

- Wire v1.0 (crate line 0.1.x); major mismatch rejected, minor informational.
- 14 stable message IDs 1–14: `ClientHello`, `ServerHello`, `Auth`, `AuthOk`,
  `RegisterService`, `RegisterAck`, `UnregisterService`, `Open`, `OpenReject`,
  `Ping`, `Pong`, `Drain`, `Error`, `DataHello`.
- Bounded types: 1 MiB frames, 4096 B tokens, 128 B service names
  (`[A-Za-z0-9-_.]`), 256 B diagnostics, 32 capabilities, 253 B target hosts.
- IDs: `SessionId` / `ConnectionId` (128-bit random, constant-time eq for the
  latter, redacted `Debug`); `Auth` token redacted; hostile-input tests +
  10k-sample fuzz-style `decode_frame` never-panics test.

## 2. Shared core — `common.rs` ([deep dive](common-core.md))

Shared vocabulary for client and server: secrets, policy, observability,
errors.

- `SecretToken`: validated (1–4096 B), redacted `Debug`, `zeroize` on drop,
  `expose()` is `pub(crate)`.
- `ClientService` (client view: id + name + requested bind + `TcpTarget`) vs
  `ServiceSpec` (server view: no target — server never learns the local
  destination).
- `BindPolicy`: loopback-only by default; allowlists, port ranges, ephemeral
  policy, per-session service ceiling (default 64); `validate()` + server-only
  `bind_to_socket()` / `permits_*`.
- `Snapshot` / `Counters` (atomics + mutex binds): connected, sessions,
  services, pending/active connections, open tasks, handshakes, high-waters,
  panics, termination, reconnects, rejects, byte counters, effective binds.
- `TunnelError` → `TerminationCategory` mapping (Cancelled/Timeout/Target/
  ResourceExhausted/PeerClosed/Auth*/Protocol/Transport/Internal).
- Server-only `verify_token` (constant-time via `subtle`).

## 3. Reverse-session client — `client.rs` ([deep dive](client.md))

Outbound-only initiator behind NAT. Establishes one authenticated TLS control
stream, registers N TCP services, then dials one TLS data connection per
accepted external connection (`DataHello`, then opaque relay).

- `ClientConfig` (`server_addr`, `tls_server_name`, optional `ca_pem`, token,
  services); `Client::start*` variants: `start` (TCP+TLS), `start_quic`,
  `start_websocket`, `start_with_outbound_proxy`,
  `start_websocket_with_outbound_proxy`, `start_with_mtls`; `ClientHandle`
  (`snapshot`, `shutdown`, `unregister_service`).
- `TargetConnector` trait + `TargetContext` (session/connection/cancellation):
  default TCP dial; embedders can supply direct in-process `ApplicationStream`
  targets (see `docs/EMBEDDING.md`, `fixtures/embedder`).
- Concurrency: control reader/writer split, `JoinSet` data-plane tasks capped
  at `MAX_OPEN_TASKS=128`, `CONTROL_QUEUE=128`, semaphores, `CancellationToken`,
  reconnect loop with `reconnects` counter.
- Timeouts: connect/handshake 10 s, relay drain 15 s, server-drain grace 1 s.
- Validation: `MAX_SERVICES=64`; QUIC = platform roots + bearer only;
  proxy+mTLS and WSS+mTLS rejected; proxy failures are typed, never silent
  fallback.

## 4. Reverse-session server — `server.rs` ([deep dive](server.md))

Reachable rendezvous + ingress. Owns service listeners, per-session state,
single-use pending-connection correlation, and per-connection data accept +
relay. Largest module (~4.5 kLOC including tests).

- `ServerConfig` (`listen_addr`, cert/key PEM, token,
  `allow_public_service_binds`); `Server::bind*` variants: `bind`,
  `bind_with_policy`, `bind_quic`, `bind_websocket`, `bind_mtls`;
  `ServerHandle` (`snapshot`, `shutdown`).
- Session lifecycle: `ClientHello`/`ServerHello` → `Auth` (constant-time check,
  100 ms failure delay) → `AuthOk(session_id)` → `RegisterService` /
  `RegisterAck(effective_bind)` → `Open(service, connection)` per external
  accept → client data dial + `DataHello(session, service, connection)` →
  `relay_with_options` opaque bytes; `Ping/Pong`, `Drain`, `Error`,
  `UnregisterService`, `OpenReject` throughout.
- Hardening: `MAX_SESSIONS=128`, pending/active-per-session 128,
  `MAX_HANDSHAKES=64`, control queue 128; 30 s pending lifetime; 90 s idle
  timeout; 10 s handshake timeout; per-source auth throttle (10 fails / 60 s
  window, 1024 sources, bounded table); `BindPolicy` enforced before bind.
- mTLS (`mtls` feature): bearer token still required; leaf SHA-256 principal
  must match on control + data connections.

## 5. Wire I/O + transports ([deep dive](transports-wire-io.md))

- `wire_io.rs` (~58 LOC): `read_message` / `write_message` over
  `AsyncRead/AsyncWrite` + `read_boxed` / `write_boxed` over Eggress
  `BoxStream`. Header-first read, length pre-check before payload copy,
  exact-consumption check.
- Baseline `tls` (TCP+TLS via `eggress-transport-tls`, Rustls `ring`, TLS 1.2+).
  Caller-owned Tokio runtime; no global runtime/tracing installed.
- Optional `quic` (`eggress-transport-quic`, UDP control + bidirectional
  streams, platform roots + bearer only, TCP service listeners retained).
- Optional `websocket` (`eggress-protocol-websocket` + `tokio-tungstenite`,
  verified TLS → binary WS upgrade, 1 MiB message cap, non-browser endpoint).
- Optional `outbound-proxy` (`eggress-outbound`, client-side only): direct /
  HTTP CONNECT / SOCKS5 single-hop + `__`-separated multi-hop chains, URI
  userinfo auth, env-var credentials, redacted diagnostics, TLS+SNI still
  end-to-end over the proxy path.
- Dependency direction: Eggtunnel owns reverse-session behavior; Eggress
  1.0.8 provides generic relay/transport primitives (see
  `plans/subsystems/reverse-session-roadmap.md`, `docs/SUPPORT.md`).

## 6. CLI + config + embedding API ([deep dive](cli-config-ops.md))

- `eggtunnel-cli` (unpublished binary `eggtunnel`): `version | check <file> |
  client <file> | server <file>`. TOML file + `token_env` indirection (never in
  file); `check` validates structure, endpoints, services, token env, CA/mTLS
  files, proxy URIs, and illegal combos (QUIC+custom-CA/mTLS/proxy,
  WSS+mTLS, proxy+mTLS, server+proxy).
- Server loop prints newly observed `effective_binds` every 250 ms; Ctrl-C →
  graceful `shutdown()`.
- Library facade `crates/eggtunnel/src/lib.rs` (feature-gated re-exports,
  `eggtunnel::proto` alias); embedder docs (`docs/API.md`, `docs/EMBEDDING.md`,
  `fixtures/embedder/`); `examples/client.toml`, `examples/server.toml`.

## 7. Ops, tooling, distribution, process docs ([deep dive](ops-tooling-distribution.md))

- `scripts/generate-third-party-notices.py`, `scripts/test-install.sh`,
  `install.sh`, `.github/workflows/ci.yml` + `release.yml`,
  `deny.toml` (`cargo deny`), `Cargo.lock`.
- `docs/` (ARCHITECTURE, PROTOCOL, CONFIGURATION, SECURITY, SUPPORT,
  OPERATIONS, DISTRIBUTION, API, EMBEDDING) and `plans/` (spec, terminology,
  roadmap, ADRs, implementation/closure evidence, subsystem roadmaps).
- Candidate release targets + qualification state in `docs/DISTRIBUTION.md`.

## 8. How everything fits together

```text
                    ┌──────────── NAT / firewall ────────────┐
                    │  outbound-only from private side       │
                    ▼                                        │
  ┌──────────┐  control TLS   ┌──────────┐  ingress   ┌──────────┐
  │  client  │ ─────────────► │  server  │ ◄───────── │ external │
  │(services │  + N × data TLS│(listeners│  TCP       │ clients  │
  │ →targets)│ ◄───────────── │ pending/ │ ─────────► │          │
  └──────────┘  Open/DataHello│ active)  │  relay     └──────────┘
       │                      └──────────┘
       ▼ local TCP or in-process ApplicationStream
  ┌──────────┐
  │ targets  │
  └──────────┘
```

Control path: exactly one authenticated control stream per session
(`ClientHello → ServerHello → Auth → AuthOk → Register* → Open/OpenReject →
Ping/Pong/Drain/Error`). Data path: one TLS connection (or QUIC bidi stream /
WSS byte stream) per external connection (`DataHello` then opaque bytes via
`eggress-relay`). Server picks effective binds; client picks local targets.
Auth always runs inside verified TLS; bearer token required in every profile,
mTLS adds a pinned leaf identity where enabled.

## Review guidance

Start here, then open the deep dive for the component under review. Each dive
documents responsibilities, key types/functions, state machines and sequence
flows, error/limit/observability behavior, feature gates, test pointers, and
review checklists with file:line anchors.
