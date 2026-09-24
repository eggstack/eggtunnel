# CLI + Config + Operations — Deep Dive

> Parent: [Architecture Overview](overview.md) §6. This file is the review-oriented deep dive for the configuration frontend, runtime CLI loops, library facade, and embedding/operations docs.

Sources (all paths relative to repo root): `crates/eggtunnel-cli/src/main.rs` (320 lines), `crates/eggtunnel-cli/Cargo.toml`, `crates/eggtunnel/src/lib.rs` (33 lines), `crates/eggtunnel/Cargo.toml`, `examples/client.toml`, `examples/server.toml`, `fixtures/embedder/src/main.rs`, `fixtures/embedder/Cargo.toml`, `docs/CONFIGURATION.md`, `docs/API.md`, `docs/EMBEDDING.md`, `docs/OPERATIONS.md`.

---

## 1. CLI surface

### 1.1 Binary and subcommands

The CLI crate is an unpublished binary (`crates/eggtunnel-cli/Cargo.toml:19-22`, `publish = false` at `crates/eggtunnel-cli/Cargo.toml:10`) named `eggtunnel`. It is a thin consumer of the `eggtunnel` library with all transports enabled (see §5.1).

Argument parsing is `clap` derive-based (`crates/eggtunnel-cli/src/main.rs:13-26`):

| Subcommand | CLI syntax | Handler | Effect |
|---|---|---|---|
| `Version` | `eggtunnel version` | `crates/eggtunnel-cli/src/main.rs:282` | Prints `eggtunnel <CARGO_PKG_VERSION>` via `env!`. No config, no I/O. |
| `Check` | `eggtunnel check <config>` (`PathBuf`) | `crates/eggtunnel-cli/src/main.rs:283-286` | `read_config` + `check_config`; on success prints `configuration is structurally valid`. Exit code non-zero on any `Err` via `main() -> Result`. |
| `Server` | `eggtunnel server <config>` | `crates/eggtunnel-cli/src/main.rs:287-309` | `read_config` → `check_config` → `server_builder().bind()` → bind-print loop → Ctrl-C → `shutdown().await`. |
| `Client` | `eggtunnel client <config>` | `crates/eggtunnel-cli/src/main.rs:310-317` | `read_config` → `check_config` → `client_builder().start()` → print waiting line → Ctrl-C → `shutdown().await`. |

`main` itself is `#[tokio::main]` (`crates/eggtunnel-cli/src/main.rs:279-280`), so the CLI owns its runtime. This is the opposite of the library, which requires a caller-owned runtime (see §5).

### 1.2 TOML schema: `FileConfig` / `FileService`

Deserialization structs at `crates/eggtunnel-cli/src/main.rs:28-72`. Unknown-field behavior is serde default (ignored); missing-field behavior is per-field `Option`/default.

#### `FileConfig` (`crates/eggtunnel-cli/src/main.rs:29-58`)

| TOML key | Rust field / type | Default | Used by | Notes |
|---|---|---|---|---|
| `mode` | `mode: String` (`crates/eggtunnel-cli/src/main.rs:30`) | **required** (no default) | `check_config` dispatch | Must be exactly `client` or `server`; anything else rejected at `crates/eggtunnel-cli/src/main.rs:274`. |
| `transport` | `transport: String` (`crates/eggtunnel-cli/src/main.rs:32`) | `default_transport()` → `"tcp_tls"` (`crates/eggtunnel-cli/src/main.rs:60-62`) | both | Must be exactly `tcp_tls`, `quic`, or `websocket_tls` (allowlisted in `check_config` at `crates/eggtunnel-cli/src/main.rs:200-205`, then mapped to a typed profile in `client_builder` at `:138-144` / `server_builder` at `:178-184`). No other aliases. |
| `token_env` | `token_env: String` (`crates/eggtunnel-cli/src/main.rs:33`) | **required** | both | **Name** of env var, not the secret. Resolved by `load_token` (§1.3). |
| `listen_addr` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:35`) | `None` | server only | Required in server mode; parsed by `checked_addr` (must be `SocketAddr`). |
| `tls_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:37`) | `None` | server only | Required in server mode; file must exist and be non-empty. |
| `tls_key` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:39`) | `None` | server only | Required in server mode; file must exist and be non-empty. |
| `allow_public_service_binds` | `bool` (`crates/eggtunnel-cli/src/main.rs:41`) | `false` | server only | Maps 1:1 to `ServerConfig.allow_public_service_binds`. Default is loopback-only. |
| `server_addr` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:43`) | `None` | client only | Required in client mode; parsed by `checked_endpoint` (hostname allowed, unlike `listen_addr`). |
| `tls_server_name` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:45`) | `None` | client only | Required, must be non-empty in client mode. Used as TLS SNI/verification name end-to-end (also over proxy). |
| `ca_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:47`) | `None` | client only (file path) | Optional. If present, file must be readable (`fs::read`). `None` means Eggress system-root verifier (`crates/eggtunnel/src/client.rs:93-94`). |
| `outbound_proxy_env` | `Option<String>` (`crates/eggtunnel-cli/src/main.rs:49`) | `None` | client only | **Name** of env var holding the proxy URI/chain. Must be non-empty name; referenced var must exist, be non-blank, and pass `validate_outbound_proxy`. |
| `client_cert` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:51`) | `None` | client only (mTLS) | Must be paired with `client_key`. Both files read; both must be non-empty. |
| `client_key` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:53`) | `None` | client only (mTLS) | Must be paired with `client_cert`. |
| `client_ca` | `Option<PathBuf>` (`crates/eggtunnel-cli/src/main.rs:55`) | `None` | server only (mTLS) | Optional trust roots for client certs. If present, file must exist and be non-empty. Rejected on QUIC/WSS. |
| `services` | `Vec<FileService>` (`crates/eggtunnel-cli/src/main.rs:57`) | `[]` (empty vec) | client only | Must be non-empty in client mode. Ignored in server mode (not validated). |

