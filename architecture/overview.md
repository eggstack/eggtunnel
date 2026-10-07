# Eggtunnel — Architecture Overview

Eggtunnel is a Rust reverse-tunnel library and CLI. A process behind NAT
connects **outward** to a reachable server; the server exposes approved local
services through **server-owned** listeners. Each external connection opens a
separate data connection back through the client to a **client-owned** target.

This file is the birds-eye view: what the modules are, what each one is
responsible for, and an index into the per-component deep dives in this
directory. Every line count, anchor, and ceiling below was verified directly
against the current tree.

- Workspace version **0.2.0** (`Cargo.toml:6`).
- **Wire protocol v1.1 with 1.0 fallback** — the wire version and the crate
  version are independent. Never conflate `0.2.0` with wire `1.1`.
- Dependency direction: `eggtunnel-proto` ← `eggtunnel` library ←
  CLI / embedders. Generic relay/transport primitives come from the
  `eggress-* =1.0.8` adapters, which Eggtunnel does not reimplement.

## Repository map

```text
crates/
  eggtunnel-proto/src/lib.rs      991   wire DTOs + framing (runtime-neutral)
  eggtunnel/src/
    lib.rs                         35   public facade (feature-gated re-exports)
    common.rs                     714   secrets, policy, observability, errors
    endpoint.rs                   183   client host:port parsing
    pem.rs                         64   rustls PEM helpers (mtls)
    wire_io.rs                    209   bounded framing over any stream
    client.rs                    1238   client orchestrator
    client/{config,reconnect,service_state,heartbeat,open}.rs
    server.rs                     353   server coordinator
    server/{config,tls,accept,auth,session,control,pending,service}.rs
    server_tests.rs + server_tests/{tcp,mtls,quic,websocket,proxy}.rs
  eggtunnel-cli/src/main.rs      1548   binary `eggtunnel` (publish = false)
  eggtunnel-cli/tests/cli.rs      296
fixtures/embedder/                       separate workspace, own lockfile
fuzz/                                    cargo-fuzz decoder target (opt-in)
scripts/                                 notices generator, install smoke test
.github/workflows/                       ci.yml, release.yml
docs/          user- and operator-facing guides (living)
plans/         spec, ADRs, roadmap, immutable closure evidence
architecture/  this directory — review deep dives
```

## Module / component index

| # | Component | Crate / path | Deep dive |
|---|-----------|--------------|-----------|
| 1 | Wire protocol | `eggtunnel-proto/src/lib.rs` (991) | [proto-wire-protocol.md](proto-wire-protocol.md) |
| 2 | Shared core | `common.rs` (714), `endpoint.rs` (183), `pem.rs` (64) | [common-core.md](common-core.md) |
| 3 | Reverse-session client | `client.rs` (1238) + `client/` (7 modules) | [client.md](client.md) |
| 4 | Reverse-session server | `server.rs` (353) + `server/` (8 modules) | [server.md](server.md) |
| 5 | Wire I/O + transports | `wire_io.rs` (209), Eggress `=1.0.8` | [transports-wire-io.md](transports-wire-io.md) |
| 6 | CLI + config + embedding | CLI `main.rs` (1548), facade `lib.rs` (35) | [cli-config-ops.md](cli-config-ops.md) |
| 7 | Ops, tooling, distribution | `scripts/`, `.github/`, `install.sh`, `docs/`, `plans/` | [ops-tooling-distribution.md](ops-tooling-distribution.md) |

## 1. Wire protocol — `eggtunnel-proto`

Runtime-neutral and `forbid(unsafe_code)`. Depends only on `serde`, `postcard`,
`thiserror`, `getrandom`, and `zeroize`. Owns the bounded DTOs and the framing
layer; **no socket, runtime, timer, or task dependency**. I/O adaptation lives
one layer up in `wire_io.rs`.

- Framing: `ETUN` magic (`lib.rs:13`) + `HEADER_LEN = 14` — u16 BE major, u16 BE
  minor, u16 BE message id, u32 BE payload length, then exactly one postcard
  payload. `encode_frame` `lib.rs:565-579`, `decode_frame` `lib.rs:583-635`
  (exactly-one-frame semantics, concatenated-frame friendly).
