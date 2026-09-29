# Reverse-session server — `crates/eggtunnel/src/server.rs` + `server/`

> Runtime is the `server.rs` coordinator (bind orchestration, `Server`/`ServerHandle`)
> plus private responsibility modules `server/{config,tls,accept,auth,session,control,pending,service}.rs`;
> transport and lifecycle tests live in
> `server_tests.rs` (harness + `ServerBuilder::validate` tests) plus
> `server_tests/{tcp,mtls,quic,websocket,proxy}.rs`.
> All `server/*.rs:NNN` anchors below track the current module layout;
> test anchors use `server_tests.rs` / `server_tests/<file>.rs` paths.
>
> M008 note: `ServerBuilder` (`server/config.rs:61-130`) is the canonical surface
> (`ServerTransportProfile` + `BindPolicy` + `RuntimePolicy` + optional
> client CA); legacy `Server::bind*` helpers delegate through it. Finite
> ceilings/timeouts come from validated `RuntimePolicy`
> (`common.rs:196-316`); `MAX_SESSIONS`/`MAX_HANDSHAKES`
> (`server_tests.rs:35-37`) are `cfg(test)`-only aliases.
> See [Architecture Overview](overview.md) §4 for the birds-eye map and
> component index. Companion dives: `common-core.md` (shared vocabulary),
> `client.md`, `proto-wire-protocol.md`, `transports-wire-io.md`.

Sources: `crates/eggtunnel/src/server.rs`, `crates/eggtunnel/src/server/`,
`crates/eggtunnel/src/common.rs`
(`BindPolicy` / `verify_token` / `bind_to_socket`), `docs/SECURITY.md`
(server sections), `docs/OPERATIONS.md`.

All anchors are `file:line` in the workspace root. Line numbers below track
the current implementation (coordinator `server.rs` plus the private `server/`
modules; transport and lifecycle tests are in focused `server_tests/` modules).

---

## 1. Role: reachable rendezvous + ingress

The server is the only reachable party. It owns:

- **Listeners**: one control+data ingress socket (`listen_addr`; TCP+TLS,
  or UDP for QUIC) plus one server-owned `TcpListener` per registered
  service (bound in `register_service`, `server/control.rs:235-343`,
  invoked from the registration arm at `server/control.rs:164-177`).
- **Sessions**: exactly one authenticated control stream per session
  (`serve_control`, `server/control.rs:67-234`). Session table is
  `Arc<Mutex<HashMap<SessionId, Weak<SessionContext>>>>`
  (`SessionRegistry`, `server/session.rs:24-33`).
- **Pending correlation**: single-use `ConnectionId → PendingEntry`
  (`server/pending.rs:18-22`) per session (`SessionContext.pending`,
  `server/session.rs:35-44`). External accept inserts; client data-dial consumes.
- **Data accept + relay**: accepts both `ClientHello` (control) and
  `DataHello` (data) on the same ingress port, validates the latter
  against session/service/connection binding, then relays opaque bytes
  via `eggress-relay` (`server/accept.rs:249-275`, `server/pending.rs:30-97`,
  `server/service.rs:121-136`).

What the server explicitly does **not** do (cf. `docs/SECURITY.md:9-12`):

- receives the client-local `TcpTarget` as non-authoritative bounded metadata but never trusts it as authority; only the client uses
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

Defined `server/config.rs:20-28`:

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
- `allow_public_service_binds` is the coarse master switch. `ServerBuilder::new`
  maps it to `BindPolicy { allow_public_addresses, ..default() }`
  (`server/config.rs:71-84`). Finer policy uses `bind_policy(...)` /
  `bind_with_policy` / `bind_mtls_with_policy` / `bind_quic_with_policy`.
- `Drop` zeroizes `private_key_pem` (`server/config.rs:29-34`); `Debug` redacts
  cert/key/token (`server/config.rs:36-50`). `SecretToken` itself redacts and
  zeroizes on drop (`common.rs:38-48`). mTLS client keys get the same
  treatment (`docs/SECURITY.md:38-39`).
- `validate_config` (`server/config.rs:132-146`) rejects empty cert/key and
  TLS material larger than `MAX_FRAME_BYTES` (1 MiB).

### 2.2 `ServerBuilder` + `Server::bind*` delegates

`ServerTransportProfile` (`server/config.rs:51-58`): `TcpTls`, plus
`Quic` (`quic`) and `WebSocket` (`websocket`).

`ServerBuilder` (`server/config.rs:61-130`): `new` (`server/config.rs:71-84`),
`bind_policy` (`server/config.rs:86-89`), `transport` (`server/config.rs:91-94`),
`runtime_policy` (`server/config.rs:96-100`), `client_ca_pem` (`mtls`,
`server/config.rs:102-105`), `validate` (`server/config.rs:107-116`), `bind`
(`server/config.rs:118-130`).

| Method | Gate | What it does | Anchors |
|---|---|---|---|
| `bind` | always | `ServerBuilder::new(config).bind()` | `server.rs:64-66` |
| `bind_with_policy` | always | builder + `bind_policy(policy)` + `bind()` | `server.rs:68-76` |
| `bind_websocket` | `websocket` | builder + `transport(WebSocket)` + `bind()`; upgrade happens per-connection in `handle_connection` | `server.rs:79-84`, `server/accept.rs:336-357` |
| `bind_quic` | `quic` | builder + `transport(Quic)` + `bind()` | `server.rs:87-93` |
| `bind_quic_with_policy` | `quic` | builder + policy + `transport(Quic)` + `bind()` | `server.rs:95-104` |
| `bind_mtls` | `mtls` | builder + `client_ca_pem` + `bind()` | `server.rs:152-166` |
| `bind_mtls_with_policy` | `mtls` | builder + policy + `client_ca_pem` + `bind()` | `server.rs:168-178` |
| `bind_profile` (private) | always | dispatches TCP-TLS / WebSocket / QUIC; mTLS CA selects `Mutual` TLS | `server.rs:180-213` |
| `bind_quic_profile` (private) | `quic` | binds `QuicListener` (idle from `RuntimePolicy`, streams = active-per-session + 1), spawns `quic_server_loop` | `server.rs:216-257` |
| `bind_with_tls_profile` (private) | always | requires caller runtime, validates config+policy, `TcpListener::bind`, captures `local_addr`, creates `CancellationToken` + policy `Counters`, spawns `server_loop` | `server.rs:260-294` |

