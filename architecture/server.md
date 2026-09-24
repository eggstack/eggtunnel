# Reverse-session server — `crates/eggtunnel/src/server.rs`

> Largest module (~4513 lines incl. tests). Reachable rendezvous + ingress.
> See [Architecture Overview](overview.md) §4 for the birds-eye map and
> component index. Companion dives: `common-core.md` (shared vocabulary),
> `client.md`, `proto-wire-protocol.md`, `transports-wire-io.md`.

Sources: `crates/eggtunnel/src/server.rs`, `crates/eggtunnel/src/common.rs`
(`BindPolicy` / `verify_token` / `bind_to_socket`), `docs/SECURITY.md`
(server sections), `docs/OPERATIONS.md`.

All anchors are `file:line` in the workspace root. Line numbers below track
the current implementation (`server.rs` contains runtime code; transport and lifecycle tests are in focused `server_tests/` modules).

---

## 1. Role: reachable rendezvous + ingress

The server is the only reachable party. It owns:

- **Listeners**: one control+data ingress socket (`listen_addr`; TCP+TLS,
  or UDP for QUIC) plus one server-owned `TcpListener` per registered
  service (`crates/eggtunnel/src/server.rs:997-1010`).
- **Sessions**: exactly one authenticated control stream per session
  (`serve_control`, `server.rs:872-1060`). Session table is
  `Arc<Mutex<HashMap<SessionId, Weak<SessionContext>>>>`
  (`server.rs:406-407`, `507-508`).
- **Pending correlation**: single-use `ConnectionId → PendingEntry`
  per session (`server.rs:686-690`, `728-736`). External accept inserts;
  client data-dial consumes.
- **Data accept + relay**: accepts both `ClientHello` (control) and
  `DataHello` (data) on the same ingress port, validates the latter
  against session/service/connection binding, then relays opaque bytes
  via `eggress-relay` (`server.rs:802-823`, `825-870`, `1189-1200`).

What the server explicitly does **not** do (cf. `docs/SECURITY.md:9-12`):

- never learns or trusts the client-local `TcpTarget`; only the client uses
  it after a valid `Open`;
- never picks a target; it picks the **effective bind** and reports it in
  `RegisterAck`;
- never sends bearer tokens; it only checks them inside verified TLS.

```
                    ┌──────────── NAT / firewall ────────────┐
                    │  outbound-only from private side       │
                    ▼                                        │
  ┌──────────┐  control TLS   ┌──────────┐  ingress   ┌──────────┐
  │  client  │ ─────────────► │  server  │ ◄───────── │ external │
  │(services │  + N × data TLS│(listeners│  TCP       │ clients  │
  │ →targets)│ ◄───────────── │ pending/ │ ─────────► │          │
  └──────────┘  Open/DataHello│ active)  │  relay     └──────────┘
```

---

## 2. Public API

### 2.1 `ServerConfig` + `Drop` zeroization

Defined `server.rs:54-62`:

```rust
pub struct ServerConfig {
    pub listen_addr: SocketAddr,
    pub certificate_pem: Vec<u8>,
    pub private_key_pem: Vec<u8>,
    pub token: SecretToken,
    pub allow_public_service_binds: bool,
}
```

- `listen_addr` accepts both control sessions and reverse data connections
  (`docs/OPERATIONS.md:7-12`). For QUIC it is the UDP control endpoint;
  service listeners remain TCP (`docs/OPERATIONS.md:11-12`).
- `allow_public_service_binds` is the coarse master switch. `Server::bind`
  maps it to `BindPolicy { allow_public_addresses, ..default() }`
  (`server.rs:109-115`). Finer policy uses `bind_with_policy` /
  `bind_mtls_with_policy` / `bind_quic_with_policy`.
- `Drop` zeroizes `private_key_pem` (`server.rs:64-69`); `Debug` redacts
  cert/key/token (`server.rs:71-84`). `SecretToken` itself redacts and
  zeroizes on drop (`common.rs:36-46`). mTLS client keys get the same
  treatment (`docs/SECURITY.md:38-39`).
- `validate_config` (`server.rs:342-356`) rejects empty cert/key and
  TLS material larger than `MAX_FRAME_BYTES` (1 MiB).

### 2.2 `Server::bind*` variants

| Method | Gate | What it does | Anchors |
|---|---|---|---|
| `bind` | always | default `BindPolicy` from `allow_public_service_binds`, then `bind_with_policy` | `server.rs:109-115` |
| `bind_with_policy` | always | validates config+policy, builds Eggress TLS server config, spawns `server_loop` | `server.rs:117-134` |
| `bind_websocket` | `websocket` | same TLS build, spawns `server_loop(..., websocket=true)`; upgrade happens per-connection in `handle_connection` | `server.rs:137-157`, `780-795` |
| `bind_quic` | `quic` | binds `QuicListener` (idle 90 s, 256 streams), spawns `quic_server_loop` | `server.rs:160-166` |
| `bind_quic_with_policy` | `quic` | same with caller policy; spawns `quic_server_loop_with_admission` | `server.rs:169-212` |
| `bind_mtls` | `mtls` | builds WebPKI client-verifier config, then TLS-profile bind | `server.rs:262-271` |
| `bind_mtls_with_policy` | `mtls` | same with caller policy | `server.rs:274-281` |
| `bind_with_tls_profile` (private) | always | `TcpListener::bind`, captures `local_addr`, creates `CancellationToken` + `Counters`, spawns `server_loop` | `server.rs:283-318` |

