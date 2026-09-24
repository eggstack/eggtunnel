# Reverse-session client — deep dive

> Parent: [Architecture Overview](overview.md) §3. This file is the
> review-oriented deep dive for the client runtime. Public configuration and
> target connector contracts live in `crates/eggtunnel/src/client/config.rs`,
> dynamic Service lifecycle state lives in `client/service_state.rs`,
> heartbeat state in `client/heartbeat.rs`, and the Open/data path in
> `client/open.rs`.
> Client tests are in `client/tests.rs`. For the server half see `server.md`; for framing see
> `proto-wire-protocol.md`; for shared types see `common-core.md`;
> for transports see `transports-wire-io.md`.

Scope: the private-side, outbound-only initiator. `client.rs` (~1410 lines)
is the orchestrator (reconnect/session loops, validation); composable logic
lives in `client/config.rs` (`ClientConfig`, canonical `ClientBuilder`,
target contract), `client/service_state.rs` (dynamic-Service lifecycle,
M009), `client/heartbeat.rs` (one-outstanding-Ping probe), and
`client/open.rs` (data path per `Open`). Tests live in `client/tests.rs`,
`client/qualification_tests.rs` (deterministic 10k-step seeded sequence),
plus unit tests in `service_state.rs`/`heartbeat.rs`; cross-transport E2E
remains under `server_tests/`. The client owns one
authenticated control stream per session, registers local services, and
dials one data connection per server `Open`. It never listens. The server
owns listeners and picks effective binds; the client owns local targets
and picks where bytes land.

> M008/M009 note: `ClientBuilder` (`client/config.rs:78-156`) is the
> canonical composition path (transport/connector/identity/proxy/
> `RuntimePolicy`); `Client::start*` (`client.rs:214-409`) delegate through
> it via `validate_client_profile` (`client.rs:524-568`). Finite
> ceilings/timeouts come from `RuntimePolicy` (`common.rs:196-316`), read via
> `counters.policy`, not `client.rs` constants. Dynamic `register_service` is
> generation-gated with one registration in flight per session
> (`client/service_state.rs`); heartbeat health is one outstanding probe plus
> a bounded `HeartbeatSnapshot` (`client/heartbeat.rs`, `common.rs:162-169`).

Primary sources: `crates/eggtunnel/src/client.rs` (full read),
`crates/eggtunnel/src/common.rs` (`ClientService`, `Snapshot`/`Counters`,
`TunnelError`), `crates/eggtunnel/src/wire_io.rs`,
`docs/EMBEDDING.md`, `docs/SECURITY.md`, `docs/SUPPORT.md`,
`crates/eggtunnel/src/lib.rs`, `crates/eggtunnel-cli/src/main.rs`
(validation matrix).

---

## 1. Role

The client runs behind NAT/firewall and initiates everything outward:

- **One control stream per session.** TCP+TLS by default, or QUIC
  (one control bidi-stream on one QUIC connection), or WSS
  (TLS → WebSocket upgrade). Authenticated with a bearer token inside
  verified TLS, then N `RegisterService` exchanges.
- **One data dial per accepted external connection.** Triggered by a
  server `Open(service_id, connection_id)` on the control channel.
  Each data path is a fresh TLS connection (or QUIC bidi-stream / WSS
  byte-stream) carrying exactly one `DataHello(session, service,
  connection)` followed by opaque relay bytes.
- **Relationship to server listeners and local targets.**
  The server binds approved addresses (`BindPolicy`, server-only) and
  reports `effective_bind` per registration. The client never sees or
  influences listener sockets; it only supplies `requested_bind` as a
  hint and `target` as its own local destination. Per
  `docs/SECURITY.md:9-12`, the server ignores the client `Target` as
  authority; only the client uses its configured target after a valid
  `Open`. That separation is the target-confusion boundary (see §10).

```
external TCP ──► server listener (server-owned) ──► pending[conn] ──► Open ──┐
                                                                              │ control TLS
client target (client-owned) ◄── relay ◄── data TLS + DataHello ◄─────────────┘
```

The client is library-first (`docs/EMBEDDING.md`): caller-owned Tokio
runtime, no global runtime/tracing installed, `Client::shutdown().await`
joins owned session and data tasks.

---

## 2. Public API

### 2.1 `ClientConfig`

Defined at `crates/eggtunnel/src/client/config.rs:45-66`, redacted `Debug` at
`config.rs:56-66`:

| Field | Type | Notes |
|---|---|---|
| `server_addr` | `String` | `host:port`; DNS via Tokio on connect. Validated by `valid_endpoint` (`client.rs:608-626`): bracketed IPv6 or `rsplit_once(':')`, non-empty host without whitespace, nonzero `u16` port. |
| `tls_server_name` | `String` | SNI + cert verification name; must be `1..=253` bytes (`client.rs:579-581`). |
| `ca_pem` | `Option<Vec<u8>>` | Custom CA bundle; if absent, system/platform roots. Size-capped at `MAX_FRAME_BYTES` (1 MiB) at `client.rs:596-604`. Rejected for QUIC (see §7). |
| `token` | `SecretToken` | `1..=4096` B, redacted `Debug`, `zeroize` on drop (`common.rs:19-48`). `expose()` is `pub(crate)` only (`common.rs:32-35`). |
| `services` | `Vec<ClientService>` | Initial set may be empty; `len() > policy.limits.services_per_session` (default 64) is rejected (`client.rs:582-586`). IDs and names must be unique (`client.rs:587-595`). |

`ClientService` (`common.rs:52-73`) is the client view: `id: ServiceId`
+ `name: ServiceName` + `requested_bind: RequestedBind` +
`target: TcpTarget`. Contrast `ServiceSpec` (server view, no target —
server never learns the local destination).

### 2.2 `Client::start*` variants

All constructors require a caller-owned Tokio runtime and validate through
the canonical path. `Client::start*` at `client.rs:214-409` are thin
delegates over `ClientBuilder` (`client/config.rs:78-156`); the builder's
`validate()` at `config.rs:130-140` calls `validate_client_profile`
(`client.rs:524-568`), which takes the `RuntimePolicy` and rejects invalid
transport/identity/proxy combinations before any I/O.