All public binders require a caller-owned Tokio runtime (checked in
`bind_with_tls_profile`, `server.rs:260-263`, and in
`bind_quic_with_admission_for_test`, `server.rs:107-112`); they never install
a global runtime or tracing subscriber (cf. `docs/SECURITY.md:41-43`).
`bind_quic_with_admission_for_test` (`server.rs:107-149`, `cfg(test)`)
additionally parameterises `max_concurrent_streams` / stream admission for
the saturation test.

`build_server_tls` (`server/tls.rs:23-35`): Eggress `TlsServerConfigBuilder`
from the configured cert/key PEMs.

`build_mtls_server_config` (`mtls`, `server/tls.rs:36-62`): parses server
cert/key + client CA via `crate::pem`, builds an empty-roots
`RootCertStore` + `WebPkiClientVerifier` + single-cert `ServerConfig`.

`certificate_principal` (`mtls`, `server/tls.rs:64-67`): `SHA-256(DER)` of the
leaf, used as the mTLS identity (see §7).

### 2.3 `Server` / `ServerHandle`

```rust
pub struct Server { cancel, task: Option<JoinHandle<()>>, handle: ServerHandle, local_addr }
pub struct ServerHandle { cancel: CancellationToken, counters: Counters }
```

- `Server::local_addr()` (`server.rs:296-298`): bound ingress address.
- `Server::handle()` (`server.rs:300-302`) → cloneable `ServerHandle`.
- `ServerHandle::snapshot()` (`server.rs:54-56`) → `Counters::snapshot()`
  (see §8).
- `ServerHandle::shutdown()` (`server.rs:58-60`) cancels the token;
  `Server::shutdown(mut self)` (`server.rs:304-310`) cancels then awaits
  the server-loop task. `Drop for Server` (`server.rs:313-318`) cancels as
  a backstop.

`validate_server_profile` (`server/config.rs:148-173`): validates
`RuntimePolicy` + `BindPolicy` + `validate_config`, rejects empty client CA,
and rejects mTLS (`trusted_client_ca.is_some()`) on any non-TCP profile
(`server/config.rs:165-169`) — the mTLS-only-TCP gate.

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
                  ServerHello(CURRENT, intersected caps)
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

Key code: first-frame dispatch `server/accept.rs:248-265`;
`serve_control` auth gate + version + capability intersection + `ServerHello` `server/control.rs:83-101`;
`Auth` read + `verify_token` failure path `server/control.rs:103-125`;
session allocation + `policy.limits.sessions` check `server/control.rs:119-144`;
`AuthOk` + split + policy-sized control channel `server/control.rs:143-146`;
main `select!` loop `server/control.rs:154-237`; teardown `server/control.rs:215-225`.

`SessionGuard` (`server/session.rs:85-115`) cancels the session, decrements
`sessions`, removes the session's `effective_binds`, reconciles `services`
from the binds delta, and removes the weak map entry on drop.
`SessionContext::drop` (`server/session.rs:45-52`) subtracts any leaked pending
count so `pending` never sticks after session teardown.

### 3.2 Message-by-message sequence (happy path)

```text
client                                   server
  │── ClientHello{version,caps} ──────────►│  serve_control: version check
  │◄─ ServerHello{CURRENT,intersected caps} │  server/control.rs:94-101
  │── Auth{token} ──────────────────────►│  verify_token (constant-time)
  │◄─ AuthOk{session_id} ─────────────────│  server/control.rs:143
  │── RegisterService{id,name,bind,target}►│  policy → bind → listener
  │◄─ RegisterAck{id,effective_bind} ─────│  server/control.rs:191
  │◄─ Open{service_id,connection_id} ─────│  per external accept (§4)
  │── OpenReject{connection_id,code} ────►│  only on client refusal (§4)
  │── Ping{nonce} ──────────────────────►│
  │◄─ Pong{nonce} ────────────────────────│  server/control.rs:196-199
  │◄─ Drain{deadline_ms} ─────────────────│  shutdown only, §5
  │── Drain{deadline_ms} ───────────────►│  capture deadline → break → bounded drain wait (cap 2)
  │── UnregisterService{id} ────────────►│  cancel + GC, §4
  │── Error{code,diagnostic} ────────────│  server→client only (auth/reg)
```

- Version: major mismatch → `UnsupportedVersion(major,minor)`
  (`server/control.rs:86-93`). Minor is informational.
- Auth failure: `Error{code:4, "authentication failed"}` then `Authentication`
  (`server/control.rs:110-116`). No session is created.
- Registration errors use `write_registration_response` (`server/control.rs:378-399`,
  always `"service registration rejected"` + numeric code):
  `1` duplicate id/name, `2` policy denial, `3` bind failure,
  `5` per-session service ceiling (`server/control.rs:235-343`; ceiling check at
  `server/control.rs:250-259`). With capability 1 negotiated the response is
  the correlated `RegisterReject{service_id, code}`; otherwise the legacy
  generic `Error` with the same codes. Success is
  `RegisterAck{service_id, effective_bind}` (`server/control.rs:333-339`).
- `OpenReject` flows **client→server only**. The server never emits it; it
  consumes it to free the pending slot (`server/control.rs:189-195`). Client codes
  (`1` target refused, `2` open-task exhausted) are opaque to the server.
