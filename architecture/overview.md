# Eggtunnel — Architecture Overview

Eggtunnel is a Rust TCP/TLS reverse-tunnel library and CLI. A process behind
NAT connects outward to a reachable server; the server exposes approved local
services through server-owned listeners. Each external connection opens a
separate TLS data connection back through the client to a client-owned target.

Workspace `0.2.0` (crate line `0.2.0`, wire protocol v1.0 — wire and crate
versions are independent). Published crates.io line and tag remain `0.1.0`
until the owner-authorized M012 publication; see `docs/DISTRIBUTION.md` and
`architecture/ops-tooling-distribution.md`.

This document is the birds-eye view and the index for discrete deep dives in
this directory. Each section below summarizes one module/component and links
to its dedicated review file.

## Module / component index

| # | Component | Crate / path | Deep dive |
|---|-----------|--------------|-----------|
| 1 | Wire protocol (`eggtunnel-proto`) | `crates/eggtunnel-proto/src/lib.rs` (754 lines) | [proto-wire-protocol.md](proto-wire-protocol.md) |
| 2 | Shared core (`common.rs`) | `crates/eggtunnel/src/common.rs` (636 lines) | [common-core.md](common-core.md) |
| 3 | Reverse-session client | `crates/eggtunnel/src/client.rs` + `crates/eggtunnel/src/client/` (`config`, `service_state`, `heartbeat`, `open`, `tests`, `qualification_tests`) | [client.md](client.md) |
| 4 | Reverse-session server | `crates/eggtunnel/src/server.rs` (1489 lines runtime) + `server_tests.rs` + `server_tests/` (`tcp`, `mtls`, `quic`, `websocket`, `proxy`) | [server.md](server.md) |
| 5 | Wire I/O + transports (TLS / QUIC / WSS / outbound-proxy, Eggress) | `crates/eggtunnel/src/wire_io.rs` (58 lines), feature gates in `crates/eggtunnel/Cargo.toml` | [transports-wire-io.md](transports-wire-io.md) |
| 6 | CLI + config + embedding API | `crates/eggtunnel-cli/src/main.rs` (320 lines), `crates/eggtunnel/src/lib.rs` (33 lines), `examples/`, `fixtures/embedder`, `docs/CONFIGURATION.md` | [cli-config-ops.md](cli-config-ops.md) |
| 7 | Ops, tooling, distribution, and process docs | `scripts/`, `.github/workflows/`, `install.sh`, `docs/`, `plans/`, `deny.toml` | [ops-tooling-distribution.md](ops-tooling-distribution.md) |

Cross-cutting security, auth, resource limits, and observability are covered
inline in each dive and summarized in §8 below.

## 1. Wire protocol — `eggtunnel-proto` ([deep dive](proto-wire-protocol.md))

Runtime-neutral, `forbid(unsafe_code)` (`lib.rs:1`), dependencies only
`serde`/`postcard`/`thiserror`/`getrandom`. Owns bounded wire DTOs, framing
(`ETUN` magic + major/minor u16 BE + message-ID u16 BE + payload-len u32 BE +
one postcard payload), and `encode_frame` / `decode_frame` (exactly-one-frame,
concatenated-frame friendly). I/O adaptation lives one layer up in
`wire_io.rs:7-58`.

- Wire v1.0 (crate line 0.2.0); major mismatch rejected (`crates/eggtunnel-proto/src/lib.rs:483-485`), minor informational.
- 14 stable message IDs 1–14: `ClientHello`, `ServerHello`, `Auth`, `AuthOk`,
  `RegisterService`, `RegisterAck`, `UnregisterService`, `Open`, `OpenReject`,
  `Ping`, `Pong`, `Drain`, `Error`, `DataHello`.
- Bounded types: 1 MiB frames, 4096 B tokens, 128 B service names
  (`[A-Za-z0-9-_.]`, byte-checked), 256 B diagnostics, 32 capabilities,
  253 B target hosts. Deserialization revalidates via `serde(try_from)` /
  `bounded_bytes`; trailing bytes inside the payload rejected.
- IDs: `SessionId` / `ConnectionId` (128-bit `getrandom`, constant-time eq for
  the latter, redacted `Debug`, 4-byte-prefix `Debug` for `SessionId`);
  `Auth` token redacted; hostile-input tests + 10k-sample fuzz-style
  `decode_frame` never-panics test in-crate (`crates/eggtunnel-proto/src/lib.rs:527-754`).
- Session-time re-registration and `Ping`/`Pong` heartbeat reuse the same
  messages; M009 adds no wire message type.