All public binders require a caller-owned Tokio runtime
(`server.rs:121-123`, `142-144`, `175-177`, `289-291`); they never install
a global runtime or tracing subscriber (cf. `docs/SECURITY.md:41-43`).
`bind_quic_with_admission_for_test` (`server.rs:215-259`, `cfg(test)`)
additionally parameterises `max_concurrent_streams` / stream admission for
the saturation test.

`build_mtls_server_config` (`server.rs:359-389`): parses server cert/key +
client CA with Rustls’s maintained `rustls-pki-types` PEM parser, rejects empty chains and ambiguous multiple private keys, builds
`WebPkiClientVerifier` + single-cert `ServerConfig`.

`certificate_principal` (`server.rs:392-395`): `SHA-256(DER)` of the leaf,
used as the mTLS identity (see §7).

### 2.3 `Server` / `ServerHandle`

```rust
pub struct Server { cancel, task: Option<JoinHandle<()>>, handle: ServerHandle, local_addr }
pub struct ServerHandle { cancel: CancellationToken, counters: Counters }
```

- `Server::local_addr()` (`server.rs:320-322`): bound ingress address.
- `Server::handle()` (`server.rs:324-326`) → cloneable `ServerHandle`.
- `ServerHandle::snapshot()` (`server.rs:100-102`) → `Counters::snapshot()`
  (see §8).
- `ServerHandle::shutdown()` (`server.rs:103-105`) cancels the token;
  `Server::shutdown(mut self)` (`server.rs:328-333`) cancels then awaits
  the server-loop task. `Drop for Server` (`server.rs:336-340`) cancels as
  a backstop.

---

## 3. Control-plane state machine + message sequence

### 3.1 Per-session state machine (server view)

```text
                TCP accept + TLS (+WSS upgrade)
                             │
                             ▼
              read first frame (HANDSHAKE_TIMEOUT)
                ┌──────────────┴──────────────┐
                │ DataHello → accept_data_hello (data path, §4)
                └──────────────┬──────────────┘
                 ClientHello → serve_control:
                             │
              auth-gate: is_blocked(source)? ──yes──► Auth error, drop
                             │no
              version.major == CURRENT.major? ─no──► Protocol/UnsupportedVersion
                             │yes
                  ServerHello(CURRENT, caps)
                             │
                  read Auth (HANDSHAKE_TIMEOUT)
                             │
              verify_token? ──no──► record_failure + 100 ms sleep
                             │           + Error{code:4} + Auth error
                             │yes
              SessionId::generate; admit if sessions<128
                             │
                  AuthOk{session_id}
                             │
               ┌───────────── loop (IDLE_TIMEOUT 90 s) ──────────────┐
               │ RegisterService → policy+bind → run_service + Ack   │
               │ UnregisterService → cancel service + GC pending     │
               │ OpenReject{connection_id} → free pending slot       │
               │ Ping{nonce} → Pong{nonce}                           │
               │ Drain → break (graceful close)                      │
               │ unexpected → Protocol/UnexpectedMessage             │
               │ open_rx Open/Drain → forward to client              │
               │ children JoinSet completions → panic accounting     │
               │ cancel/idle expiry → break                          │
               └─────────────────────────────────────────────────────┘
                             │
              cancel services, abort children, remove_all_pending, return
```

Key code: first-frame dispatch `server.rs:802-823`;
`serve_control` auth gate + version + `ServerHello` `server.rs:891-909`;
`Auth` read + `verify_token` failure path `server.rs:910-931`;
session allocation + `MAX_SESSIONS` check `server.rs:934-956`;
`AuthOk` + split + `CONTROL_QUEUE` channel `server.rs:968-971`;
main `select!` loop `server.rs:977-1052`; teardown `server.rs:1053-1059`.

`SessionGuard` (`server.rs:1266-1296`) decrements `sessions`, removes the
session's `effective_binds`, removes the weak map entry, and cancels the
session on drop. `SessionContext::drop` (`server.rs:738-745`) subtracts any
leaked pending count so `pending` never sticks after session teardown.

### 3.2 Message-by-message sequence (happy path)

```text
client                                   server
  │── ClientHello{version,caps} ──────────►│  serve_control: version check
  │◄─ ServerHello{CURRENT,default caps} ───│  server.rs:902-909
  │── Auth{token} ──────────────────────►│  verify_token (constant-time)
  │◄─ AuthOk{session_id} ─────────────────│  server.rs:968
  │── RegisterService{id,name,bind,target}►│  policy → bind → listener
  │◄─ RegisterAck{id,effective_bind} ─────│  server.rs:1015
  │◄─ Open{service_id,connection_id} ─────│  per external accept (§4)
  │── OpenReject{connection_id,code} ────►│  only on client refusal (§4)
  │── Ping{nonce} ──────────────────────►│
  │◄─ Pong{nonce} ────────────────────────│  server.rs:1034-1037
  │◄─ Drain{deadline_ms} ─────────────────│  shutdown only, §5
  │── Drain ────────────────────────────►│  client-initiated close → break
  │── UnregisterService{id} ────────────►│  cancel + GC, §4
  │── Error{code,diagnostic} ────────────│  server→client only (auth/reg)
```

- Version: major mismatch → `UnsupportedVersion(major,minor)`
  (`server.rs:894-901`). Minor is informational.
- Auth failure: `Error{code:4, "authentication failed"}` then `Authentication`
  (`server.rs:925-931`). No session is created.