- `Drain` flows both ways but with different meaning: server→client carries
  `shutdown_grace` (default 1 s, `common.rs:296`) during shutdown
  (`server/accept.rs:77-95`); client→server `Drain` breaks the control
  loop (`server/control.rs:200-203`, capturing the peer deadline) and then,
  with capability 2 negotiated, lets owned tasks drain up to
  `min(peer deadline, shutdown_grace)` before forced cancellation
  (`server/control.rs:226-248`); without it teardown stays immediate (1.0).
  Outbound `Open`/`Drain` share the same
  `open_rx` channel; a `Drain` write breaks after flushing
  (`server/control.rs:205-210`).

### 3.3 Effective-bind selection via `BindPolicy`

`BindPolicy` (`common.rs:85-124`): `allow_public_addresses`,
`allowed_addresses` (empty = any allowed by the master switch),
`allowed_port_ranges` (empty = any nonzero), `allow_ephemeral_ports`,
`max_services_per_session` (default 64, validated in
`common.rs:100-111`).

Selection (`register_service`, `server/control.rs:250-307`):

1. `bind_to_socket(&requested_bind, &policy)` (`common.rs:491-512`):
   `Loopback{port}` → `[::1]:port` if `permits_port`; `Ip{address,port}` →
   `SocketAddrV6` iff `permits_address && permits_port`.
2. `permits_address` (`common.rs:516-519`):
   `(is_loopback || allow_public_addresses) && (allowlist empty || contains)`.
   `permits_port` (`common.rs:522-531`): port 0 gated by
   `allow_ephemeral_ports`; otherwise allowlist empty or in-range.
3. `TcpListener::bind(bind_addr)` — OS assigns the ephemeral port when 0.
4. `socket_to_effective` (`server/service.rs:148-154`) normalises V4→V6-mapped and
   builds `EffectiveBind{address:[u8;16], port}`.
5. Push `(session_id, service_id, effective)` to `counters.binds`
   (`server/control.rs:307`), spawn `run_service` (`server/control.rs:310`), record
   `services` + high-water (`server/control.rs:326-327`), reply `RegisterAck`.

Auth success never grants bind permission by itself
(`docs/SECURITY.md:14-17`); every registration re-evaluates the policy.
The client `TcpTarget` in the register frame is treated as bounded metadata
only (`server/control.rs:175` comment).

---

## 4. Data plane

### 4.1 External accept → `Open`

`run_service` (`server/service.rs:66-144`) owns one service listener:

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

- `ConnectionId` is 128-bit random (`server/service.rs:87-90`), bound to the
  current session + service, 30 s lifetime by default
  (`TimeoutPolicy::pending_connection`, `common.rs:294`; used at
  `server/service.rs:93,115`), single-use (`docs/SECURITY.md:19-23`).
- `connection_admission = Semaphore(policy.limits.active_connections_per_session)`
  per session (`server/control.rs:130-132`, `1330`). The permit is moved into the
  relay task via `ActiveConnectionGuard` (`server/service.rs:86,112`), so
  `active_connections` covers pending-wait + relay.
- `opens: mpsc::Sender<Message>` is the session's policy-sized control
  channel (`server/control.rs:145`). `try_send` failure means a slow/dead control
  writer; the pending entry is removed immediately (`server/service.rs:103-107`).
- Relay uses `RelayOptions::bounded(16 KiB, timeouts.relay_drain)`
  (`server/service.rs:121`; default drain 15 s, `common.rs:295`); both `Ok(report)`
  and `Err(failure)` byte counts are added to
  `bytes_upstream/downstream` (`server/service.rs:122-131`).
- Service teardown (`UnregisterService`, session end, `run_service` exit)
  calls `remove_service_pending` / `remove_all_pending`
  (`server/pending.rs:99-121`), which `retain`/`clear` and subtract the exact
  removed count. `SessionContext::drop` is the final backstop.

### 4.2 Client `DataHello` → `DataHello` validation → relay

Data connections arrive on the **same ingress port** as control. `handle_connection`
peeks the first frame after TLS (+WSS upgrade): `DataHello` → `accept_data_hello`,
`ClientHello` → `serve_control`, anything else → `Protocol/UnexpectedMessage`
(`server/accept.rs:248-265`). Data paths drop both the handshake guard and the
accept-loop admission permit immediately (`server/accept.rs:251-252`); control
paths hold them until auth completes (`server/control.rs:112-126`).

`accept_data_hello` (`server/pending.rs:30-95`):

| Check (in order) | On failure | Counter | Rationale |
|---|---|---|---|
| session map lookup `hello.session_id` → `Weak::upgrade` | `Authentication` | `rejected++` | unknown/stale session, incl. post-restart generations |
| `session.principal != hello principal` | `Authentication` | `rejected++` | mTLS binding (§7); `None != Some` also rejects |
| `pending.remove(hello.connection_id)` | `Authorization` | `rejected++` | unknown/replayed id (already consumed) |
| `pending.service_id != hello.service_id` or `expires <= now` | `Authorization` | `rejected++` | wrong-service or stale; entry is **consumed** |
| else `data_tx.send(stream)` | `Cancelled` if receiver gone | — | handoff to the `run_service` relay task |

Notes for reviewers:

- Wrong-session misses leave the pending entry alive (unit-tested,
  `server_tests/tcp.rs:1248-1308`); wrong-service/expired **consume** it
  (`server_tests/tcp.rs:1309-1376`). This is deliberate: the id is single-use
  once the session is proven, preventing probe-reuse of a live slot.
- Bearer-token comparison is constant-time via `subtle`
  (`common.rs:485-488`, used `server/control.rs:110`). `ConnectionId` equality is
  the proto type's constant-time eq (see `proto-wire-protocol.md`); session
  lookup is a hash hit, not a secret compare — the secrecy sits in the
  128-bit id + 30 s window.
- `DataHello` is the final Eggtunnel message on a data stream; subsequent
  bytes are opaque relay (`docs/SECURITY.md:23`).
- QUIC data streams use the same validator: `handle_quic_data_stream`
  (`server/accept.rs:458-474`) reads one `DataHello` then calls `accept_data_hello`
  with `principal: None` (QUIC has no mTLS — §7).