#### `FileService` (`crates/eggtunnel-cli/src/main.rs:64-72`)

| TOML key | Rust field / type | Default | Notes |
|---|---|---|---|
| `id` | `id: u64` (`crates/eggtunnel-cli/src/main.rs:66`) | **required** | Wrapped as `ServiceId(u64)` in `client_services` (`crates/eggtunnel-cli/src/main.rs:91`). |
| `name` | `name: String` (`crates/eggtunnel-cli/src/main.rs:67`) | **required** | Validated by `ServiceName::new` (wire rules: ≤128 B, `[A-Za-z0-9-_.]`; see overview §1). Failure aborts `client_builder` at `crates/eggtunnel-cli/src/main.rs:153` and `check_config` at `:219`. |
| `target_host` | `target_host: String` (`crates/eggtunnel-cli/src/main.rs:68`) | **required** | Validated by `TcpTarget::new` (≤253 B host). Client-owned; server never sees it. |
| `target_port` | `target_port: u16` (`crates/eggtunnel-cli/src/main.rs:69`) | **required** | `u16`; TOML out-of-range is a deserialize error in `read_config`. |
| `bind_port` | `bind_port: u16` (`crates/eggtunnel-cli/src/main.rs:71`) | `0` | Server-side requested port. `0` = ephemeral loopback. Always lowered to `RequestedBind::Loopback { port }` — the CLI cannot request a non-loopback bind directly (public binds still gated by server `allow_public_service_binds` policy). |

`client_services()` (`crates/eggtunnel-cli/src/main.rs:85-100`) is the single lowering point from `FileService` to `ClientService::new(ServiceId, ServiceName, RequestedBind::Loopback, TcpTarget)`. It is called from `client_builder` (`crates/eggtunnel-cli/src/main.rs:153`), which is shared by `check_config` (`:245` via `client_builder(config)?.validate()`) and the client runtime path (`:313` via `client_builder(&config)?.start()`). There is no dynamic-service TOML key; the CLI requires ≥1 service (`:216-218`) while the library accepts an empty `services` vec for fully dynamic embedders (see `docs/API.md:27-29`). `RuntimePolicy`/`BindPolicy` are not TOML-configurable — the CLI uses `RuntimePolicy::default()` (`:156`, `:187`) and derives `BindPolicy` from the single `allow_public_service_binds` bool (`:188-191`).

### 1.3 `token_env` indirection

```rust
// crates/eggtunnel-cli/src/main.rs:79-83
fn load_token(env_name: &str) -> Result<SecretToken, Box<dyn std::error::Error>> {
    let value = env::var(env_name)
        .map_err(|_| format!("required environment variable {env_name} is not set"))?;
    Ok(SecretToken::new(value.into_bytes())?)
}
```

- The TOML file stores only the **variable name** (`token_env = "EGGTUNNEL_TOKEN"`). The secret itself lives in the environment. This is reiterated in `docs/CONFIGURATION.md:3-4` and `docs/API.md:42`.
- `load_token` is called **first** in `check_config` (`crates/eggtunnel-cli/src/main.rs:199`), so a missing/unset variable or an empty/>4096 B value (`SecretToken::new` rule, `crates/eggtunnel/src/common.rs:20-28`) fails `check` before any other validation.
- It is called **again** inside the builders on the runtime paths (`client_builder` at `crates/eggtunnel-cli/src/main.rs:152`, `server_builder` at `:175`), so rotation between `check` and `start`/`bind` is picked up, but a `check`-passing file can still fail at startup if the env changed. There is no caching.
- Error when unset is a formatted string naming the variable (`required environment variable {env_name} is not set`), not the secret value. `SecretToken` has redacted `Debug` and `zeroize` on drop (`crates/eggtunnel/src/common.rs:36-46`).

### 1.4 Transport strings

| TOML `transport` | CLI builder mapping | Library feature required | Semantics (per `docs/CONFIGURATION.md:42-67`) |
|---|---|---|---|
| `"tcp_tls"` (default) | `ClientTransportProfile::TcpTls` / `ServerTransportProfile::TcpTls` (final `else` in `client_builder` at `crates/eggtunnel-cli/src/main.rs:138-144` / `server_builder` at `:178-184`) | `tls` (+ `mtls` / `outbound-proxy` for those variants) | Baseline TCP+TLS via `eggress-transport-tls`, Rustls. |
| `"quic"` | `ClientTransportProfile::Quic` / `ServerTransportProfile::Quic` | `quic` | UDP control endpoint on `listen_addr`; service listeners remain TCP (`docs/OPERATIONS.md:11-12`). Platform roots + bearer only. |
| `"websocket_tls"` | `ClientTransportProfile::WebSocket` / `ServerTransportProfile::WebSocket` | `websocket` | Verified TLS first, then binary WebSocket upgrade. Non-browser endpoint; no mTLS. |

The accepted set is hard-coded once in `check_config` (`crates/eggtunnel-cli/src/main.rs:200-205`); the builder mapping uses `== "quic"` / `== "websocket_tls"` with `tcp_tls` as the final `else` (`crates/eggtunnel-cli/src/main.rs:138-144`, `:178-184`). Because `check_config` always runs before `client_builder(...).start()` / `server_builder(...).bind()` in both runtime paths (`:289`, `:312-313`), an invalid string can never reach the mapping — but if `check_config` were bypassed, an unknown string would silently fall through to the `TcpTls` profile. Runtime startup is a single `server_builder(&config)?.bind().await` (`:290`) / `client_builder(&config)?.start().await` (`:313`); there is no per-transport `Server::bind*` / `Client::start*` dispatch in the CLI. See review checklist §7.

---

## 2. `check_config()` validation matrix