- Registration errors use `write_registration_error` (`server.rs:1122-1129`,
  always `"service registration rejected"` + numeric code):
  `1` duplicate id/name, `2` policy denial, `3` bind failure,
  `5` per-session service ceiling (`server.rs:985-1003`). Success is
  `RegisterAck{service_id, effective_bind}` (`server.rs:1015`).
- `OpenReject` flows **client→server only**. The server never emits it; it
  consumes it to free the pending slot (`server.rs:1027-1033`). Client codes
  (`1` target refused, `2` open-task exhausted) are opaque to the server.
- `Drain` flows both ways but with different meaning: server→client carries
  `SERVER_SHUTDOWN_GRACE` (1 s) during shutdown (`server.rs:466-468`,
  `569-571`); client→server `Drain` breaks the control loop immediately
  (`server.rs:1038`). Outbound `Open`/`Drain` share the same `open_rx`
  channel; a `Drain` write breaks after flushing (`server.rs:1043-1047`).

### 3.3 Effective-bind selection via `BindPolicy`

`BindPolicy` (`common.rs:83-122`): `allow_public_addresses`,
`allowed_addresses` (empty = any allowed by the master switch),
`allowed_port_ranges` (empty = any nonzero), `allow_ephemeral_ports`,
`max_services_per_session` (default 64, validated ≤ 64 in
`common.rs:98-109`).

Selection (`serve_control`, `server.rs:997-1006`):

1. `bind_to_socket(&requested_bind, &policy)` (`common.rs:325-346`):
   `Loopback{port}` → `[::1]:port` if `permits_port`; `Ip{address,port}` →
   `SocketAddrV6` iff `permits_address && permits_port`.
2. `permits_address` (`common.rs:350-353`):
   `(is_loopback || allow_public_addresses) && (allowlist empty || contains)`.
   `permits_port` (`common.rs:356-365`): port 0 gated by
   `allow_ephemeral_ports`; otherwise allowlist empty or in-range.
3. `TcpListener::bind(bind_addr)` — OS assigns the ephemeral port when 0.
4. `socket_to_effective` (`server.rs:1298-1304`) normalises V4→V6-mapped and
   builds `EffectiveBind{address:[u8;16], port}`.
5. Push `(session_id, service_id, effective)` to `counters.binds`
   (`server.rs:1006`), spawn `run_service` (`server.rs:1010`), record
   `services` + high-water (`server.rs:1013-1014`), reply `RegisterAck`.

Auth success never grants bind permission by itself
(`docs/SECURITY.md:14-17`); every registration re-evaluates the policy.
The client `TcpTarget` in the register frame is treated as bounded metadata
only (`server.rs:996` comment).

---

## 4. Data plane

### 4.1 External accept → `Open`

`run_service` (`server.rs:1136-1211`) owns one service listener:

```text
listener.accept → try_acquire connection_admission ──fail──► rejected++, ResourceExhausted
  │success → ActiveConnectionGuard (active_connections++)
  │→ ConnectionId::generate (128-bit) ──fail──► rejected++
  │→ oneshot::channel → pending.len < 128? ──no──► rejected++, ResourceExhausted
  │→ pending.insert(id, {service_id, expires: now+30 s, data_tx})
  │→ pending++ / high_water_pending
  │→ opens.try_send(Open{service_id,connection_id}) ──fail──► remove pending, rejected++
  │→ spawn relay task: select(cancel, timeout(30 s, data_rx))
  │     ├── None (timeout/cancel) → GC pending entry if still present
  │     └── Some(data stream) → pending already consumed → relay_with_options
```

- `ConnectionId` is 128-bit random (`server.rs:1157-1160`), bound to the
  current session + service, 30 s lifetime (`PENDING_LIFETIME`,
  `server.rs:50,1168`), single-use (`docs/SECURITY.md:19-23`).
- `connection_admission = Semaphore(MAX_ACTIVE_CONNECTIONS_PER_SESSION)`
  per session (`server.rs:944`, `1151`). The permit is moved into the relay
  task via `ActiveConnectionGuard` (`server.rs:1156,1181`), so `active_connections`
  covers pending-wait + relay.
- `opens: mpsc::Sender<Message>` is the session's `CONTROL_QUEUE` (128)
  channel (`server.rs:970`). `try_send` failure means a slow/dead control
  writer; the pending entry is removed immediately (`server.rs:1172-1176`).
- Relay uses `RelayOptions::bounded(16 KiB, RELAY_DRAIN=15 s)`
  (`server.rs:1190`); both `Ok(report)` and `Err(failure)` byte counts are
  added to `bytes_upstream/downstream` (`server.rs:1191-1198`).
- Service teardown (`UnregisterService`, session end, `run_service` exit)
  calls `remove_service_pending` / `remove_all_pending`
  (`server.rs:1242-1264`), which `retain`/`clear` and subtract the exact
  removed count. `SessionContext::drop` is the final backstop.

### 4.2 Client `DataHello` → `DataHello` validation → relay

Data connections arrive on the **same ingress port** as control. `handle_connection`
peeks the first frame after TLS (+WSS upgrade): `DataHello` → `accept_data_hello`,
`ClientHello` → `serve_control`, anything else → `Protocol/UnexpectedMessage`
(`server.rs:802-823`). Data paths drop both the handshake guard and the
`MAX_HANDSHAKES` admission permit immediately (`server.rs:807-808`); control
paths hold them until auth completes (`server.rs:919-933`).