### 4.3 `OpenReject` codes

The server is a pure consumer: `OpenReject{connection_id}` removes the
pending entry and decrements `pending` (`server/control.rs:189-195`). Codes are
not inspected. For the producer side (client `1` = target refused,
`2` = open-task ceiling) see `client.md`; the
`refused_target_rejects_external_connection_and_releases_pending_capacity`
test (`server_tests/tcp.rs:1112-1179`) proves the end-to-end effect (external sees EOF,
`pending` returns to 0).

---

## 5. Concurrency

### 5.1 Accept loops

**TCP** `server_loop` (`server/accept.rs:96-178`):

- Shared: `sessions` weak map, `admission = Semaphore(policy.limits.accepted_handshakes)`,
  `auth_failures` limiter, `handlers: JoinSet`.
- `select!`: `cancel` → break; `listener.accept()` → `try_acquire_owned`
  admission (fail → `rejected++`, `ResourceExhausted`, keep looping);
  else spawn `handle_connection` with `child_token`, `HandshakeGuard`,
  `ConnectionContext`; `handlers.join_next()` → `record_join_result`
  (panic accounting).
- No `.await` holds the sessions lock across I/O; the map is only locked for
  insert/retain/upgrade.

**QUIC** `quic_server_loop[_with_admission]` (`server/accept.rs:96-146`): same
shape over `listener.accept_connection(&cancel)`. `Ok(None)` breaks (listener
closed); `Err` → `rejected++` and continues; admission-full closes the QUIC
connection with `"handshake limit reached"` (`server/accept.rs:112-117`).

**Per-QUIC-connection** `handle_quic_connection` (`server/accept.rs:159-222`):
accepts the first bidi stream as control (`handshake` timeout), spawns
`serve_control` on it, then loops `accept_stream` for data. Data streams are
gated by a per-connection `stream_admission` semaphore
(`max_active_data_streams`, default = `policy.limits.active_connections_per_session`)
via `try_acquire_owned`
(`server/accept.rs:388-396`, `762-766`); each spawns `handle_quic_data_stream` in a
`streams: JoinSet`. Control completion, cancel, or error breaks the loop,
then: cancel, `connection.close("session ended")`, `abort_all` streams,
abort a still-running control task, await it (`server/accept.rs:446-454`).

### 5.2 Per-session tasks

`serve_control` splits the control stream (`tokio::io::split`,
`server/control.rs:144`), creates `(open_tx, open_rx) = mpsc::channel(policy.limits.control_queue)`
(`server/control.rs:145`), stores a clone in `control_tx` for shutdown `Drain`
(`server/control.rs:146`), and multiplexes in one `select!`:

- control reader (`read_message`), `open_rx` forwarder, `children.join_next`
  (`run_service` tasks), `context.cancel`, and a pinned `control_idle` sleep
  reset on `Register`/`Unregister`/`OpenReject`/`Ping`
  (`server/control.rs:165,165,165,165`; idle budget `server/control.rs:150-153`).
- Each registered service spawns `run_service` in `children: JoinSet`
  (`server/control.rs:310`); each external accept spawns a relay task in
  `run_service`'s `relays: JoinSet` (`server/service.rs:111-134`).

### 5.3 Semaphores and guards

| Primitive | Ceiling (default) | Scope | Where |
|---|---|---|---|
| `admission` | `policy.limits.accepted_handshakes` (64) | whole server, unauthenticated conns | `server/accept.rs:102`, `635`, `550`, `654` |
| `HandshakeGuard` | counts `handshakes` + high-water | per accepted conn until auth/data-hello | `server/session.rs:135-156`, `562`, `661`, `775`, `948`, `1081`, `1094` |
| `sessions` map + `SessionGuard` | `policy.limits.sessions` (128) | whole server | `server/control.rs:131-138`, `1448-1478` |
| `connection_admission` | `policy.limits.active_connections_per_session` (128) | per session (pending-wait + relay) | `server/control.rs:130-132`, `1330-1335` |
| `pending` map | `policy.limits.pending_per_session` (128) | per session | `server/service.rs:94-102` |
| `ActiveConnectionGuard` | counts `active_connections` + high-water | per relay task (holds permit) | `server/service.rs:30-57` |
| `stream_admission` (QUIC) | `max_active_data_streams` (default active-per-session) or test override | per QUIC connection | `server/accept.rs:388-396`, `762-766` |
| `open_tx/open_rx` | `policy.limits.control_queue` (128) | per session control→writer | `server/control.rs:145-146`, `1352` |

`SessionGuard::drop` also reconciles `services` from the binds list length
delta (`server/session.rs:112-126`) — a deliberate single-source-of-truth choice
(`binds` is authoritative for service count on session exit).

### 5.4 Shutdown with `shutdown_grace` (default 1 s)

TCP (`server/accept.rs:96-145`) and QUIC (`server/accept.rs:147-212`) share `AcceptContext::drain` (`server/accept.rs:77-95`):

1. Break accept loop on `cancel`.
2. Upgrade weak sessions → `active`; `try_send(Drain{deadline_ms: grace_ms})`
   to each `control_tx` (bounded, so never blocks shutdown).
3. `sleep(policy.timeouts.shutdown_grace)` — gives clients 1 s to observe `Drain`.
4. Cancel every session token (cascades via `child_token` to services/relays).
5. `handlers.abort_all()` + drain `join_next` (records panics).

`serve_control` teardown (`server/control.rs:226-250`) first honors a negotiated peer drain deadline (capability 2, §3.2), then cancels services,
`abort_all`s children, drains completions, then `remove_all_pending`.
`run_service` exit (`server/service.rs:141-143`) aborts relays, drains them, then
`remove_service_pending`. `server_shutdown_cancels_incomplete_tls_and_authentication_handshakes`
(`server_tests/tcp.rs:1440-1494`) proves `active_handshakes` returns to 0 and no
session is left behind.

### 5.5 `JoinError` panic accounting