Entry: `crates/eggtunnel-cli/src/main.rs:198-277`. Called by all three config-consuming paths (`Check` at `:284`, `Server` at `:289`, `Client` at `:312`). Returns `Result<(), Box<dyn Error>>`; first failure wins (sequential `return Err`, no error accumulation). Each mode branch performs CLI-owned file-shape checks and then ends with `client_builder(config)?.validate()` (`:245`) / `server_builder(config)?.validate()` (`:272`), so the typed transport/CA/mTLS/proxy matrix is library-owned (`validate_client_profile` at `crates/eggtunnel/src/client.rs:524`, `validate_server_profile` at `crates/eggtunnel/src/server.rs:459`).

### 2.1 Common prologue (both modes)

| Order | Check | Code | Failure |
|---|---|---|---|
| C0 | Token env resolvable and 1–4096 B | `crates/eggtunnel-cli/src/main.rs:199` via `load_token` | Unset var, empty value, or >4096 B (`SecretToken::new`). |
| C1 | Transport in allowlist | `crates/eggtunnel-cli/src/main.rs:200-205` | Anything other than the three exact lowercase strings. Case-sensitive; `"TCP_TLS"`, `"tls"`, `""` all rejected. |
| C2 | Mode dispatch | `crates/eggtunnel-cli/src/main.rs:206,274` | Anything other than exactly `client`/`server`. |

### 2.2 Client branch (`crates/eggtunnel-cli/src/main.rs:207-246`)

| Order | Check | Code | Failure / reason |
|---|---|---|---|
| L1 | `server_addr` present | `:208-211` | `client config requires server_addr`. |
| L2 | `server_addr` parses as endpoint | `:212` via `checked_endpoint` | See §4. Rejects missing `:port`, port 0/unparseable, empty/whitespace host, malformed `[v6]:port`. |
| L3 | `tls_server_name` present and non-empty | `:213-215` (`is_none_or(str::is_empty)`) | `client config requires tls_server_name`. Empty string `""` rejected same as missing. |
| L4 | ≥1 service | `:216-218` | `client config requires at least one service`. Note `services` defaults to `[]`, so omitting the key fails here. The library itself accepts an empty vec (dynamic-only embedders); this floor is CLI-owned. |
| L5 | Every service lowers cleanly | `:219` via `client_services` | Bad `ServiceName` (charset/length) or bad `TcpTarget` (host length/port rules) from the proto crate. |
| L6 | `ca_cert` file readable if present | `:220-222` (`fs::read`) | Missing file / permission error propagates as io error. **Only readability is checked; PEM is not parsed** (see §7.4). |
| L7 | mTLS pair completeness | `:223-225` (`is_some() != is_some()`) | Exactly one of `client_cert`/`client_key` set → `client_cert and client_key must be configured together`. |
| L8 | Proxy env name non-empty | `:226-232` (`is_some_and(str::is_empty)`) | `outbound_proxy_env = ""` → `must name an environment variable`. Note: only the *name* is checked here; the *value* is checked next. |
| L9 | Proxy var present and non-blank | `:233-239` | Missing var → `outbound proxy variable {name} is missing`; blank/whitespace → `{name} is empty`. The URI/chain shape itself is **not** validated here — it is validated by the library (`validate_client_profile` at `crates/eggtunnel/src/client.rs:524` → `parse_outbound_proxy` at `:476-482`, mapped to `invalid outbound proxy chain`). Supported families per `docs/CONFIGURATION.md:69-80`: direct, HTTP CONNECT, SOCKS5, `__`-separated chains; userinfo auth. |
| L10 | mTLS files non-empty | `:240-244` (let-chains `if let … && (fs::read(cert)?.is_empty() \|\| fs::read(key)?.is_empty())`) | Either file empty → `client certificate and key files must not be empty`. Reads each file **twice** (once per `fs::read` call in the `\|\|` condition); TOCTOU window is negligible for CLI but noted in §7. |
| L11 | Library profile validation | `:245` via `client_builder(config)?.validate()` | QUIC + `ca_cert`/identity/proxy, WSS + mTLS, proxy + mTLS, malformed proxy chain, bad endpoint/TLS-name/service-uniqueness/size limits, and non-default `RuntimePolicy` violations — all owned by `validate_client_profile` (`crates/eggtunnel/src/client.rs:524-568`). |

`client_builder` itself (`crates/eggtunnel-cli/src/main.rs:132-164`): 3-way transport map (`:138-144`), `ClientConfig` assembly with `checked_endpoint` + `ca_pem` read + `load_token` + `client_services` (`:145-154`), `.transport(profile).runtime_policy(RuntimePolicy::default())` (`:155-156`), `.outbound_proxy(proxy)` when set (`:157-159`, value read once via `env::var` at `:133-137`), `.with_identity(ClientIdentity::new(...))` when the pair is present (`:160-162`).

### 2.3 Server branch (`crates/eggtunnel-cli/src/main.rs:247-273`)

| Order | Check | Code | Failure / reason |
|---|---|---|---|
| S1 | `listen_addr` present | `:248-251` | `server config requires listen_addr`. |
| S2 | `listen_addr` parses as `SocketAddr` | `:252` via `checked_addr` | Must be numeric IP + port (DNS hostnames rejected — see §4). |
| S3 | `tls_cert` + `tls_key` present | `:253-260` | `server config requires tls_cert` / `tls_key` respectively. |
| S4 | Cert + key files non-empty | `:261-263` | Either empty → `TLS certificate and key files must not be empty`. **PEM is not parsed here** (see §7.4). |
| S5 | `client_ca` file non-empty if present | `:264-268` (let-chains `if let … &&`) | Empty → `client CA file must not be empty`. Missing/unreadable propagates io error. |
| S6 | No `outbound_proxy_env` | `:269-271` | Any `Some` (even `""`) → `outbound_proxy is only valid in client mode`. |
| S7 | Library profile validation | `:272` via `server_builder(config)?.validate()` | mTLS-on-QUIC/WSS (`client_ca` with a non-TCP profile), empty CA bundle, bad cert/key size, invalid `BindPolicy`/`RuntimePolicy` — all owned by `validate_server_profile` (`crates/eggtunnel/src/server.rs:459-484`). |