| Entry point | Lines | Transport | Notes |
|---|---|---|---|
| `start` | `client.rs:214-216` | TCP+TLS | Default; `TcpTargetConnector`. |
| `start_with_connector` | `client.rs:219-227` | TCP+TLS | Custom `TargetConnector`; builds TLS via `build_tls_config` (`client.rs:628-636`). |
| `start_websocket` / `start_websocket_with_connector` | `client.rs:230-247` | WSS | `WebSocket` profile; verified TLS → binary WS upgrade, 1 MiB caps (`client.rs:710-729`). |
| `start_with_outbound_proxy` / `..._and_connector` | `client.rs:250-271` | TCP+TLS over proxy | Parses `pproxy` URI chain via `parse_outbound_proxy` (`client.rs:476-482`); `OutboundConnector::from_pproxy_uri`, typed `Configuration("invalid outbound proxy chain")` on failure. |
| `start_websocket_with_outbound_proxy` / `..._and_connector` | `client.rs:274-297` | WSS over proxy | Composes both adapters. |
| `start_quic` / `start_quic_with_connector` → `start_quic_profile` | `client.rs:300-317` → `client.rs:320-364` | QUIC | Separate `quic_reconnect_loop`; platform roots only, rejects `ca_pem` (`client.rs:330-334`). Test-only `start_quic_insecure_for_test` / `..._with_connector_for_test` (`client.rs:367-385`) set `insecure=true`. |
| `start_with_mtls` / `start_with_mtls_and_connector` | `client.rs:388-409` | TCP+TLS + client cert | `build_mtls_tls_config` (`client.rs:639-661`); bearer token still required (see §7). |

Internal fan-in: `Client::start_profile` (`client.rs:138-212`) validates
then dispatches to `start_with_tls_config`
(`client.rs:411-455`), which spawns `reconnect_loop` (`client.rs:664-804`),
or to `start_quic_profile`, which spawns `quic_reconnect_loop`
(`client.rs:837-939`). Both return immediately with an owner `Client` +
cloneable `ClientHandle`; the background task owns config, connector,
cancel token, counters, and command channel.

`validate_outbound_proxy` (`client.rs:471-473`) is re-exported for CLI
`check` (`lib.rs:20-21`).

### 2.3 `Client` / `ClientHandle`

- `Client` (`client.rs:51-55`): `{ cancel, task: Option<JoinHandle<()>>, handle }`. `Drop` cancels (`client.rs:518-522`).
  - `handle()` (`client.rs:457-459`) clones the handle.
  - `shutdown(mut self)` (`client.rs:461-467`) cancels then awaits the
    reconnect task (join, ignore result). This is the graceful path
    embedders must call (`docs/EMBEDDING.md:16`).
- `ClientHandle` (`client.rs:58-64`): `{ cancel, counters, commands,
  quic_client }` (last field only with `quic`).
  - `snapshot()` (`client.rs:79-81`) → `Counters::snapshot()`.
  - `shutdown(&self)` (`client.rs:82-84`) → `cancel.cancel()` (no join;
    fire-and-forget vs `Client::shutdown` which joins).
  - `register_service(service)` (`client.rs:101-126`) → requires
    `connected != 0` else `Disconnected`; captures the current
    `session_generation`, sends `ClientCommand::Register{
    service, generation, reply }` over the bounded command channel and
    awaits the `oneshot` reply. Only a `RegisterAck` from the same Session
    generation commits to desired state (`service_state.rs:96-115`);
    stale generations get `Disconnected`, duplicate id/name gets
    `ServiceAlreadyExists`, a second in-flight registration gets
    `ResourceExhausted`, and wire `Error` maps via `registration_error`
    (`client.rs:1345-1351`: code 1 → `ServiceAlreadyExists`, code 5 →
    `ResourceExhausted`, else `Authorization`).
  - `unregister_service(id)` (`client.rs:87-97`) → sends
    `ClientCommand::Unregister{ id, reply }` and awaits the reply;
    `Err(Disconnected)` if the loop is gone. Semantics (see
    `client.rs:1284-1293`): `ServiceState::unregister` removes from
    `active` *and* `desired` (so it survives reconnect), cancels a
    matching pending registration with `Cancelled` while keeping a
    tombstone (`pending.reply = None`) so its late ack resolves as
    `Abandoned` and is unregistered on the wire without mutating desired
    state (`service_state.rs:121-132`, `service_state.rs:96-115`);
    prunes the `services` counter and `binds`; writes
    `UnregisterService` on the wire. Replies `Ok(())` even if the id was
    unknown (still writes the frame).
  - `ClientCommand` (`client.rs:66-76`): `Register{ service, generation,
    reply }` + `Unregister{ id, reply }`, both with `oneshot` replies.
    At most one dynamic `Register` ack may be outstanding per Session
    because wire `Error` carries no `ServiceId`
    (`service_state.rs:1-6`).
  - `quic_client_for_test` (`client.rs:129-134`): `#[cfg(all(test,
    feature="quic"))]` accessor for assertions.

Command channel depth is `policy.limits.client_command_queue` (default 32;
`client.rs:337`, `client.rs:427` for TLS) —
distinct from `policy.limits.control_queue` (default 128, the wire-bound
outbound queue at `client.rs:1110`).

### 2.4 Target abstraction