`Counters::record_join_result` (`common.rs:428-433`): `is_panic` →
`task_panics++`, `last_termination = Internal`. Call sites:
`server_loop` (`server/accept.rs:137-139`), QUIC loops (`server/accept.rs:137-139`),
`serve_control` children (`server/control.rs:210-212`), `run_service` relays
(`server/service.rs:136-138`), QUIC streams (`server/accept.rs:441-443`). The QUIC
control task additionally distinguishes panic vs cancel explicitly
(`server/accept.rs:414-424`). Covered by
`owned_task_panic_is_counted_as_internal_termination`
(`server_tests/tcp.rs:1427-1439`).

---

## 6. Hardening: timeouts, throttle, resource-exhausted paths

### 6.1 Constants (all finite ceilings live in `RuntimePolicy`)

`server/auth.rs:20-27` holds only the auth-throttle constants plus two
`cfg(test)` aliases:

| Constant | Value | Where it is used |
|---|---|---|
| `AUTH_FAILURES_PER_SOURCE` | 10 | `AuthFailureLimiter`, `server/accept.rs:103-107`, `636-640` |
| `AUTH_FAILURE_WINDOW` | 60 s | sliding window prune, `server/auth.rs:75-92` |
| `MAX_AUTH_SOURCES` | 1024 | bounded table, `server/accept.rs:103-107` |
| `AUTH_FAILURE_DELAY` | 100 ms | after each failed token check, `server/control.rs:107` |
| `MAX_SESSIONS` (`cfg(test)` only) | 128 | test-only alias of `limits.sessions` |
| `MAX_HANDSHAKES` (`cfg(test)` only) | 64 | test-only alias of `limits.accepted_handshakes` |

Admission ceilings and timeouts are `RuntimePolicy`
(`common.rs:196-316`; `ResourceLimits` `common.rs:196-245`,
`TimeoutPolicy` `common.rs:250-302`, `RuntimePolicy` `common.rs:304-316`):

| Policy field | Default | Enforced at |
|---|---|---|
| `limits.sessions` | 128 | `serve_control` insert, `server/control.rs:137` |
| `limits.pending_per_session` | 128 | `run_service` insert, `server/service.rs:94` |
| `limits.active_connections_per_session` | 128 | `connection_admission` + QUIC `stream_admission` |
| `limits.control_queue` | 128 | per-session `open_tx/open_rx`, `server/control.rs:145` |
| `limits.accepted_handshakes` | 64 | accept-loop `admission`, `server/accept.rs:102,167` |
| `limits.services_per_session` | 64 | registration ceiling with `BindPolicy`, `server/control.rs:253` |
| `timeouts.handshake` | 10 s | TLS accept, WSS upgrade, first-frame read, `Auth` read, QUIC control/data first frames (`server/accept.rs:234,891,900,926,245,234-241,804-808`) |
| `timeouts.control_idle` | 90 s | whole control session; reset on Register/Unregister/OpenReject/Ping (`server/control.rs:150-153,165,165,165,165`) |
| `timeouts.pending_connection` | 30 s | pending entry expiry + `data_rx` wait (`server/service.rs:93,115`) |
| `timeouts.relay_drain` | 15 s | `relay_with_options` bounded drain (`server/service.rs:121`) |
| `timeouts.shutdown_grace` | 1 s | `Drain` → cancel gap (`server/accept.rs:86,700`) |

`validate_config` additionally caps TLS PEMs at `MAX_FRAME_BYTES`
(`client.rs:650-660`).

### 6.2 Per-source auth throttle

`AuthFailureLimiter` (`server/auth.rs:30-86`): `Mutex<HashMap<IpAddr,
VecDeque<Instant>>>` + `threshold/window/max_sources`. `prune` evicts
entries older than the window and drops empty deques, so the table cannot
grow unboundedly and never delays successful auth (doc comment
`server/auth.rs:30-31`).

- `is_blocked(source)` (`server/auth.rs:47-60`): prune, then: unknown source
  + table full → **blocked**; known source with `len >= 10` → blocked.
- `record_failure(source)` (`server/auth.rs:62-73`): prune, then: unknown +
  full → drop (no insert); else push timestamp.
- Enforcement (`server/control.rs:83-85`, `1078-1093`): pre-`ServerHello` blocked
  check (no failure recorded, just `Authentication`); post-check failure
  records, **drops both guards first** (frees handshake capacity before the
  sleep), sleeps 100 ms, `rejected++`, sends `Error{code:4}`, returns
  `Authentication`.

Properties (`docs/SECURITY.md:25-30`, `docs/OPERATIONS.md:27-28`):
process-local, per-source-IP, 10 fails / 60 s, ≤1024 sources, 100 ms delay,
unknown sources rejected when full. Unit-tested at
`server_tests/tcp.rs:1539-1562` (per-source isolation, full-table rejection of a second
source, window expiry prunes to an empty table).

### 6.3 Resource-exhausted paths (all bounded, all counted)

| Trigger | Response | Accounting |
|---|---|---|
| `admission.try_acquire` fails (TCP) | skip accept | `rejected++`, `ResourceExhausted` (`server/accept.rs:112-117`) |
| `admission.try_acquire` fails (QUIC) | close QUIC conn `"handshake limit reached"` | same (`server/accept.rs:112-117`) |
| `active.len() >= policy.limits.sessions` | reject session after auth | `ResourceExhausted` termination, `Authorization` error (`server/control.rs:137-139`); stale weaks pruned first (`server/control.rs:132`) |
| `pending.len() >= policy.limits.pending_per_session` | drop external accept | `rejected++`, `ResourceExhausted` (`server/service.rs:94-98`) |
| `connection_admission.try_acquire` fails | drop external accept | same (`server/service.rs:81-85`) |
| QUIC `stream_admission.try_acquire` fails | skip stream (no `DataHello` read) | same (`server/accept.rs:428-432`) |
| `opens.try_send` fails (control queue full) | remove just-created pending | `pending--`, `rejected++` (`server/service.rs:103-107`) |
| `services.len() >= bind_policy.max ⩓ policy.limits.services_per_session` | `Error{code:5}` | `rejected++`, `ResourceExhausted` (`server/control.rs:253-259`) |
| `ConnectionId::generate` fails (no entropy) | drop external accept | `rejected++` (`server/service.rs:87-90`) |

