# Shared core — `crates/eggtunnel/src/common.rs`

> Index: [architecture/overview.md](overview.md) §2. This is the deep dive for component #2 (shared vocabulary for client + server: secrets, policy, observability, errors).

`common.rs` is 636 lines, `forbid(unsafe_code)` via `crates/eggtunnel/src/lib.rs:1`, with no socket, Tokio, timer, or task dependencies of its own. It defines the types both sides agree on, plus the server-only policy/token enforcement helpers. The library facade re-exports the public vocabulary at `crates/eggtunnel/src/lib.rs:27-30`:

```rust
pub use common::{BindPolicy, ClientService, HeartbeatSnapshot, ResourceLimits, RuntimePolicy, SecretToken, ServiceSpec, Snapshot, TerminationCategory, TimeoutPolicy, TunnelError};
```
(11 re-exports; verified against `lib.rs:27-30`.)

---

## 1. Purpose: shared vocabulary for client + server

| Concern | Type(s) | Compiled under | Notes |
|---|---|---|---|
| Secret credential | `SecretToken` | always (+ `expose()` gated, see §7) | `crates/eggtunnel/src/common.rs:19` |
| Local service mapping (private side) | `ClientService` | always | `crates/eggtunnel/src/common.rs:52` |
| Server-side policy entry | `ServiceSpec` | always | `crates/eggtunnel/src/common.rs:77` |
| Listener admission policy | `BindPolicy` | struct always; enforcement server-only | `crates/eggtunnel/src/common.rs:85` |
| Observability | `Snapshot`, `Counters`, `HeartbeatSnapshot`, `ResourceLimits`, `RuntimePolicy`, `TimeoutPolicy`, `TerminationCategory` | `Snapshot`/`HeartbeatSnapshot`/`ResourceLimits`/`RuntimePolicy`/`TimeoutPolicy`/`TerminationCategory` always; `Counters` (+ `HeartbeatState`) gated | `Snapshot` at `crates/eggtunnel/src/common.rs:137`, `HeartbeatSnapshot` at `crates/eggtunnel/src/common.rs:164`, `TerminationCategory` at `crates/eggtunnel/src/common.rs:180`, `ResourceLimits` at `crates/eggtunnel/src/common.rs:196`, `TimeoutPolicy` at `crates/eggtunnel/src/common.rs:250`, `RuntimePolicy` at `crates/eggtunnel/src/common.rs:306`, `Counters` at `crates/eggtunnel/src/common.rs:320` |
| Error vocabulary | `TunnelError` + `termination_category()` | always | `crates/eggtunnel/src/common.rs:437`, `crates/eggtunnel/src/common.rs:467` |
| Server-only enforcement | `verify_token`, `bind_to_socket`, `permits_address`, `permits_port` | `server` only | `crates/eggtunnel/src/common.rs:485`, `crates/eggtunnel/src/common.rs:491`, `crates/eggtunnel/src/common.rs:516`, `crates/eggtunnel/src/common.rs:522` |

Design intent, corroborated by `docs/SECURITY.md:9-17` and `docs/ARCHITECTURE.md:1-6`:

- The wire DTOs live one layer down in `eggtunnel-proto` (runtime-neutral, bounded). `common.rs` lifts them into runtime-facing config/policy/observability types: it imports `EffectiveBind, RequestedBind, ServiceId, ServiceName, SessionId, TcpTarget` at `crates/eggtunnel/src/common.rs:12`.
- The server chooses the effective listener address; authentication success never grants bind permission by itself (`docs/SECURITY.md:14-17`). `BindPolicy` is evaluated before a listener is bound.
- The server ignores the client `Target` as authority; only the client uses its configured local target after a valid `Open` (`docs/SECURITY.md:11-12`). This is why the server-side view (`ServiceSpec`) omits the target (§3).

---

## 2. `SecretToken`

Defined at `crates/eggtunnel/src/common.rs:19`.

### 2.1 Validation — `crates/eggtunnel/src/common.rs:22-30`

```rust
pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, TunnelError> {
    let bytes = bytes.into();
    if bytes.is_empty() || bytes.len() > eggtunnel_proto::MAX_AUTH_TOKEN_BYTES {
        return Err(TunnelError::Configuration("token must contain 1..=4096 bytes"));
    }
    Ok(Self(bytes))
}
```

- Accepts any `impl Into<Vec<u8>>` (callers pass `value.into_bytes()` from env/file; see `crates/eggtunnel-cli/src/main.rs:79-82`).
- Rejects empty tokens and tokens longer than `MAX_AUTH_TOKEN_BYTES = 4096` (`crates/eggtunnel-proto/src/lib.rs:20`). The proto layer enforces the same upper bound independently at `crates/eggtunnel-proto/src/lib.rs:289` (`Auth::new`) and `crates/eggtunnel-proto/src/lib.rs:305`, so oversize input fails consistently whether it enters via `SecretToken::new` or via a raw `Auth` message (proto test at `crates/eggtunnel-proto/src/lib.rs:708-710` covers the latter).
- Failure mode is `TunnelError::Configuration(&'static str)` — a non-allocating, log-safe error (no secret bytes in the message).

### 2.2 Redacted `Debug` — `crates/eggtunnel/src/common.rs:38-42`

```rust
impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([REDACTED])")
    }
}
```

- `Clone, Eq, PartialEq` are derived (`crates/eggtunnel/src/common.rs:18`) but `Debug` is manual. There is deliberately no `Display`, no `Serialize`, and no accessor returning an owned copy.
- This matches the documented guarantee in `docs/SECURITY.md:6-7`: "secret-bearing configuration Debug output is redacted."

### 2.3 Zeroize-on-drop — `crates/eggtunnel/src/common.rs:44-48`

```rust
impl Drop for SecretToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
```

- Uses the unconditional `zeroize` dependency (`crates/eggtunnel/Cargo.toml:37`). Best-effort clearing of the heap buffer on drop. Note `Clone` duplicates the secret (each clone zeroizes independently on its own drop); reviewers should treat clone count as secret-spread surface.
- Same pattern is used for mTLS private-key buffers per `docs/SECURITY.md:39`.

### 2.4 `expose()` visibility — `crates/eggtunnel/src/common.rs:32-35`

