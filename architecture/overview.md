# Eggtunnel — Architecture Overview

Eggtunnel is a Rust TCP/TLS reverse-tunnel library and CLI. A process behind
NAT connects outward to a reachable server; the server exposes approved local
services through server-owned listeners. Each external connection opens a
separate TLS data connection back through the client to a client-owned target.

Workspace `0.2.0` (crate line `0.2.0`, wire protocol v1.1 with 1.0 fallback —
wire and crate versions are independent). M001–M016 are closed
(`plans/registry.md`); the current published crates.io line, tag, and GitHub
release are `0.2.0` (M012 qualification
`plans/closure/reverse-session/012-status.md` + M013 publication event
`plans/closure/reverse-session/013-status.md`); see `docs/DISTRIBUTION.md`
and `architecture/ops-tooling-distribution.md`.

This document is the birds-eye view and the index for discrete deep dives in
this directory. Each section below summarizes one module/component and links
to its dedicated review file. Line counts and `file:line` anchors below were
re-verified against the post-M016 tree (client/`server/` split, capability
negotiation, CLI resolution surface).

## Module / component index

| # | Component | Crate / path | Deep dive |
|---|-----------|--------------|-----------|
| 1 | Wire protocol (`eggtunnel-proto`) | `crates/eggtunnel-proto/src/lib.rs` (991 lines) | [proto-wire-protocol.md](proto-wire-protocol.md) |
| 2 | Shared core (`common.rs` + `endpoint.rs` + `pem.rs`) | `crates/eggtunnel/src/common.rs` (706 lines), `endpoint.rs` (183 lines), `pem.rs` (64 lines, `mtls`) | [common-core.md](common-core.md) |
| 3 | Reverse-session client | `crates/eggtunnel/src/client.rs` (1238 lines) + `client/` (`config` 162, `reconnect` 602, `service_state` 595, `heartbeat` 78, `open` 93, `tests` 1053, `qualification_tests` 110) | [client.md](client.md) |
| 4 | Reverse-session server | `crates/eggtunnel/src/server.rs` (353 lines coordinator) + `server/` (`config` 179, `tls` 67, `accept` 516, `auth` 133, `session` 170, `control` 570, `pending` 126, `service` 157) + `server_tests.rs` (305) + `server_tests/` (`tcp` 1952, `mtls` 187, `quic` 940, `websocket` 405, `proxy` 899) | [server.md](server.md) |
| 5 | Wire I/O + transports (TLS / QUIC / WSS / outbound-proxy, Eggress) | `crates/eggtunnel/src/wire_io.rs` (209 lines), feature gates in `crates/eggtunnel/Cargo.toml` | [transports-wire-io.md](transports-wire-io.md) |
| 6 | CLI + config + embedding API | `crates/eggtunnel-cli/src/main.rs` (1548 lines), `crates/eggtunnel/src/lib.rs` (35 lines), `examples/`, `fixtures/embedder`, `docs/CONFIGURATION.md` | [cli-config-ops.md](cli-config-ops.md) |
| 7 | Ops, tooling, distribution, and process docs | `scripts/`, `.github/workflows/`, `install.sh`, `docs/`, `plans/`, `deny.toml` | [ops-tooling-distribution.md](ops-tooling-distribution.md) |

Cross-cutting security, auth, resource limits, and observability are covered
inline in each dive and summarized in §8 below.

## 1. Wire protocol — `eggtunnel-proto` ([deep dive](proto-wire-protocol.md))

Runtime-neutral, `forbid(unsafe_code)` (`lib.rs:1`), dependencies only
`serde`/`postcard`/`thiserror`/`getrandom`. Owns bounded wire DTOs, framing
(`ETUN` magic + major/minor u16 BE + message-ID u16 BE + payload-len u32 BE +
one postcard payload), and `encode_frame` / `decode_frame` (exactly-one-frame,
concatenated-frame friendly). I/O adaptation lives one layer up in
`wire_io.rs`.

- Wire v1.1 (crate line 0.2.0; 1.0 peers interoperate at baseline); major
  mismatch rejected (`crates/eggtunnel-proto/src/lib.rs:532-536`), minor
  informational, extensions by capability intersection only (ADR-0002).