| Type | Location | Role |
|---|---|---|
| `ApplicationStream` | `client/config.rs:3-5` | Blanket impl over `AsyncRead+AsyncWrite+Send+Unpin`. Any embedder byte stream qualifies. |
| `TargetStream` | `client/config.rs:7-8` | `Box<dyn ApplicationStream>` — transport-neutral return type. |
| `TargetContext` | `client/config.rs:10-15` | `{ session_id, connection_id, cancellation: CancellationToken }`. Built per-`Open` in `handle_open` (`client/open.rs:22-26`). Child token of the session token, so session teardown cancels in-flight target dials. |
| `TargetError` | `client/config.rs:17-23` | `Refused` (default TCP maps dial failure here) vs `Failed`. Both map to `TunnelError::Target` at `client/open.rs:29` — reviewer note: the distinction is lost on the wire (both become `OpenReject code=1`). |
| `TargetFuture` | `client/config.rs:25` | Pinned boxed future for `connect`. |
| `TargetConnector` | `client/config.rs:27-30` | `fn connect(&self, service: ClientService, context: TargetContext) -> TargetFuture`. Receives the *trusted client-owned* service; server cannot rewrite it. |
| `TcpTargetConnector` | `client/config.rs:32-43` | Default: `TcpStream::connect((host, port))`, `Refused` on error. |

`docs/EMBEDDING.md:25-31` + `fixtures/embedder`: direct in-process
connectors skip loopback TCP entirely (no socket). The `DuplexEchoConnector`
test double (`server_tests.rs:191-210`) is the canonical example: refuses
unknown service names, otherwise returns a `duplex` echo pair.

---

## 3. Session state machine + control flow

### 3.1 States

```
                 ┌─────────────┐
                 │  Disconnected│◄────────────────────────┐
                 │ (backoff ‖   │                         │
                 │  reconnects++)│                        │
                 └──────┬──────┘                         │
                        │ connect_server (+TLS [+WSS])   │ any fatal error /
                        ▼                               │ Drain / cancel
                 ┌─────────────┐  ClientHello/ServerHello│
                 │ Handshaking │──► Auth/AuthOk ──►      │
                 │ (10 s caps) │    Register* × N        │ Auth/Authz fail:
                 └──────┬──────┘                         │ break, NO reconnect
                        │ success                        │
                        ▼                               │
                 ┌─────────────┐                         │
                 │Streaming    │──► Open/OpenReject ─────┘
                 │(Ping/Pong,  │    per external conn
                 │ Drain,      │
                 │ Unregister) │
                 └─────────────┘
```

### 3.2 Control sequence (annotated)

Normal path through `run_session` (`client.rs:1017-1327`):

1. `ClientHello{ version: CURRENT, capabilities: default }` —
   `handshake_write` (`client.rs:1028-1036`). Each handshake
   read/write is wrapped in `policy.timeouts.handshake` via
   `handshake_read`/`handshake_write` (`client.rs:1353-1372`);
   elapsed maps to `TunnelError::Timeout`.
2. `ServerHello{ version }` — major must equal
   `ProtocolVersion::CURRENT.major`, else `Protocol(UnexpectedMessage)`
   (`client.rs:1037-1045`).
3. `Auth::new(token.expose().to_vec())?` → `Message::Auth`
   (`client.rs:1046-1052`). Token bytes copied out of the redacted
   `SecretToken` only for this frame.
4. `AuthOk{ session_id }` — anything else is `Authentication`
   (`client.rs:1053-1056`). Then `counters.begin_session()` allocates the
   Session generation (`client.rs:1057`); heartbeat counters reset per
   generation (`common.rs:404-414`).
5. Per desired service: `RegisterService{ service_id, name, requested_bind,
   target }` → expect `RegisterAck{ service_id }` with matching id;
   push `(session_id, service_id, effective_bind)` to `counters.binds`
   (`client.rs:1063-1089`). `Message::Error(_)` or any other message →
   `Authorization`. Note: the client sends its `target` on the wire but
   the server must ignore it (target-confusion boundary).
6. Mark connected: `connected=1`, `services=len`,
   `sessions=1`, reset `reconnect_delay` to `policy.timeouts.reconnect_initial`,
   install `CounterGuard(sessions)` which zeroes `sessions` on exit
   (`client.rs:1091-1103`, `client.rs:1374-1403`).
7. Split control stream (`client.rs:1111`), spawn data tasks into
   `JoinSet opens` (`client.rs:1112`), then `select!` loop
   (`client.rs:1121-1299`) with a one-outstanding-probe heartbeat
   (`client/heartbeat.rs:5-41`), a dynamic-registration deadline, and:
    - `cancel` → send `Drain{ deadline_ms: policy.timeouts.relay_drain }` with 250 ms
      cap, then break (`client.rs:1123-1128`).
    - heartbeat tick (`policy.timeouts.heartbeat_interval`, default 20 s) →
      if `HeartbeatState::has_outstanding()`, record a missed heartbeat and
      send no new probe; else `next_nonce()` + `try_send(Ping{ nonce })`
      and `mark_sent` on success (`client.rs:1129-1140`,
      `client/heartbeat.rs:18-29`). Full queue records a missed heartbeat.
    - `read_message` → `Open` / `Ping`→`Pong` / `Pong` (only the matching
      outstanding nonce updates RTT via `counters.record_heartbeat_pong`;
      stale/mismatched nonces are ignored) / dynamic `RegisterAck` /
      `Error` (dynamic registration only — see below) / `Drain` (cancel
      session, break) / anything else →
      `Protocol(UnexpectedMessage)` which tears down the session
      (`client.rs:1157-1240`).
    - `out_rx.recv` (the `policy.limits.control_queue` queue, default 128) →
      `write_message` (`client.rs:1241`). `?` propagates write errors → session teardown.
    - `commands.recv` → `Register` / `Unregister` handling
      (`client.rs:1242-1294`; see §2.3). `Register` checks generation,
      `desired.len() + pending` vs `policy.limits.services_per_session`,
      reply liveness, and `ServiceState::begin` uniqueness/single-flight
      gates before writing `RegisterService` and arming a
      `policy.timeouts.handshake` ack deadline (`client.rs:1244-1282`).
    - `opens.join_next()` → `record_join_result` (panic accounting)
      (`client.rs:1296-1298`).
8. Teardown: `timeout(policy.timeouts.shutdown_grace=1s)` joining open tasks, then
   `abort_all` + drain (`client.rs:1301-1306`), `connected=0`
   (`client.rs:1307-1309`), fail any pending dynamic registration
   (`Disconnected`, or `Cancelled` on local cancel; `Timeout` on ack-deadline
   expiry, `client.rs:1310-1322`), `clear_active`, return `Ok(())`
   (reconnectable) or `Err` (categorized).