`accept_data_hello` (`server.rs:825-870`):

| Check (in order) | On failure | Counter | Rationale |
|---|---|---|---|
| session map lookup `hello.session_id` → `Weak::upgrade` | `Authentication` | `rejected++` | unknown/stale session, incl. post-restart generations |
| `session.principal != hello principal` | `Authentication` | `rejected++` | mTLS binding (§7); `None != Some` also rejects |
| `pending.remove(hello.connection_id)` | `Authorization` | `rejected++` | unknown/replayed id (already consumed) |
| `pending.service_id != hello.service_id` or `expires <= now` | `Authorization` | `rejected++` | wrong-service or stale; entry is **consumed** |
| else `data_tx.send(stream)` | `Cancelled` if receiver gone | — | handoff to the `run_service` relay task |

Notes for reviewers:

- Wrong-session misses leave the pending entry alive (unit-tested,
  `server.rs:3439-3497`); wrong-service/expired **consume** it
  (`server.rs:3499-3565`). This is deliberate: the id is single-use once the
  session is proven, preventing probe-reuse of a live slot.
- Bearer-token comparison is constant-time via `subtle`
  (`common.rs:319-322`, used `server.rs:917`). `ConnectionId` equality is
  the proto type's constant-time eq (see `proto-wire-protocol.md`); session
  lookup is a hash hit, not a secret compare — the secrecy sits in the
  128-bit id + 30 s window.
- `DataHello` is the final Eggtunnel message on a data stream; subsequent
  bytes are opaque relay (`docs/SECURITY.md:23`).
- QUIC data streams use the same validator: `handle_quic_data_stream`
  (`server.rs:669-684`) reads one `DataHello` then calls `accept_data_hello`
  with `principal: None` (QUIC has no mTLS — §7).

### 4.3 `OpenReject` codes

The server is a pure consumer: `OpenReject{connection_id}` removes the
pending entry and decrements `pending` (`server.rs:1027-1033`). Codes are
not inspected. For the producer side (client `1` = target refused,
`2` = open-task ceiling) see `client.md`; the
`refused_target_rejects_external_connection_and_releases_pending_capacity`
test (`server.rs:3303-3368`) proves the end-to-end effect (external sees EOF,
`pending` returns to 0).

---

## 5. Concurrency

### 5.1 Accept loops

**TCP** `server_loop` (`server.rs:397-477`):

- Shared: `sessions` weak map, `admission = Semaphore(MAX_HANDSHAKES)`,
  `auth_failures` limiter, `handlers: JoinSet`.
- `select!`: `cancel` → break; `listener.accept()` → `try_acquire_owned`
  admission (fail → `rejected++`, `ResourceExhausted`, keep looping);
  else spawn `handle_connection` with `child_token`, `HandshakeGuard`,
  `ConnectionContext`; `handlers.join_next()` → `record_join_result`
  (panic accounting).
- No `.await` holds the sessions lock across I/O; the map is only locked for
  insert/retain/upgrade.

**QUIC** `quic_server_loop[_with_admission]` (`server.rs:480-580`): same
shape over `listener.accept_connection(&cancel)`. `Ok(None)` breaks (listener
closed); `Err` → `rejected++` and continues; admission-full closes the QUIC
connection with `"handshake limit reached"` (`server.rs:528-533`).

**Per-QUIC-connection** `handle_quic_connection` (`server.rs:583-666`):
accepts the first bidi stream as control (`HANDSHAKE_TIMEOUT`), spawns
`serve_control` on it, then loops `accept_stream` for data. Data streams are
gated by a per-connection `stream_admission` semaphore
(`max_active_data_streams`, default 128) via `try_acquire_owned`
(`server.rs:602-606`, `631-635`); each spawns `handle_quic_data_stream` in a
`streams: JoinSet`. Control completion, cancel, or error breaks the loop,
then: cancel, `connection.close("session ended")`, `abort_all` streams,
abort a still-running control task, await it (`server.rs:657-665`).

### 5.2 Per-session tasks

`serve_control` splits the control stream (`tokio::io::split`,
`server.rs:969`), creates `(open_tx, open_rx) = mpsc::channel(CONTROL_QUEUE)`
(`server.rs:970`), stores a clone in `control_tx` for shutdown `Drain`
(`server.rs:971`), and multiplexes in one `select!`:

- control reader (`read_message`), `open_rx` forwarder, `children.join_next`
  (`run_service` tasks), `context.cancel`, and a pinned `IDLE_TIMEOUT` sleep
  reset on `Register`/`Unregister`/`OpenReject`/`Ping` (`server.rs:984,1018,1028,1035`).
- Each registered service spawns `run_service` in `children: JoinSet`
  (`server.rs:1010`); each external accept spawns a relay task in
  `run_service`'s `relays: JoinSet` (`server.rs:1180-1201`).

### 5.3 Semaphores and guards