Not validated in server mode: `server_addr`, `tls_server_name`, `services`, `ca_cert`, `client_cert`/`client_key` (silently ignored if set — see §7.1), and `allow_public_service_binds` (any bool accepted; passed through to `ServerConfig` and `BindPolicy`).

`server_builder` itself (`crates/eggtunnel-cli/src/main.rs:166-196`): `checked_addr` + `ServerConfig` assembly with cert/key reads + `load_token` (`:167-177`), 3-way transport map (`:178-184`), `.transport(profile).runtime_policy(RuntimePolicy::default()).bind_policy(BindPolicy { allow_public_addresses: config.allow_public_service_binds, ..default })` (`:185-191`), `.client_ca_pem(fs::read(...))` when present (`:192-194`).

### 2.4 Rejected-combination summary (why)

| Combination | Where rejected | Why (per code + docs) |
|---|---|---|
| QUIC + `ca_cert` | Library: `validate_client_profile` (`crates/eggtunnel/src/client.rs:541-548`) via CLI `:245` | Eggress QUIC uses platform roots; custom bundles unsupported (`docs/CONFIGURATION.md:57-59`). |
| QUIC + `client_cert`/`client_key` (client) / `client_ca` (server) | Library: client `:541-548`; server `validate_server_profile` (`crates/eggtunnel/src/server.rs:469-481`) via CLI `:245` / `:272` | QUIC adapter has no mTLS identity path; server mTLS is TCP/TLS-only. |
| QUIC + `outbound_proxy_env` | Library: client `:541-548` via CLI `:245` | QUIC is UDP; proxy traversal unsupported (`docs/OPERATIONS.md:40-41`, `59-60`). |
| WSS + mTLS (`client_cert`/`key` or `client_ca`) | Library: client `:549-554`; server `:469-481` via CLI `:245` / `:272` | Current WSS profile: bearer + CA roots only (`docs/CONFIGURATION.md:63-67`). |
| Proxy + mTLS | Library: client `:555-560` via CLI `:245` | Outbound-proxy path establishes TCP before TLS; mTLS identity not plumbed through it. |
| Malformed proxy URI/chain | Library: client `:561-564` (`parse_outbound_proxy` at `crates/eggtunnel/src/client.rs:476-482`) via CLI `:245`; re-exported as `validate_outbound_proxy` (`crates/eggtunnel/src/client.rs:470-473`) | `OutboundConnector::from_pproxy_uri` rejects; mapped to `invalid outbound proxy chain`. |
| Server + `outbound_proxy_env` | CLI-owned: `:269-271` | Proxy is a client-egress concept; server never dials out via proxy. |
| mTLS half-pair | CLI-owned: `:223-225` | Identity requires both cert chain and key; one without the other is a certain startup failure. |
| Empty proxy env name | CLI-owned: `:226-232` | `""` names no variable. |
| Missing/blank proxy value | CLI-owned: `:233-239` (shape deferred to library) | Name must resolve to a non-blank value; URI shape is library-validated. |
| Empty mTLS/CA/cert/key files | CLI-owned: `:240-244` (client), `:261-268` (server) | Non-emptiness only; PEM parsing deferred to startup (see §7.4). |
| `bind_port` nonzero public intent via CLI | N/A (structural) | CLI always builds `RequestedBind::Loopback` (`:93-94`); public exposure additionally requires server `allow_public_service_binds = true` + `BindPolicy` (`crates/eggtunnel/src/common.rs:83-93`). |

---

## 3. Runtime behavior

### 3.1 Server path (`crates/eggtunnel-cli/src/main.rs:287-309`)

1. `read_config` (`:288`) → `check_config` (`:289`) → `server_builder(&config)?.bind().await` (`:290`). `server_builder` (`:166-196`) re-parses `listen_addr` with `checked_addr`, re-reads `certificate_pem`/`private_key_pem` with `fs::read`, re-loads the token, maps the transport string to a typed `ServerTransportProfile`, applies `RuntimePolicy::default()` and the `BindPolicy` derived from `allow_public_service_binds`, and attaches `client_ca_pem` when present. Missing `listen_addr`/`tls_cert`/`tls_key` re-errors with `missing …` (unreachable after `check_config` unless the file changed — defensive re-check). `bind()` runs `validate()` first (`crates/eggtunnel/src/server.rs:147-158`), so startup enforces the same library matrix as `check`.
2. Print `server listening on {}` with `server.local_addr()` (`:291`).
3. Effective-bind print loop (`:292-307`): a `HashSet<(SessionId-ish, ServiceId-ish, addr, port)>` called `printed` (`:293`); a `tokio::time::interval(250 ms)` (`:294`); `tokio::select!` over `ctrl_c` (`:297`) vs `refresh.tick()` (`:298`). Each tick snapshots `handle.snapshot().effective_binds` (`:299`), keys by `(session, service, bind.address, bind.port)` (`:300`), and prints each never-before-seen key as `service {id} session {session:?} listening on [{ipv6}]:{port}` (`:302`), where the address is rendered via `Ipv6Addr::from(bind.address)` (the 16-byte wire form, so IPv4 appears as `::ffff:a.b.c.d`). Matches `docs/OPERATIONS.md:7-10` ("CLI prints newly assigned service addresses while it is running").
4. Shutdown: `break` on Ctrl-C → `server.shutdown().await` (`:308`), which sends the bounded Drain before joining (per `docs/OPERATIONS.md:4-5`).

### 3.2 Client path (`crates/eggtunnel-cli/src/main.rs:310-317`)