- Versioning: `PROTOCOL_MAJOR = 1`, `PROTOCOL_MINOR = 1` (`lib.rs:21-22`). Major
  mismatch is rejected (`lib.rs:592`); minor is informational and extensions are
  negotiated by capability intersection only (ADR-0002).
- **15** message IDs, `lib.rs:314-332`: `ClientHello = 1` … `Ping = 10`,
  `Pong = 11`, `Drain = 12`, `Error = 13`, `DataHello = 14`,
  `RegisterReject = 15`.
- Capabilities: `CAPABILITY_REGISTER_REJECT = 1` (correlated rejection),
  `CAPABILITY_DRAIN_DEADLINE = 2` (`lib.rs:28,32`).
- Bounds (`lib.rs:15-20`): 1 MiB frame, 4096 B token, 128 B service name,
  256 B diagnostic, 32 capabilities, 253 B target host. Deserialization
  revalidates via `serde(try_from)` / `bounded_bytes`; trailing bytes inside a
  payload are rejected.
- Tests: 11, in-crate at `lib.rs:637-991`, including
  `arbitrary_input_never_panics` and
  `documented_wire_version_and_message_ids_are_pinned`.

## 2. Shared core — `common.rs` + `endpoint.rs` + `pem.rs`

Shared vocabulary for both roles. `common.rs` carries no unconditional
socket/timer/task dependency, so only the `--no-default-features` build is pure
vocabulary. The facade re-exports **11** common types plus `Endpoint`
(`lib.rs:28-33`).

- `SecretToken`: 1–4096 B validated, redacted `Debug`, `zeroize` on drop,
  `expose()` is `pub(crate)` behind `any(client, server)`.
- `ClientService` (client view: id + name + requested bind + `TcpTarget`) vs
  `ServiceSpec` (server view, no target). The 0.2 runtime consumes wire
  `RegisterService` directly and treats the bounded `TcpTarget` as
  non-authoritative metadata — the server never rewrites a client target.
- `BindPolicy` default (`common.rs:131-140`): loopback-only
  (`allow_public_addresses: false`), ephemeral ports allowed,
  `max_services_per_session: 64`. `validate()` (`common.rs:116-128`) caps it at
  65536 and bounds the address allowlist at 1024 — decoupled from the
  services-per-session ceiling. `bind_to_socket()` / `permits_*` are server-only.
- `RuntimePolicy` = `ResourceLimits` + `TimeoutPolicy`, composed and owned by
  `Counters` as `Arc<RuntimePolicy>`.
  - `ResourceLimits` (struct `common.rs:213-222`, defaults `common.rs:249-262`):
    8 ceilings, defaults
    `sessions` 128, `services_per_session` 64, `pending_per_session` 128,
    `active_connections_per_session` 128, `accepted_handshakes` 64,
    `client_open_tasks` 128, `control_queue` 128, `client_command_queue` 32.
    `validate()` requires every value in `1..=65536`.
  - `TimeoutPolicy` (`common.rs:305-318`): 9 durations — connect 10 s, handshake
    10 s, control-idle 90 s, pending 30 s, relay-drain 15 s, shutdown-grace 1 s,
    reconnect 500 ms → 30 s, heartbeat 20 s.
  - The auth throttle is a fixed security policy **outside** `RuntimePolicy`.
- `Snapshot` / `Counters` (atomics + mutex binds, `Relaxed` loads,
  poison-tolerant) plus a bounded `HeartbeatSnapshot` (generation, last-Pong age,
  latest RTT, consecutive misses). The generation counter fails closed at
  `u64::MAX`.
- `TunnelError` has **13** variants and maps to `TerminationCategory`;
  `ServiceAlreadyExists` folds into `Authorization`, and
  `Io | Tls | Disconnected` into `Transport`.
- `verify_token` is server-only and constant-time via `subtle`.
- `Endpoint::parse` (`endpoint.rs`) handles the client `host:port` shape — DNS,
  IPv4, and bracketed IPv6 — with no transport or policy validation.
  `pem.rs` is `mtls`-gated rustls pki-types PEM helpers.

## 3. Reverse-session client — `client.rs` + `client/`

