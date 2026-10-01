# CLI + Config + Operations — Deep Dive

> Parent: [Architecture Overview](overview.md) §6. This file is the review-oriented deep dive for the configuration frontend, runtime CLI loops, library facade, and embedding/operations docs.

Sources (all paths relative to repo root): `crates/eggtunnel-cli/src/main.rs` (1548 lines), `crates/eggtunnel-cli/Cargo.toml`, `crates/eggtunnel/src/lib.rs` (35 lines), `crates/eggtunnel/Cargo.toml`, `examples/client.toml`, `examples/server.toml`, `fixtures/embedder/src/main.rs`, `fixtures/embedder/Cargo.toml`, `docs/CONFIGURATION.md`, `docs/API.md`, `docs/EMBEDDING.md`, `docs/OPERATIONS.md`.

---

## 1. CLI surface

### 1.1 Binary and subcommands

The CLI crate is an unpublished binary (`crates/eggtunnel-cli/Cargo.toml:19-22`, `publish = false` at `crates/eggtunnel-cli/Cargo.toml:10`) named `eggtunnel`. It is a thin consumer of the `eggtunnel` library with all transports enabled (see §5.1).

Argument parsing is `clap` derive-based (`crates/eggtunnel-cli/src/main.rs:24-127`):

| Subcommand | CLI syntax | Handler | Effect |
|---|---|---|---|
| `Version` | `eggtunnel version` | `crates/eggtunnel-cli/src/main.rs:1136-1138` | Prints `eggtunnel <CARGO_PKG_VERSION>` via `env!`. No config, no I/O. |
| `Check` | `eggtunnel check [--json] <config>` (`PathBuf`) | `run_check` (`crates/eggtunnel-cli/src/main.rs:912-958`) | Parse → resolve → library `validate()`; human `configuration is structurally valid` or one `eggtunnel.check/v1` JSON object. Exit code non-zero on any `Err` via `main() -> Result`. |
| `Server` | `eggtunnel server [--json] [--snapshot-interval-secs N] [--overrides…] <config>` | `run_server` (`crates/eggtunnel-cli/src/main.rs:960-1048`) | Parse → overrides → resolve → `validate()` (inside `bind()`) → bind-print loop → Ctrl-C → `shutdown().await`. |
| `Client` | `eggtunnel client [--json] [--snapshot-interval-secs N] [--overrides…] <config>` | `run_client` (`crates/eggtunnel-cli/src/main.rs:1054-1128`) | Parse → overrides → resolve → `validate()` (inside `start()`) → print waiting line → Ctrl-C → `shutdown().await`. |

Non-secret overrides ride on every mode (`ClientOverrides` at `crates/eggtunnel-cli/src/main.rs:69-100`,
`ServerOverrides` at `:102-127`): endpoints, TLS names, transport names,
file paths, token/proxy *variable names*, single-service `--bind-port`,
and one-way `--allow-public-service-binds`. Precedence is CLI > TOML >
built-in (see §2). There is deliberately no `--token` flag.

`main` itself is `#[tokio::main]` (`crates/eggtunnel-cli/src/main.rs:1133-1134`), so the CLI owns its runtime. This is the opposite of the library, which requires a caller-owned runtime (see §5).

### 1.2 TOML schema: `FileConfig` / `FileService`

Deserialization structs at `crates/eggtunnel-cli/src/main.rs:200-246` (syntax only — no environment, file, or semantic work). Unknown fields are rejected; missing-field behavior is per-field `Option`/default.

#### `FileConfig` (`crates/eggtunnel-cli/src/main.rs:200-231`)