- 15 stable message IDs 1–15: `ClientHello`, `ServerHello`, `Auth`, `AuthOk`,
  `RegisterService`, `RegisterAck`, `UnregisterService`, `Open`, `OpenReject`,
  `Ping`, `Pong`, `Drain`, `Error`, `DataHello`, `RegisterReject` (ID 15,
  capability-1 only). `Capabilities::supported() == [1, 2]`
  (`lib.rs:203-207`): 1 = correlated rejection, 2 = drain deadline.
- Bounded types: 1 MiB frames, 4096 B tokens, 128 B service names
  (`[A-Za-z0-9-_.]`, byte-checked), 256 B diagnostics, 32 capabilities,
  253 B target hosts. Deserialization revalidates via `serde(try_from)` /
  `bounded_bytes`; trailing bytes inside the payload rejected.
- IDs: `SessionId` / `ConnectionId` (128-bit `getrandom`, constant-time eq for
  the latter, redacted `Debug`, 4-byte-prefix `Debug` for `SessionId`);
  `Auth` token redacted; hostile-input tests + 10k-sample fuzz-style
  `decode_frame` never-panics test in-crate (proto `lib.rs` test module).
- Session-time re-registration and `Ping`/`Pong` heartbeat reuse the same
  messages; M009 adds no wire message type, M016 adds `RegisterReject` plus
  the capability registry only.

## 2. Shared core — `common.rs` + `endpoint.rs` + `pem.rs` ([deep dive](common-core.md))

Shared vocabulary for client and server: secrets, policy, observability,
errors. `common.rs` (706 lines) carries no unconditional socket/timer/task
dependency — `SocketAddr` is `server`-gated, `Instant`/atomics/`JoinError`
are `client`/`server`-gated, so only the `--no-default-features` build is
pure vocabulary. Facade re-exports 11 common types plus `Endpoint` at
`lib.rs:28-33`.

- `SecretToken`: validated (1–4096 B), redacted `Debug`, `zeroize` on drop,
  `expose()` is `pub(crate)` behind `any(client, server)`.
- `ClientService` (client view: id + name + requested bind + `TcpTarget`) vs
  `ServiceSpec` (server view, no target — load-bearing nowhere in the 0.2
  runtime; the live server consumes wire `RegisterService` directly and treats
  the bounded `TcpTarget` field as non-authoritative metadata only).
- `BindPolicy`: loopback-only by default; allowlists, port ranges, ephemeral
  policy; `validate()` ceiling is 65536 (decoupled from the default
  64-services-per-session ceiling; `sessions` defaults to 128);
  server-only `bind_to_socket()` / `permits_*`.
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
- `endpoint.rs` (183 lines, ungated): `Endpoint::parse` for client `host:port`
  shape (DNS/IPv4 plus bracketed IPv6, no transport/policy validation).
  `pem.rs` (64 lines, `mtls`-gated): rustls pki-types PEM helpers.

## 3. Reverse-session client — `client.rs` + `client/` ([deep dive](client.md))

Outbound-only initiator behind NAT. Establishes one authenticated control
stream per session, registers N TCP services, then dials one data connection
per accepted external connection (`DataHello`, then opaque relay). Never
listens. `client.rs` (1238 lines) is the orchestrator (handle/entry points,
`start_profile`, `run_session`, guards); composable logic lives in
`client/config.rs` (config + `ClientBuilder` + target contract),
`client/reconnect.rs` (supervisor + `StreamTransport`/`QuicTransport` + dial),
`client/service_state.rs` (dynamic-Service lifecycle),
`client/heartbeat.rs` (probe), `client/open.rs` (data path),
`client/tests.rs` + `client/qualification_tests.rs`. Transport/profile
validation lives in `client.rs:570-613` (`validate_client_profile`), not in
`endpoint.rs`.

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
  reconnect desired state. Legacy-serial mode (no capability 1) keeps the
  one-dynamic-registration-in-flight invariant because wire `Error` carries no
  ServiceId; capability-1 `CorrelatedBounded` mode correlates N transactions
  by ServiceId via `RegisterReject`. Unregister prunes desired + active state
  (pending tombstone for late acks). Empty initial service set is valid
  in-library (CLI still requires ≥1).
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
  `service_state.rs`, probe units in `heartbeat.rs`, reconnect units in
  `reconnect.rs`, deterministic 10k-step seeded sequence in
  `client/qualification_tests.rs`, cross-transport E2E under `server_tests/`.

## 4. Reverse-session server — `server.rs` + `server/` + `server_tests/` ([deep dive](server.md))