1. `read_config` (`:311`) → `check_config` (`:312`) → `client_builder(&config)?.start().await` (`:313`). `client_builder` (`:132-164`) re-validates `server_addr` with `checked_endpoint`, clones `tls_server_name`, reads `ca_pem` via `config.ca_cert.as_ref().map(fs::read).transpose()?` (`:151`, so read errors propagate, `None` stays `None`), re-loads the token, lowers `services` via `client_services`, maps the transport string to a typed `ClientTransportProfile`, applies `RuntimePolicy::default()`, attaches the proxy value (re-read from the environment at `:133-137`) and the mTLS identity (`ClientIdentity::new(fs::read(cert)?, fs::read(key)?)` at `:160-162`). `start()` runs `validate()` first (`crates/eggtunnel/src/client/config.rs:142-155`), so startup enforces the same library matrix as `check`.
2. Print `client started; waiting for authenticated session` (`:314`), then `tokio::signal::ctrl_c().await?` (`:315`) and `client.shutdown().await` (`:316`). There is no snapshot-polling loop on the client side; status is via `ClientHandle::snapshot` for embedders. Reconnects use bounded exponential backoff with jitter; bad auth/service authorization stops retries (`docs/OPERATIONS.md:14-17`).

Both paths require the `signal` Tokio feature, declared in `crates/eggtunnel-cli/Cargo.toml:17`.

---

## 4. Endpoint parsing: `checked_addr` vs `checked_endpoint`

| Aspect | `checked_addr` (`crates/eggtunnel-cli/src/main.rs:102-106`) | `checked_endpoint` (`crates/eggtunnel-cli/src/main.rs:108-130`) |
|---|---|---|
| Used for | `listen_addr` (server bind target) | `server_addr` (client dial target) |
| Mechanism | `value.parse::<SocketAddr>()` | Manual host/port split, then return the original string |
| DNS names | **Rejected** — `SocketAddr::from_str` accepts only numeric IP + port | **Accepted** — any non-empty whitespace-free host (e.g. `tunnel.example.net:9443`) |
| IPv6 | Standard `SocketAddr` forms (`::1:9443` is ambiguous and rejected; `[::1]:9443` accepted by the std parser) | Explicit bracket handling: leading `[` requires matching `]` followed by `:` (`:110-116`); non-bracket path splits on the **last** `:` (`rsplit_once`, `:118-122`) |
| Whitespace | Rejected by std parser | Explicitly rejected: `host.chars().any(char::is_whitespace)` (`:123-125`) — covers spaces/tabs inside host; note the *port* side is validated only via `parse::<u16>`, so `"host: 443"` fails on the port parse, and leading/trailing whitespace around the whole value fails either the host check or the port parse |
| Port rules | Std range `0–65535`; **port 0 accepted** by the parser (bind-ephemeral is legal for `listen_addr`) | Must parse as `u16` **and be nonzero** (`is_ok_and(\|port\| port != 0)`, `:125`); port 0, empty port, non-numeric port, and `>65535` all map to `endpoint must contain a host and numeric port` |
| Empty host | Rejected by std parser | Explicitly rejected (`host.is_empty()`, `:123`) — covers `":9443"` and `"[]:9443"` (inner slice empty) |
| Error strings | `{key} must be a socket address such as 127.0.0.1:443` (`:105`) | Three distinct messages: `IPv6 endpoint must use [address]:port syntax` (`:112`), `endpoint must end with :port` (`:114`), `endpoint must use host:port syntax` (`:120`), and the catch-all `endpoint must contain a host and numeric port` (`:127`). None echo the offending value. |
| Return | `SocketAddr` (ready for `ServerConfig.listen_addr`) | `String` (the original `value.to_owned()`, `:129`; DNS resolution deferred to Tokio connect per `crates/eggtunnel/src/client.rs:90-91`) |

Edge cases reviewers should keep in mind (see §7.5):

- `checked_endpoint` does **not** validate that a bracketed host is a real IPv6 literal — `[not-an-ip]:443` passes `check` and fails later at connect time.
- Unbracketed `::1:9443` splits host=`::1:944` port=`9443` via `rsplit_once` and passes structurally; whether dial succeeds depends on the resolver. Prefer `[::1]:9443`.
- `checked_endpoint` preserves the input verbatim (no trimming/normalization), so `ClientConfig.server_addr` carries whatever the file contained.
- Neither function logs or redacts — safe because neither handles secrets, only addresses.

---

## 5. Library facade (`crates/eggtunnel/src/lib.rs`)

Full file is 33 lines (`crates/eggtunnel/src/lib.rs:1-33`).

### 5.1 Safety, runtime, and re-export posture

- `#![forbid(unsafe_code)]` (`crates/eggtunnel/src/lib.rs:1`; mirrored by the CLI at `crates/eggtunnel-cli/src/main.rs:1`). The fuzz/never-panics posture for hostile input lives in `eggtunnel-proto`; the facade itself introduces no unsafe.
- **No global runtime or tracing**: documented in the crate docs (`crates/eggtunnel/src/lib.rs:2-5`) and enforced by API shape — `ClientBuilder::start` / `ServerBuilder::bind` return an error if no caller-owned Tokio runtime exists, and neither crate installs a tracing subscriber. The embedder fixture owns both (see §5.3). `docs/API.md:40-42` and `docs/EMBEDDING.md:3-6` state the same contract.
- Feature-gated re-exports:

| Export | Gate | Line |
|---|---|---|
| `client::{ApplicationStream, Client, ClientBuilder, ClientConfig, ClientHandle, ClientTransportProfile, TargetConnector, TargetContext, TargetError, TargetFuture, TargetStream}` | `feature = "client"` | `crates/eggtunnel/src/lib.rs:23-26` |
| `client::ClientIdentity` | `client` + `mtls` | `crates/eggtunnel/src/lib.rs:18-19` |
| `client::validate_outbound_proxy` | `feature = "outbound-proxy"` | `crates/eggtunnel/src/lib.rs:20-21` |
| `server::{Server, ServerBuilder, ServerConfig, ServerHandle, ServerTransportProfile}` | `feature = "server"` | `crates/eggtunnel/src/lib.rs:32-33` |
| `common::{BindPolicy, ClientService, HeartbeatSnapshot, ResourceLimits, RuntimePolicy, SecretToken, ServiceSpec, Snapshot, TerminationCategory, TimeoutPolicy, TunnelError}` | always | `crates/eggtunnel/src/lib.rs:27-30` |
| `eggtunnel_proto as proto` | always (type alias) | `crates/eggtunnel/src/lib.rs:31` |