| Primitive | Ceiling | Scope | Where |
|---|---|---|---|
| `admission` | `MAX_HANDSHAKES = 64` | whole server, unauthenticated conns | `server.rs:408`, `509`, `420`, `528` |
| `HandshakeGuard` | counts `handshakes` + high-water | per accepted conn until auth/data-hello | `server.rs:705-726`, `431`, `535`, `644`, `807`, `919`, `932` |
| `sessions` map + `SessionGuard` | `MAX_SESSIONS = 128` | whole server | `server.rs:948-967`, `1266-1296` |
| `connection_admission` | `MAX_ACTIVE_CONNECTIONS_PER_SESSION = 128` | per session (pending-wait + relay) | `server.rs:944`, `1151-1156` |
| `pending` map | `MAX_PENDING_PER_SESSION = 128` | per session | `server.rs:1163-1171` |
| `ActiveConnectionGuard` | counts `active_connections` + high-water | per relay task (holds permit) | `server.rs:1213-1240` |
| `stream_admission` (QUIC) | 128 (or test override) | per QUIC connection | `server.rs:602-606`, `631-635` |
| `open_tx/open_rx` | `CONTROL_QUEUE = 128` | per session control→writer | `server.rs:970-971`, `1172` |

`SessionGuard::drop` also reconciles `services` from the binds list length
delta (`server.rs:1277-1291`) — a deliberate single-source-of-truth choice
(`binds` is authoritative for service count on session exit).

### 5.4 Shutdown with `SERVER_SHUTDOWN_GRACE = 1 s`

TCP (`server.rs:458-476`) and QUIC (`server.rs:561-579`) share the sequence:

1. Break accept loop on `cancel`.
2. Upgrade weak sessions → `active`; `try_send(Drain{deadline_ms:1000})`
   to each `control_tx` (bounded, so never blocks shutdown).
3. `sleep(SERVER_SHUTDOWN_GRACE)` — gives clients 1 s to observe `Drain`.
4. Cancel every session token (cascades via `child_token` to services/relays).
5. `handlers.abort_all()` + drain `join_next` (records panics).

`serve_control` teardown (`server.rs:1053-1059`) cancels services,
`abort_all`s children, drains completions, then `remove_all_pending`.
`run_service` exit (`server.rs:1208-1210`) aborts relays, drains them, then
`remove_service_pending`. `server_shutdown_cancels_incomplete_tls_and_authentication_handshakes`
(`server.rs:3685-3737`) proves `active_handshakes` returns to 0 and no
session is left behind.

### 5.5 `JoinError` panic accounting

`Counters::record_join_result` (`common.rs:265-270`): `is_panic` →
`task_panics++`, `last_termination = Internal`. Call sites:
`server_loop` (`server.rs:453-455`), QUIC loops (`server.rs:555-557`),
`serve_control` children (`server.rs:1048-1050`), `run_service` relays
(`server.rs:1203-1205`), QUIC streams (`server.rs:652-654`). The QUIC
control task additionally distinguishes panic vs cancel explicitly
(`server.rs:617-628`). Covered by
`owned_task_panic_is_counted_as_internal_termination`
(`server.rs:3672-3682`).

---

## 6. Hardening: timeouts, throttle, resource-exhausted paths

### 6.1 Constants

**Admission ceilings** (`server.rs:39-47`):

| Constant | Value | Enforced at |
|---|---|---|
| `MAX_SESSIONS` | 128 | `serve_control` insert, `server.rs:951` |
| `MAX_PENDING_PER_SESSION` | 128 | `run_service` insert, `server.rs:1163` |
| `MAX_ACTIVE_CONNECTIONS_PER_SESSION` | 128 | `connection_admission` + QUIC `stream_admission` |
| `CONTROL_QUEUE` | 128 | per-session `open_tx/open_rx`, `server.rs:970` |
| `MAX_HANDSHAKES` | 64 | accept-loop `admission`, `server.rs:420,528` |
| `AUTH_FAILURES_PER_SOURCE` | 10 | `AuthFailureLimiter`, `server.rs:409-413` |
| `AUTH_FAILURE_WINDOW` | 60 s | sliding window prune, `server.rs:1109-1119` |
| `MAX_AUTH_SOURCES` | 1024 | bounded table, `server.rs:412` |
| `AUTH_FAILURE_DELAY` | 100 ms | after each failed token check, `server.rs:921` |

**Timeouts** (`server.rs:48-52`):

| Constant | Value | Covers |
|---|---|---|
| `HANDSHAKE_TIMEOUT` | 10 s | TLS accept, WSS upgrade, first-frame read, `Auth` read, QUIC control/data first frames (`server.rs:760,769,786,802,810,591,593,675`) |
| `IDLE_TIMEOUT` | 90 s | whole control session; reset on Register/Unregister/OpenReject/Ping (`server.rs:975-976,984,1018,1028,1035`) |
| `PENDING_LIFETIME` | 30 s | pending entry expiry + `data_rx` wait (`server.rs:1168,1184`) |
| `RELAY_DRAIN` | 15 s | `relay_with_options` bounded drain (`server.rs:1190`) |
| `SERVER_SHUTDOWN_GRACE` | 1 s | `Drain` → cancel gap (`server.rs:471,574`) |

`validate_config` additionally caps TLS PEMs at `MAX_FRAME_BYTES`
(`server.rs:348-354`).

### 6.2 Per-source auth throttle

`AuthFailureLimiter` (`server.rs:1064-1120`): `Mutex<HashMap<IpAddr,
VecDeque<Instant>>>` + `threshold/window/max_sources`. `prune` evicts
entries older than the window and drops empty deques, so the table cannot
grow unboundedly and never delays successful auth (doc comment
`server.rs:1062-1063`).

- `is_blocked(source)` (`server.rs:1081-1094`): prune, then: unknown source
  + table full → **blocked**; known source with `len >= 10` → blocked.
- `record_failure(source)` (`server.rs:1096-1107`): prune, then: unknown +
  full → drop (no insert); else push timestamp.