Outbound-only initiator behind NAT. It establishes one authenticated control
stream per session, registers N TCP services, then dials **one data connection
per accepted external connection** (`DataHello`, then opaque relay). **It never
listens.** `client.rs` is the orchestrator; composable logic lives in
`client/config.rs`, `client/reconnect.rs` (supervisor + transports + dial),
`client/service_state.rs` (dynamic-Service lifecycle), `client/heartbeat.rs`
(probe), and `client/open.rs` (data path).

- Composition is builder-first: `ClientBuilder` (`client/config.rs:84`) is the
  canonical surface; the 12 legacy `Client::start*` variants (10 at
  `client.rs:218-321`, plus the 2 mTLS constructors at `client.rs:412-434`)
  delegate through it.
- `TargetConnector` + `TargetContext` let embedders supply in-process
  `ApplicationStream` targets instead of TCP.
- **Dynamic Services** enter reconnect desired state only after a matching
  `RegisterAck` from the current session generation. Wire `Error` carries no
  ServiceId, so legacy-serial mode (no capability 1) keeps the
  one-registration-in-flight invariant; capability-1 `CorrelatedBounded` mode
  correlates N transactions by ServiceId via `RegisterReject`.
- Bounded heartbeat: one outstanding `(nonce, Instant)` per session, with no
  extra probe while one is outstanding.
- Data plane (`client/open.rs`): target dial before data dial, per-`Open` child
  cancel token, 16 KiB bounded relay; failures produce an advisory
  `OpenReject` code=1, semaphore overload code=2.
- Validation lives in `validate_client_profile` (`client.rs:569`): QUIC is
  platform-roots + bearer only, and proxy+mTLS, WSS+mTLS, and QUIC+proxy are
  rejected fail-closed. Proxy failures are typed, never a silent direct fallback.

## 4. Reverse-session server — `server.rs` + `server/`

The reachable rendezvous and ingress. The server **owns the listeners** and is
the only party that binds. `server.rs` is the coordinator; the runtime lives in
`server/{config,tls,accept,auth,session,control,pending,service}.rs`.

- Composition is builder-first: `ServerBuilder` (`server/config.rs:61-131`) takes
  a `ServerTransportProfile` + `BindPolicy` + `RuntimePolicy` + optional client
  CA. The legacy `Server::bind*` helpers (`server.rs:64-192` — `bind`,
  `bind_with_policy`, `bind_websocket`, `bind_quic`, `bind_quic_with_policy`,
  `bind_mtls`, `bind_mtls_with_policy`) delegate through it.
  `validate_server_profile` (`server/config.rs:149`) rejects mTLS on QUIC/WSS
  before bind.
- Session lifecycle (`serve_control`, `server/control.rs:82-286`):
  `ClientHello`/`ServerHello` (capability intersection) → `Auth`
  (constant-time check, 100 ms failure delay, per-source 10-fails/60 s/
  1024-source throttle) → `AuthOk(session_id)` → `RegisterService` /
  `RegisterAck(effective_bind)` (or capability-1 `RegisterReject`, else legacy
  `Error`) → `Open` per external accept → client data dial +
  `DataHello(session, service, connection)` → opaque relay. `Ping`/`Pong`,
  `Drain` (capability-2 peer deadline), `Error`, `UnregisterService`, and
  `OpenReject` are handled throughout.
- Pending correlation is single-use: `ConnectionId → PendingEntry` per session
  (`server/pending.rs`). External accept inserts; the client data dial consumes.
  `BindPolicy` is re-checked per registration **before** bind
  (`server/control.rs:295-448`).
- mTLS (`mtls` feature): the bearer token is still required, and the leaf
  SHA-256 principal is pinned at session creation and rechecked on every
  `DataHello`. QUIC is bearer-only (`principal: None`).
- `MAX_SESSIONS` / `MAX_HANDSHAKES` in `server_tests.rs:35-36` are
  `cfg(test)`-only aliases, **not** production ceilings.

## 5. Wire I/O + transports

- `wire_io.rs`: `read_message` / `write_message` over `AsyncRead`/`AsyncWrite`
  plus `read_boxed` / `write_boxed` over the Eggress `BoxStream`. Header-first
  read, hostile-header fast reject, length pre-check **before** payload
  allocation, and an exact-consumption check. All transports converge on
  `BoxStream`, so session logic never branches on socket type.