The `proto` alias lets CLI/embedder code refer to `eggtunnel::proto::{RequestedBind, ServiceId, ServiceName, TcpTarget}` (`crates/eggtunnel-cli/src/main.rs:6-10`, `fixtures/embedder/src/main.rs:6`) without a direct `eggtunnel-proto` dependency. `docs/API.md:5-12` makes the package roles explicit: downstream depends on `eggtunnel`, never on the CLI.

Feature definitions live in `crates/eggtunnel/Cargo.toml:15-23`: `default = ["client", "tls"]`; `client`/`server` pull Tokio + Eggress relay/TLS; `quic`, `websocket`, `outbound-proxy`, `mtls` are strictly additive. The CLI enables all of them (`crates/eggtunnel-cli/Cargo.toml:13`); the embedder fixture enables only `["client", "tls"]` with `default-features = false` (see §5.3).

### 5.2 Embedding API (what the CLI is built from)

| Type / function | Role | Key definition |
|---|---|---|
| `ClientConfig { server_addr, tls_server_name, ca_pem, token, services }` | Programmatic equivalent of the client TOML (minus `token_env` indirection) | `crates/eggtunnel/src/client/config.rs:46-54`; `Debug` redacts token and CA bytes (`:56-66`) |
| `ClientBuilder` + `ClientTransportProfile` | Typed composition: `new(config).transport(profile).runtime_policy(policy).with_connector(...).outbound_proxy(...).with_identity(...)`; `validate()` then `start()` | `crates/eggtunnel/src/client/config.rs:68-156` (`validate` at `:130-140`, `start` at `:142-155`); profile validation at `crates/eggtunnel/src/client.rs:524-568` |
| `Client` + `ClientHandle` | `start` family via builder profiles (TCP/QUIC/WebSocket × connector/proxy/mTLS compositions); `handle()`, joined `shutdown().await`; handle offers `snapshot()`, `shutdown()`, `register_service(id)`, `unregister_service(id)` | `crates/eggtunnel/src/client.rs` (handle methods; `shutdown` at `:461-467`) |
| `TargetConnector` / `TargetContext` / `TargetStream` / `TargetFuture` / `TargetError` | Application-owned dial: `connect(service: ClientService, context: TargetContext) -> TargetFuture`; default is TCP dial (`TcpTargetConnector`, `config.rs:32-43`); server can never rewrite the target | `crates/eggtunnel/src/client/config.rs:3-43` |
| `ServerConfig { listen_addr: SocketAddr, certificate_pem, private_key_pem, token, allow_public_service_binds }` | Programmatic equivalent of the server TOML | `crates/eggtunnel/src/server.rs:49-56`; `Debug` redacts key/token (`:65-78`); `Drop` zeroizes key (`:58-63`) |
| `ServerBuilder` + `ServerTransportProfile` | Typed composition: `new(config).transport(profile).runtime_policy(policy).bind_policy(policy).client_ca_pem(...)`; `validate()` then `bind()` | `crates/eggtunnel/src/server.rs:80-159` (`validate` at `:136-145`, `bind` at `:147-158`); profile validation at `:459-484` |
| `Server` + `ServerHandle` | `Server::bind*` legacy constructors delegate to the builder (`bind` at `:184-186`); handle offers `snapshot()` + `shutdown()` (`:174-181`) | `crates/eggtunnel/src/server.rs:161-186` |
| `BindPolicy` | Typed admission policy the CLI does not expose beyond the bool: `allow_public_addresses`, `allowed_addresses`, `allowed_port_ranges`, `allow_ephemeral_ports`, `max_services_per_session` (default 64); `validate()` + `loopback_only()` | `crates/eggtunnel/src/common.rs:83-124` |
| `RuntimePolicy` + `ResourceLimits` + `TimeoutPolicy` | Caller-selected finite ceilings/timeouts; CLI uses `RuntimePolicy::default()` and does not expose TOML knobs | `crates/eggtunnel/src/common.rs:194-316`; `HeartbeatSnapshot` at `:162-169` |
| `validate_outbound_proxy(&str)` | `parse_outbound_proxy` (`OutboundConnector::from_pproxy_uri`) mapped to `TunnelError::Configuration("invalid outbound proxy chain")` | `crates/eggtunnel/src/client.rs:470-482`; re-exported at `crates/eggtunnel/src/lib.rs:20-21`; enforced via builder `validate()` at `client.rs:561-564` (shared by CLI `check` at `crates/eggtunnel-cli/src/main.rs:245` and startup at `:313`) |
| `SecretToken`, `ClientService`, `Snapshot`, `TunnelError`, … | Shared vocabulary (redacted secrets, client-vs-server service views, counters, typed errors) | `crates/eggtunnel/src/common.rs:17-71`; `crates/eggtunnel/src/lib.rs:27-30` |

### 5.3 `fixtures/embedder` walkthrough

The fixture is the compile-checked proof that the §5.1 contract holds (`docs/API.md:52`, `docs/EMBEDDING.md:36-38`).