Reachable rendezvous + ingress. Owns service listeners, per-session state,
single-use pending-connection correlation, and per-connection data accept +
relay. `server.rs` (324 lines) is the coordinator (`Server`/`ServerHandle`,
`bind*` → `ServerBuilder::bind`, `bind_profile`/`bind_quic_profile`/
`bind_with_tls_profile`); runtime lives in `server/` (`config` 173,
`tls` 67, `accept` 477, `auth` 113, `session` 163, `control` 421,
`pending` 122, `service` 154). Tests live in `server_tests.rs` (255 lines
harness) + `server_tests/` (`tcp` 1920, `mtls` 187, `quic` 967,
`websocket` 405, `proxy` 905).

- Canonical composition via `ServerBuilder` (`server/config.rs:61-130`):
  `ServerTransportProfile` (TCP/TLS, QUIC via `quic-server`, WebSocket via
  `websocket-server`) + `BindPolicy` + `RuntimePolicy` + optional client CA;
  `validate()` rejects mTLS on QUIC/WSS before bind; legacy `Server::bind*`
  helpers delegate through it.
- Session lifecycle: `ClientHello`/`ServerHello` (capability intersection) →
  `Auth` (constant-time check, 100 ms failure delay, per-source 10-fails/60 s/
  1024-source throttle) → `AuthOk(session_id)` → `RegisterService` /
  `RegisterAck(effective_bind)` (or capability-1 `RegisterReject`, else legacy
  `Error`) → `Open(service, connection)` per external accept → client data
  dial + `DataHello(session, service, connection)` → `relay_with_options`
  opaque bytes; `Ping/Pong`, `Drain` (capability-2 peer deadline honored),
  `Error`, `UnregisterService`, `OpenReject` throughout.
- All ceilings/timeouts come from validated `RuntimePolicy` (defaults: 128
  sessions, 64 services/session, 128 pending/active-per-session, 64
  handshakes, queues 128/32; 10 s handshake, 90 s idle, 30 s pending, 15 s
  relay-drain, 1 s shutdown-grace); `MAX_SESSIONS`/`MAX_HANDSHAKES` in
  `server_tests.rs` are test-only. `BindPolicy` re-checked per registration
  before bind.
- mTLS (`mtls` feature): bearer token still required; leaf SHA-256 principal
  pinned at session creation and rechecked on every `DataHello`; QUIC is
  bearer-only (`principal: None`).

## 5. Wire I/O + transports ([deep dive](transports-wire-io.md))

- `wire_io.rs` (209 lines): `read_message` / `write_message` over
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
  `validate_client_profile` (`client.rs:570-613`) /
  `validate_server_profile` (`server/config.rs:148-173`) and surfaced through
  `eggtunnel check`.
- Optional `quic` (`eggress-transport-quic`, UDP control + one bidi stream per
  external connection, platform roots + bearer only, TCP service listeners
  retained). Server transport knob is `active_connections_per_session + 1`
  (129 by default); client passes `client_open_tasks` (128) as its QUIC
  `max_concurrent_streams` — not symmetric. Role slices `quic-client` /
  `quic-server` (and `websocket-client` / `websocket-server`) allow
  single-role builds. Under Eggress 1024/4096 task caps and Eggtunnel
  64-handshake / 128-per-session admission.
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

- `eggtunnel-cli` (unpublished binary `eggtunnel`, all features, 1407 lines):
  `version | check [--json] <file> | client <file> | server <file>`
  (`main.rs:31-64`, dispatch `1030-1060`, handlers `run_check 824-870`,
  `run_server 872-944`, `run_client 958-1028`). TOML file +
  `token_env`/`outbound_proxy_env` indirection (secrets in env, never in
  file). `check` is structural only — no PEM parsing, DNS resolution, or
  dialing. Non-secret `--overrides` are CLI > TOML > built-in; there is no
  `--token` flag.
- Resolution pipeline: parse (`read_config`) → overrides
  (`apply_client_overrides` / `apply_server_overrides`) → single-read
  snapshot (`resolve_client_with` / `resolve_server_with`, every env var and
  file read exactly once) → `client_builder` / `server_builder` translate
  the snapshot into `ClientBuilder` / `ServerBuilder`; `check` and startup
  share `validate()`; startup calls single `start()` / `bind()`. Typed
  transport/CA/mTLS/proxy matrix is library-owned; CLI owns file shape, env
  lookup, pair-completeness, and role-only rules (e.g. server+proxy). CLI pins
  `RuntimePolicy::default()` and bool-derived `BindPolicy`; custom ceilings
  are builder-only for embedders. No dynamic-service TOML key (CLI requires
  ≥1 service; library allows empty; `register_service` is programmatic-only).