- Baseline `tls` (TCP+TLS via `eggress-transport-tls`, rustls `ring`, TLS 1.2+)
  on the **caller's** Tokio runtime — the library installs no global runtime or
  tracing state. SNI and system/custom-CA verification apply to the control
  stream and every data connection; auth always runs inside verified TLS.
- Optional `quic` (`eggress-transport-quic`): UDP control endpoint on
  `listen_addr` with **TCP service listeners retained**; platform roots and
  bearer only.
- Optional `websocket` (`eggress-protocol-websocket` + `tokio-tungstenite`):
  verified TLS then a binary WS upgrade, 1 MiB `max_message_size`, non-browser
  endpoint, and whole-connection close (no TCP half-close equivalence).
- Optional `outbound-proxy` (`eggress-outbound`, client-side only): direct /
  HTTP CONNECT / SOCKS5 single-hop and `__`-separated multi-hop chains
  (`socks5://a:1080__http://b:8080`), URI userinfo auth, env-var credentials,
  redacted diagnostics, and typed failures with **no silent direct fallback**.
  WSS-over-proxy is the one allowed composition.
- Eggress is pinned `=1.0.8` and supplies generic primitives; Eggtunnel owns the
  reverse-session behavior on top.

## 6. CLI + config + embedding API

- `eggtunnel` binary (`main.rs`, 1548 lines, all features, `publish = false`):
  `version | check [--json] <file> | client <file> | server <file>`
  (`main.rs:32-64`), dispatch at `main.rs:1136-1160`, handlers `run_check`
  (`912`), `run_server` (`960`), `run_client` (`1054`).
- **Secrets never appear in the file.** `token_env` / `outbound_proxy_env` name
  environment variables holding the bearer token and proxy URI/chain. There is
  no `--token` flag; non-secret `--overrides` resolve CLI > TOML > built-in.
- Resolution pipeline: `read_config` (`250`) → `apply_client_overrides` (`266`) /
  `apply_server_overrides` (`306`) → single-read `resolve_client_with` (`550`) /
  `resolve_server_with` (`658`) → `client_builder` (`721`) / `server_builder`
  (`741`) → validate → a single `start()` / `bind()`. `check` and startup share
  the library's `validate()`.
- **`check` is structural only** — no PEM parsing, no DNS, no dialing. A passing
  `check` does not mean the certs or addresses are live. It is nonetheless
  mandatory before `client`/`server`, because it rejects invalid combinations
  instead of ignoring them.
- CLI requires at least one `[[services]]`; the library allows an empty set via
  programmatic `register_service`, and there is no dynamic-service TOML key.
- Operational surface: the server loop prints newly observed `effective_binds`
  every 250 ms, the client loop tracks `session_ready` / `session_lost`, and
  `--snapshot-interval-secs` (minimum 5, requires `--json`) streams `Snapshot`
  events. Ctrl-C triggers a graceful joined `shutdown()` on both sides.
- Library facade `lib.rs` (35 lines, `forbid(unsafe_code)`,
  feature-gated re-exports, `eggtunnel::proto` alias plus
  `Endpoint`/`EndpointError`). Embedder guidance lives in `docs/API.md`,
  `docs/EMBEDDING.md`, and `fixtures/embedder` (a **separate workspace** with
  its own lockfile, minimal `client+tls`, caller runtime/tracing,
  `TargetConnector`, and dynamic registration).

## 7. Ops, tooling, distribution

- Published line is **0.2.0** across the workspace manifest, crates.io, tag
  `v0.2.0`, and the GitHub release. The release tag must equal the workspace
  `version`; the wire protocol version is independent.
- Release surface: 4 targets (linux x64/arm64, macOS Intel/arm64), a
  tag-triggered `release.yml` with a version gate, per-runner install/version
  smoke, a global `SHA256SUMS` plus attestations, and a 4-triple allowlist in
  `install.sh`. Releases build only `-p eggtunnel-cli` and regenerate notices via
  `python3 scripts/generate-third-party-notices.py`.