`rejected_connections` is the single funnel for all of the above plus every
`accept_data_hello` rejection and every registration denial — check it first
when triaging `last_termination = ResourceExhausted`.

---

## 7. mTLS profile: bearer-still-required, leaf SHA-256 principal

Feature `mtls` (`docs/SECURITY.md:32-39`):

- Trust roots are explicit server input (`client_ca_pem` on `ServerBuilder`,
  `server/config.rs:96-101`; `bind_mtls[*]`, `server/config.rs:77-122`); client still
  validates server name + system/configured roots. No
  enrollment/revocation service.
- **Bearer token is still required.** mTLS adds identity; it does not replace
  `Auth`. `mtls_requires_trusted_client_certificate_and_keeps_server_name_validation`
  (`server_tests/mtls.rs:5-136`, `mtls`) proves trusted-cert + correct token registers while
  rogue-CA, wrong-SNI, and cert-less clients never register.
- Principal = `SHA-256(leaf DER)` (`server/tls.rs:35-63`), extracted from the
  first peer certificate in `handle_connection` (`server/accept.rs:240-246`) and
  stored in `ConnectionContext.principal` → `SessionContext.principal`
  (`server/accept.rs:255`, `1102-1112`).
- Enforcement is on **both** planes: `accept_data_hello` rejects
  `session.principal != data principal` as `Authentication`
  (`server/pending.rs:52-61`). `None != Some([..])` also rejects, so a cert-less
  data dial cannot attach to a pinned session and vice versa.
  `mtls_principal_mismatch_cannot_attach_data_stream`
  (`server_tests/mtls.rs:137-187`, `mtls`) proves the pending entry survives a principal
  mismatch (no consumption, no decrement).
- QUIC has no mTLS: `handle_quic_connection` / `handle_quic_data_stream`
  hardcode `principal: None` (`server/accept.rs:192`, `773`, `800-816`), the
  profile gate `validate_server_profile` rejects QUIC+mTLS
  (`server/config.rs:165-169`), and the client rejects QUIC+mTLS combinations
  (see `client.md`).

Reviewer note: only the **first** peer certificate is pinned; intermediates
are verified by WebPKI but do not participate in the principal. Rotation =
new leaf → new principal → new session (old pendings are unusable).

---

## 8. Observability: snapshot fields the server updates

`Snapshot` / `Counters` live in `common.rs:136-160`, `319-344`
(`snapshot()` at `common.rs:355-395`).
The server updates every field except `connected`/`reconnects`/`open_tasks`
(client-side concerns):

| Snapshot field | Server writer |
|---|---|
| `active_sessions` / `high_water_sessions` | `fetch_add` on admit (`server/control.rs:138-144`), `fetch_sub` in `SessionGuard::drop` (`server/session.rs:108-111`) |
| `registered_services` / `high_water_services` | `fetch_add` on `RegisterAck` (`server/control.rs:326-327`), `fetch_sub` on `Unregister` (`server/control.rs:184`) and session-exit reconcile (`server/session.rs:123-126`) |
| `pending_connections` / `high_water_pending` | `fetch_add` on insert (`server/service.rs:101-102`), `fetch_sub` on consume (`server/pending.rs:73-76`), relay-task GC (`server/service.rs:117-119`), `remove_service/all_pending` (`server/pending.rs:99-121`), `SessionContext::drop` (`server/session.rs:45-52`); unit-pinned in `server_tests/tcp.rs:1248-1376` |
| `active_connections` / `high_water_active` | `ActiveConnectionGuard::new/drop` (`server/service.rs:35-57`) |
| `active_handshakes` / `high_water_handshakes` | `HandshakeGuard::new/drop` (`server/session.rs:137-156`) |
| `task_panics`, `last_termination` | `record_join_result` on every `JoinSet` drain (§5.5); `record_termination` on handler errors, admission-full, session-full, service-ceiling |
| `rejected_connections` | every auth/reg/pending/data-hello/admission denial (§6.3) |
| `bytes_upstream/downstream` | relay reports incl. failure partials (`server/service.rs:122-131`) |
| `effective_binds: Vec<(SessionId, ServiceId, EffectiveBind)>` | push on register (`server/control.rs:307`), retain on unregister (`server/control.rs:307`), retain on session exit (`server/session.rs:120`); CLI polls it every 250 ms |
| `resource_limits` | `policy.limits` (`common.rs:377`) — echoes the **selected** `RuntimePolicy` (128/64/128/128/64/128/128/32 incl. `client_command_queue: 32` by default, `common.rs:232-245`), pinned by `server_tests/tcp.rs:882` |

`last_termination` retains only the most recent category, never history or
error text (`docs/OPERATIONS.md:34-35`). `Snapshot` never carries secrets;
proxy credentials and tokens are redacted from diagnostics
(`docs/SECURITY.md:76-78`).

---

## 9. Test inventory (`server_tests.rs` + `server_tests/`)

Harness (`crates/eggtunnel/src/server_tests.rs`, 232 lines): `test_session`
(`server_tests.rs:17-40`) builds an isolated `SessionContext` + weak map +
counters; `test_data_stream` (`server_tests.rs:42-45`) is a 32-byte duplex;
`builder` (`server_tests.rs:47-57`) is a `ServerBuilder` with dummy cert/key;
`certificate` / `mtls_certificates` (`server_tests.rs:108-177`) mint rcgen
fixtures; `roundtrip` (`server_tests.rs:179-189`) writes + shutdown +
`read_to_end` against an effective bind. `ServerBuilder::validate` matrix
(`server_tests.rs:59-106`): TCP default ok, QUIC ok, WebSocket ok, TCP+mTLS
ok, QUIC+mTLS rejected, WebSocket+mTLS rejected. Module wiring
(`server_tests.rs:218-232`): `tcp` always; `mtls` / `quic` / `websocket` /
`proxy` behind their features.