## 2. Shared core — `common.rs` ([deep dive](common-core.md))

Shared vocabulary for client and server (636 lines, no sockets/timers):
secrets, policy, observability, errors. Facade re-exports 11 types including
`RuntimePolicy`, `TimeoutPolicy`, `HeartbeatSnapshot` (`lib.rs:27-30`).

- `SecretToken`: validated (1–4096 B), redacted `Debug`, `zeroize` on drop,
  `expose()` is `pub(crate)`.
- `ClientService` (client view: id + name + requested bind + `TcpTarget`) vs
  `ServiceSpec` (server view: no target — server never learns the local
  destination).
- `BindPolicy`: loopback-only by default; allowlists, port ranges, ephemeral
  policy; `validate()` ceiling is 65536 (decoupled from the default
  64-session service count); server-only `bind_to_socket()` / `permits_*`.
- M008 policy split: `ResourceLimits` (8 ceilings incl. `client_command_queue
  = 32`, validated `1..=65536`) + `TimeoutPolicy` (9 durations, heartbeat <
  control-idle, reconnect initial ≤ max) composed as `RuntimePolicy`;
  `ClientBuilder` / `ServerBuilder` carry and validate it; `Counters` owns
  `Arc<RuntimePolicy>` and `snapshot().resource_limits` echoes the selected
  policy. Auth throttle stays a fixed security policy outside `RuntimePolicy`.
- `Snapshot` / `Counters` (atomics + mutex binds, `Relaxed` loads,
  poison-tolerant): connected, sessions, services, pending/active connections,
  open tasks, handshakes, high-waters, panics, termination, reconnects,
  rejects, byte counters, effective binds, plus bounded `HeartbeatSnapshot`
  (generation, last-Pong age, latest RTT, consecutive misses). Generation
  counter fails closed at `u64::MAX`.
- `TunnelError` (13 variants incl. `ServiceAlreadyExists` → `Authorization`)
  → `TerminationCategory` mapping (Cancelled/Timeout/Target/
  ResourceExhausted/PeerClosed/Auth*/Protocol/Transport/Internal).
- Server-only `verify_token` (constant-time via `subtle`).

## 3. Reverse-session client — `client.rs` + `client/` ([deep dive](client.md))

Outbound-only initiator behind NAT. Establishes one authenticated control
stream per session, registers N TCP services, then dials one data connection
per accepted external connection (`DataHello`, then opaque relay). Never
listens. `client.rs` (~1410 lines) is the orchestrator; composable logic lives
in `client/config.rs` (config + `ClientBuilder` + target contract),
`service_state.rs` (dynamic-Service lifecycle), `heartbeat.rs` (probe),
`open.rs` (data path), `tests.rs` + `qualification_tests.rs`.

- Canonical composition via `ClientBuilder` (`client/config.rs:78-156`):
  transport profile / connector / mTLS identity / proxy / `RuntimePolicy`
  validated before start; legacy `Client::start*` variants delegate through
  it. `RuntimePolicy` defaults preserve prior ceilings (128 open tasks, 128
  control queue, 32 command queue; 10 s connect/handshake; 500 ms → 30 s
  backoff + jitter; 20 s heartbeat).
- `TargetConnector` trait + `TargetContext` (session/connection/cancellation):
  default TCP dial; embedders can supply direct in-process `ApplicationStream`
  targets (see `docs/EMBEDDING.md`, `fixtures/embedder`).
- Dynamic Services (M009): `register_service` captures the session generation
  and waits for the matching `RegisterAck`; only acked services join the
  reconnect desired state. Wire `Error` carries no ServiceId, so one dynamic
  registration is in flight per session (`service_state.rs`); unregister
  prunes desired + active state (pending tombstone for late acks). Empty
  initial service set is valid in-library (CLI still requires ≥1).
- Bounded heartbeat health: one outstanding `(nonce, Instant)` per session
  (`client/heartbeat.rs`); `Snapshot.heartbeat` exposes generation, last-Pong
  age, latest RTT, consecutive misses — no unbounded history, no extra probes
  while one is outstanding.
- Data plane (`client/open.rs`): target dial before data dial, per-`Open`
  child cancel token, 16 KiB bounded relay, failures → advisory `OpenReject
  code=1`; semaphore overload → `code=2`, unknown service → `code=1`.
- Validation: QUIC = platform roots + bearer only; proxy+mTLS and WSS+mTLS
  and QUIC+proxy rejected fail-closed via `validate_client_profile`; proxy
  failures are typed, never silent fallback.