- `fixtures/embedder/Cargo.toml:1-12`: separate package (`publish = false`, empty `[workspace]` to detach), depends on `eggtunnel` by path with `default-features = false, features = ["client", "tls"]` (`:10`) — the minimal surface from `docs/API.md:18-20`. No CLI dependency. Tokio with `rt-multi-thread` + `tracing` are caller-owned (`:11-12`).
- `fixtures/embedder/src/main.rs:9-23`: `struct InProcessEcho; impl TargetConnector` — `connect` ignores the TCP target, opens a `tokio::io::duplex(16 KiB)` pair, spawns an echo task (`split` + `copy` + `shutdown`), and returns the application half as `TargetStream`. Demonstrates the "no loopback socket" path from `docs/EMBEDDING.md:25-31`.
- `fixtures/embedder/src/main.rs:25-40`: `client_config()` builds `ClientConfig` programmatically — literal `server_addr`, `tls_server_name`, `ca_pem: None` (system roots), `SecretToken::new(...)` from a caller-owned secret (no `token_env`), one `ClientService` with `RequestedBind::Loopback { port: 0 }`.
- `fixtures/embedder/src/main.rs:42-63`: `run()` builds a non-default `RuntimePolicy` (`limits.services_per_session = 8` at `:44`), calls `ClientBuilder::new(...).with_connector(Arc::new(InProcessEcho)).runtime_policy(policy).start()` (`:45-49`), registers a second service dynamically via `handle().register_service(dynamic_service).await` (`:50-59`), logs via caller-owned `tracing` (`:60`), and joins with `client.shutdown().await` (`:61`). `main` (`:65-70`) builds its own multi-thread Tokio runtime and `block_on(run())` — the exact inversion of the CLI's `#[tokio::main]` (`crates/eggtunnel-cli/src/main.rs:279-280`).

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
| Start / stop | `server server.toml` / `client client.toml`; both stop on Ctrl-C; server sends bounded Drain before closing (`docs/OPERATIONS.md:3-5`) | `:297` + `:308` (server), `:315-316` (client) |
| Listening / binds | `listen_addr` takes control + data; every connection starts with TLS; actual service address is server-assigned, visible via `ServerHandle` snapshot; CLI prints new addresses while running. QUIC: `listen_addr` is UDP control; service listeners stay TCP (`docs/OPERATIONS.md:7-12`) | `:291-302` (print loop); `crates/eggtunnel/src/server.rs:147-158` (builder `bind`) |
| Client resilience | Bounded exponential backoff + jitter on transient failures; invalid auth/authorization stops retries; registrations restored after new authenticated Session (`docs/OPERATIONS.md:14-17`) | Library reconnect loop (see client deep dive); CLI just prints `waiting for authenticated session` (`:314`) |
| Certs / permissions | Trusted cert with SAN covering `tls_server_name`; token + key files readable only by the service account; loopback-only unless explicitly enabled (`docs/OPERATIONS.md:19-22`) | `tls_server_name` check (`:213-215`); `allow_public_service_binds` passthrough (`server_builder` at `:171-177`, `:188-191`) |
| Limits / throttling | 64 concurrent handshakes, 128 sessions, 64 services / 128 pending / 128 active per session, 128 client open tasks + control queue; per-IP auth throttle (10 fails / 60 s, 1024-source table) (`docs/OPERATIONS.md:24-28`) | `BindPolicy` defaults (`crates/eggtunnel/src/common.rs:114-124`); server constants (`crates/eggtunnel/src/server.rs:39-46`); `ResourceLimits::default` (`crates/eggtunnel/src/common.rs:232-245`) |
| Snapshot monitoring | Current + high-water counts for sessions/services/pending/active/open/handshakes; latest termination category + panicked-task count; no event history or error text; counters are per-process, not persisted (`docs/OPERATIONS.md:30-35`) | `handle.snapshot().effective_binds` (`:299`); `Snapshot` type (`crates/eggtunnel/src/common.rs:136-160`) |
| Restricted egress | `outbound_proxy_env` → HTTP CONNECT / SOCKS5 / `__` chains; TLS+SNI stays end-to-end; WSS on TCP endpoint; QUIC has no proxy (`docs/OPERATIONS.md:37-48`) | CLI-owned name/value checks (`:226-239`); library shape + dispatch (`crates/eggtunnel/src/client.rs:541-564` via CLI `:245`, `:313`) |

---

## 7. Review checklist

### 7.1 Config-vs-code drift

- [ ] **Server-mode silent ignores.** `ca_cert`, `client_cert`, `client_key`, `server_addr`, `tls_server_name`, `services` are accepted-but-ignored in server mode (no branch reads them). A user who pastes client keys into a server file gets `configuration is structurally valid` with no warning. Consider warn-or-reject for cross-mode keys.
- [ ] **Client-mode silent ignores.** `listen_addr`, `tls_cert`, `tls_key`, `client_ca`, `allow_public_service_binds` are likewise ignored in client mode. Same recommendation.
- [ ] **`allow_public_service_binds` in client files.** Accepted and ignored; only meaningful for the server. Easy to misplace — worth a cross-mode lint.
- [ ] **Docs vs dispatch for `bind_port`.** `docs/CONFIGURATION.md:34-36` says "`bind_port` requests the server-side service port" while the code always sends `RequestedBind::Loopback` (`crates/eggtunnel-cli/src/main.rs:93-94`). Public binds depend on server policy, not on any client TOML value — confirm the doc sentence is read that way and not as "set `bind_port` to a public port to get one."
- [ ] **Transport doc drift.** If a fourth transport is ever added, three places must move together: the allowlist (`:200-205`), the builder profile maps (`client_builder` at `:138-144`, `server_builder` at `:178-184`), and the single startup calls (`server_builder(...).bind()` at `:290`, `client_builder(...).start()` at `:313`).

### 7.2 Env-var handling

- [ ] **Double-read of proxy var.** `check_config` reads `outbound_proxy_env`'s target (`:234`) and `client_builder` reads it again (`:133-137`). Both `check` (`:245` via `validate()`) and `start` (`:313`) go through the builder, so one `client` invocation reads the var twice (check + builder-at-startup). A var changed/removed between `check` and `start` yields different behavior than `check` reported. Same pattern for `token_env` (`:199` vs builder `:152`/`:175`).
- [ ] **Raw `VarError` on builder path.** The `:133-137` `env::var(...).transpose()?` propagates `NotPresent`/`NotUnicode` without the friendly `outbound proxy variable {name} is missing/empty` wrapping that `check` provides (`:234-238`).
- [ ] **Empty-string `token_env` name.** `token_env = ""` looks up the empty variable name and reports `required environment variable  is not set`. Harmless but confusing; contrast with `outbound_proxy_env = ""`, which has an explicit `must name an environment variable` guard (`:226-232`).
- [ ] **Secrets never touch the file.** Enforced by schema (no secret-valued key exists) — verify no future field reintroduces inline secrets, per `docs/CONFIGURATION.md:3-4`.