`Open` dispatch detail (`client.rs:1159-1193`):

- Unknown `service_id` (lookup in `service_state.active()`) → `rejected++`,
  `record_termination(Authorization)`,
  `try_send(OpenReject{ connection_id, code: 1 })`, continue
  (`client.rs:1160-1166`).
- `semaphore.try_acquire_owned()` fails (`policy.limits.client_open_tasks`
  tasks busy) →
  `rejected++`, `record_termination(ResourceExhausted)`,
  `try_send(OpenReject{ code: 2 })`, continue (`client.rs:1167-1174`).
- Else spawn `handle_open` (`client/open.rs:12-89`) with child cancel token, cloned `out` sender,
  `OpenTaskGuard` (bumps `open_tasks` + high-water, decrements on drop),
  and `OpenContext` (`client.rs:1175-1193`).

Dynamic ack detail (`client.rs:1202-1231`):

- `RegisterAck`: clear the ack deadline, then `service_state.take_ack(id,
  generation)` 4-way disposition (`service_state.rs:96-115`):
  `Unexpected` (no/ mismatched pending) → `Protocol(UnexpectedMessage)`
  session teardown; `Stale` (wrong generation) → reply `Disconnected`;
  `Abandoned` (unregistered/cancelled tombstone) → write
  `UnregisterService` and continue without mutating desired state;
  `Commit` → push binds, `commit()` to active+desired, update
  `services`/high-water, reply `Ok(effective_bind)`.
- `Error`: clear the deadline; if it answers the one pending dynamic
  registration, map via `registration_error` (`client.rs:1345-1351`) and
  reply; else → `Authorization` session teardown. Wire `Error` carries no
  `ServiceId`, hence the single-flight invariant.

`OpenReject` codes are client-originated advisory signals (1 = refused /
unknown / target failure; 2 = overloaded). The client also *receives* no
`OpenReject` — it only sends them.

### 3.3 Reconnect behavior

`reconnect_loop` (`client.rs:664-804`) and `quic_reconnect_loop`
(`client.rs:837-939`):

- Pre-dial: drain pending commands via `apply_disconnected_command`
  (`client.rs:687-689`, `client.rs:864-866`, `client.rs:1329-1343`) —
  `Register` fails `Disconnected` while offline, `Unregister` mutates
  desired state immediately — so reconnect re-registers the pruned set
  from `ServiceState::desired()`.
- Dial: `connect_server` (proxy-aware) → TLS (`policy.timeouts.handshake`
  cap) → optional WSS upgrade (`policy.timeouts.handshake` cap) →
  `run_session` (`client.rs:690-762`). QUIC:
  `QuicClient::connect` (`policy.timeouts.connect`) → `get_connection`
  (`policy.timeouts.connect`) → `open_stream` for control
  (`policy.timeouts.connect`) → `run_session`
  (`client.rs:875-913`).
- **Auth/Authz failures do not reconnect** — `break` after recording
  termination (`client.rs:765-773`, `client.rs:920-928`). Everything else
  records termination, zeroes `connected/services/binds`
  (`client.rs:778-788`), increments `reconnects`
  (`client.rs:792-794`), sleeps `delay + jitter`, doubles `delay`
  (`reconnect_initial → reconnect_max`, `client.rs:800-802`).
- Jitter: `random_jitter_ms` (`client.rs:993-1004`) = uniform
  `[0, min(delay/4, 7500)ms]` via `getrandom`; returns 0 if `max==0` or
  RNG fails (no panic path).
- QUIC teardown closes the QUIC connection and clears
  `handle.quic_client` (`client.rs:918`, `client.rs:937`).
- Successful session resets `delay` to `policy.timeouts.reconnect_initial`
  (`client.rs:1102`).

Review implication: reconnect storms are bounded by backoff + jitter, but
there is no global circuit breaker — a flapping server with valid
credentials reconnects forever until `cancel`. Auth failures are the only
hard stop. See §10.

---

## 4. Data path per `Open`

`handle_open` (`client/open.rs:12-89`) runs once per spawned task (permit +
`OpenTaskGuard` held by the parent future at `client.rs:1189-1193`);
`OpenContext` is defined at `client/open.rs:3-10`:

1. **Target dial first.** Build `TargetContext{ session_id,
   connection_id, cancellation }`, then
   `timeout(policy.timeouts.connect, connector.connect(...))`
   (`client/open.rs:22-30`). Timeout → `Timeout`; connector `Refused` /
   `Failed` → `Target`. All races also select on `cancel` →
   `Cancelled`. Ordering note: the target is dialed *before* the data
   connection — a slow/malicious target holds one of the
   `policy.limits.client_open_tasks` open-task slots without yet consuming
   server pending state.
2. **Data dial.** TCP/TLS profile (`client/open.rs:32-57`):
   `connect_server` (proxy-aware, cancellable) → `tls_connect`
   (`policy.timeouts.handshake`, cancellable) → optional WSS upgrade
   (`policy.timeouts.handshake`, cancellable). QUIC profile
   (`client/open.rs:59-64`): `connection.open_stream()`
   (`policy.timeouts.connect`, cancellable). No custom-CA/mTLS/proxy knobs
   here beyond what the session already validated — the stored
   `ClientDataTransport` (`client.rs:38-49`) carries them.
3. **`DataHello{ session_id, service_id, connection_id }`** via
   `write_boxed` — no timeout wrapper (relies on session cancel +
   transport timeouts), `?` propagates (`client/open.rs:66`). This is the
   last structured message; everything after is opaque bytes.
4. **Relay.** `relay_with_options(target, data,
   RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain))`
   (`client/open.rs:67`). Both `Ok(report)` and `Err(failure)` still credit
   `bytes_upstream/downstream` (`client/open.rs:68-75`) — partial-byte
   accounting survives failures. Relay is full-duplex until EOF/error;
   half-close semantics are transport-dependent (WSS full-close only —
   see `docs/SUPPORT.md:16-20`).