`server_tests/tcp.rs` (1657 lines) — core TCP/TLS lifecycle:

1. `tcp_tls_reverse_session_registers_and_relays_data` (`tcp.rs:800-933`)
   — canonical end-to-end: 2 services register (2 `effective_binds`),
   parallel echo roundtrips, byte counters > 0 on both ends,
   `resource_limits.sessions == 128`, high-waters for
   services/active/pending/handshakes, `Unregister` shrinks binds to 1,
   client shutdown drains the active relay to 0/0.
2. `bad_token_does_not_create_a_registered_session` (`tcp.rs:976-1020`)
   — wrong bearer → `rejected++`, `last_termination == Authentication`, 0
   sessions, client never reconnects (auth is non-retryable).
3. `public_service_bind_is_denied_without_explicit_policy`
   (`tcp.rs:1063-1111`) — `RequestedBind::Ip{2001:db8::1, 9000}` with
   default policy → `rejected++`, 0 services, empty `effective_binds`.
4. `data_hello_is_session_service_bound_and_single_use`
   (`tcp.rs:1248-1308`) — unit: wrong `SessionId` → `Authentication` and
   pending survives; correct triple consumes via `data_tx`; replay of the
   same triple → `Authorization` (single-use proven without sockets).
5. `wrong_service_and_expired_data_hellos_consume_and_reject_pending_state`
   (`tcp.rs:1309-1376`) — unit: wrong `ServiceId` and expired entries are
   consumed (map + counter return to 0), closing the reuse window.
6. `unauthenticated_handshake_admission_caps_at_limit_and_recovers`
   (`tcp.rs:1495-1538`) — 65 bare TCP connects → `active_handshakes ==
   64` + `rejected > 0`; dropping peers returns handshakes to 0 (no leak).
7. `auth_failure_limiter_is_per_source_bounded_and_expires`
   (`tcp.rs:1539-1562`) — pure unit: threshold blocks per-source, full
   table (max 1) rejects an unknown source, window expiry prunes to empty.
8. `bind_policy_enforces_address_port_and_ephemeral_rules`
   (`tcp.rs:1563-1623`) — unit over `bind_to_socket`: allowlisted
   addr+port ok; ephemeral-off, out-of-range port, and non-allowlisted addr
   all err.
9. `server_shutdown_cancels_incomplete_tls_and_authentication_handshakes`
   (`tcp.rs:1440-1494`) — bare TCP + post-`ServerHello` control peer
   hold 2 handshakes; `shutdown()` returns both handshake and session counts
   to 0.
10. `repeated_client_server_start_stop_returns_runtime_counts_to_zero`
    (`tcp.rs:1377-1426`) — 3 bind/start/shutdown cycles leave sessions,
    services, pending, active, handshakes all at 0 (no cross-cycle leak).
11. `refused_target_rejects_external_connection_and_releases_pending_capacity`
    (`tcp.rs:1112-1179`, `OpenReject` path),
    `client_reconnects_and_restores_services_in_a_new_session_generation`
    (`tcp.rs:1180-1247`, new `SessionId` after server restart),
    `owned_task_panic_is_counted_as_internal_termination`
    (`tcp.rs:1427-1439`, panic accounting).
12. Dynamic-registration + policy surface: `acknowledged_dynamic_service_…`
    (`tcp.rs:4-134`), `dynamic_registration_maps_server_service_limit_…`
    (`tcp.rs:135-192`), custom-policy/timeout probes
    (`tcp.rs:527-799`: pending-limit saturation, pending-timeout expiry,
    handshake-timeout, control-idle-timeout).

`server_tests/mtls.rs` (187 lines, `mtls`):
`mtls_requires_trusted_client_certificate_and_keeps_server_name_validation`
(`mtls.rs:5-136`) — trusted leaf registers; rogue CA, wrong SNI, and missing
cert never register;
`mtls_principal_mismatch_cannot_attach_data_stream` (`mtls.rs:137-187`) —
mismatched data principal is rejected and the pending slot survives.

`server_tests/quic.rs` (893 lines, `quic`):
`quic_session_multiplexes_isolated_data_streams_for_two_services`
(`quic.rs:6-93`),
`quic_wrong_session_data_hello_is_rejected_and_pending_entry_survives`
(`quic.rs:287-405`), `quic_replay_data_hello_on_second_stream_is_rejected`
(`quic.rs:406-529`),
`quic_stale_old_generation_data_hello_is_rejected_after_reconnect`
(`quic.rs:530-640`),
`quic_stream_saturation_recovers_capacity_and_keeps_unrelated_streams_alive`
(`quic.rs:641-792`, uses `bind_quic_with_admission_for_test`,
`server/config.rs:65-77`),
`quic_connection_replacement_creates_new_session_and_reregisters_services`
(`quic.rs:213-286`), `quic_half_close_preserves_response_after_request_eof`
(`quic.rs:816-893`), plus the ignored
`qualification_quic_stream_churn_soak` (`quic.rs:94-212`).

`server_tests/websocket.rs` (347 lines, `websocket`):
`websocket_tls_session_registers_and_relays_data_paths`
(`websocket.rs:17-76`), `wss_peer_close_during_active_relay_terminates_cleanly`
(`websocket.rs:195-273`),
`wss_payload_larger_than_message_cap_roundtrips_multiple_frames`
(`websocket.rs:274-347`), plus the ignored
`qualification_wss_connection_churn_soak` (`websocket.rs:77-194`).