```rust
#[cfg(any(feature = "client", feature = "server"))]
pub(crate) fn expose(&self) -> &[u8] {
    &self.0
}
```

- `pub(crate)` + gated on `client` or `server`. External embedders and the CLI cannot call it; only in-crate client/server code can. The borrow (not owned copy) keeps the zeroize-on-drop guarantee meaningful — no detached copy outlives the token unless the caller copies it (and the two call sites that do are audited below).

### 2.5 How client and server use it

| Side | Use site | Behavior |
|---|---|---|
| Client | `crates/eggtunnel/src/client.rs:1046`: `Auth::new(token.expose().to_vec())?` | Copies the secret into a wire `Auth` message once per session handshake, inside the already-established verified TLS stream. The `Auth` wire type itself has redacted `Debug` (proto layer). |
| Server | `crates/eggtunnel/src/server.rs:1078`: `verify_token(&token, auth.token())` | Never serializes or logs the expected token; compares in constant time (§6.2). On mismatch: records per-source auth failure, drops handshake guards, sleeps 100 ms, bumps `rejected`, sends generic `Error{code:4, "authentication failed"}`, returns `TunnelError::Authentication` (`crates/eggtunnel/src/server.rs:1078-1093`; non-`Auth` message where `Auth` is expected returns `Authentication` at `:1075-1076`). No Session is created and no service is registered before this check passes (`docs/SECURITY.md:5-6`). |
| Config plumbing | `ServerConfig.token` (`crates/eggtunnel/src/server.rs:59`), `ClientConfig` token, CLI `load_token` (`crates/eggtunnel-cli/src/main.rs:79-82`) | Both configs own a `SecretToken`; construction fails early on empty/oversize input. |
| Tests | e.g. `crates/eggtunnel/src/server_tests.rs:47-57` | `SecretToken::new(b"...".to_vec()).unwrap()` per test harness; short literal tokens are fine because the lower bound is 1 byte. |

---

## 3. `ClientService` vs `ServiceSpec`

### 3.1 Fields

`ClientService` — `crates/eggtunnel/src/common.rs:52-57`, constructor `crates/eggtunnel/src/common.rs:60-73`:

```rust
pub struct ClientService {
    pub id: ServiceId,
    pub name: ServiceName,
    pub requested_bind: RequestedBind,
    pub target: TcpTarget,
}
```

`ServiceSpec` — `crates/eggtunnel/src/common.rs:77-81`, constructor `crates/eggtunnel/src/common.rs:127-133`:

```rust
pub struct ServiceSpec {
    pub id: ServiceId,
    pub name: ServiceName,
    pub requested_bind: RequestedBind,
}
```

Both are `Clone, Debug` (all fields are plain data; `ServiceName`/`TcpTarget` already validate their invariants at construction in proto). All fields are `pub` — these are vocabulary structs, not invariant-enforcing wrappers.

`RequestedBind` itself (`crates/eggtunnel-proto/src/lib.rs:130-133`) has exactly two variants:

```rust
pub enum RequestedBind {
    Loopback { port: u16 },
    Ip { address: [u8; 16], port: u16 },
}
```

`TcpTarget` (`crates/eggtunnel-proto/src/lib.rs:142-179`) carries a private `host: String` + `port: u16`; `TcpTarget::new` rejects empty hosts, hosts longer than `MAX_TARGET_HOST_BYTES` (253), hosts containing control characters, and port 0.

### 3.2 Security property: the server never sees `TcpTarget` as authority

The difference is exactly one field: `ServiceSpec` has **no `target`**.

- The client registers with a full `RegisterService { service_id, name, requested_bind, target }` built from `ClientService` at `crates/eggtunnel/src/client.rs:1064-1070`, and consumes `service.target` locally when an `Open` arrives via `TcpTargetConnector::connect` at `crates/eggtunnel/src/client/config.rs:32-43` (`TcpStream::connect((service.target.host(), service.target.port()))`), or via an embedder-supplied `TargetConnector`.
- The server reads only `register.service_id`, `register.name`, and `register.requested_bind` at `crates/eggtunnel/src/server.rs:1154-1189` (registration arm at `:1154`). The code comment at `crates/eggtunnel/src/server.rs:1169` is explicit: "The target descriptor is client-owned. The server uses it only as bounded registration metadata." It never dials it, never binds from it, and never echoes it back (the ack carries only `RegisterAck { service_id, effective_bind }` at `crates/eggtunnel/src/server.rs:1189`).
- This is the `docs/SECURITY.md:11-12` property: "The server ignores the client Target as authority; only the client uses its configured local target after a valid Open."

### 3.3 Where each is constructed / consumed

| Type | Constructed | Consumed |
|---|---|---|
| `ClientService` | CLI `client_services()` (`crates/eggtunnel-cli/src/main.rs:85-100`); `ClientService::new` (`crates/eggtunnel/src/common.rs:60`); test call sites in `server_tests/` (e.g. `crates/eggtunnel/src/server_tests.rs:47-57`) | Client registration loop `crates/eggtunnel/src/client.rs:1064-1091`; `TargetConnector::connect(service.clone(), ctx)` in `crates/eggtunnel/src/client/open.rs:12-30`; desired/active lifecycle in `ServiceState` (`crates/eggtunnel/src/client/service_state.rs:47`, `activate_initial`) |
| `ServiceSpec` | `ServiceSpec::new` (`crates/eggtunnel/src/common.rs:127`); re-exported at `crates/eggtunnel/src/lib.rs:27-30` | **No in-tree consumer.** A repo-wide search finds only the definition, the `lib.rs` re-export, and mentions in `architecture/overview.md` and plans. The live server path consumes the wire `RegisterService` message directly (`crates/eggtunnel/src/server.rs:1154`), not `ServiceSpec`. Treat `ServiceSpec` as public embedder-facing vocabulary (the "server view" of a service for policy/registry APIs) rather than a load-bearing runtime type today. |

Review note: if a future server refactor starts accepting `ServiceSpec` from embedders, the conversion from `RegisterService` must drop `target` explicitly at the boundary and must not let an embedder-supplied `requested_bind` bypass `bind_to_socket` (§4). The current code is safe because the only `requested_bind → socket` path goes through `bind_to_socket`.

---