5. **Failure → `OpenReject`.** Any `Err` records its termination
   category and, unless cancelled, `try_send(OpenReject{
   connection_id, code: 1 })` (`client/open.rs:79-88`). If the session is
   already gone the reject is silently dropped — correct, since the
   server has already reaped the pending entry.

Cancellation threads through every await (`cancel.cancelled()` arms at
`client/open.rs:28,34,39,50,61`), and `TargetContext.cancellation`
lets embedder connectors abort early. Session teardown cancels
`session_cancel`, whose children are the per-`Open` tokens
(`client.rs:1175`), so all in-flight dials/relays observe cancellation
promptly.

---

## 5. Concurrency model

| Primitive | Where | Purpose |
|---|---|---|
| Control `split` reader/writer | `client.rs:1111` | Full-duplex control: `reader` only in the `read_message` arm, `writer` only for `out_rx` drain + `Register`/`Unregister` + terminal `Drain`. No lock; single owner task. |
| `JoinSet opens` | `client.rs:1112` | Data-plane tasks (`handle_open`). Reaped via `join_next` inside the loop (`client.rs:1296-1298`) and at teardown (`client.rs:1301-1306`). Panics counted via `record_join_result` → `task_panics++` + `Internal` (`common.rs:428-433`). |
| `Semaphore(policy.limits.client_open_tasks)` + `try_acquire_owned` | `client.rs:1109`, `client.rs:1167-1174` | Admission for `Open` flood (default 128). Non-blocking: overload → immediate `OpenReject code=2`, no queueing. Permit moved into the task (`client.rs:1190`). |
| `mpsc(control_queue)` (`out_tx/out_rx`) | `client.rs:1110` | Outbound control queue (default 128; `Pong`, `Ping`, `OpenReject`). All producers use `try_send` (never block the data plane); drops on full are silent (`let _ =`). Missed-ping accounting still records via `record_heartbeat_missed`. |
| `mpsc(client_command_queue)` commands | `client.rs:337`, `client.rs:427` | `ClientHandle → reconnect_loop`/`run_session` (`Register`/`Unregister`). Drained pre-dial via `apply_disconnected_command`, polled in-session. `send` (async) from the handle; `try_recv` pre-dial, `recv` in-session. |
| `CancellationToken` tree | `client.rs:425`, `client.rs:1113`, `client.rs:1175` | Root `cancel` (handle + owner) → `session_cancel` per session → per-`Open` child. `Drop for Client` cancels root (`client.rs:518-522`). |
| `OpenTaskGuard` / `CounterGuard` | `client.rs:1374-1403` | RAII counters: `open_tasks` inc/high-water + dec on drop; `sessions` zeroed on session exit. Note `CounterGuard::new` ignores its inner value except on drop (`client.rs:1374-1379`) — it always stores 0, even if nested (no nesting occurs today). |
| Heartbeat ticker + `HeartbeatState` | `client.rs:1114-1119`, `client/heartbeat.rs:5-41` | One-outstanding-probe keepalive on `policy.timeouts.heartbeat_interval` (default 20 s); first tick one interval after session start. Nonce wraps (`wrapping_add`). Unanswered ticks increment `missed_heartbeats` without sending; a matching `Pong` records RTT and clears misses (`common.rs:416-426`). `HeartbeatSnapshot` in `Snapshot` carries only generation, last-Pong age, latest RTT, missed count. |

Shutdown ordering (graceful):

1. `Client::shutdown` cancels root (`client.rs:461-467`) *or*
   `ClientHandle::shutdown` cancels without joining
   (`client.rs:82-84`).
2. Session loop observes `cancel` → cancels `session_cancel` (data tasks
   see it), sends `Drain` (250 ms cap), breaks (`client.rs:1123-1128`).
3. `policy.timeouts.shutdown_grace` (default 1 s) join window → `abort_all` → drain
   (`client.rs:1301-1306`).
4. Reconnect loop observes `cancel`, breaks without incrementing
   `reconnects` (`client.rs:789-791`).
5. `Client::shutdown` awaits the reconnect task.

Server-initiated `Drain` flips it: session cancels children and breaks
immediately (`client.rs:1232-1236`), then the same grace applies.
The asymmetry (`policy.timeouts.relay_drain` default 15 s advertised to the server vs 1 s local
grace) is intentional: the server gives the client time, but a locally
shutting-down client does not wait long.

---

## 6. Timeouts / resource limits

There are no `MAX_SERVICES` / `MAX_OPEN_TASKS` / `CONTROL_QUEUE` constants
in `client.rs` anymore. Finite ceilings and lifecycle timeouts come from
`RuntimePolicy` (`common.rs:196-316`), carried by `Counters.policy` and
read at `client.rs`/`client/open.rs` use sites. Defaults preserve the
pre-split runtime (`common.rs:232-301`):

| Policy field | Default | Read via | Meaning |
|---|---|---|---|
| `limits.services_per_session` | 64 | `client.rs:582` (initial `validate_config`), `client.rs:1249` (dynamic `Register` gate) | Empty initial set is valid; `len() > limit` rejected. Dynamic path also counts the one in-flight pending registration. |
| `limits.client_open_tasks` | 128 | `client.rs:1109` (semaphore), `client.rs:871` (QUIC `max_concurrent_streams = +1`) | Concurrent `handle_open` tasks. |
| `limits.control_queue` | 128 | `client.rs:1110` | Outbound control `mpsc` depth. |
| `limits.client_command_queue` | 32 | `client.rs:337`, `client.rs:427` | `ClientHandle →` loop command depth. |
| `timeouts.connect` | 10 s | `client.rs:692` (TCP/proxy dial), `open.rs:29,35,62`, `client.rs:877,890,894` (QUIC) | TCP connect (direct or proxy), target `connect()`, QUIC connect/get/open. |
| `timeouts.handshake` | 10 s | `client.rs:698` (TLS), `client.rs:719-726` (WSS), `client.rs:1028-1077` (control handshake), `client.rs:1266-1282` (dynamic `Register` write + ack deadline) | TLS handshake, WSS upgrade, every control handshake read/write, dynamic registration round-trip. |
| `timeouts.relay_drain` | 15 s | `client/open.rs:67`, `client.rs:1125` | `RelayOptions` drain bound and advertised `Drain.deadline_ms` on local shutdown. |
| `timeouts.shutdown_grace` | 1 s | `client.rs:1301` | Local join window for open tasks after session break. |
| `timeouts.reconnect_initial` → `reconnect_max` | 500 ms → 30 s, ×2 + jitter | `client.rs:675,852`, `client.rs:800-802` | Reset to initial on successful registration (`client.rs:1102`). |
| `timeouts.heartbeat_interval` | 20 s | `client.rs:1114-1117` | One-outstanding-probe `Ping`; matching `Pong` updates RTT, mismatched/stale nonces ignored (`client.rs:1196-1201`). Unanswered ticks increment saturating `missed_heartbeats`. |
| Terminal `Drain` write cap | 250 ms | `client.rs:1126` | Best-effort courtesy on local shutdown. |