- Enforcement (`server.rs:891-892`, `917-931`): pre-`ServerHello` blocked
  check (no failure recorded, just `Authentication`); post-check failure
  records, **drops both guards first** (frees handshake capacity before the
  sleep), sleeps 100 ms, `rejected++`, sends `Error{code:4}`, returns
  `Authentication`.

Properties (`docs/SECURITY.md:25-30`, `docs/OPERATIONS.md:27-28`):
process-local, per-source-IP, 10 fails / 60 s, ≤1024 sources, 100 ms delay,
unknown sources rejected when full. Unit-tested at
`server.rs:3784-3805` (per-source isolation, full-table rejection of a second
source, window expiry prunes to an empty table).

### 6.3 Resource-exhausted paths (all bounded, all counted)

| Trigger | Response | Accounting |
|---|---|---|
| `admission.try_acquire` fails (TCP) | skip accept | `rejected++`, `ResourceExhausted` (`server.rs:420-424`) |
| `admission.try_acquire` fails (QUIC) | close QUIC conn `"handshake limit reached"` | same (`server.rs:528-533`) |
| `sessions.len() >= 128` | reject session after auth | `ResourceExhausted` termination, `Authorization` error (`server.rs:951-954`); stale weaks pruned first (`server.rs:950`) |
| `pending.len() >= 128` | drop external accept | `rejected++`, `ResourceExhausted` (`server.rs:1163-1167`) |
| `connection_admission.try_acquire` fails | drop external accept | same (`server.rs:1151-1155`) |
| QUIC `stream_admission.try_acquire` fails | skip stream (no `DataHello` read) | same (`server.rs:631-635`) |
| `opens.try_send` fails (control queue full) | remove just-created pending | `pending--`, `rejected++` (`server.rs:1172-1176`) |
| `services.len() >= max_services_per_session` | `Error{code:5}` | `rejected++`, `ResourceExhausted` (`server.rs:985-990`) |
| `ConnectionId::generate` fails (no entropy) | drop external accept | `rejected++` (`server.rs:1157-1160`) |

`rejected_connections` is the single funnel for all of the above plus every
`accept_data_hello` rejection and every registration denial — check it first
when triaging `last_termination = ResourceExhausted`.

---

## 7. mTLS profile: bearer-still-required, leaf SHA-256 principal

Feature `mtls` (`docs/SECURITY.md:32-39`):

- Trust roots are explicit server input (`trusted_client_ca_pem`,
  `server.rs:262-281`); client still validates server name + system/configured
  roots. No enrollment/revocation service.
- **Bearer token is still required.** mTLS adds identity; it does not replace
  `Auth`. `mtls_requires_trusted_client_certificate_and_keeps_server_name_validation`
  (`server.rs:2704-2832`) proves trusted-cert + correct token registers while
  rogue-CA, wrong-SNI, and cert-less clients never register.
- Principal = `SHA-256(leaf DER)` (`server.rs:392-395`), extracted from the
  first peer certificate in `handle_connection` (`server.rs:771-777`) and
  stored in `ConnectionContext.principal` → `SessionContext.principal`
  (`server.rs:801`, `939-942`).
- Enforcement is on **both** planes: `accept_data_hello` rejects
  `session.principal != data principal` as `Authentication`
  (`server.rs:843-848`). `None != Some([..])` also rejects, so a cert-less
  data dial cannot attach to a pinned session and vice versa.
  `mtls_principal_mismatch_cannot_attach_data_stream`
  (`server.rs:3569-3619`) proves the pending entry survives a principal
  mismatch (no consumption, no decrement).
- QUIC has no mTLS: `handle_quic_connection` / `handle_quic_data_stream`
  hardcode `principal: None` (`server.rs:542`, `642`, `669-684`), and the
  client rejects QUIC+mTLS combinations (see `client.md`).

Reviewer note: only the **first** peer certificate is pinned; intermediates
are verified by WebPKI but do not participate in the principal. Rotation =
new leaf → new principal → new session (old pendings are unusable).

---

## 8. Observability: snapshot fields the server updates

`Snapshot` / `Counters` live in `common.rs:135-157`, `202-256`.
The server updates every field except `connected`/`reconnects`/`open_tasks`
(client-side concerns):

| Snapshot field | Server writer |
|---|---|
| `active_sessions` / `high_water_sessions` | `fetch_add` on admit (`server.rs:957-963`), `fetch_sub` in `SessionGuard::drop` (`server.rs:1273-1276`) |
| `registered_services` / `high_water_services` | `fetch_add` on `RegisterAck` (`server.rs:1013-1014`), `fetch_sub` on `Unregister` (`server.rs:1023`) and session-exit reconcile (`server.rs:1288-1291`) |
| `pending_connections` / `high_water_pending` | `fetch_add` on insert (`server.rs:1170-1171`), `fetch_sub` on consume (`server.rs:857-859`), timeout GC (`server.rs:1186-1188`), `remove_service/all_pending` (`server.rs:1242-1264`), `SessionContext::drop` (`server.rs:738-745`); unit-pinned in `server.rs:3439-3565` |
| `active_connections` / `high_water_active` | `ActiveConnectionGuard::new/drop` (`server.rs:1218-1240`) |
| `active_handshakes` / `high_water_handshakes` | `HandshakeGuard::new/drop` (`server.rs:705-726`) |
| `task_panics`, `last_termination` | `record_join_result` on every `JoinSet` drain (§5.5); `record_termination` on handler errors, admission-full, session-full, service-ceiling |
| `rejected_connections` | every auth/reg/pending/data-hello/admission denial (§6.3) |
| `bytes_upstream/downstream` | relay reports incl. failure partials (`server.rs:1191-1198`) |
| `effective_binds: Vec<(SessionId, ServiceId, EffectiveBind)>` | push on register (`server.rs:1006`), retain on unregister (`server.rs:1022`), retain on session exit (`server.rs:1285`); CLI polls it every 250 ms |
| `resource_limits` | `ResourceLimits::default()` (128/64/128/128/64/128/128, `common.rs:186-198`) — documents ceilings, not live config |