## 4. `BindPolicy`

Typed admission policy for server-owned service listeners. Struct at `crates/eggtunnel/src/common.rs:85-93`.

### 4.1 Every field

| Field | Type | Meaning |
|---|---|---|
| `allow_public_addresses` | `bool` | Master switch for non-loopback bind addresses. `false` (default) means loopback only. Set from `ServerConfig.allow_public_service_binds` in `ServerBuilder::new` (`crates/eggtunnel/src/server.rs:100-113`) and the `bind_websocket` / `bind_quic` variants. |
| `allowed_addresses` | `Vec<[u8; 16]>` | Exact IPv6-byte allowlist. **Empty permits any address allowed by `allow_public_addresses`** (`crates/eggtunnel/src/common.rs:88`). Non-empty is an intersection: the address must both pass the loopback/public gate and be a member of the list (`crates/eggtunnel/src/common.rs:516-519`). |
| `allowed_port_ranges` | `Vec<(u16, u16)>` | Inclusive port ranges. **Empty permits every nonzero port** (`crates/eggtunnel/src/common.rs:90`). Port 0 (ephemeral) is never governed by ranges — see `allow_ephemeral_ports`. |
| `allow_ephemeral_ports` | `bool` | Whether `port == 0` (OS-assigned ephemeral) is permitted. Default `true`. Checked first in `permits_port` (`crates/eggtunnel/src/common.rs:522-531`). |
| `max_services_per_session` | `usize` | Per-session service ceiling, intersected at registration with the selected `RuntimePolicy` (`crates/eggtunnel/src/server.rs:1156`). Default 64. `validate()` upper-bounds at 65_536, decoupled from the default. |

### 4.2 Defaults — `crates/eggtunnel/src/common.rs:114-124`

```rust
Self {
    allow_public_addresses: false,   // loopback_only
    allowed_addresses: Vec::new(),   // any loopback address
    allowed_port_ranges: Vec::new(), // any nonzero port
    allow_ephemeral_ports: true,
    max_services_per_session: 64,
}
```

`BindPolicy::loopback_only()` at `crates/eggtunnel/src/common.rs:96-98` is `Self::default()`. This is the `docs/SECURITY.md:9-10` default: "Service binds are loopback only unless `allow_public_service_binds` is explicitly enabled."

### 4.3 `validate()` rules — `crates/eggtunnel/src/common.rs:100-111`

Validated by `ServerBuilder::validate` (`crates/eggtunnel/src/server.rs:136-145`) via `validate_server_profile` (`crates/eggtunnel/src/server.rs:459-484`) before any TLS/listener setup. Fails with `TunnelError::Configuration("bind policy is invalid")` if **any** of:

1. `max_services_per_session == 0`, or
2. `max_services_per_session > 65_536`, or
3. any `(start, end)` in `allowed_port_ranges` has `start == 0` (port 0 belongs to the ephemeral policy, not ranges) or `start > end` (inverted/empty range).

Note what is *not* validated: `allowed_addresses` entries are never checked for loopback-ness at `validate()` time (a public address in the list is harmless while `allow_public_addresses == false` because the gate in `permits_address` still rejects it); overlapping/duplicate ranges are permitted (they are purely additive in `permits_port`).

### 4.4 Server-only enforcement: `permits_address` / `permits_port` / `bind_to_socket()`

`permits_address` — `crates/eggtunnel/src/common.rs:516-519` (`#[cfg(feature = "server")]`):

```rust
fn permits_address(&self, address: [u8; 16], is_loopback: bool) -> bool {
    (is_loopback || self.allow_public_addresses)
        && (self.allowed_addresses.is_empty() || self.allowed_addresses.contains(&address))
}
```

`permits_port` — `crates/eggtunnel/src/common.rs:522-531` (`#[cfg(feature = "server")]`):

```rust
fn permits_port(&self, port: u16) -> bool {
    if port == 0 {
        return self.allow_ephemeral_ports;
    }
    self.allowed_port_ranges.is_empty()
        || self.allowed_port_ranges.iter().any(|(start, end)| (*start..=*end).contains(&port))
}
```

`bind_to_socket` — `crates/eggtunnel/src/common.rs:491-512` (`#[cfg(feature = "server")]`, `pub(crate)`):

- `RequestedBind::Loopback { port }` → checks `permits_port` only, then binds `SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, port, 0, 0))` (`crates/eggtunnel/src/common.rs:497-502`). Loopback never consults `allow_public_addresses`/`allowed_addresses` — it is always address-permitted.
- `RequestedBind::Ip { address, port }` → converts with `Ipv6Addr::from(*address)`, checks `permits_address(*address, ip.is_loopback()) && permits_port(*port)`, then binds `SocketAddrV6::new(ip, port, 0, 0)` (`crates/eggtunnel/src/common.rs:503-509`). An `Ip` request carrying `::1` is treated as loopback even though it arrived via the `Ip` variant.
- Either rejection returns `TunnelError::Authorization` (which the registration loop translates into `rejected += 1` plus `write_registration_error(code 2)` at `crates/eggtunnel/src/server.rs:1172`).
- Both arms produce `SocketAddr::V6` unconditionally — dual-stack listeners via IPv6 sockets; there is no IPv4-mapped special-casing beyond what the 16-byte representation already encodes.

Call site: exactly one, the `RegisterService` handler at `crates/eggtunnel/src/server.rs:1170`. Policy is therefore evaluated per registration, before `TcpListener::bind` on the accepted address.

### 4.5 Policy decision table