`Counters::snapshot()` echoes `policy.limits` as `Snapshot.resource_limits`
(`common.rs:377`) and `HeartbeatSnapshot` as `Snapshot.heartbeat`
(`common.rs:378-388`). `handshake_read`/`handshake_write` map expiry to
`Timeout` (`client.rs:1353-1372`) — reviewers tracing `last_termination`
should expect `Timeout` for a stalled control handshake or dynamic-register
round-trip.

---

## 7. Validation matrix

| Check | Where | Behavior |
|---|---|---|
| Service count vs policy | `client.rs:582-586` | Empty initial set is valid; `len() > policy.limits.services_per_session` → `Configuration("service count exceeds the configured per-session limit")` before any I/O. Dynamic `Register` uses the same limit including the in-flight pending slot (`client.rs:1249`). |
| Unique service IDs + names | `client.rs:587-595` (+ `service_state.rs:64-95` for dynamic) | `Configuration("service IDs and names must be unique")`. Prevents `RegisterAck` aliasing (match on `service_id`, `client.rs:1078`). Dynamic duplicates → `ServiceAlreadyExists`. |
| `server_addr` shape | `client.rs:574-578`, `client.rs:608-626` | `Configuration("server_addr must be a host:port endpoint")`. IPv6 bracket-aware; rejects whitespace hosts, zero ports. |
| `tls_server_name` non-empty, ≤253 | `client.rs:579-581` | `Configuration("TLS server name is invalid")`. |
| `ca_pem` ≤ 1 MiB | `client.rs:596-604` | `Configuration("custom CA bundle exceeds configured size limit")`. |
| QUIC + custom CA / mTLS / proxy | `client.rs:542-548` | `Configuration("Eggress QUIC currently supports platform roots and bearer auth only")` via `validate_client_profile`. Bearer token still required inside the encrypted control stream (`docs/SECURITY.md:46-49`). Legacy per-path QUIC CA guard remains at `start_quic_profile` (`client.rs:330-334`). |
| WSS + mTLS | `client.rs:550-554` | `Configuration("WebSocket transport currently does not support mTLS")` (`docs/SUPPORT.md:7`). No WSS+mTLS constructor exists. |
| Proxy + mTLS | `client.rs:556-560` | `Configuration("outbound proxy mode currently does not support mTLS")`. No `start_with_mtls`+proxy constructor exists — the combination is unconstructable in-process too. |
| Proxy chain syntax | `client.rs:476-482` | `OutboundConnector::from_pproxy_uri` failure → `Configuration("invalid outbound proxy chain")`. Validated in `validate_client_profile` (`client.rs:562-564`) and surfaced through `validate_outbound_proxy` for `check`. |
| QUIC + proxy | `client.rs:542-548`; `docs/SUPPORT.md:8,14-15` | Rejected as part of the QUIC bearer-only guard (proxy traversal unsupported for QUIC). |
| Server + proxy (CLI) | `main.rs:269-271` | `"outbound_proxy is only valid in client mode"`. Proxy types are client-side only (`docs/SECURITY.md:74`). |
| **No silent proxy fallback** | `client.rs:806-834`; `docs/SECURITY.md:79-81` | Proxy errors map to typed variants (`Authentication`/`Authorization`/`Timeout`/`Disconnected` via `OutboundConnectErrorKind`, `client.rs:821-826`); direct TCP is never attempted when a proxy is configured. |
| Typed termination | `common.rs:466-481` | Every reconnect-loop and `handle_open` error records `termination_category()` (`client.rs:770,775,925,930,open.rs:80`): `Cancelled/Timeout/Target/ResourceExhausted/PeerClosed/Auth*/Protocol/Transport/Internal`. `Configuration` → `Internal` (start-time only, never a session outcome). `ServiceAlreadyExists` → `Authorization`. |

mTLS specifics (`client.rs:639-661`, `docs/SECURITY.md:33-40`): system or
configured roots for the server + required client cert/key; empty cert
chain → `Tls`; key/cert parse failures → `Tls` (no detail — avoids
oracle). Private key is zeroized on drop (`client.rs:510-516`) and
redacted in `Debug` (`client.rs:500-508`).

---

## 8. Observability

`ClientHandle::snapshot()` (`client.rs:79-81`) returns
`Counters::snapshot()` (`common.rs:355-395`). Fields the client actually
drives:

| Snapshot field | Updated by client at | Notes |
|---|---|---|
| `connected` | `client.rs:1092-1094` (set 1), `client.rs:778-780,966-968,1307-1309` (set 0) | Bool view over atomic. |
| `registered_services` (`services`) | `client.rs:1095-1098`, `client.rs:1215,1287`, cleared `client.rs:781-783` | Incremented via initial + dynamic commits; decremented on `Unregister`; zeroed on reconnect. High-water via `high_water_services`. |
| `active_sessions` (`sessions`) | `client.rs:1099-1101` + `CounterGuard` zero on exit | Always 0/1 for a client (single session). |
| `effective_binds` | `client.rs:1079-1083,1213`, pruned `client.rs:1288`, cleared `client.rs:784-788` | `(SessionId, ServiceId, EffectiveBind)` — the only place the client learns server-chosen addresses. CLI/`check` flows poll this. |
| `reconnects` | `client.rs:792-794` (+ QUIC `client.rs:951-953` via `record_quic_reconnect`) | Incremented per failed session (not on clean cancel). `client_reconnects…` test asserts `>0`. |
| `rejected_connections` (`rejected`) | `client.rs:1161,1169` (Open-path), indirectly via `handle_open` failures? No — `handle_open` failures record termination but do *not* bump `rejected` | Gap: target/data failures are visible only in `last_termination`, not the reject counter. |
| `bytes_upstream/downstream` | `client/open.rs:68-75` (both success and failure reports) | Bounded `u64` totals; asserted `>0` in roundtrip tests. |
| `active_client_open_tasks` + high-water | `OpenTaskGuard` (`client.rs:1381-1398`) | `fetch_add` + `fetch_max`; asserted `>=1` after relay. |
| `heartbeat` (`HeartbeatSnapshot`) | `record_heartbeat_missed` (`client.rs:1131,1137`), `record_heartbeat_pong` (`client.rs:1199`), reset per generation in `begin_session` (`common.rs:404-414`) | Bounded per-Session view only: `session_generation`, `last_pong_age_ms`, `latest_rtt_ms`, `missed_heartbeats` (`common.rs:162-169`, `common.rs:378-388`). At most one Ping outstanding (`client/heartbeat.rs:5-41`). `docs/EMBEDDING.md:73-76` states the same contract. |
| `last_termination` | `record_termination` at `client.rs:770,775,925,930,1162,1170,open.rs:80` | Includes `Authorization` for unknown-service `Open` and `ResourceExhausted` for overload — reviewers can distinguish the two reject causes here (wire codes 1/2 are not surfaced in `Snapshot`). |
| `task_panics` | `record_join_result` (`client.rs:1296-1298` → `common.rs:428-433`) | `Internal` termination on panic. |
| `resource_limits` | `policy.limits` (`common.rs:377`) | Live ceilings (`sessions:128, services:64, …, client_open_tasks:128, control_queue:128, client_command_queue:32` by default) echoed for operators. |

Fields the client never meaningfully drives: `pending_connections`,
`active_connections`, `active_handshakes` (+ high-waters) stay 0 —
they are server-side. `high_water_services/sessions` likewise. This is
expected but worth knowing when comparing client vs server snapshots.

---

## 9. Test inventory for client behavior

Client-side validation is independently qualified in `client/tests.rs`
(endpoint shape without the `server` feature, duplicate Service identity,
token redaction, plus dynamic `Register`/`Unregister` generation gating,
ack-disposition, timeout, and tombstone cases over a duplex control pair).
`client/qualification_tests.rs` runs the deterministic 10k-step seeded
`ServiceState` sequence (begin/ack/reject/unregister/disconnect invariants).
`client/service_state.rs:149-252` and `client/heartbeat.rs:43-61` hold
focused unit tests (single-flight ack commit, disconnect exclusion,
pending tombstone → `Abandoned`, duplicate id/name rejection, stale ack,
outstanding-probe matching). Cross-transport integration tests are organized
under `server_tests/` by TCP/lifecycle, mTLS, QUIC, WebSocket, and
outbound-proxy behavior. The shared test fixtures live in
`server_tests.rs`; QUIC-only test seams (`start_quic_insecure_for_test`,
`quic_client_for_test`) are `#[cfg(all(test, feature = "quic"))]`.

The end-to-end suite covers registration and relay, custom target connectors,
reconnect restoration, cancellation/resource recovery, authentication and SNI
failure, stale/single-use DataHello handling, mTLS identity binding, transport
replacement/saturation/half-close, WebSocket close/backpressure, and proxy
credential/refusal/timeout/cancellation behavior. Run the complete matrix with
`cargo test --locked --workspace --all-targets --all-features`; CI also runs
per-profile `cargo test` for each supported library feature slice.

## 10. Review checklist