- Server loop prints newly observed `effective_binds` every 250 ms; client
  loop tracks `session_ready`/`session_lost`; `--snapshot-interval-secs`
  (min 5 s) streams `Snapshot` JSON events; Ctrl-C → graceful joined
  `shutdown()` on both sides.
- Library facade `crates/eggtunnel/src/lib.rs` (35 lines,
  `forbid(unsafe_code)`, feature-gated re-exports, `eggtunnel::proto` alias
  plus `Endpoint`/`EndpointError`); embedder docs (`docs/API.md`,
  `docs/EMBEDDING.md`, `fixtures/embedder/` with minimal `client+tls`,
  caller runtime/tracing, `TargetConnector`, and dynamic registration);
  `examples/client.toml`, `examples/server.toml`.

## 7. Ops, tooling, distribution, process docs ([deep dive](ops-tooling-distribution.md))

- Workspace `0.2.0` is the current published line (`Cargo.toml:6`, proto pin
  `0.2.0`); published crates.io line, tag `v0.2.0`, and GitHub release are
  `0.2.0` (`docs/DISTRIBUTION.md:5-9`,
  `plans/closure/reverse-session/012-status.md` +
  `plans/closure/reverse-session/013-status.md`). Wire is v1.1 with 1.0
  fallback (never conflate crate `0.2.0` with wire `1.1`).
- Release surface unchanged: 4 targets (linux x64/arm64, macOS Intel/arm64),
  tag-triggered `release.yml` (tag `v*`, version gate, per-runner
  install/version smoke, global `SHA256SUMS` + attestations, `install.sh`
  4-triple allowlist). `scripts/test-install.sh` pins `0.2.0`.
- CI is 4 jobs (check / 14 feature slices / MSRV 1.89 / minimal-deps);
  M001–M016 closed (M014 topology consolidation, M015 CLI resolution surface,
  M016 capability negotiation; latest hosted CI evidence in
  `016-status.md`); sustained fuzz/soak stays developer-invoked, not per-push.
  Post-M016 test volume is lib 113 + CLI bin 14 + CLI integ 7 + proto 10.
- Supply chain: Eggress `=1.0.8`, `cargo audit` 0 vulns + 1 informational
  `atomic-polyfill` unmaintained finding, `cargo deny check licenses` pass
  (4 clarifies, 10-entry permissive allow). API/EMBEDDING snippets at
  `version = "0.2"` track the published line. `THIRD_PARTY_NOTICES.md` is a
  per-release generated artifact, not committed.
- `scripts/generate-third-party-notices.py`, `scripts/test-install.sh`,
  `install.sh`, `.github/workflows/ci.yml` + `release.yml`,
  `deny.toml` (`cargo deny`), `Cargo.lock` (225 packages).
- `docs/` (ARCHITECTURE, PROTOCOL, CONFIGURATION, SECURITY, SUPPORT,
  OPERATIONS, DISTRIBUTION, API, EMBEDDING) and `plans/` (spec, terminology,
  roadmap, ADRs 0001–0002, implementation/closure evidence incl. M001–M016,
  subsystem roadmaps). Supported release targets + qualification state in
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
Ping/Pong/Drain/Error`, with capability-1 `RegisterReject` and capability-2
`Drain` deadline). Data path: one TLS connection (or QUIC bidi stream /
WSS byte stream) per external connection (`DataHello` then opaque bytes via
`eggress-relay`). Server picks effective binds (per-registration `BindPolicy`
+ `RuntimePolicy` ceilings); client picks local targets. Auth always runs
inside verified TLS; bearer token required in every profile, mTLS adds a
pinned leaf identity where enabled. Composition is builder-first
(`ClientBuilder` / `ServerBuilder` + `RuntimePolicy`); the CLI is a thin
all-features consumer with parse → override → single-read-resolve →
validate → `start`/`bind` semantics.

## Review guidance

Start here, then open the deep dive for the component under review. Each dive
documents responsibilities, key types/functions, state machines and sequence
flows, error/limit/observability behavior, feature gates, test pointers, and
review checklists with file:line anchors. Anchors were re-verified for the
post-M016 tree; where a dive still cites a pre-split path, prefer the
module/file named in the index table above (`client.rs` + `client/`,
`server.rs` + `server/`, `common.rs` + `endpoint.rs`).