| Request | Policy | Decision | Why |
|---|---|---|---|
| `Loopback{port: 8080}` | default | ✅ permit (subject to OS bind) | Address gate skipped for loopback; port 8080 ≠ 0 and ranges empty → permit |
| `Loopback{port: 0}` | default | ✅ permit (ephemeral) | `permits_port(0) == allow_ephemeral_ports == true` |
| `Loopback{port: 0}` | `allow_ephemeral_ports = false` | ❌ `Authorization` | Port-0 branch returns `false` regardless of ranges |
| `Loopback{port: 8080}` | `allowed_port_ranges = [(8000, 9000)]` | ✅ permit | 8080 ∈ [8000, 9000] |
| `Loopback{port: 22}` | `allowed_port_ranges = [(8000, 9000)]` | ❌ `Authorization` | Non-empty ranges are a whitelist; 22 ∉ any range |
| `Ip{::1, 8080}` | default | ✅ permit | `is_loopback == true` passes the address gate; port passes |
| `Ip{203.0.113.7, 8080}` | default (`allow_public_addresses = false`) | ❌ `Authorization` | `is_loopback == false`, master switch off — even though `allowed_addresses` is empty |
| `Ip{203.0.113.7, 8080}` | `allow_public_addresses = true` | ✅ permit | Public gate open; empty `allowed_addresses` = any; empty ranges = any nonzero port |
| `Ip{203.0.113.7, 8080}` | `allow_public_addresses = true, allowed_addresses = [198.51.100.9]` | ❌ `Authorization` | Non-empty allowlist is an intersection; 203.0.113.7 ∉ list |
| `Ip{203.0.113.7, 0}` | `allow_public_addresses = true, allow_ephemeral_ports = false` | ❌ `Authorization` | Port-0 branch fails even though the address passes |
| any | `max_services_per_session = 0` or `> 65536`, or range with `start == 0` / `start > end` | ❌ `Configuration` at bind time | `validate()` rejects the whole policy before serving |
| any | `max_services_per_session = 4`, 5th `RegisterService` | ❌ registration error + `rejected += 1`, `ResourceExhausted` termination | `services.len() >= bind_policy.max_services_per_session.min(policy.limits.services_per_session)` at `crates/eggtunnel/src/server.rs:1156` |

Authentication success never grants bind permission by itself (`docs/SECURITY.md:17`): auth is checked before any `bind_to_socket` on that session, and policy is re-evaluated per `RegisterService` against the session's policy (`crates/eggtunnel/src/server.rs:1170`).

---

## 5. `Snapshot` / `Counters` / `ResourceLimits` / `RuntimePolicy` / `TimeoutPolicy`

### 5.1 `ResourceLimits` — `crates/eggtunnel/src/common.rs:196-245`

Immutable finite ceilings for the caller-selected runtime profile (`RuntimePolicy.limits`, `crates/eggtunnel/src/common.rs:306-309`). `Clone, Copy, Debug, Eq, PartialEq`. Every field validates `1..=65536` via `ResourceLimits::validate` (`crates/eggtunnel/src/common.rs:208-229`); defaults reproduce the pre-M008 effective runtime (`crates/eggtunnel/src/common.rs:232-245`, pinned by the test at `crates/eggtunnel/src/common.rs:539-568`).

| Field | Default | Selected-policy enforcement |
|---|---|---|
| `sessions` | 128 | Server session admission: `active.len() >= counters.policy.limits.sessions` (`crates/eggtunnel/src/server.rs:1116`) |
| `services_per_session` | 64 | Registration cap `min(bind_policy.max_services_per_session, limits.services_per_session)` (`crates/eggtunnel/src/server.rs:1156`); client config validation (`crates/eggtunnel/src/client.rs:582`) |
| `pending_per_session` | 128 | Pending ConnectionId admission (`crates/eggtunnel/src/server.rs:1343`) |
| `active_connections_per_session` | 128 | Per-session data-stream caps (`crates/eggtunnel/src/server.rs:619`, `crates/eggtunnel/src/server.rs:1108`); stream admission |
| `accepted_handshakes` | 64 | Pre-auth handshake semaphores (`crates/eggtunnel/src/server.rs:538`, `crates/eggtunnel/src/server.rs:635`) |
| `client_open_tasks` | 128 | Client `Open`-task semaphore (`crates/eggtunnel/src/client.rs:1109`); QUIC stream budget derived from it (`crates/eggtunnel/src/client.rs:871`) |
| `control_queue` | 128 | Bounded control channels: server per-session queue (`crates/eggtunnel/src/server.rs:1135`) and client outbound queue (`crates/eggtunnel/src/client.rs:1110`) |
| `client_command_queue` | 32 | Client API command channel (`crates/eggtunnel/src/client.rs:337`) |

`Snapshot.resource_limits` echoes the **selected** policy (`resource_limits: self.policy.limits` at `crates/eggtunnel/src/common.rs:377`) — it is not always the default. `ClientBuilder::runtime_policy` (`crates/eggtunnel/src/client/config.rs:113-116`) and `ServerBuilder::runtime_policy` (`crates/eggtunnel/src/server.rs:125-128`) install the policy; both builders run `RuntimePolicy::validate` first via `validate_client_profile` (`crates/eggtunnel/src/client.rs:524-532`) and `validate_server_profile` (`crates/eggtunnel/src/server.rs:459-467`). The server test at `crates/eggtunnel/src/server_tests/tcp.rs:882` asserts `server_snapshot.resource_limits.sessions == MAX_SESSIONS` (test-only constant), pinning snapshot/policy coherence.

`TimeoutPolicy` (`crates/eggtunnel/src/common.rs:250-302`) carries the 9 durations: `connect`, `handshake`, `control_idle`, `pending_connection`, `relay_drain`, `shutdown_grace`, `reconnect_initial`, `reconnect_max`, `heartbeat_interval`. `validate()` (`crates/eggtunnel/src/common.rs:263-285`) requires every duration positive and ≤ 24 h, `reconnect_initial <= reconnect_max`, and `heartbeat_interval < control_idle`. Defaults (`crates/eggtunnel/src/common.rs:288-302`): connect 10 s, handshake 10 s, control idle 90 s, pending ConnectionId 30 s, relay drain 15 s, shutdown grace 1 s, reconnect 500 ms → 30 s, heartbeat 20 s.

### 5.2 `Snapshot` — `crates/eggtunnel/src/common.rs:137-160`

`Clone, Debug, Default, Eq, PartialEq`. Public, returned by `ServerHandle::snapshot` (`crates/eggtunnel/src/server.rs:100-102`) and `ClientHandle::snapshot` (`crates/eggtunnel/src/client.rs:132`).