- CI is 4 jobs: `check` (the full gate), `feature-slices` (**14** matrix entries
  via `--no-default-features`, including the role-specific `quic-client`,
  `quic-server`, `websocket-client`, `websocket-server` slices), `msrv` (1.89),
  and `minimal-dependencies` (proves minimal slices pull no unrequested
  quic/websocket/outbound deps).
- Test volume on the current tree: library **134** passed + 3 ignored (opt-in
  soak/fuzz), CLI bin **16**, CLI integration **7**, proto **11**.
- Supply chain: Eggress pinned `=1.0.8`, `deny.toml` enforces permissive-only
  licenses (GPL/AGPL/LGPL denied; 10-entry allow, 4 clarifies), and
  `Cargo.lock` carries 230 packages. `THIRD_PARTY_NOTICES.md` is a per-release
  generated artifact and is not committed.
- `plans/closure/` records are **immutable historical evidence** — never rewrite
  a closed milestone, only add a new record. `docs/` guides are living.
- Planning state: M001–M016 + C001 closed, **M017 ready**, M018/M019/M020
  blocked. `plans/registry.md` is the control surface; the roadmap status table
  duplicates it and both must flip together. M020 (standalone
  `RuntimePolicy`/`BindPolicy` TOML) is planned but **not implemented** — the CLI
  still installs `RuntimePolicy::default()`. Do not document planned work in
  `docs/`, `README.md`, or `AGENTS.md`.
- Agent skills live in `.agents/skills/` (`verify`, `plan`, `docs-sync`,
  `release`), with `.opencode/skills/<name>` as relative symlinks to the same
  directories. Keep the symlink structure; edit the real files under
  `.agents/`. The skills carry operational rules that this review layer only
  summarizes — notably that a release **requires a version bump** (`v0.1.0` and
  `v0.2.0` are already tagged and published while `Cargo.toml:6` still reads
  `0.2.0`) and that `scripts/test-install.sh:5` hardcodes its own version copy.

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

**Control path** — exactly one authenticated control stream per session:
`ClientHello → ServerHello → Auth → AuthOk → Register* → Open/OpenReject →
Ping/Pong/Drain/Error`, with capability-1 `RegisterReject` and the capability-2
`Drain` deadline.

**Data path** — one TLS connection (or QUIC bidi stream / WSS byte stream) per
external connection: `DataHello`, then opaque bytes via `eggress-relay`.

The **server** chooses effective binds (per-registration `BindPolicy` plus
`RuntimePolicy` ceilings); the **client** chooses local targets. Auth always
runs inside verified TLS, and a bearer token is required in every profile — mTLS
adds a pinned leaf identity where enabled. Composition is builder-first
(`ClientBuilder` / `ServerBuilder` + `RuntimePolicy`); the CLI is a thin
all-features consumer with parse → override → single-read-resolve → validate →
`start`/`bind` semantics.

## 9. Cross-cutting review checklist

Use this when reviewing any component, alongside that component's own checklist.

- [ ] Secrets only via `token_env` / `outbound_proxy_env`; no token in TOML,
      flags, JSON, `Debug`, or logs.
- [ ] Bounded types revalidated on **deserialization**, not only on
      construction; trailing payload bytes rejected.
- [ ] Length pre-checked before payload allocation; hostile headers fail fast.
- [ ] `forbid(unsafe_code)` intact; no new `unsafe` anywhere.
- [ ] Feature gates stay **additive**; no default-path import leaks an optional
      dependency (CI's `minimal-dependencies` job enforces this).
- [ ] All finite ceilings and timeouts come from validated `RuntimePolicy`
      rather than new inline constants.
- [ ] The server never rewrites a client-authoritative target, and binds stay
      loopback-only unless `allow_public_service_binds = true`.
- [ ] The one-dynamic-registration-in-flight invariant holds in legacy-serial
      mode (wire `Error` carries no ServiceId).
- [ ] Proxy and transport failures are typed — no silent direct fallback.
- [ ] `Snapshot` output stays redacted and bounded.
- [ ] `cargo fmt`, `check`, `test`, `clippy -D warnings`, and `doc -D warnings`
      all pass with `--locked` and `--all-features`.