`server_tests/proxy.rs` (905 lines, `outbound-proxy`, client-side paths —
see `client.md` for the matrix): HTTP CONNECT (`proxy.rs:5-90`), SOCKS5
(`proxy.rs:91-188`), refused-endpoint secrecy (`proxy.rs:189-252`),
handshake-timeout (`proxy.rs:253-322`), cancellation (`proxy.rs:323-380`),
HTTP/SOCKS5 auth success + failure (`proxy.rs:381-772`), two-hop
SOCKS5→HTTP chain (`proxy.rs:773-905`).

---

## 10. Review checklist

| # | Risk | Where to look | What good looks like / probe |
|---|---|---|---|
| 1 | **Listener hijack** — client requests a public/ephemeral bind it should not get | `server/control.rs:176-195`, `common.rs:491-531`, `server_tests/tcp.rs:1063-1111,1563-1623` | `bind_to_socket` runs **before** `TcpListener::bind`; default denies non-loopback; allowlist + port-range + ephemeral bits all enforced. Probe: register `Ip{0.0.0.0,80}` and ephemeral-0 under a restrictive policy; expect codes 2/3, no listener. |
| 2 | **Session confusion** — data hello attaches to the wrong session/service | `server/pending.rs:30-95`, tests `server_tests/tcp.rs:1248-1376`, `server_tests/quic.rs:287-640` | triple `(session, service, connection)` checked in order; wrong-session → `Authentication` without consuming; wrong-service/expired → `Authorization` with consumption. Probe: cross-wire two concurrent sessions' ids; exactly one `rejected++`, no relay. |
| 3 | **Pending exhaustion / GC** — attacker or churn fills 128 slots | `server/service.rs:94-107,115-119,99-121,45-52` | insert capped; `try_send` failure rolls back; relay timeout GCs; unregister/session-exit/`drop` reconcile counters. Probe: 128 hung externals + 1 more → `ResourceExhausted`; cancel session → `pending == 0`. Watch for double-`fetch_sub` (consume at `1009` vs relay-task GC at `1366` is guarded by `remove` return). |
| 4 | **Throttle bypass behind NAT** — shared source IP, table-full behavior | `server/control.rs:83-124,30-86`, `docs/SECURITY.md:25-30` | throttle is per-source-IP, process-local; 10/60 s, 1024 sources, 100 ms delay, full-table rejects unknowns. Behind shared NAT one bad actor blocks the whole egress IP until the window expires — documented, not fixed. Probe: 10 fails from one IP blocks an innocent second client on the same IP; confirm ops runbook accounts for it. |
| 5 | **Shutdown races** — `Drain` lost, relays dangling, counters stuck | `server/accept.rs:78-96,213-232,215-225,141-143`, tests `server_tests/tcp.rs:1377-1494` | `Drain{grace}` via `try_send`, grace sleep, session-cancel cascade, `abort_all` + drain; `serve_control`/`run_service` clear pendings; start/stop cycles return all counts to 0. Probe: kill server mid-relay with an active external; client must see `Drain`, external must see EOF, snapshot must settle to 0/0. |
| 6 | **Handshake pile-up** — pre-auth work before admission | `server/accept.rs:112,112,135-156,234,891` | TCP+QUIC both `try_acquire` **before** spawning; `HandshakeGuard` counts until auth/data-hello; full → `rejected++` without reading. Residual: QUIC/TLS adapter work below Eggtunnel (Eggress caps 1024 conns / 4096 streams, `docs/SECURITY.md:49-62`). Probe: SYN/TLS flood → handshakes pinned at 64, sessions stay 0. |
| 7 | **mTLS downgrade** — bearer-only data dial on a pinned session | `server/accept.rs:240-246,255,52-61,125-135` | principal pinned at session creation and rechecked on every `DataHello`; `None != Some` rejects. Probe: control with cert + data without → `Authentication`, pending survives. Confirm QUIC deployments do not expect mTLS (hard `None`). |
| 8 | **Control-queue backpressure** — slow client stalls `Open` delivery | `server/control.rs:145,205-209,103-107` | `open_tx` is policy-bounded (default 128) with `try_send`; full → pending rolled back + `rejected++` rather than blocking the accept loop. Probe: register then stall control reads, burst 129 externals; last `Open` must fail open, not deadlock. |
| 9 | **Idle vs relay liveness** — 90 s idle killing active relays | `server/control.rs:150-153,165-237` | idle timer is only reset by control frames and only breaks the **control** loop; relays live in `children`/`relays` JoinSets and are torn down explicitly. Long relays with no control traffic still hit the 90 s control timeout by design — confirm this matches ops expectations for quiet services. |
| 10 | **Panic visibility** — child task panic silently dropping a service | `common.rs:428-433`, `server/accept.rs:137,137,210,136,441` | every `join_next` records `is_panic → task_panics++, Internal`. Probe: inject a relay panic; `task_panics == 1`, `last_termination == Internal`, sibling services unaffected. |

---

## 11. Runtime policy and composition (M008)

`ServerBuilder` composes TCP/TLS, QUIC, WebSocket, optional trusted client CA,
`BindPolicy`, and `RuntimePolicy` without exposing adapter-specific types
(`server/config.rs:60-130`). `validate()` (`server/config.rs:107-116`) rejects mTLS on
unsupported transports before binding (see `validate_server_profile`,
`server/config.rs:148-173`). The legacy `Server::bind_*` helpers
(`server/config.rs:34-151`) delegate through the builder. Session, handshake,
service, pending, active-connection, queue, and timeout behavior comes from
the immutable finite policy held by `Counters`; `BindPolicy` can further
restrict service binds.

Dynamic client registration reuses the established control protocol. Server
registration handling continues to validate duplicate identity, service
capacity, bind authorization, and listener creation before sending
RegisterAck (`server/control.rs:164-200`). UnregisterService remains idempotent
and removes the listener and its pending ConnectionIds
(`server/control.rs:178-187`). Structured events report coarse outcomes without
emitting bearer tokens or TLS material.

---

*End of server deep dive. Next: `transports-wire-io.md` for the framing/relay
primitives this module calls into, or `client.md` for the other half of the
`Open`/`DataHello` handshake.*