| Risk | Where to look | Status / question for reviewer |
|---|---|---|
| Reconnect storms | `client.rs:675,800-802,993-1004` | Backoff `reconnect_initial→reconnect_max` + jitter bounds a single client, but valid-credential flapping reconnects forever. Confirm deployment guidance (no circuit breaker by design). Auth-fail hard-stop (`client.rs:765-773`) prevents credential-spray loops — verify callers surface `last_termination=Authentication` rather than retrying with new tokens silently. |
| Task leaks | `client.rs:1112,1189-1193,1296-1306` | `JoinSet` reaped in-loop + `shutdown_grace` + `abort_all`. `OpenTaskGuard`/`CounterGuard` are RAII. `repeated_client_server_start_stop…` test asserts zero. Ask: is 1 s grace vs 15 s `relay_drain` the intended asymmetry (fast local exit, slow server courtesy)? Yes per §5, but confirm operators expect truncated relays on Ctrl-C. |
| `Open` flood | `client.rs:1109-1110,1167-1174` | `try_acquire_owned` → `code=2` + `ResourceExhausted`; unknown service → `code=1` + `Authorization`. Both `try_send` — a full control queue silently drops the reject, leaving the server pending entry to expire (30 s). Confirm that tradeoff is acceptable; consider `rejected++` already covers observability even when the frame is dropped — but `last_termination` is overwritten per event, so burst cause is lossy. |
| Target confusion | `client/config.rs:27-43`, `client.rs:1160-1166`, `client/open.rs:22-30` | Server cannot select/rewrite targets; `active` lookup is by `service_id` with `OpenReject code=1` fallback. `DuplexEchoConnector` shows name-checking is the embedder's job. Confirm: `TargetError::Refused` vs `Failed` collapse to one wire code — should operators distinguish "bad name" from "target down"? Currently only `last_termination=Target` either way. |
| Credential handling | `common.rs:19-48`, `client.rs:500-516`, `client.rs:1046-1052` | Token redacted + zeroized; `expose()` crate-only; key zeroized. Proxy creds from env, redacted (`docs/SECURITY.md:74-83`). `handshake_write(Auth)` copies token bytes into one frame — confirm no logging of `Message::Auth` anywhere (wire path uses `write_message`/`write_boxed` with no debug of payload — verified `wire_io.rs:38-57`). |
| Control-queue silence | `client.rs:1135,1164,1172,1195,open.rs:83` | All `try_send` sites ignore full-queue errors. `Ping` loss records `missed_heartbeats` (benign); `Pong`/`OpenReject` loss delays server cleanup to timeouts. Consider a `rejected`-adjacent counter for dropped control frames if this ever matters in review. |
| Timeout categorization | `client.rs:1353-1372` | Handshake/registration stall → `Timeout`. Target/data dial stalls → `Timeout`. Intentional — document when triaging `last_termination`. Dynamic ack-deadline expiry breaks the session with `Timeout` (`client.rs:1147-1156,1274-1278,1310-1326`). |
| QUIC/WSS/mTLS rejections | `client.rs:524-568`, `main.rs:240-245` | Fail-closed with `Configuration` via `ClientBuilder::validate`. Confirm no code path constructs the rejected combos in-process (no WSS+mTLS or proxy+mTLS constructors exist — only builder guards + QUIC CA guard in `start_quic_profile`). |
| `CounterGuard` always-zero | `client.rs:1374-1403` | Drops store 0 unconditionally. Safe today (single session), but a future concurrent-session refactor would silently zero a live counter. Flag if sessions ever multiplex. |
| Target-before-data ordering | `client/open.rs:22-56` | Slow target holds an open-task slot before server state is touched — correct for server protection, but a hung connector starves legitimate `Open`s (`client_open_tasks` slots, `connect` timeout each). `PendingConnector` test proves cancel works; confirm the policy timeout is the right bound for slow app targets. |
| Dynamic registration single-flight | `client/service_state.rs:1-6`, `client.rs:1244-1282,1202-1231` | Wire `Error` has no `ServiceId`, so only one dynamic ack may be outstanding; second `begin` → `ResourceExhausted`. Stale-generation acks → `Disconnected`; cancelled tombstones → `Abandoned` + wire `Unregister`. Confirm operators expect `Timeout` (not `Disconnected`) when the ack deadline fires mid-session. |

---

## Appendix — control + data sequence (text diagram)

```text
client                                            server
  │                                                  │
  │── TCP connect ──────────────────────────────────►│  timeouts.connect 10s default
  │── TLS handshake (SNI=server_name) ──────────────►│  timeouts.handshake 10s default
  │   [WSS: + upgrade, 1 MiB caps / proxy: via chain]│
  │── ClientHello(version, caps) ───────────────────►│
  │◄── ServerHello(version) ─────────────────────────│  major must match
  │── Auth(bearer) ────────────────────────────────►│  inside verified TLS
  │◄── AuthOk(session_id) ───────────────────────────│  else Authentication (no retry)
  │── RegisterService × N ─────────────────────────►│
  │◄── RegisterAck(effective_bind) × N ──────────────│  else Authorization (no retry)
  │                                                  │
  │◄── Open(service, connection) ────────────────────│  per external accept
  │   ├── unknown service → OpenReject(code=1) ─────►│
  │   ├── overloaded → OpenReject(code=2) ──────────►│
  │   └── spawn handle_open:                        │
  │        target dial ──► local target              │  timeouts.connect 10s default
  │        data TLS dial ──────────────────────────►│  (+TLS/handshake 10s / QUIC stream 10s defaults)
  │        DataHello(session, svc, conn) ──────────►│  single-use correlation
  │        relay opaque bytes ◄────────────────────►│  16 KiB bound, drain 15s default
  │        on failure → OpenReject(code=1) ────────►│
  │                                                  │
  │◄── Ping ─── Pong ──► / ── Ping ──► ◄── Pong ─────│  heartbeat_interval 20s default; one outstanding probe, matching Pong updates RTT, stale ignored
  │── Drain(deadline=relay_drain) ──► / ◄── Drain ───│  local(250 ms cap) / remote(break)
  │── RegisterService/UnregisterService ───────────►│  dynamic Register generation-gated, one in flight; Unregister persistent across reconnect
```

Wire framing for every control/data-hello frame: `wire_io.rs:7-57`
(header-first read, length pre-check vs 1 MiB, exact-consumption check).
Relay bytes bypass framing entirely.

### Runtime policy and composition (M008)

`ClientBuilder` is the canonical typed composition path for TCP/TLS, QUIC,
WebSocket, custom connectors, mTLS identity, outbound proxy, and
`RuntimePolicy`. The builder validates profile combinations before startup
(`config.rs:130-140` → `client.rs:524-568`);
`Client::start_*` functions delegate through it. Runtime ceilings and
timeouts are read from the policy carried by `Counters`. The
`policy.limits.control_queue` (default 128) protocol-control and
`policy.limits.client_command_queue` (default 32) handle-command queues
remain separate finite limits.

### Dynamic Services and heartbeat health (M009)

`ClientHandle::register_service` sends a typed command through the bounded
command channel and waits for the current Session's RegisterAck. A connected
session generation is captured at enqueue time; stale commands are rejected.
Only a matching successful acknowledgement appends the Service to the bounded
desired-state vector used by subsequent reconnects. There is one dynamic
registration request in flight at a time because wire Error messages carry no
ServiceId; RegisterAck does carry the ID. Unregister removes desired state and
is sent for the current Session when present. A canceled registration keeps a
bounded tombstone until its response so its late acknowledgement can be
unregistered without mutating desired state.

Heartbeat health stores one outstanding `(nonce, monotonic send time)` per
Session. Matching Pong updates RTT and last-success time and clears consecutive
misses. Unanswered intervals increment a saturating count without sending
additional probes. `Snapshot.heartbeat` contains only the current generation,
last-Pong age, latest RTT, and missed count; no unbounded history is kept.
Tracing events use typed IDs/categories and never format credentials, proxy
chains, or full config objects. The library emits events only and leaves
subscriber setup to the embedder.