- Tests: unit/session harness in `client/tests.rs`, state units in
  `service_state.rs`, probe units in `heartbeat.rs`, deterministic 10k-step
  seeded sequence in `client/qualification_tests.rs`, cross-transport E2E
  under `server_tests/`.

## 4. Reverse-session server — `server.rs` + `server_tests/` ([deep dive](server.md))

Reachable rendezvous + ingress. Owns service listeners, per-session state,
single-use pending-connection correlation, and per-connection data accept +
relay. Runtime is `server.rs` (1489 lines); tests moved to `server_tests.rs`
(harness + `ServerBuilder::validate` tests) + `server_tests/{tcp,mtls,quic,
websocket,proxy}.rs`.

- Canonical composition via `ServerBuilder` (`server.rs:89-159`):
  `ServerTransportProfile` (TCP/TLS, QUIC, WebSocket) + `BindPolicy` +
  `RuntimePolicy` + optional client CA; `validate()` rejects mTLS on
  QUIC/WSS before bind; legacy `Server::bind*` helpers delegate through it
  (`bind_profile`, `bind_quic_profile`, `bind_with_tls_profile`).
- Session lifecycle: `ClientHello`/`ServerHello` → `Auth` (constant-time check,
  100 ms failure delay, per-source 10-fails/60 s/1024-source throttle) →
  `AuthOk(session_id)` → `RegisterService` / `RegisterAck(effective_bind)` →
  `Open(service, connection)` per external accept → client data dial +
  `DataHello(session, service, connection)` → `relay_with_options` opaque
  bytes; `Ping/Pong`, `Drain`, `Error`, `UnregisterService`, `OpenReject`
  throughout.
- All ceilings/timeouts come from validated `RuntimePolicy` (defaults: 128
  sessions, 64 services/session, 128 pending/active-per-session, 64
  handshakes, queues 128/32; 10 s handshake, 90 s idle, 30 s pending, 15 s
  relay-drain, 1 s shutdown-grace); `MAX_SESSIONS`/`MAX_HANDSHAKES` in
  `server.rs` are test-only. `BindPolicy` re-checked per registration before
  bind.
- mTLS (`mtls` feature): bearer token still required; leaf SHA-256 principal
  pinned at session creation and rechecked on every `DataHello`; QUIC is
  bearer-only (`principal: None`).

## 5. Wire I/O + transports ([deep dive](transports-wire-io.md))

- `wire_io.rs` (58 lines, unchanged): `read_message` / `write_message` over
  `AsyncRead/AsyncWrite` + `read_boxed` / `write_boxed` over Eggress
  `BoxStream`. Header-first read, hostile-header fast reject, length pre-check
  before payload allocation, exact-consumption check. All transports converge
  on `BoxStream`; session logic never branches on socket type.
- Baseline `tls` (TCP+TLS via `eggress-transport-tls 1.0.8`, Rustls `ring`,
  TLS 1.2+). Caller-owned Tokio runtime; no global runtime/tracing installed.
  SNI + system/custom-CA verification on control and every data connection;
  auth always inside verified TLS.
- Canonical composition is `ClientBuilder` / `ServerBuilder` +
  `Client/ServerTransportProfile` + `RuntimePolicy`; `Client::start*` /
  `Server::bind*` are conveniences. The rejection matrix (QUIC+custom-CA/mTLS/
  proxy, WSS+mTLS, proxy+mTLS, server+proxy) is enforced by
  `validate_client_profile` / `validate_server_profile` and surfaced through
  `eggtunnel check`.
- Optional `quic` (`eggress-transport-quic`, UDP control + one bidi stream per
  external connection, platform roots + bearer only, TCP service listeners
  retained). Transport stream cap is policy-derived
  (`active/client_open_tasks + 1` = 129 by default), under Eggress 1024/4096
  task caps and Eggtunnel 64-handshake / 128-per-session admission.
- Optional `websocket` (`eggress-protocol-websocket` + `tokio-tungstenite`,
  verified TLS → binary WS upgrade, 1 MiB `max_message_size`, non-browser
  endpoint, whole-connection close — no TCP half-close equivalence).
- Optional `outbound-proxy` (`eggress-outbound` with `pproxy-compat`,
  client-side only): direct / HTTP CONNECT / SOCKS5 single-hop +
  `__`-separated multi-hop chains, URI userinfo auth, env-var credentials,
  redacted diagnostics/`Snapshot`, TLS+SNI still end-to-end over the proxy
  path, typed failures with no silent direct fallback. WSS-over-proxy is the
  one allowed composition.
