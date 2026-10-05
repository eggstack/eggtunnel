# CLI + Config + Operations — Deep Dive

> Parent: [Architecture Overview](overview.md) §6. This file is the review-oriented deep dive for the configuration frontend, runtime CLI loops, library facade, and embedding/operations docs.

Sources (all paths relative to repo root): `crates/eggtunnel-cli/src/main.rs` (1548 lines), `crates/eggtunnel-cli/Cargo.toml`, `crates/eggtunnel/src/lib.rs` (35 lines), `crates/eggtunnel/Cargo.toml`, `examples/client.toml`, `examples/server.toml`, `fixtures/embedder/src/main.rs`, `fixtures/embedder/Cargo.toml`, `docs/CONFIGURATION.md`, `docs/API.md`, `docs/EMBEDDING.md`, `docs/OPERATIONS.md`.

---

## 1. CLI surface

### 1.1 Binary and subcommands

The CLI crate is an unpublished binary (`crates/eggtunnel-cli/Cargo.toml:20-23`, `publish = false` at `crates/eggtunnel-cli/Cargo.toml:10`) named `eggtunnel`. It is a thin consumer of the `eggtunnel` library with all transports enabled (see §5.1).

Argument parsing is `clap` derive-based (`crates/eggtunnel-cli/src/main.rs:24-126`):

| Subcommand | CLI syntax | Handler | Effect |
|---|---|---|---|
| `Version` | `eggtunnel version` | `crates/eggtunnel-cli/src/main.rs:1136-1139` | Prints `eggtunnel <CARGO_PKG_VERSION>` via `env!`. No config, no I/O. |
| `Check` | `eggtunnel check [--json] <config>` (`PathBuf`) | `run_check` (`crates/eggtunnel-cli/src/main.rs:912-958`) | Parse → resolve → library `validate()`; human `configuration is structurally valid` or one `eggtunnel.check/v1` JSON object. Exit code non-zero on any `Err` via `main() -> Result`. |
| `Server` | `eggtunnel server [--json] [--snapshot-interval-secs N] [--overrides…] <config>` | `run_server` (`crates/eggtunnel-cli/src/main.rs:960-1047`) | Parse → overrides → resolve → `validate()` (inside `bind()`) → bind-print loop → Ctrl-C → `shutdown().await`. |
| `Client` | `eggtunnel client [--json] [--snapshot-interval-secs N] [--overrides…] <config>` | `run_client` (`crates/eggtunnel-cli/src/main.rs:1054-1131`) | Parse → overrides → resolve → `validate()` (inside `start()`) → print waiting line → Ctrl-C → `shutdown().await`. |

Non-secret overrides ride on every mode (`ClientOverrides` at `crates/eggtunnel-cli/src/main.rs:69-99`,
`ServerOverrides` at `:102-126`): endpoints, TLS names, transport names,
file paths, token/proxy *variable names*, single-service `--bind-port`,
and one-way `--allow-public-service-binds`. Precedence is CLI > TOML >
built-in (see §2). There is deliberately no `--token` flag.

The complete flag set per mode (every one is a `Option<T>` except the one-way
bool; all are optional and non-secret):

| Mode | Flags | Struct |
|---|---|---|
| client | `--server-addr`, `--tls-server-name`, `--transport`, `--ca-cert`, `--token-env`, `--outbound-proxy-env`, `--client-cert`, `--client-key`, `--bind-port` | `:70-99` |
| server | `--listen-addr`, `--transport`, `--tls-cert`, `--tls-key`, `--client-ca`, `--token-env`, `--allow-public-service-binds` | `:103-126` |

`--snapshot-interval-secs N` is **not** part of either overrides struct: it is
declared on `Client`/`Server` directly and carries `requires = "json"`, so clap
rejects the combination before any config is read (`:47-50`, `:59-62`). No
flag accepts a secret value; `--token-env`/`--outbound-proxy-env` only rename
which environment variable is read (`docs/CONFIGURATION.md:108-113`).

`main` itself is `#[tokio::main]` (`crates/eggtunnel-cli/src/main.rs:1133-1134`), so the CLI owns its runtime. This is the opposite of the library, which requires a caller-owned runtime (see §5).

### 1.2 TOML schema: `FileConfig` / `FileService`

Deserialization structs at `crates/eggtunnel-cli/src/main.rs:200-246` (syntax only — no environment, file, or semantic work). Unknown fields are rejected; missing-field behavior is per-field `Option`/default.

#### `FileConfig` (`crates/eggtunnel-cli/src/main.rs:200-231`)