`last_termination` retains only the most recent category, never history or
error text (`docs/OPERATIONS.md:34-35`). `Snapshot` never carries secrets;
proxy credentials and tokens are redacted from diagnostics
(`docs/SECURITY.md:76-78`).

---

## 9. Test inventory (`server.rs:1307-4513`, `#[cfg(all(test, feature="client"))]`)

Helpers: `test_session` (`server.rs:1321-1344`) builds an isolated
`SessionContext` + weak map + counters; `test_data_stream`
(`server.rs:1346-1349`) is a 32-byte duplex; `certificate` / `mtls_certificates`
(`server.rs:1351-1420`) mint rcgen fixtures; `roundtrip` (`server.rs:1422-1432`)
writes + shutdown + `read_to_end` against an effective bind.

12 most review-relevant groups (outbound-proxy auth/matrix tests omitted —
see `client.md`):

1. `tcp_tls_reverse_session_registers_and_relays_data` (`server.rs:2835-2965`)
   — canonical end-to-end: 2 services register (2 `effective_binds`), parallel
   echo roundtrips, byte counters > 0 on both ends, `resource_limits.sessions
   == 128`, high-waters for services/active/pending/handshakes, `Unregister`
   shrinks binds to 1, client shutdown drains the active relay to 0/0.
2. `bad_token_does_not_create_a_registered_session` (`server.rs:3167-3209`)
   — wrong bearer → `rejected++`, `last_termination == Authentication`, 0
   sessions, client never reconnects (auth is non-retryable).
3. `public_service_bind_is_denied_without_explicit_policy`
   (`server.rs:3254-3300`) — `RequestedBind::Ip{2001:db8::1, 9000}` with
   default policy → `rejected++`, 0 services, empty `effective_binds`.
4. `data_hello_is_session_service_bound_and_single_use`
   (`server.rs:3439-3497`) — unit: wrong `SessionId` → `Authentication` and
   pending survives; correct triple consumes via `data_tx`; replay of the
   same triple → `Authorization` (single-use proven without sockets).
5. `wrong_service_and_expired_data_hellos_consume_and_reject_pending_state`
   (`server.rs:3500-3565`) — unit: wrong `ServiceId` and expired entries are
   consumed (map + counter return to 0), closing the reuse window.
6. `mtls_requires_trusted_client_certificate_and_keeps_server_name_validation`
   (`server.rs:2704-2832`, `mtls`) + `mtls_principal_mismatch_cannot_attach_data_stream`
   (`server.rs:3569-3619`, `mtls`) — trusted leaf registers; rogue CA, wrong
   SNI, and missing cert never register; mismatched data principal is
   rejected and the pending slot survives.
7. `unauthenticated_handshake_admission_caps_at_limit_and_recovers`
   (`server.rs:3740-3781`) — 65 bare TCP connects → `active_handshakes ==
   64` + `rejected > 0`; dropping peers returns handshakes to 0 (no leak).
8. `auth_failure_limiter_is_per_source_bounded_and_expires`
   (`server.rs:3784-3805`) — pure unit: threshold blocks per-source, full
   table (max 1) rejects an unknown source, window expiry prunes to empty.
9. `bind_policy_enforces_address_port_and_ephemeral_rules`
   (`server.rs:3808-3860`) — unit over `bind_to_socket`: allowlisted
   addr+port ok; ephemeral-off, out-of-range port, and non-allowlisted addr
   all err.
10. `server_shutdown_cancels_incomplete_tls_and_authentication_handshakes`
    (`server.rs:3685-3737`) — bare TCP + post-`ServerHello` control peer
    hold 2 handshakes; `shutdown()` returns both handshake and session counts
    to 0.
11. `repeated_client_server_start_stop_returns_runtime_counts_to_zero`
    (`server.rs:3622-3669`) — 3 bind/start/shutdown cycles leave sessions,
    services, pending, active, handshakes all at 0 (no cross-cycle leak).
12. QUIC correlation quartet (`quic`): `quic_wrong_session_data_hello_is_rejected_and_pending_entry_survives`
    (`server.rs:3906-4021`), `quic_replay_data_hello_on_second_stream_is_rejected`
    (`server.rs:4024-4145`), `quic_stale_old_generation_data_hello_is_rejected_after_reconnect`
    (`server.rs:4149-4256`), `quic_stream_saturation_recovers_capacity_and_keeps_unrelated_streams_alive`
    (`server.rs:4260-4403`) — wrong-session keeps pending + `rejected++`;
    consumed-id replay → `Authorization`; post-restart old `SessionId` →
    `Authentication` on the new server; stream-admission ceiling rejects
    extras and recovers on drop. Plus `quic_half_close_preserves_response_after_request_eof`
    (`server.rs:4435-4512`) for relay EOF semantics.