### 7.3 File-read error paths

- [ ] **`fs::read` errors are undecorated.** `ca_cert` (`:221`), cert/key emptiness checks (`:241`, `:261`, `:265`), `client_ca` attach (`:193`), and the builder `ServerConfig`/`ClientConfig` builds (`:173-174`, `:151`, `:161`) propagate raw `io::Error` (path visible, but no "which field" prefix except via backtrace). Reviewers triaging `No such file or directory (os error 2)` must map it back manually.
- [ ] **Double-read of mTLS files.** `:241` reads cert and key twice each (`fs::read(cert)?.is_empty() || fs::read(key)?.is_empty()`). The same files are read a third time in `client_builder` at `:161` on the runtime path. Correct but wasteful; also a TOCTOU window (file swapped between reads). Consider read-once-and-reuse.
- [ ] **`ca_pem` mapping.** `config.ca_cert.as_ref().map(fs::read).transpose()?` (`:151`) is the one place the `Option<PathBuf> → Option<Vec<u8>>` fallibility is handled idiomatically; keep as the pattern if more optional file fields are added.

### 7.4 Misleading `check` success (structural only)

- [ ] **No PEM parsing at `check` time.** `check` asserts existence + non-emptiness only (`:221`, `:240-244`, `:261-268`). Garbage bytes (or a valid PEM of the wrong type) pass `eggtunnel check` and fail at `client_builder(...).start()` / `server_builder(...).bind()` when the TLS builders parse them — the doc explicitly scopes this: "`eggtunnel check` validate[s] the TOML structure…" while "server startup also parses and validates its certificate and key" (`docs/CONFIGURATION.md:82-84`). Any test or runbook that treats `check`-green as deployable is over-reading.
- [ ] **No network I/O at `check` time.** `server_addr` DNS is never resolved, ports never dialed, proxy never connected — `checked_endpoint` is string-level only (§4). A typo'd hostname passes `check`.
- [ ] **`ServiceName`/`TcpTarget` are the exception.** Because L5 (`:219`) runs the real proto constructors, those two validations are as strong at `check` time as at runtime. Everything else file-shaped is weaker.
- [ ] **Library re-validates anyway.** `validate_client_profile` (`crates/eggtunnel/src/client.rs:524-568`) and `validate_server_profile` (`crates/eggtunnel/src/server.rs:459-484`) re-apply the transport/identity/proxy matrix at `start`/`bind` time, so CLI `check` is defense-in-depth, not the enforcement point for embedders.

### 7.5 IPv6 / endpoint edge cases

- [ ] **`listen_addr` cannot take DNS.** `checked_addr` (`:102-106`) requires `SocketAddr`; `listen_addr = "localhost:9443"` fails `check` with the `must be a socket address` message. Intended (bind needs an IP), but the error message's single example (`127.0.0.1:443`) doesn't mention `[::1]:9443` for v6 users.
- [ ] **`server_addr` bracket contents unchecked.** `[anything]:port` with a non-empty whitespace-free interior passes — `[not-an-ip]:443` is `check`-green and fails only at connect. Decide whether to resolve/parse strictly at `check` time or keep the documented string-level contract.
- [ ] **Unbracketed v6 ambiguity.** `::1:9443` splits on the last colon (host `::1:944`) and passes; dial behavior then depends on resolver handling. Docs never show a v6 `server_addr` example — add `[::1]:port` guidance if v6 clients are supported.
- [ ] **Port-0 asymmetry is intentional but subtle.** `listen_addr` port 0 passes (ephemeral bind); `server_addr` port 0 is rejected (`:125`). Both are correct for their roles (bind vs dial) but the two functions' error messages don't explain the asymmetry.
- [ ] **Effective-bind display is v6-normalized.** The server loop renders every bind via `Ipv6Addr::from` (`:302`), so IPv4 service addresses print as `::ffff:127.0.0.1`-style. Log scrapers matching `127.0.0.1:port` will miss them — note for operations dashboards.
- [ ] **No `bind_port` range check at `check` time.** Any `u16` is accepted; out-of-policy ports are a runtime `RegisterAck`/`OpenReject` matter under `BindPolicy` (`crates/eggtunnel/src/common.rs:100-111`). `check`-green ≠ bind-granted.

---

*Backlink: this dive expands [Architecture Overview](overview.md) §6. For the wire behavior behind these knobs, see the client/server/transport dives; for release and CI handling of this binary, see the ops/tooling dive.*

### Library profile validation (M008)

`client_builder` (`crates/eggtunnel-cli/src/main.rs:132-164`: 3-way
transport map at `:138-144`, `RuntimePolicy::default()` at `:156`,
`outbound_proxy` at `:157-159`, `with_identity` at `:160-162`) and
`server_builder` (`:166-196`: profile map at `:178-184`,
`runtime_policy` at `:187`, `bind_policy` from the bool at `:188-191`,
`client_ca_pem` at `:192-194`) translate TOML into the public library
builders. `check_config` ends each branch with `builder.validate()`
(`:245` client, `:272` server), and the runtime paths start/bind the
same builder after the same config check (`:290`, `:313`).
Transport/CA/mTLS/proxy compatibility is library-owned
(`validate_client_profile` at `crates/eggtunnel/src/client.rs:524`,
`validate_server_profile` at `crates/eggtunnel/src/server.rs:459`);
file shape, environment lookup, pair-completeness, proxy name/value,
mTLS cert/key/client-CA non-emptiness, and server+proxy remain
CLI-owned (§2.2-§2.3).