| Field | Type | Meaning |
|---|---|---|
| `connected` | `bool` | `counters.connected > 0`. Client sets `connected = 1` after successful registration (`crates/eggtunnel/src/client.rs:933-935`); server resets via guard on drop. |
| `active_sessions` | `usize` | Live sessions (server) / 1-or-0 session flag (client stores `sessions.len()` at `crates/eggtunnel/src/client.rs:940-941`, zeroed by `CounterGuard` on drop at `crates/eggtunnel/src/client.rs:1177-1180`). |
| `registered_services` | `usize` | Count of registered services. |
| `pending_connections` | `usize` | Single-use pending correlations awaiting client data dial (30 s lifetime per `docs/SECURITY.md:19-20`). |
| `active_connections` | `usize` | Currently relaying data connections. |
| `active_client_open_tasks` | `usize` | Client-side in-flight `Open`→dial tasks (server snapshots report 0 / its own `open_tasks` counter, which tracks handshake-adjacent work depending on profile). |
| `active_handshakes` | `usize` | Concurrent unauthenticated handshakes (capped at 64). |
| `high_water_*` (6 fields) | `usize` | Maxima for sessions, services, pending, active, open tasks, handshakes. Monotonic within process lifetime; `fetch_max` on every increment, never decremented. |
| `task_panics` | `u64` | Panics observed in supervised `JoinSet`s via `record_join_result`. |
| `last_termination` | `Option<TerminationCategory>` | Most recent termination cause. **Last-write-wins** — overwritten by every `record_termination` call. Starts `None` (via `Counters::default`). |
| `heartbeat` | `HeartbeatSnapshot` | Bounded current-session heartbeat health (`crates/eggtunnel/src/common.rs:164-169`): `session_generation`, `last_pong_age_ms`, `latest_rtt_ms`, `missed_heartbeats`. Reset by `begin_session` on each new generation; see §5.5. |
| `resource_limits` | `ResourceLimits` | Echoes the selected `RuntimePolicy` limits (`crates/eggtunnel/src/common.rs:377`), not necessarily the default profile (see §5.1). |
| `reconnects` | `u64` | Client reconnect-loop iterations (`reconnects.fetch_add` in `reconnect_loop` at `crates/eggtunnel/src/client.rs:792-794` and `record_quic_reconnect` at `crates/eggtunnel/src/client.rs:951-953`). |
| `rejected_connections` | `u64` | Every refused registration / saturated admission / failed auth (`rejected.fetch_add` at ~10 server sites, e.g. `crates/eggtunnel/src/server.rs:1084-1086`, `crates/eggtunnel/src/server.rs:1157`, `crates/eggtunnel/src/server.rs:1164`). |
| `bytes_upstream` / `bytes_downstream` | `u64` | Relay byte totals, accumulated from per-connection reports on both sides (`crates/eggtunnel/src/server.rs:1370-1380`, `crates/eggtunnel/src/client/open.rs:67-75`). Monotonic; wrap only at u64 overflow. |
| `effective_binds` | `Vec<(SessionId, ServiceId, EffectiveBind)>` | Currently bound listeners with server-chosen addresses. Pushed on successful bind (`crates/eggtunnel/src/server.rs:1180`; client echoes acks at `crates/eggtunnel/src/client.rs:1079-1083`), removed on unregister (`crates/eggtunnel/src/server.rs:1196-1197`). The CLI prints newly observed entries every 250 ms. |

### 5.3 `Counters` — `crates/eggtunnel/src/common.rs:320-344`

`pub(crate)`, `Clone, Default`, gated on `any(feature = "client", feature = "server")`. All counters are `Arc<atomic>` so `Clone` shares state across tasks; `last_termination`, `binds`, and `heartbeat` are `Arc<Mutex<…>>` (low contention: written on termination/bind/heartbeat events, cloned wholesale on snapshot). `Counters` owns the selected policy as `policy: Arc<RuntimePolicy>` (`crates/eggtunnel/src/common.rs:321`), installed via `with_policy()` (`crates/eggtunnel/src/common.rs:348-353`); admission sites read `counters.policy.limits.*` / `counters.policy.timeouts.*` (§5.1).

Field correspondence (`Counters` → `Snapshot`): `connected→connected`, `sessions→active_sessions`, `services→registered_services`, `pending→pending_connections`, `active_connections→active_connections`, `open_tasks→active_client_open_tasks`, `handshakes→active_handshakes`, six `high_water_*` (note the names compress: `high_water_pending`, `high_water_open_tasks`), `task_panics`, `last_termination`, `policy.limits→resource_limits` (selected policy, not the default), `session_generation→heartbeat.session_generation`, `heartbeat (HeartbeatState)→heartbeat (HeartbeatSnapshot)`, `reconnects→reconnects`, `rejected→rejected_connections`, `bytes_*`, `binds→effective_binds`.

### 5.4 `snapshot()` semantics — `crates/eggtunnel/src/common.rs:355-395`

- Every atomic is loaded with `Ordering::Relaxed`. Snapshot is observational, not synchronized: concurrent increments may or may not be visible, but no snapshot ever tears (each field is one atomic load) and counters never block the data path.
- `connected` is derived (`> 0`), not stored as bool.
- Mutexes are locked with `lock().unwrap_or_else(|p| p.into_inner())` — a poisoned mutex still yields its inner value rather than panicking the observability path.
- `effective_binds` is cloned under lock; cost is O(services). `resource_limits` is copied from the selected policy (`self.policy.limits`).
- `heartbeat` maps the private `HeartbeatState` (`last_pong_at: Option<Instant>`, `latest_rtt_ms`, `missed_heartbeats` at `crates/eggtunnel/src/common.rs:173-177`) to the public bounded `HeartbeatSnapshot`: `last_pong_at` becomes an age in milliseconds (saturating at `u64::MAX`), RTT likewise, plus the live `session_generation`.

### 5.5 Session generations, heartbeat recording, `record_termination` / `record_join_result` — `crates/eggtunnel/src/common.rs:397-433`

```rust
pub fn record_termination(&self, category: TerminationCategory) { *lock = Some(category); }
pub fn record_join_result<T>(&self, result: &Result<T, tokio::task::JoinError>) {
    if result.as_ref().is_err_and(tokio::task::JoinError::is_panic) {
        self.task_panics.fetch_add(1, Ordering::Relaxed);
        self.record_termination(TerminationCategory::Internal);
    }
}
```