| TOML key | Rust field / type | Default | Used by | Notes |
|---|---|---|---|---|
| `mode` | `mode: String` (`crates/eggtunnel-cli/src/main.rs:203`) | **required** (no default) | mode dispatch in `resolve_client`/`resolve_server` | Must be exactly `client` or `server`; anything else is `config_resolution` (`crates/eggtunnel-cli/src/main.rs:554-559`, `:662-667`). |
| `transport` | `transport: String` (`crates/eggtunnel-cli/src/main.rs:205`) | `default_transport()` → `"tcp_tls"` (`crates/eggtunnel-cli/src/main.rs:233-235`) | both | Must be exactly `tcp_tls`, `quic`, or `websocket_tls`, mapped by `client_transport`/`server_transport` (`crates/eggtunnel-cli/src/main.rs:495-517`) to a typed profile; unknown names are `transport` errors. No other aliases. |
| `token_env` | `token_env: String` (`crates/eggtunnel-cli/src/main.rs:206`) | **required** | both | **Name** of env var, not the secret. Read once by `load_token_with` (§1.3). |
| `listen_addr` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:208`) | `None` | server only | Required in server mode; parsed as `SocketAddr` (`crates/eggtunnel-cli/src/main.rs:688-703`, `bind_validation`). |
| `tls_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:210`) | `None` | server only | Required in server mode; file read once and must be non-empty (`tls_material` otherwise). |
| `tls_key` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:212`) | `None` | server only | Required in server mode; file read once and must be non-empty. |
| `allow_public_service_binds` | `bool` (`crates/eggtunnel-cli/src/main.rs:214`) | `false` | server only | Maps 1:1 to `ServerConfig.allow_public_service_binds`. Default is loopback-only. |
| `server_addr` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:216`) | `None` | client only | Required in client mode; parsed by the canonical library `Endpoint::parse` (hostname allowed, unlike `listen_addr`). |
| `tls_server_name` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:218`) | `None` | client only | Required, must be non-empty in client mode. Used as TLS SNI/verification name end-to-end (also over proxy). |
| `ca_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:220`) | `None` | client only (file path) | Optional. If present, file read once and must be non-empty. `None` means Eggress system-root verifier (`crates/eggtunnel/src/client.rs:649-657`). |
| `outbound_proxy_env` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:222`) | `None` | client only | **Name** of env var holding the proxy URI/chain. Must be non-empty name; referenced var must exist, be non-blank, and pass `validate_outbound_proxy`. |
| `client_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:224`) | `None` | client only (mTLS) | Must be paired with `client_key`. Both files read once; both must be non-empty. |
| `client_key` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:226`) | `None` | client only (mTLS) | Must be paired with `client_cert`. |
| `client_ca` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:228`) | `None` | server only (mTLS) | Optional trust roots for client certs. If present, file read once and must be non-empty. Rejected on QUIC/WSS. |
| `services` | `Vec<FileService>` (`crates/eggtunnel-cli/src/main.rs:230`) | `[]` (empty vec) | client only | Must be non-empty in client mode. A non-empty list in server mode is a `config_resolution` cross-mode error (`:680-686`), not ignored. |

#### `FileService` (`crates/eggtunnel-cli/src/main.rs:237-246`)

| TOML key | Rust field / type | Default | Notes |
|---|---|---|---|
| `id` | `id: u64` (`crates/eggtunnel-cli/src/main.rs:240`) | **required** | Wrapped as `ServiceId(u64)` in `resolve_client_services` (`crates/eggtunnel-cli/src/main.rs:519-544`). |
| `name` | `name: String` (`crates/eggtunnel-cli/src/main.rs:241`) | **required** | Validated by `ServiceName::new` (wire rules: ≤128 B, `[A-Za-z0-9-_.]`; see overview §1). Failure aborts resolution (`config_resolution`). |
| `target_host` | `target_host: String` (`crates/eggtunnel-cli/src/main.rs:242`) | **required** | Validated by `TcpTarget::new` (≤253 B host). Client-owned and client-authoritative; the server receives it inside `RegisterService` as non-authoritative bounded metadata only. |
| `target_port` | `target_port: u16` (`crates/eggtunnel-cli/src/main.rs:243`) | **required** | `u16`; TOML out-of-range is a deserialize error in `read_config`. |
| `bind_port` | `bind_port: u16` (`crates/eggtunnel-cli/src/main.rs:245`) | `0` | Server-side requested port. `0` = ephemeral loopback. Always lowered to `RequestedBind::Loopback { port }` — the CLI cannot request a non-loopback bind directly (public binds still gated by server `allow_public_service_binds` policy). Overridable per-file via `--bind-port` only when exactly one service is configured. |

`resolve_client_services()` (`crates/eggtunnel-cli/src/main.rs:519-544`) is the single lowering point from `FileService` to `ClientService::new(ServiceId, ServiceName, RequestedBind::Loopback, TcpTarget)`. It runs once inside `resolve_client_with`, whose output feeds both `check` (via `client_builder(resolved).validate()`) and the client runtime path (via `client_builder(resolved).start()`). There is no dynamic-service TOML key; the CLI requires ≥1 service while the library accepts an empty `services` vec for fully dynamic embedders (see `docs/API.md:27-29`). `RuntimePolicy`/`BindPolicy` are not TOML-configurable — the CLI uses `RuntimePolicy::default()` and derives `BindPolicy` from the single `allow_public_service_binds` bool.

### 1.3 `token_env` indirection

```rust
// crates/eggtunnel-cli/src/main.rs:338-360
fn load_token_with(
    env_name: &str,
    var: &dyn Fn(&str) -> Result<String, env::VarError>,
) -> Result<SecretToken, CliError> {
```

- The TOML file stores only the **variable name** (`token_env = "EGGTUNNEL_TOKEN"`). The secret itself lives in the environment. This is reiterated in `docs/CONFIGURATION.md:3-4` and `docs/API.md:65-66`.
- Each variable is read **exactly once** per resolution (`resolve_client_with` at `crates/eggtunnel-cli/src/main.rs:550-652`, `resolve_server_with` at `:658-717`). The resolved `SecretToken` is owned by `ResolvedClient`/`ResolvedServer` from then on; builders take owned values and never re-read the environment, so rotation between `check` and `start`/`bind` does **not** propagate — the process runs from its snapshot.
- An empty name is rejected up front with `config_resolution` (`token_env must name an environment variable`), mirroring the `outbound_proxy_env` guard, so no lookup of the empty name is ever issued. Unset names are `missing_secret_reference` (`required environment variable {env_name} is not set`); empty/oversize values are `config_resolution`. Messages name the variable, never the value. `SecretToken` has redacted `Debug` and `zeroize` on drop (`crates/eggtunnel/src/common.rs:43-53`).
- The lookup is injectable (`&dyn Fn`) so tests prove single-read snapshot semantics without touching ambient process state (single-read proof at `crates/eggtunnel-cli/src/main.rs:1327-1339`).

### 1.4 Transport strings

| TOML `transport` | CLI mapping | Library feature required | Semantics (per `docs/CONFIGURATION.md:57-67`) |
|---|---|---|---|
| `"tcp_tls"` (default) | `ClientTransportProfile::TcpTls` / `ServerTransportProfile::TcpTls` via `client_transport` / `server_transport` (`crates/eggtunnel-cli/src/main.rs:495-517`) | `tls` (+ `mtls` / `outbound-proxy` for those variants) | Baseline TCP+TLS via `eggress-transport-tls`, Rustls. |
| `"quic"` | `ClientTransportProfile::Quic` / `ServerTransportProfile::Quic` | `quic` (umbrella; role slices `quic-client`/`quic-server` in the library) | UDP control endpoint on `listen_addr`; service listeners remain TCP (`docs/OPERATIONS.md:18-19`). Platform roots + bearer only. |
| `"websocket_tls"` | `ClientTransportProfile::WebSocket` / `ServerTransportProfile::WebSocket` | `websocket` (umbrella; role slices in the library) | Verified TLS first, then binary WebSocket upgrade. Non-browser endpoint; no mTLS. |

Unknown strings are `transport` errors at resolution time — there is no silent fallthrough to `TcpTls`. Runtime startup is `server_builder(resolved).bind().await` / `client_builder(resolved).start().await` from the already-resolved snapshot; there is no per-transport `Server::bind*` / `Client::start*` dispatch in the CLI. See review checklist §7.

---

## 2. Resolution pipeline (parse → override → resolve → validate → launch)

Every config-consuming path runs the same five stages exactly once.
`run_check` at `crates/eggtunnel-cli/src/main.rs:912-958`,
`run_server` at `:960-1047`, and `run_client` at `:1054-1131` share stages
1–4; only stage 5 differs (report vs bind vs start). There is no second
validation pass and no re-read of environment variables or files between
validation and launch: builders take owned values out of the resolved
snapshot. First failure wins (sequential `?`, no error accumulation);
every failure is a `CliError` (`crates/eggtunnel-cli/src/main.rs:161-164`,
library-error mapping at `:183-198`) with a stable `ErrorCategory` (`:130-140`).

### 2.1 Stages 1–2: syntax parse + non-secret overrides

| Stage | Code | Behavior |
|---|---|---|
| 1. Parse | `read_config` (`crates/eggtunnel-cli/src/main.rs:250-263`) | Missing/unreadable file and invalid TOML are `config_parse`. No environment, file-content, or semantic work. |
| 2. Overrides | `apply_client_overrides` (`:266-304`) / `apply_server_overrides` (`:306-332`) | Each `Some(...)` CLI flag overwrites its TOML field; `None` leaves it. `--bind-port` requires exactly one configured service (`config_resolution`, deterministic selector). `--allow-public-service-binds` is one-way enable. Precedence: CLI > TOML > built-in. |

### 2.2 Stage 3: single-read resolution

`resolve_client_with` (`crates/eggtunnel-cli/src/main.rs:550-652`) and
`resolve_server_with` (`:658-717`) read every environment variable and
file **exactly once** into `ResolvedClient` (`:422-431`) /
`ResolvedServer` (`:458-466`). Both snapshots have hand-written redacted
`Debug` (tokens/keys/proxy values render as `[REDACTED]`/`[configured]`)
and no `Serialize` impl, so secrets cannot leak through diagnostics or a
future derive.

| Order | Check | Code | Failure / reason |
|---|---|---|---|
| R0 | Mode matches command **and** no wrong-mode key is present | resolve entry (`:554-559`, `:662-667`); cross-mode guards (`:560-572`, `:674-686`) | `mode must be 'client'/'server' for the … command`, or `server-only fields are not valid in client mode` / `client-only fields are not valid in server mode` — all `config_resolution`. |
| R1 | Transport name | `client_transport`/`server_transport` (`:495-517`) | Anything but the three exact strings (`transport`). No silent `TcpTls` fallthrough. |
| R2 | Endpoint shape | client `server_addr` via library `Endpoint::parse` (`:574-578`); server `listen_addr` via `SocketAddr::from_str` (`:688-703`) | Missing/invalid → `config_resolution` / `bind_validation`. See §4. |
| R3 | Required scalar presence | `tls_server_name` non-empty (`:579-588`); ≥1 service (`resolve_client_services` at `:519-544`); mTLS pair completeness (`:589-594`); server `outbound_proxy_env` absence (`:668-673`); proxy-name non-empty (`:595-608`) | `config_resolution` in all cases. The CLI-owned ≥1-service floor remains (the library accepts empty for dynamic embedders). |
| R4 | Token variable (read once) | `load_token_with` (`:338-360`) | Unset → `missing_secret_reference`; empty/oversize → `config_resolution` (`SecretToken::new` rule, `crates/eggtunnel/src/common.rs:27-35`). |
| R5 | Files (read once each) | `read_material` (`:365-407`) / `read_required_material` (`:409-416`) | Missing/unreadable/is-a-directory → `config_resolution` (message names the field, never contents); empty or over the 1 MiB `MAX_MATERIAL_BYTES` cap → `tls_material` (`:366`, `:382-387`, `:394-405`). **PEM is not parsed** — malformed material passes resolution and fails at TLS-build time (`tls_material`, see §7.4). |
| R6 | Proxy value (read once) | `:613-633` | Missing var → `missing_secret_reference`; blank → `config_resolution`. URI/chain shape is library-validated (`validate_client_profile` → `parse_outbound_proxy`, mapped to `invalid outbound proxy chain`). Supported families per `docs/CONFIGURATION.md:69-80`: direct, HTTP CONNECT, SOCKS5, `__`-separated chains; userinfo auth. |
| R7 | Service lowering | `resolve_client_services` (`:519-544`) | Bad `ServiceName` (charset/length) or bad `TcpTarget` (host length/port rules) from the proto crate (`config_resolution`). |

### 2.3 Stages 4–5: builder lowering + library validation, then launch

`client_builder(resolved)` (`crates/eggtunnel-cli/src/main.rs:721-739`)
assembles `ClientConfig` from owned snapshot values, applies the typed
profile + `RuntimePolicy::default()`, and attaches the already-resolved
proxy string / `ClientIdentity` without re-reading anything.
`server_builder(resolved)` (`:741-760`) does the same for
`ServerConfig` + `BindPolicy` + optional client CA. The CLI pins
`RuntimePolicy::default()` on both sides and derives `BindPolicy` from the one
bool (`allow_public_addresses: resolved.allow_public_service_binds`,
`crates/eggtunnel-cli/src/main.rs:752-755`), which satisfies the library
invariant that the two must be equal (`crates/eggtunnel/src/server/config.rs:158-162`).
`check` then calls
`.validate()`; `client`/`server` call `.start()`/`.bind()`, which
validate first — so the typed transport/CA/mTLS/proxy matrix is
library-owned (`validate_client_profile` at
`crates/eggtunnel/src/client.rs:569-612`, `validate_server_profile` at
`crates/eggtunnel/src/server/config.rs:149-179`). Startup TLS-build
failures map to `tls_material`, bind/connect failures to
`runtime_start` (via `From<TunnelError>`, `:183-198`).

### 2.4 Error taxonomy at the CLI boundary

`ErrorCategory` (`crates/eggtunnel-cli/src/main.rs:130-140`) is the
stable coarse vocabulary for JSON and exit status:
`config_parse`, `config_resolution`, `missing_secret_reference`,
`tls_material`, `profile_validation`, `bind_validation`,
`runtime_start`, `transport`, `authentication`. Messages name
variables/paths, never secret values or key material. Exit status stays
nonzero for every failure. JSON `check` failures print the `ok:false`
object to stdout before the process exits — except `config_parse`, which stage 1
raises before the file is known (`:913`), so a missing file, malformed TOML, or
unknown field prints no JSON object at all.

### 2.5 Rejected-combination summary (why)

| Combination | Where rejected | Why (per code + docs) |
|---|---|---|
| QUIC + `ca_cert` | Library: `validate_client_profile` (`crates/eggtunnel/src/client.rs:587-594`) via builder `validate()` | Eggress QUIC uses platform roots; custom bundles unsupported (`docs/CONFIGURATION.md:57-59`). |
| QUIC + `client_cert`/`client_key` (client) / `client_ca` (server) | Library: client `crates/eggtunnel/src/client.rs:587-594`; server `validate_server_profile` (`crates/eggtunnel/src/server/config.rs:164-176`) | QUIC adapter has no mTLS identity path; server mTLS is TCP/TLS-only. |
| QUIC + `outbound_proxy_env` | Library: client `crates/eggtunnel/src/client.rs:587-594` | QUIC is UDP; proxy traversal unsupported (`docs/OPERATIONS.md:77-78`). |
| WSS + mTLS (`client_cert`/`key` or `client_ca`) | Library: client `crates/eggtunnel/src/client.rs:595-600`; server `crates/eggtunnel/src/server/config.rs:164-176` | Current WSS profile: bearer + CA roots only (`docs/CONFIGURATION.md:63-67`). |
| Proxy + mTLS | Library: client `crates/eggtunnel/src/client.rs:601-606` | Outbound-proxy path establishes TCP before TLS; mTLS identity not plumbed through it. |
| Malformed proxy URI/chain | Library: `parse_outbound_proxy` (`crates/eggtunnel/src/client.rs:521-527`) via builder `validate()`; re-exported as `validate_outbound_proxy` (`crates/eggtunnel/src/lib.rs:22`) | `OutboundConnector::from_pproxy_uri` rejects; mapped to `invalid outbound proxy chain`. |
| Server + `outbound_proxy_env` | CLI-owned (`resolve_server_with`, `crates/eggtunnel-cli/src/main.rs:668-673`) | Proxy is a client-egress concept; server never dials out via proxy. |
| mTLS half-pair | CLI-owned (`:589-594`) | Identity requires both cert chain and key; one without the other is a certain startup failure. |
| Empty proxy env name | CLI-owned (`:595-608`) | `""` names no variable. |
| Missing/blank proxy value | CLI-owned (`:613-633`, shape deferred to library) | Name must resolve to a non-blank value; URI shape is library-validated. |
| Empty mTLS/CA/cert/key files | CLI-owned (`read_material`, `:365-407`) | Non-emptiness only; PEM parsing deferred to startup (see §7.4). |
| `bind_port` nonzero public intent via CLI | N/A (structural) | CLI always builds `RequestedBind::Loopback` (`resolve_client_services` at `:519-544`); public exposure additionally requires server `allow_public_service_binds = true` + `BindPolicy` (`crates/eggtunnel/src/common.rs:101-129`). |

### 2.6 `check --json` schema

`CheckReport` (`crates/eggtunnel-cli/src/main.rs:782-793`) serializes one
`eggtunnel.check/v1` object: `schema`, `ok`, `mode`, `transport`,
`services`, `custom_ca`/`mtls`/`outbound_proxy` booleans, and a
null-or-`{category, message}` error. There is no separate `version` field —
the versioned marker is the `schema` string (`CHECK_SCHEMA`, `:20`); the
`version` field belongs to the runtime `startup` event instead (`:982`,
`:1076`). No raw configuration, paths, or secret-bearing values appear.
`custom_ca`/`mtls` are scoped by mode so a wrong-mode key can never inflate
the opposite mode's booleans (`check_report_ok` at `:801-826`,
`check_report_err` at `:828-854`). Human output
(`configuration is structurally valid`) remains the default, and the shape is
pinned by `check_json_is_stable_redacted_and_versioned`
(`crates/eggtunnel-cli/tests/cli.rs:79-102`) plus
`check_json_schema_is_small_stable_and_redacted`
(`crates/eggtunnel-cli/src/main.rs:1368-1388`).

---

## 3. Runtime behavior

### 3.1 Server path (`run_server` at `crates/eggtunnel-cli/src/main.rs:960-1047`)

1. Validate `--snapshot-interval-secs` first (a flag error must not open listeners and *then* fail), then parse → overrides → `resolve_server` → `server_builder(resolved).bind().await`. `bind()` runs `validate()` first (`crates/eggtunnel/src/server/config.rs:119-130`), so startup enforces the same library matrix as `check` — from the same snapshot, with no re-read.
2. Startup: two JSON events, `startup` (version, mode, transport) then `server_listening` with the `server.local_addr()` string, or the single human line `server listening on {addr}` (`:978-993`).
3. Effective-bind loop: a `printed: HashSet<(SessionId, ServiceId, [u8;16], u16)>`, a 250 ms `refresh` interval, and `tokio::select!` over `ctrl_c` vs `refresh.tick()` vs the optional snapshot ticker. Each tick snapshots `handle.snapshot().effective_binds`, retains only live keys in `printed` so the set stays bounded across Session churn (`:1005-1009`), then prints/emits each never-before-seen key — human `service {id} session {session:?} listening on [{ipv6}]:{port}` (address via `Ipv6Addr::from(bind.address)`, so IPv4 appears as `::ffff:a.b.c.d`), or a `service_bind` JSON event with the same fields. Matches `docs/OPERATIONS.md:11-19`.
4. `--snapshot-interval-secs N` (validated by `validate_snapshot_interval` before any bind/start, minimum 5 s) adds a periodic `snapshot` event rendered by `snapshot_event` straight from the bounded library `Snapshot` — counters, heartbeat health, termination, and the bind list. The ticker helper (`futures_time_tick` at `:1050`) creates no ticker when disabled.
5. Shutdown: `break` on Ctrl-C → `shutdown` JSON event (`reason: signal`) → `server.shutdown().await`, which sends the bounded Drain before joining (per `docs/OPERATIONS.md:4-5`).

### 3.2 Client path (`run_client` at `crates/eggtunnel-cli/src/main.rs:1054-1131`)

1. Validate `--snapshot-interval-secs` first (a flag error must not start the runtime and *then* fail), then parse → overrides → `resolve_client` → `client_builder(resolved).start().await` (validates first via `crates/eggtunnel/src/client/config.rs:148-160`).
2. Startup event (JSON: version, mode, transport, service count) or `client started; waiting for authenticated session` (human).
3. Session tracking loop: each 250 ms tick compares `snapshot.connected` against the previous tick and emits `session_ready` (generation + registered services) on false→true and `session_lost` (termination + reconnects) on true→false. Same snapshot-ticker and shutdown-event shape as the server path.
4. Reconnects use bounded exponential backoff with jitter; bad auth/service authorization stops retries (`docs/OPERATIONS.md:32-35`).

Both paths require the `signal` Tokio feature, declared in `crates/eggtunnel-cli/Cargo.toml:18`. JSON rendering failure can never corrupt tunnel state: events are printed with `println!` outside the tunnel tasks, and a broken stdout pipe terminates the CLI cleanly.

---

## 4. Endpoint parsing: `SocketAddr` vs library `Endpoint`

`listen_addr` (server bind target) is parsed as `SocketAddr` inline in `resolve_server_with` (`crates/eggtunnel-cli/src/main.rs:688-703`, `bind_validation`): numeric IP + port only, so DNS hostnames are rejected; port 0 is accepted (ephemeral bind is legal).

`server_addr` (client dial target) is parsed by the canonical library `Endpoint::parse` (`crates/eggtunnel/src/endpoint.rs:42-85`) — the same semantic owner the client runtime uses, so CLI and library can no longer drift:

| Aspect | Library `Endpoint` |
|---|---|
| Shapes | `host:port` (DNS-like or IPv4) or `[ipv6]:port` (bracketed literal only) |
| DNS names | Accepted (`tunnel.example.net:9443`) |
| IPv6 | Bracket contents must parse as `Ipv6Addr`; unbracketed `::1:9443` is rejected as ambiguous |
| Host hygiene | `validate_host` (`crates/eggtunnel/src/endpoint.rs:114-118`) defers to the wire `validate_target_host`: empty hosts, whitespace/control characters, and URL-authority-ambiguous characters (`/?#@[]\"'<>`) rejected; length capped at `MAX_TARGET_HOST_BYTES` = 253 (`crates/eggtunnel-proto/src/lib.rs:19`) |
| Port rules | Decimal `1..=65535`; zero/empty/non-numeric rejected (`InvalidPort`) |
| Return | `Endpoint` keeping the original text (`as_str()`), host (`host()`), and port (`port()`) — DNS resolution stays deferred to Tokio connect |

Edge cases reviewers should keep in mind (see §7.5):

- `Endpoint` is stricter than the old CLI `checked_endpoint`: unbracketed IPv6 and URL-ambiguous hosts that previously passed `check` now fail with `bind_validation`. Per M015 §11 this is a correctness fix, documented in `docs/CONFIGURATION.md`.
- Neither parser logs or redacts — safe because neither handles secrets, only addresses.
- Effective-bind display is v6-normalized (`Ipv6Addr::from`), so IPv4 service addresses print as `::ffff:127.0.0.1`-style. Log scrapers matching `127.0.0.1:port` will miss them — note for operations dashboards.
- No `bind_port` range check at `check` time. Any `u16` is accepted; out-of-policy ports are a runtime `RegisterAck`/`OpenReject` matter under `BindPolicy` (`crates/eggtunnel/src/common.rs:101-129`). `check`-green ≠ bind-granted.

---

## 5. Library facade (`crates/eggtunnel/src/lib.rs`, 35 lines)

Full file is 35 lines (`crates/eggtunnel/src/lib.rs:1-35`), now also re-exporting the canonical `Endpoint`/`EndpointError` (`:33`) for configuration adapters.

### 5.1 Safety, runtime, and re-export posture

- `#![forbid(unsafe_code)]` (`crates/eggtunnel/src/lib.rs:1`; mirrored by the CLI at `crates/eggtunnel-cli/src/main.rs:1`). The fuzz/never-panics posture for hostile input lives in `eggtunnel-proto`; the facade itself introduces no unsafe.
- **No global runtime or tracing**: documented in the crate docs (`crates/eggtunnel/src/lib.rs:2-5`) and enforced by API shape — `ClientBuilder::start` / `ServerBuilder::bind` return an error if no caller-owned Tokio runtime exists, and neither crate installs a tracing subscriber. The embedder fixture owns both (see §5.3). `docs/API.md:61-63` and `docs/EMBEDDING.md:3-6` state the same contract.
- Feature-gated re-exports:

| Export | Gate | Line |
|---|---|---|
| `client::{ApplicationStream, Client, ClientBuilder, ClientConfig, ClientHandle, ClientTransportProfile, TargetConnector, TargetContext, TargetError, TargetFuture, TargetStream}` | `feature = "client"` | `crates/eggtunnel/src/lib.rs:24-27` |
| `client::ClientIdentity` | `client` + `mtls` | `crates/eggtunnel/src/lib.rs:20` |
| `client::validate_outbound_proxy` | `feature = "outbound-proxy"` | `crates/eggtunnel/src/lib.rs:22` |
| `endpoint::{Endpoint, EndpointError}` | always | `crates/eggtunnel/src/lib.rs:33` |
| `server::{Server, ServerBuilder, ServerConfig, ServerHandle, ServerTransportProfile}` | `feature = "server"` | `crates/eggtunnel/src/lib.rs:35` |
| `common::{BindPolicy, ClientService, HeartbeatSnapshot, ResourceLimits, RuntimePolicy, SecretToken, ServiceSpec, Snapshot, TerminationCategory, TimeoutPolicy, TunnelError}` | always | `crates/eggtunnel/src/lib.rs:28-31` |
| `eggtunnel_proto as proto` | always (type alias) | `crates/eggtunnel/src/lib.rs:32` |

The `proto` alias lets CLI/embedder code refer to `eggtunnel::proto::{RequestedBind, ServiceId, ServiceName, TcpTarget}` (`crates/eggtunnel-cli/src/main.rs:13-17`, `fixtures/embedder/src/main.rs:6`) without a direct `eggtunnel-proto` dependency. `docs/API.md:5-12` makes the package roles explicit: downstream depends on `eggtunnel`, never on the CLI.

Feature definitions live in `crates/eggtunnel/Cargo.toml:15-32`: `default = ["client", "tls"]`; `client`/`server` pull Tokio + Eggress relay/TLS; `quic`, `websocket`, `outbound-proxy`, `mtls` are strictly additive. The CLI enables all of them (`crates/eggtunnel-cli/Cargo.toml:13`); the embedder fixture enables only `["client", "tls"]` with `default-features = false` (see §5.3).

### 5.2 Embedding API (what the CLI is built from)

| Type / function | Role | Key definition |
|---|---|---|
| `ClientConfig { server_addr, tls_server_name, ca_pem, token, services }` | Programmatic equivalent of the client TOML (minus `token_env` indirection) | `crates/eggtunnel/src/client/config.rs:52-60`; `Debug` redacts token and CA bytes (`:62-72`) |
| `ClientBuilder` + `ClientTransportProfile` | Typed composition: `new(config).transport(profile).runtime_policy(policy).with_connector(...).outbound_proxy(...).with_identity(...)`; `validate()` then `start()` | `crates/eggtunnel/src/client/config.rs:84-161` (`validate` at `:136-146`, `start` at `:148-160`); profile validation at `crates/eggtunnel/src/client.rs:569-612` |
| `Client` + `ClientHandle` | `start` family via builder profiles (TCP/QUIC/WebSocket × connector/proxy/mTLS compositions); `handle()`, joined `shutdown().await`; handle offers `snapshot()`, `shutdown()`, `register_service(id)`, `unregister_service(id)` | `crates/eggtunnel/src/client.rs` (handle methods at `:82-139`; `shutdown` at `:506-512`) |
| `TargetConnector` / `TargetContext` / `TargetStream` / `TargetFuture` / `TargetError` | Application-owned dial: `connect(service: ClientService, context: TargetContext) -> TargetFuture`; default is TCP dial (`TcpTargetConnector`, `config.rs:32-49`); server can never rewrite the target | `crates/eggtunnel/src/client/config.rs:4-49` |
| `ServerConfig { listen_addr: SocketAddr, certificate_pem, private_key_pem, token, allow_public_service_binds }` | Programmatic equivalent of the server TOML | `crates/eggtunnel/src/server/config.rs:20-27`; `Debug` redacts key/token (`:36-49`); `Drop` zeroizes key (`:29-34`) |
| `ServerBuilder` + `ServerTransportProfile` | Typed composition: `new(config).transport(profile).runtime_policy(policy).bind_policy(policy).client_ca_pem(...)`; `validate()` then `bind()` | `crates/eggtunnel/src/server/config.rs:61-131` (`validate` at `:108-117`, `bind` at `:119-130`); profile validation at `:149-179` |
| `Server` + `ServerHandle` | `Server::bind*` legacy constructors delegate to the builder (`bind` at `:64-66`); handle offers `snapshot()` + `shutdown()` (`:54-60`) | `crates/eggtunnel/src/server.rs:41-66` |
| `BindPolicy` | Typed admission policy the CLI does not expose beyond the bool: `allow_public_addresses`, `allowed_addresses`, `allowed_port_ranges`, `allow_ephemeral_ports`, `max_services_per_session` (default 64); `validate()` + `loopback_only()` | `crates/eggtunnel/src/common.rs:101-129` |
| `RuntimePolicy` + `ResourceLimits` + `TimeoutPolicy` | Caller-selected finite ceilings/timeouts; CLI uses `RuntimePolicy::default()` and does not expose TOML knobs, so custom ceilings stay builder-only for embedders (`docs/OPERATIONS.md:61-66`) | `crates/eggtunnel/src/common.rs:211-333`; `HeartbeatSnapshot` at `:181-186` |
| `validate_outbound_proxy(&str)` | `parse_outbound_proxy` (`OutboundConnector::from_pproxy_uri`) mapped to `TunnelError::Configuration("invalid outbound proxy chain")` | `crates/eggtunnel/src/client.rs:516-527`; re-exported at `crates/eggtunnel/src/lib.rs:22`; enforced via builder `validate()` at `crates/eggtunnel/src/client.rs:569-612` (shared by CLI `check` and startup through the resolved snapshot) |
| `SecretToken`, `ClientService`, `Snapshot`, `TunnelError`, … | Shared vocabulary (redacted secrets, client-vs-server service views, counters, typed errors) | `crates/eggtunnel/src/common.rs:19-97`; `crates/eggtunnel/src/lib.rs:28-31` |

### 5.3 `fixtures/embedder` walkthrough

The fixture is the compile-checked proof that the §5.1 contract holds (`docs/API.md:78-80`, `docs/EMBEDDING.md:36-38`).

- `fixtures/embedder/Cargo.toml:1-12`: separate package (`publish = false`, empty `[workspace]` to detach), depends on `eggtunnel` by path with `default-features = false, features = ["client", "tls"]` (`:10`) — the minimal surface from `docs/API.md:18-20`. No CLI dependency. Tokio with `rt-multi-thread` + `tracing` are caller-owned (`:11-12`).
- `fixtures/embedder/src/main.rs:9-23`: `struct InProcessEcho; impl TargetConnector` — `connect` ignores the TCP target, opens a `tokio::io::duplex(16 KiB)` pair, spawns an echo task (`split` + `copy` + `shutdown`), and returns the application half as `TargetStream`. Demonstrates the "no loopback socket" path from `docs/EMBEDDING.md:25-31`.
- `fixtures/embedder/src/main.rs:25-40`: `client_config()` builds `ClientConfig` programmatically — literal `server_addr`, `tls_server_name`, `ca_pem: None` (system roots), `SecretToken::new(...)` from a caller-owned secret (no `token_env`), one `ClientService` with `RequestedBind::Loopback { port: 0 }`.
- `fixtures/embedder/src/main.rs:42-63`: `run()` builds a non-default `RuntimePolicy` (`limits.services_per_session = 8` at `:44`), calls `ClientBuilder::new(...).with_connector(Arc::new(InProcessEcho)).runtime_policy(policy).start()` (`:45-49`), registers a second service dynamically via `handle().register_service(dynamic_service).await` (`:50-59`), logs via caller-owned `tracing` (`:60`), and joins with `client.shutdown().await` (`:61`). `main` (`:65-70`) builds its own multi-thread Tokio runtime and `block_on(run())` — the exact inversion of the CLI's `#[tokio::main]` (`crates/eggtunnel-cli/src/main.rs:1133`).

---

## 6. Examples + operations

### 6.1 `examples/client.toml` (annotated)

```toml
# examples/client.toml:1 — mode selects the resolve_client branch of check (§2.2).
mode = "client"
# examples/client.toml:2 — validated by the library Endpoint::parse (§4); DNS allowed.
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
# examples/server.toml:1 — mode selects the resolve_server branch of check (§2.2).
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
| Start / stop | `server server.toml` / `client client.toml`; both stop on Ctrl-C; server sends bounded Drain before closing (`docs/OPERATIONS.md:3-5`) | `run_server` shutdown (`crates/eggtunnel-cli/src/main.rs:1038-1045`), `run_client` shutdown (`:1122-1129`) |
| Listening / binds | `listen_addr` takes control + data; every connection starts with TLS; actual service address is server-assigned, visible via `ServerHandle` snapshot; CLI prints new addresses while running. QUIC: `listen_addr` is UDP control; service listeners stay TCP (`docs/OPERATIONS.md:11-19`) | bind-print loop (`crates/eggtunnel-cli/src/main.rs:995-1027`); `ServerBuilder::bind` (`crates/eggtunnel/src/server/config.rs:119-130`) |
| Client resilience | Bounded exponential backoff + jitter on transient failures; invalid auth/authorization stops retries; registrations restored after new authenticated Session (`docs/OPERATIONS.md:32-35`) | Library reconnect supervisor (see client deep dive §3.3); CLI prints `waiting for authenticated session` or `session_ready`/`session_lost` JSON events |
| Certs / permissions | Trusted cert with SAN covering `tls_server_name`; token + key files readable only by the service account; loopback-only unless explicitly enabled (`docs/OPERATIONS.md:37-40`) | `tls_server_name` presence (`crates/eggtunnel-cli/src/main.rs:579-588`); `allow_public_service_binds` passthrough (`server_builder` at `crates/eggtunnel-cli/src/main.rs:741-760`) |
| Limits / throttling | 64 concurrent handshakes, 128 sessions, 64 services / 128 pending / 128 active per session, 128 client open tasks + control queue; per-IP auth throttle (10 fails / 60 s, 1024-source table) (`docs/OPERATIONS.md:42-47`) | `BindPolicy` defaults (`crates/eggtunnel/src/common.rs:131-141`); server constants (`crates/eggtunnel/src/server/auth.rs:22-25`); `ResourceLimits::default` (`crates/eggtunnel/src/common.rs:249-262`) |
| Snapshot monitoring | Current + high-water counts for sessions/services/pending/active/open/handshakes; latest termination category + panicked-task count; no event history or error text; counters are per-process, not persisted (`docs/OPERATIONS.md:49-59`); `--snapshot-interval-secs` streams the same `Snapshot` as JSON (`crates/eggtunnel-cli/src/main.rs:864-892`) | `handle.snapshot().effective_binds` (polled in both runtime loops); `Snapshot` type (`crates/eggtunnel/src/common.rs:154-177`) |
| Restricted egress | `outbound_proxy_env` → HTTP CONNECT / SOCKS5 / `__` chains; TLS+SNI stays end-to-end; WSS on TCP endpoint; QUIC has no proxy (`docs/OPERATIONS.md:74-86`) | CLI-owned name/value checks (`crates/eggtunnel-cli/src/main.rs:595-633`); library shape + dispatch (`crates/eggtunnel/src/client.rs:607-610` into the resolved snapshot) |

---

## 7. Review checklist

### 7.1 Config-vs-code drift

Cross-mode fields are rejected during resolution: server-only fields in client
files and client-only fields in server files cannot be silently ignored.
- [ ] **Docs vs dispatch for `bind_port`.** `docs/CONFIGURATION.md:34-36` says "`bind_port` requests the server-side service port" while the code always sends `RequestedBind::Loopback` (`resolve_client_services` at `crates/eggtunnel-cli/src/main.rs:519-544`). Public binds depend on server policy, not on any client TOML value — confirm the doc sentence is read that way and not as "set `bind_port` to a public port to get one."
- [ ] **Transport doc drift.** If a fourth transport is ever added, three places must move together: `client_transport`/`server_transport` (`crates/eggtunnel-cli/src/main.rs:495-517`), the override field docs (§1.1), and the `transport` row in §1.2.

### 7.2 Env-var handling (M015: single-read snapshot)

- [x] **Double-read eliminated.** Every variable is read exactly once per resolution (`resolve_client_with` / `resolve_server_with`); builders consume owned snapshot values, so rotation between `check` and `start`/`bind` no longer propagates. Proven by `resolved_snapshot_ignores_later_input_changes` (`crates/eggtunnel-cli/src/main.rs:1327-1339`) and the injectable lookup (`load_token_with` at `:338-360`).
- [x] **No raw `VarError`.** All environment failures map to `missing_secret_reference` (unset) or `config_resolution` (empty/unusable) with the variable *name* in the message.
- [x] **Empty-string `token_env` name.** `load_token_with` rejects it with the same explicit `token_env must name an environment variable` message the proxy-name guard uses, before issuing any lookup. Proven by `an_empty_token_reference_is_rejected_explicitly` (`crates/eggtunnel-cli/src/main.rs:1461-1483`; client and server shapes, asserting the lookup count stays zero).
- [ ] **Secrets never touch the file or argv.** Enforced by schema (no secret-valued key exists) plus the absence of `--token`/password flags — verify no future field/flag reintroduces inline secrets, per `docs/CONFIGURATION.md:108-113`. JSON/`Debug`/error paths are covered by redaction tests (`crates/eggtunnel-cli/src/main.rs:1342-1365`, integration `tests/cli.rs`).

### 7.3 File-read error paths (M015: read-once + field-named errors)

- [x] **Decorated errors.** `read_material`/`read_required_material` (`crates/eggtunnel-cli/src/main.rs:365-416`) prefix every failure with the field (`{what} file is missing or unreadable / must not be empty / is required but not configured`).
- [x] **Read-once.** Each file is read exactly once per resolution into the snapshot; the old triple-read/TOCTOU pattern is gone.

### 7.4 Misleading `check` success (structural only)

- [ ] **No PEM parsing at `check` time.** `check` asserts existence + non-emptiness only. Garbage bytes (or a valid PEM of the wrong type) pass `eggtunnel check` and fail at TLS-build time (`tls_material`) — the doc explicitly scopes this: "`eggtunnel check` validate[s] the TOML structure…" while "server startup also parses and validates its certificate and key" (`docs/CONFIGURATION.md:82-86`). Any test or runbook that treats `check`-green as deployable is over-reading.
- [ ] **No network I/O at `check` time.** `server_addr` DNS is never resolved, ports never dialed, proxy never connected — `Endpoint::parse` is shape-level only (§4). A typo'd hostname passes `check`.
- [ ] **`ServiceName`/`TcpTarget` are the exception.** Because `resolve_client_services` runs the real proto constructors, those two validations are as strong at `check` time as at runtime. Everything else file-shaped is weaker.
- [ ] **Library re-validates anyway.** `validate_client_profile` (`crates/eggtunnel/src/client.rs:569-612`) and `validate_server_profile` (`crates/eggtunnel/src/server/config.rs:149-179`) re-apply the transport/identity/proxy matrix at `start`/`bind` time from the same snapshot, so CLI `check` is defense-in-depth, not the enforcement point for embedders.

### 7.5 IPv6 / endpoint edge cases

- [ ] **`listen_addr` cannot take DNS.** Resolution requires `SocketAddr`; `listen_addr = "localhost:9443"` fails `check` with the `must be a socket address` message. Intended (bind needs an IP), but the error message's single example (`127.0.0.1:443`) doesn't mention `[::1]:9443` for v6 users.
- [x] **`server_addr` bracket contents now validated.** The canonical `Endpoint::parse` requires bracket interiors to parse as `Ipv6Addr`; `[not-an-ip]:443` fails `check` with `bind_validation` (§4). Stricter than the old CLI parser by design (M015 §11 correctness fix).
- [x] **Unbracketed v6 rejected.** `::1:9443` is `UnbracketedIpv6`, not silently split. `docs/CONFIGURATION.md` examples remain IPv4/DNS; `[::1]:port` is accepted for v6 users.
- [ ] **Port-0 asymmetry is intentional but subtle.** `listen_addr` port 0 passes (ephemeral bind); `server_addr` port 0 is rejected (`InvalidPort`). Both are correct for their roles (bind vs dial) but the two error messages don't explain the asymmetry.
- [ ] **Effective-bind display is v6-normalized.** The server loop renders every bind via `Ipv6Addr::from`, so IPv4 service addresses print as `::ffff:127.0.0.1`-style. Log scrapers matching `127.0.0.1:port` will miss them — note for operations dashboards.
- [ ] **No `bind_port` range check at `check` time.** Any `u16` is accepted; out-of-policy ports are a runtime `RegisterAck`/`OpenReject` matter under `BindPolicy` (`crates/eggtunnel/src/common.rs:101-129`). `check`-green ≠ bind-granted.

---

*Backlink: this dive expands [Architecture Overview](overview.md) §6. For the wire behavior behind these knobs, see the client/server/transport dives; for release and CI handling of this binary, see the ops/tooling dive.*

### Library profile validation (M008/M015)

`client_builder(resolved)` (`crates/eggtunnel-cli/src/main.rs:721-739`) and
`server_builder(resolved)` (`:741-760`) translate the resolved snapshot into
the public library builders with `RuntimePolicy::default()` (client) and
`RuntimePolicy::default()` + derived `BindPolicy` (server). `check` ends with
`builder.validate()`; the runtime paths start/bind the same builder from the
same snapshot — there is no separate re-validation pass and no second read.
Transport/CA/mTLS/proxy compatibility is library-owned
(`validate_client_profile` at `crates/eggtunnel/src/client.rs:569-612`,
`validate_server_profile` at `crates/eggtunnel/src/server/config.rs:149-179`);
file shape, environment lookup, pair-completeness, proxy name/value,
mTLS cert/key/client-CA non-emptiness, and server+proxy remain CLI-owned
(§2.2).