Also notable: `refused_target_rejects_external_connection_and_releases_pending_capacity`
(`server.rs:3303-3368`, `OpenReject` path), `client_reconnects_and_restores_services_in_a_new_session_generation`
(`server.rs:3371-3436`, new `SessionId` after server restart),
`owned_task_panic_is_counted_as_internal_termination`
(`server.rs:3672-3682`, panic accounting), and the WSS pair
(`server.rs:1517-1572`, `1576-1653`) for upgrade + peer-close-during-relay.

---

## 10. Review checklist

| # | Risk | Where to look | What good looks like / probe |
|---|---|---|---|
| 1 | **Listener hijack** — client requests a public/ephemeral bind it should not get | `server.rs:997-1006`, `common.rs:325-365`, `server.rs:3808-3860` | `bind_to_socket` runs **before** `TcpListener::bind`; default denies non-loopback; allowlist + port-range + ephemeral bits all enforced. Probe: register `Ip{0.0.0.0,80}` and ephemeral-0 under a restrictive policy; expect codes 2/3, no listener. |
| 2 | **Session confusion** — data hello attaches to the wrong session/service | `server.rs:825-870`, tests `server.rs:3439-3565,3906-4256` | triple `(session, service, connection)` checked in order; wrong-session → `Authentication` without consuming; wrong-service/expired → `Authorization` with consumption. Probe: cross-wire two concurrent sessions' ids; exactly one `rejected++`, no relay. |
| 3 | **Pending exhaustion / GC** — attacker or churn fills 128 slots | `server.rs:1163-1176,1184-1188,1242-1264,738-745` | insert capped; `try_send` failure rolls back; relay timeout GCs; unregister/session-exit/`drop` reconcile counters. Probe: 128 hung externals + 1 more → `ResourceExhausted`; cancel session → `pending == 0`. Watch for double-`fetch_sub` (consume at `857` vs timeout-GC at `1186` is guarded by `remove` return). |
| 4 | **Throttle bypass behind NAT** — shared source IP, table-full behavior | `server.rs:891-931,1064-1120`, `docs/SECURITY.md:25-30` | throttle is per-source-IP, process-local; 10/60 s, 1024 sources, 100 ms delay, full-table rejects unknowns. Behind shared NAT one bad actor blocks the whole egress IP until the window expires — documented, not fixed. Probe: 10 fails from one IP blocks an innocent second client on the same IP; confirm ops runbook accounts for it. |
| 5 | **Shutdown races** — `Drain` lost, relays dangling, counters stuck | `server.rs:458-476,561-579,1053-1059,1208-1210`, tests `server.rs:3622-3737` | `Drain{1000ms}` via `try_send`, 1 s sleep, session-cancel cascade, `abort_all` + drain; `serve_control`/`run_service` clear pendings; start/stop cycles return all counts to 0. Probe: kill server mid-relay with an active external; client must see `Drain`, external must see EOF, snapshot must settle to 0/0. |
| 6 | **Handshake pile-up** — pre-auth work before admission | `server.rs:420,528,705-726,760,769` | TCP+QUIC both `try_acquire` **before** spawning; `HandshakeGuard` counts until auth/data-hello; full → `rejected++` without reading. Residual: QUIC/TLS adapter work below Eggtunnel (Eggress caps 1024 conns / 4096 streams, `docs/SECURITY.md:49-62`). Probe: SYN/TLS flood → handshakes pinned at 64, sessions stay 0. |
| 7 | **mTLS downgrade** — bearer-only data dial on a pinned session | `server.rs:765-778,801,843-848,939-942` | principal pinned at session creation and rechecked on every `DataHello`; `None != Some` rejects. Probe: control with cert + data without → `Authentication`, pending survives. Confirm QUIC deployments do not expect mTLS (hard `None`). |
| 8 | **Control-queue backpressure** — slow client stalls `Open` delivery | `server.rs:970,1043-1047,1172-1176` | `open_tx` is bounded 128 with `try_send`; full → pending rolled back + `rejected++` rather than blocking the accept loop. Probe: register then stall control reads, burst 129 externals; last `Open` must fail open, not deadlock. |
| 9 | **Idle vs relay liveness** — 90 s idle killing active relays | `server.rs:975-976,984-1037` | idle timer is only reset by control frames and only breaks the **control** loop; relays live in `children`/`relays` JoinSets and are torn down explicitly. Long relays with no control traffic still hit the 90 s control timeout by design — confirm this matches ops expectations for quiet services. |
| 10 | **Panic visibility** — child task panic silently dropping a service | `common.rs:265-270`, `server.rs:453,555,1048,1203,652` | every `join_next` records `is_panic → task_panics++, Internal`. Probe: inject a relay panic; `task_panics == 1`, `last_termination == Internal`, sibling services unaffected. |

---

*End of server deep dive. Next: `transports-wire-io.md` for the framing/relay
primitives this module calls into, or `client.md` for the other half of the
`Open`/`DataHello` handshake.*

### Runtime policy and composition (M008)

`ServerBuilder` composes TCP/TLS, QUIC, WebSocket, optional trusted client CA,
`BindPolicy`, and `RuntimePolicy` without exposing adapter-specific types.
`validate()` rejects mTLS on unsupported transports before binding. The
legacy `Server::bind_*` helpers delegate through the builder. Session,
handshake, service, pending, active-connection, queue, and timeout behavior
comes from the immutable finite policy held by `Counters`; `BindPolicy` can
further restrict service binds.