- `record_termination` overwrites unconditionally — reviewers debugging flaky `last_termination` assertions should expect races between concurrent tasks; the value is "last writer wins", not a history.
- `record_join_result` counts **only panics** (`is_panic`), not cancellations or ordinary join errors. Panics additionally force `TerminationCategory::Internal`. The client calls the shared helper (`crates/eggtunnel/src/client.rs:654` etc.); the server has both the shared helper (service tasks at `crates/eggtunnel/src/server.rs:1049`) and an inline equivalent for the accept loop (`crates/eggtunnel/src/server.rs:622-623`).
- `begin_session` (`crates/eggtunnel/src/common.rs:404-414`) bumps `session_generation` by 1 (checked add; exhaustion at `u64::MAX` fails closed with `TunnelError::ResourceExhausted`) and resets `HeartbeatState` to default, so each Session generation starts with `last_pong_age_ms: None`, `latest_rtt_ms: None`, `missed_heartbeats: 0` (covered by the test at `crates/eggtunnel/src/common.rs:604-622`).
- `record_heartbeat_missed` (`crates/eggtunnel/src/common.rs:416-419`) saturating-increments `missed_heartbeats`. `record_heartbeat_pong(sent_at)` (`crates/eggtunnel/src/common.rs:421-426`) stamps `last_pong_at = now`, records the RTT in milliseconds (saturating at `u64::MAX`), and clears `missed_heartbeats` to 0. Snapshot health is therefore bounded fixed-size data only — no per-ping history.

Counter increment/decrement discipline (spot-checked):

- Sessions: `fetch_add + fetch_max(high_water)` on admit (`crates/eggtunnel/src/server.rs:1122-1128`); decrement via `SessionGuard` on exit. Client uses `CounterGuard` that **stores 0** on drop rather than decrementing (`crates/eggtunnel/src/client.rs:1103`, def at `:1374-1399`) — correct because the client has at most one session.
- Services: `fetch_add + fetch_max` on register (`crates/eggtunnel/src/server.rs:1187-1188`); `fetch_sub` on unregister (`crates/eggtunnel/src/server.rs:1196-1197`). Client stores the count once after bulk registration (`crates/eggtunnel/src/client.rs:1095-1098`).
- Pending: `fetch_add + fetch_max` on accept (`crates/eggtunnel/src/server.rs:1350-1351`); `fetch_sub` on consume/reject/expire (`crates/eggtunnel/src/server.rs:1204-1205` on `OpenReject`, `:1353` on failed `Open` send, `:1366-1368` on relay settle, `remove_service_pending` at `:1424-1433`).
- Active: `fetch_add + fetch_max` in `ActiveConnectionGuard::new` (`crates/eggtunnel/src/server.rs:1401-1408`); `fetch_sub` on `Drop` (`crates/eggtunnel/src/server.rs:1416-1422`).
- Handshakes: `HandshakeGuard::new(fetch_add + fetch_max)` (def at `crates/eggtunnel/src/server.rs:839-858`); constructed on accept (`crates/eggtunnel/src/server.rs:562`, `:661`, `:775`); saturation covered by the `MAX_HANDSHAKES + 1` test (`crates/eggtunnel/src/server_tests/tcp.rs:1508-1523`).
- Open tasks (client): `OpenTaskGuard::new(fetch_add + fetch_max)` (def at `crates/eggtunnel/src/client.rs:1381-1394`); constructed per-`Open` (`crates/eggtunnel/src/client.rs:1177-1180`).

---

## 6. `TunnelError`, `termination_category()`, `verify_token`

### 6.1 `TunnelError` variants — `crates/eggtunnel/src/common.rs:437-464`

`#[derive(Debug, Error)]` via `thiserror` — 13 variants. Display strings are fixed, secret-free literals except for the wrapped `#[from]` sources (`io::Error`, `ProtocolError`).

| Variant | Display | Typical origin |
|---|---|---|
| `Configuration(&'static str)` | `invalid configuration: {0}` | Bad token bytes, bad `BindPolicy`, bad `RuntimePolicy`, missing runtime, oversized PEM (`crates/eggtunnel/src/server.rs:449-455` checks PEM material against `MAX_FRAME_BYTES`), bad client service count |
| `Io(#[from] std::io::Error)` | `I/O operation failed: {0}` | Listener bind / dial / relay I/O |
| `Tls` | `TLS setup or handshake failed` | Unit variant — deliberately carries no rustls detail (avoids leaking handshake internals) |
| `Protocol(#[from] ProtocolError)` | `protocol error: {0}` | Framing / unexpected message / hostile input |
| `Authentication` | `server authentication failed` | Unit; bad token, wrong message where `Auth` expected (`crates/eggtunnel/src/server.rs:1075-1076`, `crates/eggtunnel/src/server.rs:1078-1093`) |
| `Authorization` | `service was rejected by server policy` | Unit; covers both policy rejection (`bind_to_socket` failure) and resource-capacity rejection (`sessions` overflow at `crates/eggtunnel/src/server.rs:1116-1118` returns `Authorization` after recording `ResourceExhausted` — see mapping note below) |
| `Disconnected` | `server connection ended` | Unit; control stream EOF / handshake timeout mapping |
| `Cancelled` | `operation was cancelled` | Unit; `CancellationToken` / shutdown / `Drain` |
| `Timeout` | `operation timed out` | Unit; connect/handshake/relay timeouts |
| `Target` | `local target rejected or failed the connection` | Unit; client-side dial/connect failure (`crates/eggtunnel/src/client/open.rs:29` maps connector/timeout errors to `Target`) vs `TargetError`/`TargetConnector` trait errors which are the connector's own vocabulary |
| `ResourceExhausted` | `runtime resource limit was reached` | Unit; caps, full control queue, saturated semaphores, session-generation exhaustion (`begin_session` at `crates/eggtunnel/src/common.rs:404-414`) |
| `ServiceAlreadyExists` | `service identifier or name is already registered` | Unit; duplicate service id/name on registration (`crates/eggtunnel/src/server.rs:1163-1166`); maps to `Authorization` termination (`crates/eggtunnel/src/common.rs:476`) |
| `PeerClosed` | `peer closed the connection` | Unit; clean remote close during relay |

### 6.2 `termination_category()` mapping — `crates/eggtunnel/src/common.rs:467-481`