| TOML key | Rust field / type | Default | Used by | Notes |
|---|---|---|---|---|
| `mode` | `mode: String` (`crates/eggtunnel-cli/src/main.rs:199`) | **required** (no default) | mode dispatch in `resolve_client`/`resolve_server` | Must be exactly `client` or `server`; anything else is `config_resolution` (`crates/eggtunnel-cli/src/main.rs:501-507`, `:595-601`). |
| `transport` | `transport: String` (`crates/eggtunnel-cli/src/main.rs:201`) | `default_transport()` → `"tcp_tls"` (`crates/eggtunnel-cli/src/main.rs:229-231`) | both | Must be exactly `tcp_tls`, `quic`, or `websocket_tls`, mapped by `client_transport`/`server_transport` (`crates/eggtunnel-cli/src/main.rs:441-463`) to a typed profile; unknown names are `transport` errors. No other aliases. |
| `token_env` | `token_env: String` (`crates/eggtunnel-cli/src/main.rs:202`) | **required** | both | **Name** of env var, not the secret. Read once by `load_token_with` (§1.3). |
| `listen_addr` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:204`) | `None` | server only | Required in server mode; parsed as `SocketAddr` (`crates/eggtunnel-cli/src/main.rs:610-622`, `bind_validation`). |
| `tls_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:206`) | `None` | server only | Required in server mode; file read once and must be non-empty (`tls_material` otherwise). |
| `tls_key` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:208`) | `None` | server only | Required in server mode; file read once and must be non-empty. |
| `allow_public_service_binds` | `bool` (`crates/eggtunnel-cli/src/main.rs:210`) | `false` | server only | Maps 1:1 to `ServerConfig.allow_public_service_binds`. Default is loopback-only. |
| `server_addr` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:212`) | `None` | client only | Required in client mode; parsed by the canonical library `Endpoint::parse` (hostname allowed, unlike `listen_addr`). |
| `tls_server_name` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:214`) | `None` | client only | Required, must be non-empty in client mode. Used as TLS SNI/verification name end-to-end (also over proxy). |
| `ca_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:216`) | `None` | client only (file path) | Optional. If present, file read once and must be non-empty. `None` means Eggress system-root verifier (`crates/eggtunnel/src/client.rs:650-659`). |
| `outbound_proxy_env` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:218`) | `None` | client only | **Name** of env var holding the proxy URI/chain. Must be non-empty name; referenced var must exist, be non-blank, and pass `validate_outbound_proxy`. |
| `client_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:220`) | `None` | client only (mTLS) | Must be paired with `client_key`. Both files read once; both must be non-empty. |
| `client_key` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:222`) | `None` | client only (mTLS) | Must be paired with `client_cert`. |
| `client_ca` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:224`) | `None` | server only (mTLS) | Optional trust roots for client certs. If present, file read once and must be non-empty. Rejected on QUIC/WSS. |
| `services` | `Vec<FileService>` (`crates/eggtunnel-cli/src/main.rs:226`) | `[]` (empty vec) | client only | Must be non-empty in client mode. Ignored in server mode (not validated). |

#### `FileService` (`crates/eggtunnel-cli/src/main.rs:234-241`)

| TOML key | Rust field / type | Default | Notes |
|---|---|---|---|
| `id` | `id: u64` (`crates/eggtunnel-cli/src/main.rs:235`) | **required** | Wrapped as `ServiceId(u64)` in `resolve_client_services` (`crates/eggtunnel-cli/src/main.rs:465-491`). |
| `name` | `name: String` (`crates/eggtunnel-cli/src/main.rs:236`) | **required** | Validated by `ServiceName::new` (wire rules: ≤128 B, `[A-Za-z0-9-_.]`; see overview §1). Failure aborts resolution (`config_resolution`). |
| `target_host` | `target_host: String` (`crates/eggtunnel-cli/src/main.rs:237`) | **required** | Validated by `TcpTarget::new` (≤253 B host). Client-owned and client-authoritative; the server receives it inside `RegisterService` as non-authoritative bounded metadata only. |
| `target_port` | `target_port: u16` (`crates/eggtunnel-cli/src/main.rs:238`) | **required** | `u16`; TOML out-of-range is a deserialize error in `read_config`. |
| `bind_port` | `bind_port: u16` (`crates/eggtunnel-cli/src/main.rs:240`) | `0` | Server-side requested port. `0` = ephemeral loopback. Always lowered to `RequestedBind::Loopback { port }` — the CLI cannot request a non-loopback bind directly (public binds still gated by server `allow_public_service_binds` policy). Overridable per-file via `--bind-port` only when exactly one service is configured. |

`resolve_client_services()` (`crates/eggtunnel-cli/src/main.rs:465-491`) is the single lowering point from `FileService` to `ClientService::new(ServiceId, ServiceName, RequestedBind::Loopback, TcpTarget)`. It runs once inside `resolve_client_with`, whose output feeds both `check` (via `client_builder(resolved).validate()`) and the client runtime path (via `client_builder(resolved).start()`). There is no dynamic-service TOML key; the CLI requires ≥1 service while the library accepts an empty `services` vec for fully dynamic embedders (see `docs/API.md:27-29`). `RuntimePolicy`/`BindPolicy` are not TOML-configurable — the CLI uses `RuntimePolicy::default()` and derives `BindPolicy` from the single `allow_public_service_binds` bool.

### 1.3 `token_env` indirection

```rust
// crates/eggtunnel-cli/src/main.rs:323-341
fn load_token_with(
    env_name: &str,
    var: &dyn Fn(&str) -> Result<String, env::VarError>,
) -> Result<SecretToken, CliError> {
```

- The TOML file stores only the **variable name** (`token_env = "EGGTUNNEL_TOKEN"`). The secret itself lives in the environment. This is reiterated in `docs/CONFIGURATION.md:3-4` and `docs/API.md:42`.
- Each variable is read **exactly once** per resolution (`resolve_client_with` at `crates/eggtunnel-cli/src/main.rs:496-585`, `resolve_server_with` at `:590-640`). The resolved `SecretToken` is owned by `ResolvedClient`/`ResolvedServer` from then on; builders take owned values and never re-read the environment, so rotation between `check` and `start`/`bind` does **not** propagate — the process runs from its snapshot.
- An empty name is rejected up front with `config_resolution` (`token_env must name an environment variable`), mirroring the `outbound_proxy_env` guard, so no lookup of the empty name is ever issued. Unset names are `missing_secret_reference` (`required environment variable {env_name} is not set`); empty/oversize values are `config_resolution`. Messages name the variable, never the value. `SecretToken` has redacted `Debug` and `zeroize` on drop (`crates/eggtunnel/src/common.rs:36-46`).
- The lookup is injectable (`&dyn Fn`) so tests prove single-read snapshot semantics without touching ambient process state (`crates/eggtunnel-cli/src/main.rs:1039-1120`).

### 1.4 Transport strings

| TOML `transport` | CLI mapping | Library feature required | Semantics (per `docs/CONFIGURATION.md:42-67`) |
|---|---|---|---|
| `"tcp_tls"` (default) | `ClientTransportProfile::TcpTls` / `ServerTransportProfile::TcpTls` via `client_transport` / `server_transport` (`crates/eggtunnel-cli/src/main.rs:441-463`) | `tls` (+ `mtls` / `outbound-proxy` for those variants) | Baseline TCP+TLS via `eggress-transport-tls`, Rustls. |
| `"quic"` | `ClientTransportProfile::Quic` / `ServerTransportProfile::Quic` | `quic` (umbrella; role slices `quic-client`/`quic-server` in the library) | UDP control endpoint on `listen_addr`; service listeners remain TCP (`docs/OPERATIONS.md:11-12`). Platform roots + bearer only. |
| `"websocket_tls"` | `ClientTransportProfile::WebSocket` / `ServerTransportProfile::WebSocket` | `websocket` (umbrella; role slices in the library) | Verified TLS first, then binary WebSocket upgrade. Non-browser endpoint; no mTLS. |

Unknown strings are `transport` errors at resolution time — there is no silent fallthrough to `TcpTls`. Runtime startup is `server_builder(resolved).bind().await` / `client_builder(resolved).start().await` from the already-resolved snapshot; there is no per-transport `Server::bind*` / `Client::start*` dispatch in the CLI. See review checklist §7.

---

## 2. Resolution pipeline (parse → override → resolve → validate → launch)

Every config-consuming path runs the same five stages exactly once.
`run_check` at `crates/eggtunnel-cli/src/main.rs:912-958`,
`run_server` at `:960-1048`, and `run_client` at `:1054-1128` share stages
1–4; only stage 5 differs (report vs bind vs start). There is no second
validation pass and no re-read of environment variables or files between
validation and launch: builders take owned values out of the resolved
snapshot. First failure wins (sequential `?`, no error accumulation);
every failure is a `CliError` (`crates/eggtunnel-cli/src/main.rs:161-196`)
with a stable `ErrorCategory` (`:130-158`).

### 2.1 Stages 1–2: syntax parse + non-secret overrides

| Stage | Code | Behavior |
|---|---|---|
| 1. Parse | `read_config` (`crates/eggtunnel-cli/src/main.rs:245-255`) | Missing/unreadable file and invalid TOML are `config_parse`. No environment, file-content, or semantic work. |
| 2. Overrides | `apply_client_overrides` (`:257-293`) / `apply_server_overrides` (`:294-321`) | Each `Some(...)` CLI flag overwrites its TOML field; `None` leaves it. `--bind-port` requires exactly one configured service (`config_resolution`, deterministic selector). `--allow-public-service-binds` is one-way enable. Precedence: CLI > TOML > built-in. |

### 2.2 Stage 3: single-read resolution

`resolve_client_with` (`crates/eggtunnel-cli/src/main.rs:496-585`) and
`resolve_server_with` (`:590-640`) read every environment variable and
file **exactly once** into `ResolvedClient` (`:374-406`) /
`ResolvedServer` (`:407-440`). Both snapshots have hand-written redacted
`Debug` (tokens/keys/proxy values render as `[REDACTED]`/`[configured]`)
and no `Serialize` impl, so secrets cannot leak through diagnostics or a
future derive.

| Order | Check | Code | Failure / reason |
|---|---|---|---|
| R0 | Mode matches command | resolve entry (`:501-507`, `:595-601`) | `mode must be 'client'/'server' for the … command` (`config_resolution`). |
| R1 | Transport name | `client_transport`/`server_transport` (`:441-463`) | Anything but the three exact strings (`transport`). No silent `TcpTls` fallthrough. |
| R2 | Endpoint shape | client `server_addr` via library `Endpoint::parse` (`:508-515`); server `listen_addr` via `SocketAddr::from_str` (`:610-622`) | Missing/invalid → `config_resolution` / `bind_validation`. See §4. |
| R3 | Required scalar presence | `tls_server_name` non-empty (`:516-525`); ≥1 service (`resolve_client_services` at `:465-491`); mTLS pair completeness (`:532-537`); server `outbound_proxy_env` absence (`:602-607`); proxy-name non-empty (`:539-547`) | `config_resolution` in all cases. The CLI-owned ≥1-service floor remains (the library accepts empty for dynamic embedders). |
| R4 | Token variable (read once) | `load_token_with` (`:323-341`) | Unset → `missing_secret_reference`; empty/oversize → `config_resolution` (`SecretToken::new` rule, `crates/eggtunnel/src/common.rs:20-28`). |
| R5 | Files (read once each) | `read_material` (`:342-360`) / `read_required_material` (`:361-373`) | Missing/unreadable → `config_resolution` (message names the field, never contents); empty → `tls_material`. **PEM is not parsed** — malformed material passes resolution and fails at TLS-build time (`tls_material`, see §7.4). |
| R6 | Proxy value (read once) | `:548-566` | Missing var → `missing_secret_reference`; blank → `config_resolution`. URI/chain shape is library-validated (`validate_client_profile` → `parse_outbound_proxy`, mapped to `invalid outbound proxy chain`). Supported families per `docs/CONFIGURATION.md:69-80`: direct, HTTP CONNECT, SOCKS5, `__`-separated chains; userinfo auth. |
| R7 | Service lowering | `resolve_client_services` (`:465-491`) | Bad `ServiceName` (charset/length) or bad `TcpTarget` (host length/port rules) from the proto crate (`config_resolution`). |

### 2.3 Stages 4–5: builder lowering + library validation, then launch

`client_builder(resolved)` (`crates/eggtunnel-cli/src/main.rs:641-660`)
assembles `ClientConfig` from owned snapshot values, applies the typed
profile + `RuntimePolicy::default()`, and attaches the already-resolved
proxy string / `ClientIdentity` without re-reading anything.
`server_builder(resolved)` (`:661-681`) does the same for
`ServerConfig` + `BindPolicy` + optional client CA. `check` then calls
`.validate()`; `client`/`server` call `.start()`/`.bind()`, which
validate first — so the typed transport/CA/mTLS/proxy matrix is
library-owned (`validate_client_profile` at
`crates/eggtunnel/src/client.rs:570-614`, `validate_server_profile` at
`crates/eggtunnel/src/server/config.rs:148-173`). Startup TLS-build
failures map to `tls_material`, bind/connect failures to
`runtime_start` (via `From<TunnelError>`, `:184-196`).

### 2.4 Error taxonomy at the CLI boundary

`ErrorCategory` (`crates/eggtunnel-cli/src/main.rs:130-158`) is the
stable coarse vocabulary for JSON and exit status:
`config_parse`, `config_resolution`, `missing_secret_reference`,
`tls_material`, `profile_validation`, `bind_validation`,
`runtime_start`, `transport`, `authentication`. Messages name
variables/paths, never secret values or key material. Exit status stays
nonzero for every failure; JSON `check` failures still print the
`ok:false` object to stdout before the process exits.

### 2.5 Rejected-combination summary (why)

| Combination | Where rejected | Why (per code + docs) |
|---|---|---|
| QUIC + `ca_cert` | Library: `validate_client_profile` (`crates/eggtunnel/src/client.rs:585-596`) via builder `validate()` | Eggress QUIC uses platform roots; custom bundles unsupported (`docs/CONFIGURATION.md:57-59`). |
| QUIC + `client_cert`/`client_key` (client) / `client_ca` (server) | Library: client `crates/eggtunnel/src/client.rs:585-596`; server `validate_server_profile` (`crates/eggtunnel/src/server/config.rs:165-169`) | QUIC adapter has no mTLS identity path; server mTLS is TCP/TLS-only. |
| QUIC + `outbound_proxy_env` | Library: client `crates/eggtunnel/src/client.rs:585-596` | QUIC is UDP; proxy traversal unsupported (`docs/OPERATIONS.md:40-41`, `59-60`). |
| WSS + mTLS (`client_cert`/`key` or `client_ca`) | Library: client `crates/eggtunnel/src/client.rs:597-600`; server `crates/eggtunnel/src/server/config.rs:165-169` | Current WSS profile: bearer + CA roots only (`docs/CONFIGURATION.md:63-67`). |
| Proxy + mTLS | Library: client `crates/eggtunnel/src/client.rs:601-606` | Outbound-proxy path establishes TCP before TLS; mTLS identity not plumbed through it. |
| Malformed proxy URI/chain | Library: `parse_outbound_proxy` (`crates/eggtunnel/src/client.rs:522-528`) via builder `validate()`; re-exported as `validate_outbound_proxy` (`crates/eggtunnel/src/lib.rs:22`) | `OutboundConnector::from_pproxy_uri` rejects; mapped to `invalid outbound proxy chain`. |
| Server + `outbound_proxy_env` | CLI-owned (`resolve_server_with`, `crates/eggtunnel-cli/src/main.rs:602-607`) | Proxy is a client-egress concept; server never dials out via proxy. |
| mTLS half-pair | CLI-owned (`:532-537`) | Identity requires both cert chain and key; one without the other is a certain startup failure. |
| Empty proxy env name | CLI-owned (`:539-547`) | `""` names no variable. |
| Missing/blank proxy value | CLI-owned (`:548-566`, shape deferred to library) | Name must resolve to a non-blank value; URI shape is library-validated. |
| Empty mTLS/CA/cert/key files | CLI-owned (`read_material`, `:342-360`) | Non-emptiness only; PEM parsing deferred to startup (see §7.4). |
| `bind_port` nonzero public intent via CLI | N/A (structural) | CLI always builds `RequestedBind::Loopback` (`resolve_client_services` at `:465-491`); public exposure additionally requires server `allow_public_service_binds = true` + `BindPolicy` (`crates/eggtunnel/src/common.rs:83-93`). |

### 2.6 `check --json` schema

`CheckReport` (`crates/eggtunnel-cli/src/main.rs:782-793`) serializes one
`eggtunnel.check/v1` object: `ok`, `mode`, `transport`, `services`,
`custom_ca`/`mtls`/`outbound_proxy` booleans, and a null-or-`{category,
message}` error. No raw configuration, paths, or secret-bearing values
appear. Human output (`configuration is structurally valid`) remains the
default.

---

## 3. Runtime behavior

### 3.1 Server path (`run_server` at `crates/eggtunnel-cli/src/main.rs:960-1048`)

1. Validate `--snapshot-interval-secs` first (a flag error must not open listeners and *then* fail), then parse → overrides → `resolve_server` → `server_builder(resolved).bind().await`. `bind()` runs `validate()` first (`crates/eggtunnel/src/server/config.rs:118-129`), so startup enforces the same library matrix as `check` — from the same snapshot, with no re-read.
2. Startup event (JSON) or `server listening on {addr}` with `server.local_addr()` (human).
3. Effective-bind loop: a `printed: HashSet<(SessionId, ServiceId, [u8;16], u16)>`, a 250 ms `refresh` interval, and `tokio::select!` over `ctrl_c` vs `refresh.tick()` vs the optional snapshot ticker. Each tick snapshots `handle.snapshot().effective_binds` and prints/emits each never-before-seen key — human `service {id} session {session:?} listening on [{ipv6}]:{port}` (address via `Ipv6Addr::from(bind.address)`, so IPv4 appears as `::ffff:a.b.c.d`), or a `service_bind` JSON event with the same fields. Matches `docs/OPERATIONS.md:7-10`.
4. `--snapshot-interval-secs N` (validated by `validate_snapshot_interval` before any bind/start, minimum 5 s) adds a periodic `snapshot` event rendered by `snapshot_event` straight from the bounded library `Snapshot` — counters, heartbeat health, termination, and the bind list. The ticker helper (`futures_time_tick` at `:1050`) creates no ticker when disabled.
5. Shutdown: `break` on Ctrl-C → `shutdown` JSON event (`reason: signal`) → `server.shutdown().await`, which sends the bounded Drain before joining (per `docs/OPERATIONS.md:4-5`).

### 3.2 Client path (`run_client` at `crates/eggtunnel-cli/src/main.rs:1054-1128`)

1. Validate `--snapshot-interval-secs` first (a flag error must not start the runtime and *then* fail), then parse → overrides → `resolve_client` → `client_builder(resolved).start().await` (validates first via `crates/eggtunnel/src/client/config.rs:142-155`).
2. Startup event (JSON: version, mode, transport, service count) or `client started; waiting for authenticated session` (human).
3. Session tracking loop: each 250 ms tick compares `snapshot.connected` against the previous tick and emits `session_ready` (generation + registered services) on false→true and `session_lost` (termination + reconnects) on true→false. Same snapshot-ticker and shutdown-event shape as the server path.
4. Reconnects use bounded exponential backoff with jitter; bad auth/service authorization stops retries (`docs/OPERATIONS.md:14-17`).

Both paths require the `signal` Tokio feature, declared in `crates/eggtunnel-cli/Cargo.toml:18`. JSON rendering failure can never corrupt tunnel state: events are printed with `println!` outside the tunnel tasks, and a broken stdout pipe terminates the CLI cleanly.

---

## 4. Endpoint parsing: `SocketAddr` vs library `Endpoint`

`listen_addr` (server bind target) is parsed as `SocketAddr` inline in `resolve_server_with` (`crates/eggtunnel-cli/src/main.rs:610-622`, `bind_validation`): numeric IP + port only, so DNS hostnames are rejected; port 0 is accepted (ephemeral bind is legal).

`server_addr` (client dial target) is parsed by the canonical library `Endpoint::parse` (`crates/eggtunnel/src/endpoint.rs:42-107`) — the same semantic owner the client runtime uses, so CLI and library can no longer drift:

| Aspect | Library `Endpoint` |
|---|---|
| Shapes | `host:port` (DNS-like or IPv4) or `[ipv6]:port` (bracketed literal only) |
| DNS names | Accepted (`tunnel.example.net:9443`) |
| IPv6 | Bracket contents must parse as `Ipv6Addr`; unbracketed `::1:9443` is rejected as ambiguous |
| Host hygiene | Empty hosts, whitespace/control characters, and URL-authority-ambiguous characters (`/?#@[]\"'<>`) rejected; length capped at `MAX_TARGET_HOST_BYTES` |
| Port rules | Decimal `1..=65535`; zero/empty/non-numeric rejected (`InvalidPort`) |
| Return | `Endpoint` keeping the original text (`as_str()`), host (`host()`), and port (`port()`) — DNS resolution stays deferred to Tokio connect |

Edge cases reviewers should keep in mind (see §7.5):

- `Endpoint` is stricter than the old CLI `checked_endpoint`: unbracketed IPv6 and URL-ambiguous hosts that previously passed `check` now fail with `bind_validation`. Per M015 §11 this is a correctness fix, documented in `docs/CONFIGURATION.md`.
- Neither parser logs or redacts — safe because neither handles secrets, only addresses.
- Effective-bind display is v6-normalized (`Ipv6Addr::from`), so IPv4 service addresses print as `::ffff:127.0.0.1`-style. Log scrapers matching `127.0.0.1:port` will miss them — note for operations dashboards.
- No `bind_port` range check at `check` time. Any `u16` is accepted; out-of-policy ports are a runtime `RegisterAck`/`OpenReject` matter under `BindPolicy` (`crates/eggtunnel/src/common.rs:100-111`). `check`-green ≠ bind-granted.

---

## 5. Library facade (`crates/eggtunnel/src/lib.rs`, 35 lines)

Full file is 35 lines (`crates/eggtunnel/src/lib.rs:1-35`), now also re-exporting the canonical `Endpoint`/`EndpointError` (`:33`) for configuration adapters.

### 5.1 Safety, runtime, and re-export posture

- `#![forbid(unsafe_code)]` (`crates/eggtunnel/src/lib.rs:1`; mirrored by the CLI at `crates/eggtunnel-cli/src/main.rs:1`). The fuzz/never-panics posture for hostile input lives in `eggtunnel-proto`; the facade itself introduces no unsafe.
- **No global runtime or tracing**: documented in the crate docs (`crates/eggtunnel/src/lib.rs:2-5`) and enforced by API shape — `ClientBuilder::start` / `ServerBuilder::bind` return an error if no caller-owned Tokio runtime exists, and neither crate installs a tracing subscriber. The embedder fixture owns both (see §5.3). `docs/API.md:40-42` and `docs/EMBEDDING.md:3-6` state the same contract.
- Feature-gated re-exports:

| Export | Gate | Line |
|---|---|---|
| `client::{ApplicationStream, Client, ClientBuilder, ClientConfig, ClientHandle, ClientTransportProfile, TargetConnector, TargetContext, TargetError, TargetFuture, TargetStream}` | `feature = "client"` | `crates/eggtunnel/src/lib.rs:24-26` |
| `client::ClientIdentity` | `client` + `mtls` | `crates/eggtunnel/src/lib.rs:20` |
| `client::validate_outbound_proxy` | `feature = "outbound-proxy"` | `crates/eggtunnel/src/lib.rs:22` |
| `endpoint::{Endpoint, EndpointError}` | always | `crates/eggtunnel/src/lib.rs:33` |
| `server::{Server, ServerBuilder, ServerConfig, ServerHandle, ServerTransportProfile}` | `feature = "server"` | `crates/eggtunnel/src/lib.rs:35` |
| `common::{BindPolicy, ClientService, HeartbeatSnapshot, ResourceLimits, RuntimePolicy, SecretToken, ServiceSpec, Snapshot, TerminationCategory, TimeoutPolicy, TunnelError}` | always | `crates/eggtunnel/src/lib.rs:28-31` |
| `eggtunnel_proto as proto` | always (type alias) | `crates/eggtunnel/src/lib.rs:32` |

The `proto` alias lets CLI/embedder code refer to `eggtunnel::proto::{RequestedBind, ServiceId, ServiceName, TcpTarget}` (`crates/eggtunnel-cli/src/main.rs:6-11`, `fixtures/embedder/src/main.rs:6`) without a direct `eggtunnel-proto` dependency. `docs/API.md:5-12` makes the package roles explicit: downstream depends on `eggtunnel`, never on the CLI.

Feature definitions live in `crates/eggtunnel/Cargo.toml:15-23`: `default = ["client", "tls"]`; `client`/`server` pull Tokio + Eggress relay/TLS; `quic`, `websocket`, `outbound-proxy`, `mtls` are strictly additive. The CLI enables all of them (`crates/eggtunnel-cli/Cargo.toml:13`); the embedder fixture enables only `["client", "tls"]` with `default-features = false` (see §5.3).

### 5.2 Embedding API (what the CLI is built from)

| Type / function | Role | Key definition |
|---|---|---|
| `ClientConfig { server_addr, tls_server_name, ca_pem, token, services }` | Programmatic equivalent of the client TOML (minus `token_env` indirection) | `crates/eggtunnel/src/client/config.rs:46-54`; `Debug` redacts token and CA bytes (`:56-66`) |
| `ClientBuilder` + `ClientTransportProfile` | Typed composition: `new(config).transport(profile).runtime_policy(policy).with_connector(...).outbound_proxy(...).with_identity(...)`; `validate()` then `start()` | `crates/eggtunnel/src/client/config.rs:68-156` (`validate` at `:130-140`, `start` at `:142-155`); profile validation at `crates/eggtunnel/src/endpoint.rs:58-102` |
| `Client` + `ClientHandle` | `start` family via builder profiles (TCP/QUIC/WebSocket × connector/proxy/mTLS compositions); `handle()`, joined `shutdown().await`; handle offers `snapshot()`, `shutdown()`, `register_service(id)`, `unregister_service(id)` | `crates/eggtunnel/src/client.rs` (handle methods at `:84-139`; `shutdown` at `:507-513`) |
| `TargetConnector` / `TargetContext` / `TargetStream` / `TargetFuture` / `TargetError` | Application-owned dial: `connect(service: ClientService, context: TargetContext) -> TargetFuture`; default is TCP dial (`TcpTargetConnector`, `config.rs:32-43`); server can never rewrite the target | `crates/eggtunnel/src/client/config.rs:3-43` |
| `ServerConfig { listen_addr: SocketAddr, certificate_pem, private_key_pem, token, allow_public_service_binds }` | Programmatic equivalent of the server TOML | `crates/eggtunnel/src/server/config.rs:20-27`; `Debug` redacts key/token (`:65-78`); `Drop` zeroizes key (`:58-63`) |
| `ServerBuilder` + `ServerTransportProfile` | Typed composition: `new(config).transport(profile).runtime_policy(policy).bind_policy(policy).client_ca_pem(...)`; `validate()` then `bind()` | `crates/eggtunnel/src/server/config.rs:61-130` (`validate` at `:107-116`, `bind` at `:118-130`); profile validation at `:148-173` |
| `Server` + `ServerHandle` | `Server::bind*` legacy constructors delegate to the builder (`bind` at `:64-66`); handle offers `snapshot()` + `shutdown()` (`:54-60`) | `crates/eggtunnel/src/server.rs:33-60` |
| `BindPolicy` | Typed admission policy the CLI does not expose beyond the bool: `allow_public_addresses`, `allowed_addresses`, `allowed_port_ranges`, `allow_ephemeral_ports`, `max_services_per_session` (default 64); `validate()` + `loopback_only()` | `crates/eggtunnel/src/common.rs:83-124` |
| `RuntimePolicy` + `ResourceLimits` + `TimeoutPolicy` | Caller-selected finite ceilings/timeouts; CLI uses `RuntimePolicy::default()` and does not expose TOML knobs | `crates/eggtunnel/src/common.rs:194-316`; `HeartbeatSnapshot` at `:162-169` |
| `validate_outbound_proxy(&str)` | `parse_outbound_proxy` (`OutboundConnector::from_pproxy_uri`) mapped to `TunnelError::Configuration("invalid outbound proxy chain")` | `crates/eggtunnel/src/client.rs:517-528`; re-exported at `crates/eggtunnel/src/lib.rs:22`; enforced via builder `validate()` at `crates/eggtunnel/src/client.rs:570-614` (shared by CLI `check` and startup through the resolved snapshot) |
| `SecretToken`, `ClientService`, `Snapshot`, `TunnelError`, … | Shared vocabulary (redacted secrets, client-vs-server service views, counters, typed errors) | `crates/eggtunnel/src/common.rs:17-71`; `crates/eggtunnel/src/lib.rs:28-31` |

### 5.3 `fixtures/embedder` walkthrough

The fixture is the compile-checked proof that the §5.1 contract holds (`docs/API.md:52`, `docs/EMBEDDING.md:36-38`).

- `fixtures/embedder/Cargo.toml:1-12`: separate package (`publish = false`, empty `[workspace]` to detach), depends on `eggtunnel` by path with `default-features = false, features = ["client", "tls"]` (`:10`) — the minimal surface from `docs/API.md:18-20`. No CLI dependency. Tokio with `rt-multi-thread` + `tracing` are caller-owned (`:11-12`).
- `fixtures/embedder/src/main.rs:9-23`: `struct InProcessEcho; impl TargetConnector` — `connect` ignores the TCP target, opens a `tokio::io::duplex(16 KiB)` pair, spawns an echo task (`split` + `copy` + `shutdown`), and returns the application half as `TargetStream`. Demonstrates the "no loopback socket" path from `docs/EMBEDDING.md:25-31`.
- `fixtures/embedder/src/main.rs:25-40`: `client_config()` builds `ClientConfig` programmatically — literal `server_addr`, `tls_server_name`, `ca_pem: None` (system roots), `SecretToken::new(...)` from a caller-owned secret (no `token_env`), one `ClientService` with `RequestedBind::Loopback { port: 0 }`.
- `fixtures/embedder/src/main.rs:42-63`: `run()` builds a non-default `RuntimePolicy` (`limits.services_per_session = 8` at `:44`), calls `ClientBuilder::new(...).with_connector(Arc::new(InProcessEcho)).runtime_policy(policy).start()` (`:45-49`), registers a second service dynamically via `handle().register_service(dynamic_service).await` (`:50-59`), logs via caller-owned `tracing` (`:60`), and joins with `client.shutdown().await` (`:61`). `main` (`:65-70`) builds its own multi-thread Tokio runtime and `block_on(run())` — the exact inversion of the CLI's `#[tokio::main]` (`crates/eggtunnel-cli/src/main.rs:1003`).

---

## 6. Examples + operations

### 6.1 `examples/client.toml` (annotated)

```toml
# examples/client.toml:1 — mode selects the check_config client branch (§2.2).
mode = "client"
# examples/client.toml:2 — validated by checked_endpoint (§4); DNS allowed.
server_addr = "tunnel.example.net:9443"
# examples/client.toml:3 — required non-empty; SNI + verification name.
tls_server_name = "tunnel.example.net"
# examples/client.toml:4 — variable NAME; export EGGTUNNEL_TOKEN separately.
token_env = "EGGTUNNEL_TOKEN"
# examples/client.toml:5-6 — optional; omit => system roots.
# ca_cert = "/etc/eggtunnel/server-ca.pem"

# examples/client.toml:8-13 — at least one [[services]] required.
[[services]]
id = 1
name = "web"
target_host = "127.0.0.1"
target_port = 8080
bind_port = 0 # ephemeral loopback listener (§1.2)
```

Not shown (all optional, documented in `docs/CONFIGURATION.md:8-24`): `transport` (defaults to `tcp_tls`), `client_cert`/`client_key` (mTLS pair), `outbound_proxy_env` (proxy chain var). The doc example also demonstrates proxy URI shapes (`http://user:pass@proxy:3128`, `socks5://…`, `__`-separated chains) at `docs/CONFIGURATION.md:19-24`.

### 6.2 `examples/server.toml` (annotated)

```toml
# examples/server.toml:1 — mode selects the check_config server branch (§2.3).
mode = "server"
# examples/server.toml:2 — must be numeric SocketAddr (no DNS); port 0 legal.
listen_addr = "0.0.0.0:9443"
# examples/server.toml:3-4 — required; must exist and be non-empty at check time.
tls_cert = "/etc/eggtunnel/server-chain.pem"
tls_key = "/etc/eggtunnel/server-key.pem"
# examples/server.toml:5 — variable NAME, same indirection as client.
token_env = "EGGTUNNEL_TOKEN"
# examples/server.toml:6 — false (default) => loopback-only service binds.
allow_public_service_binds = false
```

Not shown: `transport` (defaults to `tcp_tls`), `client_ca` (mTLS trust roots; rejected on QUIC/WSS). Full server reference at `docs/CONFIGURATION.md:38-51`.

### 6.3 `docs/OPERATIONS.md` highlights

| Topic | Doc | CLI/code counterpart |
|---|---|---|
| Start / stop | `server server.toml` / `client client.toml`; both stop on Ctrl-C; server sends bounded Drain before closing (`docs/OPERATIONS.md:3-5`) | `run_server` shutdown (`crates/eggtunnel-cli/src/main.rs:1043-1048`), `run_client` shutdown (`:1122-1130`) |
| Listening / binds | `listen_addr` takes control + data; every connection starts with TLS; actual service address is server-assigned, visible via `ServerHandle` snapshot; CLI prints new addresses while running. QUIC: `listen_addr` is UDP control; service listeners stay TCP (`docs/OPERATIONS.md:7-12`) | bind-print loop (`crates/eggtunnel-cli/src/main.rs:886-916`); `ServerBuilder::bind` (`crates/eggtunnel/src/server/config.rs:118-130`) |
| Client resilience | Bounded exponential backoff + jitter on transient failures; invalid auth/authorization stops retries; registrations restored after new authenticated Session (`docs/OPERATIONS.md:14-17`) | Library reconnect supervisor (see client deep dive §3.3); CLI prints `waiting for authenticated session` or `session_ready`/`session_lost` JSON events |
| Certs / permissions | Trusted cert with SAN covering `tls_server_name`; token + key files readable only by the service account; loopback-only unless explicitly enabled (`docs/OPERATIONS.md:19-22`) | `tls_server_name` presence (`crates/eggtunnel-cli/src/main.rs:516-525`); `allow_public_service_binds` passthrough (`server_builder` at `crates/eggtunnel-cli/src/main.rs:661-681`) |
| Limits / throttling | 64 concurrent handshakes, 128 sessions, 64 services / 128 pending / 128 active per session, 128 client open tasks + control queue; per-IP auth throttle (10 fails / 60 s, 1024-source table) (`docs/OPERATIONS.md:24-28`) | `BindPolicy` defaults (`crates/eggtunnel/src/common.rs:114-124`); server constants (`crates/eggtunnel/src/server/auth.rs:20-23`); `ResourceLimits::default` (`crates/eggtunnel/src/common.rs:232-245`) |
| Snapshot monitoring | Current + high-water counts for sessions/services/pending/active/open/handshakes; latest termination category + panicked-task count; no event history or error text; counters are per-process, not persisted (`docs/OPERATIONS.md:30-35`); `--snapshot-interval-secs` streams the same `Snapshot` as JSON (`crates/eggtunnel-cli/src/main.rs:864-891`) | `handle.snapshot().effective_binds` (polled in both runtime loops); `Snapshot` type (`crates/eggtunnel/src/common.rs:151-175`) |
| Restricted egress | `outbound_proxy_env` → HTTP CONNECT / SOCKS5 / `__` chains; TLS+SNI stays end-to-end; WSS on TCP endpoint; QUIC has no proxy (`docs/OPERATIONS.md:37-48`) | CLI-owned name/value checks (`crates/eggtunnel-cli/src/main.rs:539-566`); library shape + dispatch (`crates/eggtunnel/src/client.rs:609-612` into the resolved snapshot) |

---

## 7. Review checklist

### 7.1 Config-vs-code drift

Cross-mode fields are rejected during resolution: server-only fields in client
files and client-only fields in server files cannot be silently ignored.
- [ ] **Docs vs dispatch for `bind_port`.** `docs/CONFIGURATION.md:34-36` says "`bind_port` requests the server-side service port" while the code always sends `RequestedBind::Loopback` (`resolve_client_services` at `crates/eggtunnel-cli/src/main.rs:465-491`). Public binds depend on server policy, not on any client TOML value — confirm the doc sentence is read that way and not as "set `bind_port` to a public port to get one."
- [ ] **Transport doc drift.** If a fourth transport is ever added, three places must move together: `client_transport`/`server_transport` (`crates/eggtunnel-cli/src/main.rs:441-463`), the override field docs (§1.1), and the `transport` row in §1.2.

### 7.2 Env-var handling (M015: single-read snapshot)

- [x] **Double-read eliminated.** Every variable is read exactly once per resolution (`resolve_client_with` / `resolve_server_with`); builders consume owned snapshot values, so rotation between `check` and `start`/`bind` no longer propagates. Proven by `resolved_snapshot_ignores_later_input_changes` (`crates/eggtunnel-cli/src/main.rs:1183-1196`) and the injectable lookup (`load_token_with` at `:323-341`).
- [x] **No raw `VarError`.** All environment failures map to `missing_secret_reference` (unset) or `config_resolution` (empty/unusable) with the variable *name* in the message.
- [x] **Empty-string `token_env` name.** `load_token_with` rejects it with the same explicit `token_env must name an environment variable` message the proxy-name guard uses, before issuing any lookup. Proven by `an_empty_token_reference_is_rejected_explicitly` (client and server shapes, asserting the lookup count stays zero).
- [ ] **Secrets never touch the file or argv.** Enforced by schema (no secret-valued key exists) plus the absence of `--token`/password flags — verify no future field/flag reintroduces inline secrets, per `docs/CONFIGURATION.md:85-97`. JSON/`Debug`/error paths are covered by redaction tests (`:1198-1225`, integration `tests/cli.rs`).

### 7.3 File-read error paths (M015: read-once + field-named errors)

- [x] **Decorated errors.** `read_material`/`read_required_material` (`crates/eggtunnel-cli/src/main.rs:342-373`) prefix every failure with the field (`{what} file is missing or unreadable / must not be empty / is required but not configured`).
- [x] **Read-once.** Each file is read exactly once per resolution into the snapshot; the old triple-read/TOCTOU pattern is gone.

### 7.4 Misleading `check` success (structural only)

- [ ] **No PEM parsing at `check` time.** `check` asserts existence + non-emptiness only. Garbage bytes (or a valid PEM of the wrong type) pass `eggtunnel check` and fail at TLS-build time (`tls_material`) — the doc explicitly scopes this: "`eggtunnel check` validate[s] the TOML structure…" while "server startup also parses and validates its certificate and key" (`docs/CONFIGURATION.md:82-84`). Any test or runbook that treats `check`-green as deployable is over-reading.
- [ ] **No network I/O at `check` time.** `server_addr` DNS is never resolved, ports never dialed, proxy never connected — `Endpoint::parse` is shape-level only (§4). A typo'd hostname passes `check`.
- [ ] **`ServiceName`/`TcpTarget` are the exception.** Because `resolve_client_services` runs the real proto constructors, those two validations are as strong at `check` time as at runtime. Everything else file-shaped is weaker.
- [ ] **Library re-validates anyway.** `validate_client_profile` (`crates/eggtunnel/src/client.rs:570-614`) and `validate_server_profile` (`crates/eggtunnel/src/server/config.rs:148-173`) re-apply the transport/identity/proxy matrix at `start`/`bind` time from the same snapshot, so CLI `check` is defense-in-depth, not the enforcement point for embedders.

### 7.5 IPv6 / endpoint edge cases

- [ ] **`listen_addr` cannot take DNS.** Resolution requires `SocketAddr`; `listen_addr = "localhost:9443"` fails `check` with the `must be a socket address` message. Intended (bind needs an IP), but the error message's single example (`127.0.0.1:443`) doesn't mention `[::1]:9443` for v6 users.
- [x] **`server_addr` bracket contents now validated.** The canonical `Endpoint::parse` requires bracket interiors to parse as `Ipv6Addr`; `[not-an-ip]:443` fails `check` with `bind_validation` (§4). Stricter than the old CLI parser by design (M015 §11 correctness fix).
- [x] **Unbracketed v6 rejected.** `::1:9443` is `UnbracketedIpv6`, not silently split. `docs/CONFIGURATION.md` examples remain IPv4/DNS; `[::1]:port` is accepted for v6 users.
- [ ] **Port-0 asymmetry is intentional but subtle.** `listen_addr` port 0 passes (ephemeral bind); `server_addr` port 0 is rejected (`InvalidPort`). Both are correct for their roles (bind vs dial) but the two error messages don't explain the asymmetry.
- [ ] **Effective-bind display is v6-normalized.** The server loop renders every bind via `Ipv6Addr::from`, so IPv4 service addresses print as `::ffff:127.0.0.1`-style. Log scrapers matching `127.0.0.1:port` will miss them — note for operations dashboards.
- [ ] **No `bind_port` range check at `check` time.** Any `u16` is accepted; out-of-policy ports are a runtime `RegisterAck`/`OpenReject` matter under `BindPolicy` (`crates/eggtunnel/src/common.rs:100-111`). `check`-green ≠ bind-granted.

---

*Backlink: this dive expands [Architecture Overview](overview.md) §6. For the wire behavior behind these knobs, see the client/server/transport dives; for release and CI handling of this binary, see the ops/tooling dive.*

### Library profile validation (M008/M015)

`client_builder(resolved)` (`crates/eggtunnel-cli/src/main.rs:641-660`) and
`server_builder(resolved)` (`:661-681`) translate the resolved snapshot into
the public library builders with `RuntimePolicy::default()` (client) and
`RuntimePolicy::default()` + derived `BindPolicy` (server). `check` ends with
`builder.validate()`; the runtime paths start/bind the same builder from the
same snapshot — there is no `check_config` re-validation and no second read.
Transport/CA/mTLS/proxy compatibility is library-owned
(`validate_client_profile` at `crates/eggtunnel/src/client.rs:570-614`,
`validate_server_profile` at `crates/eggtunnel/src/server/config.rs:148-173`);
file shape, environment lookup, pair-completeness, proxy name/value,
mTLS cert/key/client-CA non-emptiness, and server+proxy remain CLI-owned
(§2.2).