- Dependency direction: Eggtunnel owns reverse-session behavior; Eggress
  1.0.8 (exact pins) provides generic relay/transport primitives (see
  `plans/subsystems/reverse-session-roadmap.md`, `docs/SUPPORT.md`, ADR-0001).

## 6. CLI + config + embedding API ([deep dive](cli-config-ops.md))

- `eggtunnel-cli` (unpublished binary `eggtunnel`, all features):
  `version | check <file> | client <file> | server <file>`
  (`main.rs:13-26,279-320`). TOML file + `token_env`/`outbound_proxy_env`
  indirection (secrets in env, never in file). `check` is structural only —
  no PEM parsing, DNS resolution, or dialing.
- M008 path: `client_builder` (`main.rs:132-164`) / `server_builder`
  (`main.rs:166-196`) translate TOML → `ClientBuilder` / `ServerBuilder`;
  `check` and startup share `validate()`; startup calls single `start()` /
  `bind()`. Typed transport/CA/mTLS/proxy matrix is library-owned; CLI owns
  file shape, env lookup, and role-only rules. CLI pins
  `RuntimePolicy::default()` and bool-derived `BindPolicy`; custom ceilings
  are builder-only for embedders. No dynamic-service TOML key (CLI requires
  ≥1 service; library allows empty; `register_service` is programmatic-only).
- Server loop prints newly observed `effective_binds` every 250 ms; Ctrl-C →
  graceful joined `shutdown()` on both sides.
- Library facade `crates/eggtunnel/src/lib.rs` (33 lines,
  `forbid(unsafe_code)`, feature-gated re-exports, `eggtunnel::proto` alias);
  embedder docs (`docs/API.md`, `docs/EMBEDDING.md`, `fixtures/embedder/`
  with minimal `client+tls`, caller runtime/tracing, `TargetConnector`, and
  dynamic registration); `examples/client.toml`, `examples/server.toml`.

## 7. Ops, tooling, distribution, process docs ([deep dive](ops-tooling-distribution.md))

- Workspace `0.2.0` candidate (`Cargo.toml:6`, proto pin `0.2.0`); published
  crates.io line, tag `v0.1.0`, and GitHub release remain `0.1.0` until
  owner-authorized M012 publication (`docs/DISTRIBUTION.md:5-20`,
  `plans/closure/reverse-session/012-status.md`). Wire stays v1.0.
- Release surface unchanged: 4 targets (linux x64/arm64, macOS Intel/arm64),
  tag-triggered `release.yml`, per-runner install/version smoke, global
  `SHA256SUMS` + attestations, `install.sh` allowlist.
  `scripts/test-install.sh` already bumped to `0.2.0`.
- CI still 4 jobs (check / 7 feature slices / MSRV 1.89 / minimal-deps);
  M007–M011 closed, M012 conditionally closed with exact-head hosted CI;
  sustained fuzz/soak stays developer-invoked, not per-push.
- Supply chain: Eggress `=1.0.8`, `cargo audit` 0 vulns + 1 informational
  `atomic-polyfill` unmaintained finding, `cargo deny check licenses` pass
  (4 clarifies). API/EMBEDDING `0.1` snippets intentionally frozen until
  publication.
- `scripts/generate-third-party-notices.py`, `scripts/test-install.sh`,
  `install.sh`, `.github/workflows/ci.yml` + `release.yml`,
  `deny.toml` (`cargo deny`), `Cargo.lock`.
- `docs/` (ARCHITECTURE, PROTOCOL, CONFIGURATION, SECURITY, SUPPORT,
  OPERATIONS, DISTRIBUTION, API, EMBEDDING) and `plans/` (spec, terminology,
  roadmap, ADRs, implementation/closure evidence incl. M007–M012, subsystem
  roadmaps). Candidate release targets + qualification state in
  `docs/DISTRIBUTION.md`.

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
`eggress-relay`). Server picks effective binds (per-registration `BindPolicy`
+ `RuntimePolicy` ceilings); client picks local targets. Auth always runs
inside verified TLS; bearer token required in every profile, mTLS adds a
pinned leaf identity where enabled. Composition is builder-first
(`ClientBuilder` / `ServerBuilder` + `RuntimePolicy`); the CLI is a thin
all-features consumer with `check`-then-`start`/`bind` semantics.

## Review guidance

Start here, then open the deep dive for the component under review. Each dive
documents responsibilities, key types/functions, state machines and sequence
flows, error/limit/observability behavior, feature gates, test pointers, and
review checklists with file:line anchors.