`TerminationCategory` itself is defined at `crates/eggtunnel/src/common.rs:180-192` (`Clean, Cancelled, Timeout, Authentication, Authorization, Protocol, Transport, Target, ResourceExhausted, PeerClosed, Internal`).

| `TunnelError` | `TerminationCategory` | Notes |
|---|---|---|
| `Cancelled` | `Cancelled` | 1:1 |
| `Timeout` | `Timeout` | 1:1 |
| `Target` | `Target` | 1:1 (client target failures stay distinguishable from transport) |
| `ResourceExhausted` | `ResourceExhausted` | 1:1 |
| `PeerClosed` | `PeerClosed` | 1:1 |
| `Authentication` | `Authentication` | 1:1 |
| `Authorization` | `Authorization` | 1:1 — **including** the `sessions`-limit overflow path, which records `ResourceExhausted` via `record_termination` but returns `Authorization` as the `TunnelError` (`crates/eggtunnel/src/server.rs:1116-1118`). The snapshot's `last_termination` and the returned error therefore disagree by design on that path; assert on the right one. |
| `ServiceAlreadyExists` | `Authorization` | Duplicate id/name registrations share the `Authorization` category with policy denials (`crates/eggtunnel/src/common.rs:476`) |
| `Protocol(_)` | `Protocol` | Inner `ProtocolError` discriminant is collapsed |
| `Io(_)` / `Tls` / `Disconnected` | `Transport` | Three error variants share one category; use the error value (not the category) when the distinction matters |
| `Configuration(_)` | `Internal` | Config bugs surface as `Internal` termination — intentional: they indicate embedder/setup faults, not runtime conditions |

`Clean` is never produced by `termination_category()` — it is reserved for graceful shutdown paths that record termination directly. Every fallible session/handshake exit is expected to funnel through `record_termination(error.termination_category())` (client: `crates/eggtunnel/src/client.rs:654`, `crates/eggtunnel/src/client.rs:1142`; server: `crates/eggtunnel/src/server.rs:449`, `crates/eggtunnel/src/server.rs:620`).

### 6.3 `verify_token` constant-time semantics — `crates/eggtunnel/src/common.rs:485-488`

```rust
#[cfg(feature = "server")]
pub(crate) fn verify_token(expected: &SecretToken, received: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    expected.expose().len() == received.len() && bool::from(expected.expose().ct_eq(received))
}
```

- `subtle::ConstantTimeEq::ct_eq` (unconditional `subtle` dependency at `crates/eggtunnel/Cargo.toml:36`) compares byte content in constant time for the compared length. The `&&` short-circuits the content comparison when lengths differ — this leaks length, which is acceptable because token length is not the secret (and the wire encoding already reveals it); content comparison itself does not early-exit on first mismatch.
- `pub(crate)` + `server`-gated: unreachable from client builds and from downstream crates. Sole call site is the auth check at `crates/eggtunnel/src/server.rs:917`, followed by the 100 ms failure delay + per-source throttle (`docs/SECURITY.md:25-30`), so online guessing is rate-limited on top of the constant-time compare.

---

## 7. Feature gates

`crates/eggtunnel/Cargo.toml:15-23`: `default = ["client", "tls"]`; `client`, `server`, `tls`, `quic` (implies client+server), `websocket` (implies client+server), `outbound-proxy` (implies client), `mtls`.

| Item in `common.rs` | Gate | Effect of building without it |
|---|---|---|
| `SecretToken` struct, `new`, `Debug`, `Drop` | none (always compiled) | Always available; `lib.rs` re-export unconditional |
| `ClientService`, `ServiceSpec`, `BindPolicy` struct, `loopback_only`, `validate`, `Default` | none | Usable even with `--no-default-features` for config construction |
| `Snapshot`, `HeartbeatSnapshot`, `TerminationCategory`, `ResourceLimits`, `RuntimePolicy`, `TimeoutPolicy`, `TunnelError`, `termination_category()` | none | Error/policy/observability vocabulary always linked |
| `use std::net::SocketAddr` (`crates/eggtunnel/src/common.rs:2-3`) | `server` | Client-only builds do not monomorphize socket mapping |
| `SecretToken::expose` (`crates/eggtunnel/src/common.rs:32`) | `any(client, server)` | With neither feature, the secret cannot leave the type at all (no accessor exists) |
| `Counters` struct + `with_policy`/`snapshot`/`begin_session`/`record_heartbeat_*`/`record_termination`/`record_join_result` (`crates/eggtunnel/src/common.rs:320-433`), `Instant` + atomic imports (`crates/eggtunnel/src/common.rs:4-10`) | `any(client, server)` | `--no-default-features` build has `Snapshot` as a pure data type with no producer |
| `verify_token` (`crates/eggtunnel/src/common.rs:485`), `bind_to_socket` (`crates/eggtunnel/src/common.rs:491`), `permits_address` (`crates/eggtunnel/src/common.rs:516`), `permits_port` (`crates/eggtunnel/src/common.rs:522`) | `server` | Client-only builds cannot evaluate policy or compare tokens; `BindPolicy::validate` (un-gated) remains callable for fail-fast config checks |
| `lib.rs` module wiring (`crates/eggtunnel/src/lib.rs:7-16`) | `client` / `server` per module | `common` is always compiled; `wire_io` only with either side; `Client*` re-exports need `client`, `Server*` need `server` |

Consequences for reviewers: `cargo check -p eggtunnel --no-default-features` exercises only the un-gated vocabulary; `--features server` is required to type-check the enforcement helpers; `--features client,server` (or default) covers `Counters`/`expose`. Transport features (`quic`, `websocket`, `outbound-proxy`, `mtls`) do not change `common.rs` compilation — they only add `bind_*`/`start_*` variants in `server.rs`/`client.rs` that still funnel through the same `verify_token`/`bind_to_socket`/`Counters` paths.

---

## 8. Review checklist

### 8.1 Secret handling

- [ ] New code paths touching `SecretToken` use `expose()` (borrow) rather than adding an owned/cloned accessor; any `.to_vec()` copy is scoped to the handshake message and not logged. (Current copies: `crates/eggtunnel/src/client.rs:1046` only.)
- [ ] No `Debug`/`Display`/`Serialize` impl is added for `SecretToken`, `Auth` token bytes, proxy credentials, or mTLS key buffers. `Snapshot` must stay credential-free (proxy creds are env-var + redacted per `docs/SECURITY.md:74-78`).
- [ ] `Clone` derivations on secret-bearing configs are justified; each clone extends zeroize responsibility. Prefer moving over cloning in session setup.
- [ ] `verify_token` remains the sole comparison; no `==` on token bytes is introduced anywhere (length short-circuit is the only accepted early exit).

### 8.2 Policy bypass risks

- [ ] Every `RequestedBind → TcpListener::bind` path goes through `bind_to_socket` (`crates/eggtunnel/src/common.rs:491`). Today there is exactly one call site (`crates/eggtunnel/src/server.rs:1170`); QUIC/WSS variants reuse the same registration loop — confirm any new `bind_*` variant does too.
- [ ] `allow_public_addresses` defaults to `false`; any change to `ServerConfig.allow_public_service_binds` plumbing (`ServerBuilder::new` at `crates/eggtunnel/src/server.rs:100-113`, validated at `crates/eggtunnel/src/server.rs:136-145`) preserves opt-in publicity.
- [ ] `BindPolicy::validate` is called before serving in every constructor. A new constructor that skips it could admit `max_services_per_session = 0` (denial of all registrations) or `> 65536` (above the `validate()` ceiling), or port ranges containing 0.
- [ ] `ServiceSpec` has no `target`, but the wire `RegisterService` still carries one. Any future use of `ServiceSpec` as a registration input must construct it server-side (dropping `target`) rather than trusting a client-supplied projection.
- [ ] `Ip{ address: ::1 }` is intentionally treated as loopback (`ip.is_loopback()` at `crates/eggtunnel/src/common.rs:505`). Do not "fix" this into a rejection without also handling IPv4-mapped `::ffff:127.0.0.1`, which `is_loopback()` does *not* flag — changing either behavior alters the loopback allowlist surface.
- [ ] Auth-then-policy ordering is preserved: `verify_token` (`crates/eggtunnel/src/server.rs:1078`) before any `bind_to_socket` on that session, and policy re-evaluated per `RegisterService` (a session cannot escalate by registering after a policy change — each registration checks the live `bind_policy`).

### 8.3 Counter consistency

- [ ] Every `fetch_add` has a matching `fetch_sub`/guard on all exit paths (normal, error, cancel, panic-abort). Pay attention to the asymmetric client `CounterGuard` (stores 0 on drop, `crates/eggtunnel/src/client.rs:1103`, def at `:1374-1399`) vs server `SessionGuard` (decrements) — copying one to the other side double-counts or leaks.
- [ ] Every `fetch_add` that should move the high-water is immediately followed by `fetch_max`. Missing `fetch_max` silently freezes the high-water and breaks capacity-planning assertions (e.g. `crates/eggtunnel/src/server_tests/tcp.rs:882-887`).
- [ ] `rejected` is bumped on *every* refusal path (auth fail, duplicate id/name, policy deny, bind error, session/pending/active/handshake saturation). A new refusal that forgets `rejected.fetch_add` is invisible in `Snapshot.rejected_connections`.
- [ ] `record_termination` is last-write-wins; tests asserting `last_termination` must serialize against concurrent tasks or assert `task_panics`/counters instead. `record_join_result` counts panics only — task cancellation and `Ok(Err(_))` relay errors must not inflate `task_panics`.
- [ ] All snapshot loads stay `Relaxed`; introducing `SeqCst` or a snapshot mutex around atomics buys nothing (snapshot is already eventually consistent) and risks data-path contention.
- [ ] `effective_binds` push (`crates/eggtunnel/src/server.rs:1180`) and retain-on-unregister (`crates/eggtunnel/src/server.rs:1196-1197`) stay paired; a leaked entry makes the CLI report a listener that no longer exists, and an unbounded `binds` vec on a churning session is a memory-growth vector (bounded in practice by `max_services_per_session`, but only if unregister/`remove_all_pending` cleanup at `crates/eggtunnel/src/server.rs:1228-1233` runs).

### 8.4 Limit coherence (`common.rs` ↔ `server.rs` ↔ `client.rs`)

| `ResourceLimits` field (default) | Server enforcement | Client enforcement | Check |
|---|---|---|---|
| `sessions = 128` | server session admission | n/a (single session) | Server session admission reads `RuntimePolicy.limits.sessions`; snapshots retain the selected policy |
| `services_per_session = 64` | intersected with `BindPolicy.max_services_per_session` | config validation | Both roles enforce the runtime ceiling; server authorization may impose a smaller ceiling |
| `pending_per_session = 128` | pending ConnectionId admission | n/a | Pending map insertion and its semaphore use the selected policy |
| `active_connections_per_session = 128` | active data admission | n/a | TCP and QUIC stream admission use the selected policy |
| `accepted_handshakes = 64` | pre-auth admission | n/a | TCP and QUIC handshake semaphores use the selected policy |
| `client_open_tasks = 128` | n/a | Open task semaphore | Client task admission and high-water accounting use the selected policy |
| `control_queue = 128` | Open control queue | outbound protocol control queue | Bounded Tokio channels use the selected policy; queue-full behavior remains explicit |
| `client_command_queue = 32` | n/a | API command queue | Client handle command channel preserves its pre-M008 capacity and is configurable |

- `BindPolicy.max_services_per_session` default (64) is intersected with `ResourceLimits.services_per_session`; the caller may choose a smaller bind policy ceiling.
- Authentication throttling remains a separate fixed security policy (10 failures per 60 seconds, 1,024 source entries, 100 ms failure delay); it is intentionally not caller-tunable through `RuntimePolicy`.

The associated `TimeoutPolicy` defaults are connect 10 s, handshake 10 s,
control idle 90 s, pending ConnectionId 30 s, relay drain 15 s, shutdown grace
1 s, reconnect 500 ms to 30 s, and heartbeat 20 s. Validation requires
positive durations no longer than 24 hours, an initial reconnect delay no
greater than its maximum, and heartbeat shorter than control idle.

`Snapshot.heartbeat` is a fixed-size view: generation, matching-Pong age in
milliseconds, latest RTT in milliseconds, and consecutive missed heartbeat
intervals. Shared counters reset this state at each new client generation.
