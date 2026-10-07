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

Scope: the private-side, outbound-only initiator. `client.rs` (1238 lines)
is the orchestrator (entry points, `start_profile`, `run_session`,
`validate_client_profile` at `client.rs:569-612`); composable logic
lives in `client/config.rs` (`ClientConfig`, canonical `ClientBuilder`,
target contract), `client/reconnect.rs` (supervisor + `StreamTransport` /
`QuicTransport` dial), `client/service_state.rs` (dynamic-Service lifecycle,
M009), `client/heartbeat.rs` (one-outstanding-Ping probe), and
`client/open.rs` (data path per `Open`). (`endpoint.rs` owns only
`Endpoint::parse` for `host:port` shape — no transport/profile validation.) Tests live in `client/tests.rs`,
`client/qualification_tests.rs` (deterministic 10k-step seeded sequence),
plus unit tests in `service_state.rs`/`heartbeat.rs`; cross-transport E2E
remains under `server_tests/`. The client owns one
authenticated control stream per session, registers local services, and
dials one data connection per server `Open`. It never listens. The server
owns listeners and picks effective binds; the client owns local targets
and picks where bytes land.

> M008/M009 note: `ClientBuilder` (`client/config.rs:84-161`) is the
> canonical composition path (transport/connector/identity/proxy/
> `RuntimePolicy`); `Client::start*` (`client.rs:218-321`,
> `client.rs:412-434`) delegate through it via `validate_client_profile`
> (`client.rs:569-612`). Finite ceilings/timeouts come from `RuntimePolicy`
> (`common.rs:321-333`), read via `counters.policy`, not `client.rs`
> constants. Dynamic `register_service` is generation-gated with one
> registration in flight per Session in legacy-serial mode
> (`client/service_state.rs`); heartbeat health is one outstanding probe plus
> a bounded `HeartbeatSnapshot` (`client/heartbeat.rs`, `common.rs:179-186`).

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
  `docs/SECURITY.md:12-15`, the server ignores the client `Target` as
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

Defined at `crates/eggtunnel/src/client/config.rs:51-60`, redacted `Debug` at
`config.rs:62-72`:

| Field | Type | Notes |
|---|---|---|
| `server_addr` | `String` | `host:port`; DNS via Tokio on connect. Validated by `Endpoint::parse` (`endpoint.rs:42-86`): bracketed IPv6 or `rsplit_once(':')`, non-empty host without whitespace, nonzero `u16` port. |
| `tls_server_name` | `String` | SNI + cert verification name; must be `1..=253` bytes (`client.rs:620-622`). |
| `ca_pem` | `Option<Vec<u8>>` | Custom CA bundle; if absent, system/platform roots. Size-capped at `MAX_FRAME_BYTES` (1 MiB) at `client.rs:637-645`. Rejected for QUIC (see §7). |
| `token` | `SecretToken` | `1..=4096` B, redacted `Debug`, `zeroize` on drop (`common.rs:19-52`). `expose()` is `pub(crate)` only (`common.rs:37-40`). |
| `services` | `Vec<ClientService>` | Initial set may be empty; `len() > policy.limits.services_per_session` (default 64) is rejected (`client.rs:623-627`). IDs and names must be unique (`client.rs:628-636`). |

`ClientService` (`common.rs:55-78`) is the client view: `id: ServiceId`
+ `name: ServiceName` + `requested_bind: RequestedBind` +
`target: TcpTarget`. Contrast `ServiceSpec` (server view, no target —
server receives the bounded `TcpTarget` wire field as non-authoritative registration metadata but never uses it to select or rewrite the client-side destination).

### 2.2 `Client::start*` variants

All constructors require a caller-owned Tokio runtime and validate through
the canonical path. The 12 legacy `Client::start*` async constructors at
`client.rs:218-321` (10) and `client.rs:412-434` (2) are thin
delegates over `ClientBuilder` (`client/config.rs:84-161`); the builder's
`validate()` at `config.rs:136-146` calls `validate_client_profile`
(`client.rs:569-612`), which takes the `RuntimePolicy` and rejects invalid
transport/identity/proxy combinations before any I/O.

| Entry point | Lines | Transport | Notes |
|---|---|---|---|
| `start` | `client.rs:218-220` | TCP+TLS | Default; `TcpTargetConnector`. |
| `start_with_connector` | `client.rs:222-231` | TCP+TLS | Custom `TargetConnector`; builds TLS via `build_tls_config` (`client.rs:649-657`). |
| `start_websocket` / `start_websocket_with_connector` | `client.rs:233-239` / `client.rs:241-251` | WSS | `WebSocket` profile; verified TLS → binary WS upgrade, 1 MiB caps (`client/reconnect.rs:306-334`). |
| `start_with_outbound_proxy` / `..._and_connector` | `client.rs:253-262` / `client.rs:264-275` | TCP+TLS over proxy | Parses `pproxy` URI chain via `parse_outbound_proxy` (`client.rs:520-527`); `OutboundConnector::from_pproxy_uri`, typed `Configuration("invalid outbound proxy chain")` on failure. |
| `start_websocket_with_outbound_proxy` / `..._and_connector` | `client.rs:277-287` / `client.rs:289-301` | WSS over proxy | Composes both adapters. |
| `start_quic` / `start_quic_with_connector` → `start_quic_profile` | `client.rs:303-309` / `client.rs:311-321` → `client.rs:323-389` | QUIC | Shared `drive` + `QuicTransport`; platform roots only, rejects `ca_pem` (`client.rs:334-338`). Test-only `start_quic_insecure_for_test` / `start_quic_insecure_with_connector_for_test` (`client.rs:391-410`) set `insecure=true`. |
| `start_with_mtls` / `start_with_mtls_and_connector` | `client.rs:412-421` / `client.rs:423-434` | TCP+TLS + client cert | `build_mtls_tls_config` (`client.rs:659-682`); bearer token still required (see §7). |

Internal fan-in: `Client::start_profile` (`client.rs:141-216`) validates
then dispatches to `start_with_tls_config`
(`client.rs:436-500`), which spawns `drive` with `StreamTransport` (`client/reconnect.rs:171-222`),
or to `start_quic_profile` (`client.rs:323-389`), which spawns `drive` with `QuicTransport`. Both return immediately with an owner `Client` +
cloneable `ClientHandle`; the background task owns config, connector,
cancel token, counters, and command channel.

`validate_outbound_proxy` (`client.rs:515-518`) is re-exported for CLI
`check` (`lib.rs:21-22`).

### 2.3 `Client` / `ClientHandle`

- `Client` (`client.rs:56-60`): `{ cancel, task: Option<JoinHandle<()>>, handle }`. `Drop` cancels (`client.rs:563-567`).
  - `handle()` (`client.rs:502-504`) clones the handle.
  - `shutdown(mut self)` (`client.rs:506-512`) cancels then awaits the
    reconnect task (join, ignore result). This is the graceful path
    embedders must call (`docs/EMBEDDING.md:15`).
- `ClientHandle` (`client.rs:62-69`): `{ cancel, counters, commands,
  quic_client }` (last field only with `quic-client`).
  - `snapshot()` (`client.rs:83-85`) → `Counters::snapshot()`.
  - `shutdown(&self)` (`client.rs:86-88`) → `cancel.cancel()` (no join;
    fire-and-forget vs `Client::shutdown` which joins).
  - `register_service(service)` (`client.rs:109-130`) → requires
    `connected != 0` else `Disconnected`, then sends
    `ClientCommand::Register{ service, reply }` over the bounded command
    channel and awaits the `oneshot` reply. The handle does *not* snapshot
    the generation: the worker stamps the live Session generation at
    `ServiceState::begin` time (`client.rs:1013-1017`, `client.rs:754-760`),
    so a rotation between the `connected` check and command processing
    cannot spuriously fail a registration a live Session could serve. Only a
    `RegisterAck` matching that generation commits to desired state
    (`service_state.rs:256-283`, `service_state.rs:339-342`); stale
    generations get `Disconnected`, duplicate id/name gets
    `ServiceAlreadyExists`, a second in-flight registration gets
    `ResourceExhausted` (`service_state.rs:187-230`), and wire `Error` maps
    via `registration_error` (`client.rs:1157-1169`: code 1 →
    `ServiceAlreadyExists`, code 5 → `ResourceExhausted`, else
    `Authorization`).
  - `unregister_service(id)` (`client.rs:91-101`) → sends
    `ClientCommand::Unregister{ id, reply }` and awaits the reply;
    `Err(Cancelled)` on local cancel, `Err(Disconnected)` if the loop is
    gone. Semantics: `ServiceState::unregister` (`service_state.rs:322-351`)
    prunes the id from `desired` *and* `active` (so it is not re-registered
    on reconnect) and cancels a matching pending registration with
    `Cancelled`. In legacy-serial mode the transaction is *released*, not
    merely disarmed: the slot is cleared and its generation tombstoned, so
    the late ack resolves as `Abandoned` (`service_state.rs:256-283`) and is
    unregistered on the wire without mutating desired state
    (`client.rs:920-926`). Releasing matters — leaving a reply-less `Some`
    would make every later `begin()` answer `ResourceExhausted` and would let
    the retained deadline end the whole Session. The Session loop
    additionally prunes the `services` counter and `binds` and writes
    `UnregisterService` (`client.rs:1070-1085`). Replies `Ok(())` even if
    the id was unknown (still writes the frame, best-effort).
  - `ClientCommand` (`client.rs:71-80`): `Register{ service, reply }` +
    `Unregister{ id, reply }`, both with `oneshot` replies. In
    legacy-serial mode at most one dynamic `Register` ack may be outstanding
    per Session because wire `Error` carries no `ServiceId`
    (`service_state.rs:3-16`, enforced at `service_state.rs:204-215`).
  - `quic_client_for_test` (`client.rs:132-138`): `#[cfg(all(test,
    feature="quic-client"))]` accessor for assertions.

Command channel depth is `policy.limits.client_command_queue` (default 32;
`client.rs:343` for QUIC, `client.rs:454` for TLS) —
distinct from `policy.limits.control_queue` (default 128, the wire-bound
outbound queue at `client.rs:817`).

### 2.4 Target abstraction

| Type | Location | Role |
|---|---|---|
| `ApplicationStream` | `client/config.rs:3-5` | Blanket impl over `AsyncRead+AsyncWrite+Send+Unpin`. Any embedder byte stream qualifies. |
| `TargetStream` | `client/config.rs:7-8` | `Box<dyn ApplicationStream>` — transport-neutral return type. |
| `TargetContext` | `client/config.rs:10-15` | `{ session_id, connection_id, cancellation: CancellationToken }`. Built per-`Open` in `handle_open` (`client/open.rs:23-27`). Child token of the session token, so session teardown cancels in-flight target dials. |
| `TargetError` | `client/config.rs:17-23` | `Refused` (default TCP maps dial failure here) vs `Failed`. They map to *different* terminal categories at `client/open.rs:30-33`: `Refused` → `TunnelError::Target`, `Failed` → `TunnelError::Io` (`Transport`). Reviewer note: the distinction is still lost on the wire — both become `OpenReject code=1` (`client/open.rs:87-90`). |
| `TargetFuture` | `client/config.rs:25` | Pinned boxed future for `connect`. |
| `TargetConnector` | `client/config.rs:27-30` | `fn connect(&self, service: ClientService, context: TargetContext) -> TargetFuture`. Receives the *trusted client-owned* service; server cannot rewrite it. |
| `TcpTargetConnector` | `client/config.rs:32-49` | Default: `TcpStream::connect((host, port))`, `Refused` on error. |

`docs/EMBEDDING.md:25-31` + `fixtures/embedder`: direct in-process
connectors skip loopback TCP entirely (no socket). The `DuplexEchoConnector`
test double (`server_tests.rs:286-304`) is the canonical example: refuses
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

Normal path through `run_session` (`crates/eggtunnel/src/client.rs:697-1130`):

1. `ClientHello{ version: CURRENT, capabilities: supported }` —
   `handshake_write` (`crates/eggtunnel/src/client.rs:713-722`). The client
   always advertises the full supported set (`[1, 2]`). Each handshake
   read/write is wrapped in `policy.timeouts.handshake` via
   `handshake_read`/`handshake_write` (`crates/eggtunnel/src/client.rs:1183-1200`);
   elapsed maps to `TunnelError::Timeout`.
2. `ServerHello{ version, capabilities }` — major must equal
   `ProtocolVersion::CURRENT.major`, else `Protocol(UnexpectedMessage)`;
   the negotiated set is the strict intersection
   (`negotiate_capabilities`, `crates/eggtunnel/src/client.rs:723-735`,
   `client.rs:1148-1155`).
   Server-claimed extras outside the advertisement are ignored
   (extension behavior stays off; unnegotiated extension messages fail
   closed below).
3. `Auth::new(token.expose().to_vec())?` → `Message::Auth`
   (`crates/eggtunnel/src/client.rs:738-744`). Token bytes copied out of the redacted
   `SecretToken` only for this frame.
4. `AuthOk{ session_id }` — anything else is `Authentication`
   (`crates/eggtunnel/src/client.rs:745-748`). Then `counters.begin_session()` allocates the
   Session generation (`crates/eggtunnel/src/client.rs:749`,
   `common.rs:425-442`); heartbeat counters reset per
   generation (`common.rs:441`). The registration wire mode is set from
   the negotiated set (`set_mode` at `crates/eggtunnel/src/client.rs:750-760`,
   `client/service_state.rs:93-97`):
   `CorrelatedBounded` (ceiling = `client_command_queue`) with capability 1,
   `LegacySerial` otherwise. `set_mode` first fails any transaction left over
   from the previous Session and clears the abandoned tombstones.
5. Per desired service: `RegisterService{ service_id, name, requested_bind,
   target }` → expect `RegisterAck{ service_id }` with matching id;
   push `(session_id, service_id, effective_bind)` to `counters.binds`
   (`crates/eggtunnel/src/client.rs:767-796`, binds push at `:782-786`).
   `Message::Error(_)` or
   `Message::RegisterReject(_)` or any other message → `Authorization`
   (initial registration stays sequential and fail-closed in both modes).
   Note: the client sends its `target` on the wire but
   the server must ignore it (target-confusion boundary).
6. `activate_initial()` (copies desired → `active` so the `Open` lookup
   works), then mark connected: `connected=1`, `services=len`,
   `sessions=1`, reset `reconnect_delay` to `policy.timeouts.reconnect_initial`,
   install `CounterGuard(sessions)` which zeroes `sessions` on exit
   (`crates/eggtunnel/src/client.rs:798-815`).
7. Split control stream (`crates/eggtunnel/src/client.rs:818`), spawn data tasks into
   `JoinSet opens` (`crates/eggtunnel/src/client.rs:819`), then `select!` loop
   (`crates/eggtunnel/src/client.rs:828-1092`) with a one-outstanding-probe heartbeat
   (`client/heartbeat.rs:5-43`), a per-transaction ack-deadline arm, and:
    - `cancel` → send `Drain{ deadline_ms: policy.timeouts.relay_drain }` with 250 ms
      cap, then break (`crates/eggtunnel/src/client.rs:834-839`).
    - heartbeat tick (`policy.timeouts.heartbeat_interval`, default 20 s) →
      if `HeartbeatState::has_outstanding()`, record a missed heartbeat and
      send no new probe; else `next_nonce()` + `try_send(Ping{ nonce })`
      and `mark_sent` on success (`crates/eggtunnel/src/client.rs:840-851`,
      `client/heartbeat.rs:18-31`). Full queue records a missed heartbeat.
    - registration-deadline arm → `expire_overdue`; `Expiry::Legacy` breaks
      the loop and ends the Session with `Timeout`, `Expiry::Correlated(n)`
      fails only the n overdue callers (`crates/eggtunnel/src/client.rs:852-870`,
      `client/service_state.rs:143-174`).
    - `read_message` → `Open` / `Ping`→`Pong` / `Pong` (only the matching
      outstanding nonce updates RTT via `counters.record_heartbeat_pong`;
      stale/mismatched nonces are ignored, `client.rs:910-915`) / dynamic
      `RegisterAck` / `Error` (legacy-correlated per mode — see below) /
      `RegisterReject` (capability-1 only, else
      `Protocol(UnexpectedMessage)`) / `Drain` (capture peer deadline, cancel
      session, break) / anything else → `Protocol(UnexpectedMessage)` which
      tears down the session (`crates/eggtunnel/src/client.rs:871-996`).
    - `out_rx.recv` (the `policy.limits.control_queue` queue, default 128) →
      `write_control` (`client.rs:1171-1181`), a
      `timeout(policy.timeouts.handshake, …)` wrapper. `?` propagates write
      errors → session teardown (`crates/eggtunnel/src/client.rs:997`).
    - `commands.recv` → `Register` / `Unregister` handling
      (`crates/eggtunnel/src/client.rs:998-1087`). `Register` checks
      `desired.len() + unacknowledged` vs
      `policy.limits.services_per_session`, reply liveness, and
      `ServiceState::begin` uniqueness/mode-ceiling gates before writing
      `RegisterService` and arming a per-transaction
      `policy.timeouts.handshake` ack deadline
      (`crates/eggtunnel/src/client.rs:1000-1069`).
      A write timeout fails just that transaction in correlated mode
      (`abandon`, `crates/eggtunnel/src/client.rs:1049`) but ends the Session
      in legacy mode (`client.rs:1037-1040`), preserving 1.0 behavior.
    - `opens.join_next()` → `record_join_result` (panic accounting)
      (`crates/eggtunnel/src/client.rs:1088-1090`).
8. Teardown: join open tasks up to the effective drain wait — `min(peer
   deadline, shutdown_grace)` when capability 2 was negotiated and the
   server asked us to drain, else exactly `shutdown_grace` (1.0 timing,
   `crates/eggtunnel/src/client.rs:1093-1108`) — then `abort_all` + drain,
   `connected=0`, fail pending transactions
   (`Disconnected`, or `Cancelled` on local cancel; per-transaction
   `Timeout` on ack-deadline expiry via `expire_overdue` and
   `finish_pending`, `client/service_state.rs:143-174`,
   `client/service_state.rs:346-358`), `clear_active`,
   return `Ok(())` (reconnectable) or `Err(Timeout)` when a legacy
   registration deadline fired (categorized).

`Open` dispatch detail (`crates/eggtunnel/src/client.rs:873-888`):

- Unknown `service_id` (lookup in `service_state.active()`) → `rejected++`,
  `record_termination(Authorization)`,
  `try_send(OpenReject{ connection_id, code: 1 })`, continue
  (`crates/eggtunnel/src/client.rs:874-880`).
- `semaphore.try_acquire_owned()` fails (`policy.limits.client_open_tasks`
  tasks busy) →
  `rejected++`, `record_termination(ResourceExhausted)`,
  `try_send(OpenReject{ code: 2 })`, continue (`crates/eggtunnel/src/client.rs:881-888`).
- Else spawn `handle_open` (`client/open.rs:13-93`) with child cancel token, cloned `out` sender,
  `OpenTaskGuard` (bumps `open_tasks` + high-water, decrements on drop),
  and `OpenContext` (`crates/eggtunnel/src/client.rs:889-907`).

Dynamic ack detail (`crates/eggtunnel/src/client.rs:916-954`):

- `RegisterAck`: `service_state.take_ack(id,
  generation)` 4-way disposition (`service_state.rs:256-283`):
  `Unexpected` (no/mismatched transaction) → `Protocol(UnexpectedMessage)`
  session teardown; `Stale` (wrong generation) → reply `Disconnected`;
  `Abandoned` (dropped caller, write-timeout tombstone, or unregistered
  legacy pending) → write `UnregisterService` and continue without mutating
  desired state; `Commit` → push binds, `commit()` to active+desired, update
  `services`/high-water, reply `Ok(effective_bind)`.
- `Error`: if it answers the legacy pending registration, map via
  `registration_error` (`crates/eggtunnel/src/client.rs:947-953`,
  shared code vocabulary at `:1161-1169`) and reply; else →
  `Authorization` session teardown. Wire `Error` carries no
  `ServiceId`, hence the legacy single-flight invariant: in correlated mode
  a generic `Error` cannot be attributed, so the server sends
  `RegisterReject` instead and a generic `Error` there is a protocol
  violation that fails the Session closed.
- `RegisterReject`: without negotiated capability 1 → fail closed
  (`Protocol(UnexpectedMessage)`). With it, `take_reject(id,
  generation)` (`service_state.rs:284-300`): `Reject` → map the shared
  code vocabulary and reply; `Stale` → reply `Disconnected`; `Unknown`
  (no transaction for this Service in this generation) → fail closed.
  Ack deadlines are per-transaction (`next_deadline` /
  `expire_overdue`); a correlated timeout fails only its caller while
  legacy timeout ends the Session.

Ordering subtlety: in correlated mode `unregister` removes an in-flight
transaction outright without leaving a tombstone
(`client/service_state.rs:344-348`), so a late `RegisterAck`/`RegisterReject`
for it is `Unexpected`/`Unknown` and fails the Session closed. The
write-timeout path is the one that tombstones (`abandon`,
`client/service_state.rs:310-321`, bounded at 256 entries and cleared per
Session switch). `unregister` tombstones too, but only for the legacy
`pending` slot (`client/service_state.rs:322-343`), whose late ack must
therefore be `Abandoned` rather than `Unexpected`.

`OpenReject` codes are client-originated advisory signals (1 = refused /
unknown / target failure; 2 = overloaded). The client also *receives* no
`OpenReject` — it only sends them.

### 3.3 Reconnect behavior

One shared supervisor drives every transport: `drive` (`client/reconnect.rs:171-222`)
with the private `Transport` adapter (`client/reconnect.rs:45-52`) and
`ReconnectSupervisor` (`client/reconnect.rs:55-159`):

- Pre-dial: drain pending commands via `apply_disconnected_command`
  (`client.rs:1132-1146`, invoked from `drain_disconnected_commands` at
  `client/reconnect.rs:81-86` and re-applied while a dial is in flight at
  `client/reconnect.rs:186-193`) —
  `Register` fails `Disconnected` while offline, `Unregister` mutates
  desired state immediately — so reconnect re-registers the pruned set
  from `ServiceState::desired()`.
- Dial is transport-owned: `StreamTransport::establish`
  (`client/reconnect.rs:293-347`, proxy-aware `connect_tcp` at
  `client/reconnect.rs:352-380` → TLS (`policy.timeouts.handshake`
  cap) → optional WSS upgrade (`policy.timeouts.handshake` cap));
  `QuicTransport::establish` (`client/reconnect.rs:424-467`:
  `QuicClient::connect` → `get_connection` → `open_stream` for control, all
  three inside one `timeout(policy.timeouts.connect, …)` budget at
  `client/reconnect.rs:461-463`). Both hand the established control stream to
  the shared `run_session`.
- **Auth/Authz/local-exhaustion failures do not reconnect** — `Terminal` after
  recording termination (`client/reconnect.rs:108-122`). `ResourceExhausted`
  joins that set because it is a local, unrecoverable condition (a spent
  Session generation counter): retrying it would back off forever and never
  surface the error. Everything else
  records termination, zeroes `connected/services/binds`
  (`client/reconnect.rs:95-107`), then backs off —
  sleep `min(delay + jitter, reconnect_max)`, increment `reconnects`, double
  `delay` (`reconnect_initial → reconnect_max`,
  `client/reconnect.rs:139-158`). A cancelled sleep returns early and is
  never counted as a reconnect.
- Jitter: `random_jitter_ms` (`client/reconnect.rs:226-245`) = uniform
  `[0, min(delay/4, 7500)ms]` via `getrandom`; returns 0 if `max==0` and
  falls back to `max` if the RNG is unavailable (no panic path).
- QUIC teardown closes the QUIC connection and clears
  `handle.quic_client` (`client/reconnect.rs:469-474`).
- Successful session resets `delay` to `policy.timeouts.reconnect_initial`
  (`client.rs:809`).

Review implication: reconnect storms are bounded by backoff + jitter, but
there is no global circuit breaker — a flapping server with valid
credentials reconnects forever until `cancel`. Auth failures are the only
hard stop. See §10.

---

## 4. Data path per `Open`

`handle_open` (`client/open.rs:13-93`) runs once per spawned task (permit +
`OpenTaskGuard` moved into the task at `client.rs:903-907`);
`OpenContext` is defined at `client/open.rs:4-11`:

1. **Target dial first.** Build `TargetContext{ session_id,
   connection_id, cancellation }`, then
   `timeout(policy.timeouts.connect, connector.connect(...))`
   (`client/open.rs:22-34`). Timeout → `Timeout`; connector `Refused` →
   `Target`, `Failed` → `Io`. All races also select on `cancel` →
   `Cancelled`. Ordering note: the target is dialed *before* the data
   connection — a slow/malicious target holds one of the
   `policy.limits.client_open_tasks` open-task slots without yet consuming
   server pending state.
2. **Data dial.** TCP/TLS profile (`client/open.rs:35-61`):
   `connect_tcp` (proxy-aware, cancellable) → `tls_connect`
   (`policy.timeouts.handshake`, cancellable) → optional WSS upgrade
   (`policy.timeouts.handshake`, cancellable). QUIC profile
   (`client/open.rs:62-68`): `connection.open_stream()`
   (`policy.timeouts.connect`, cancellable). No custom-CA/mTLS/proxy knobs
   here beyond what the session already validated — the stored
   `ClientDataTransport` (`client.rs:42-54`) carries them.
3. **`DataHello{ session_id, service_id, connection_id }`** via
   `write_boxed` under `timeout(policy.timeouts.handshake, …)`; expiry →
   `Timeout` (`client/open.rs:70-71`). This is the
   last structured message; everything after is opaque bytes.
4. **Relay.** `relay_with_options(target, data,
   RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain))`
   (`client/open.rs:72`). Both `Ok(report)` and `Err(failure)` still credit
   `bytes_upstream/downstream` (`client/open.rs:73-80`) — partial-byte
   accounting survives failures. Relay is full-duplex until EOF/error;
   half-close semantics are transport-dependent (WSS full-close only —
   see `docs/SUPPORT.md:31-32`).
5. **Failure → `OpenReject`.** Any `Err` is traced with its
   `termination_category()` and, unless cancelled,
   `try_send(OpenReject{ connection_id, code: 1 })` (`client/open.rs:84-92`).
   `handle_open` deliberately does *not* call `record_termination` or bump
   `rejected`, so a data-path failure is visible only in the debug trace
   and the wire reject — see the gap in §8. If the session is
   already gone the reject is silently dropped — correct, since the
   server has already reaped the pending entry.

Cancellation threads through every await (`cancel.cancelled()` arms at
`client/open.rs:29,38,43,54,65`), and `TargetContext.cancellation`
lets embedder connectors abort early. Session teardown cancels
`session_cancel` (`client.rs:820`), whose children are the per-`Open` tokens
(`client.rs:889`), so all in-flight dials/relays observe cancellation
promptly.

---

## 5. Concurrency model

| Primitive | Where | Purpose |
|---|---|---|
| Control `split` reader/writer | `client.rs:818` | Full-duplex control: `reader` only in the `read_message` arm, `writer` only for `out_rx` drain + `Register`/`Unregister` + terminal `Drain`. No lock; single owner task. Every one of those writes goes through `write_control` (`client.rs:1171-1181`), a `timeout(policy.timeouts.handshake, …)` wrapper, so a server that stops reading cannot wedge the whole `select!` loop. |
| `JoinSet opens` | `client.rs:819` | Data-plane tasks (`handle_open`). Reaped via `join_next` inside the loop (`client.rs:1088-1090`) and at teardown (`client.rs:1103-1108`). Panics counted via `record_join_result` → `task_panics++` + `Internal` (`common.rs:463-468`). |
| `Semaphore(policy.limits.client_open_tasks)` + `try_acquire_owned` | `client.rs:816`, `client.rs:881-888` | Admission for `Open` flood (default 128). Non-blocking: overload → immediate `OpenReject code=2`, no queueing. Permit moved into the task (`client.rs:904`). |
| `mpsc(control_queue)` (`out_tx/out_rx`) | `client.rs:817` | Outbound control queue (default 128; `Pong`, `Ping`, `OpenReject`). All producers use `try_send` (never block the data plane); drops on full are silent (`let _ =`). Missed-ping accounting still records via `record_heartbeat_missed`. The single consumer (`out_rx`) is drained in the `select!` arm through `write_control`, whose timeout error ends the Session. |
| `mpsc(client_command_queue)` commands | `client.rs:343`, `client.rs:454` | `ClientHandle →` `drive`/`run_session` (`Register`/`Unregister`). Drained pre-dial via `apply_disconnected_command`, polled in-session. `send` (async) from the handle; `try_recv` pre-dial, `recv` in-session. Its bound is also the `CorrelatedBounded` in-flight ceiling (`client.rs:754-757`). |
| `CancellationToken` tree | `client.rs:341`, `client.rs:820`, `client.rs:889` | Root `cancel` (handle + owner) → `session_cancel` per session → per-`Open` child. `Drop for Client` cancels root (`client.rs:563-567`). |
| `OpenTaskGuard` / `CounterGuard` | `client.rs:1202-1231` | RAII counters: `open_tasks` inc/high-water + dec on drop; `sessions` zeroed on session exit. Note `CounterGuard::new` ignores its inner value except on drop (`client.rs:1202-1207`) — it always stores 0, even if nested (no nesting occurs today). |
| Heartbeat ticker + `HeartbeatState` | `client.rs:821-825`, `client/heartbeat.rs:5-43` | One-outstanding-probe keepalive on `policy.timeouts.heartbeat_interval` (default 20 s); first tick one interval after session start. Nonce wraps (`wrapping_add`). Unanswered ticks increment `missed_heartbeats` without sending; a matching `Pong` records RTT and clears misses (`common.rs:450-461`). `HeartbeatSnapshot` in `Snapshot` carries only generation, last-Pong age, latest RTT, missed count (`common.rs:179-186`). |

Shutdown ordering (graceful):

1. `Client::shutdown` cancels root (`client.rs:506-512`) *or*
   `ClientHandle::shutdown` cancels without joining
   (`client.rs:86-88`).
2. Session loop observes `cancel` → cancels `session_cancel` (data tasks
   see it), sends `Drain` (250 ms cap), breaks (`client.rs:834-839`).
3. `policy.timeouts.shutdown_grace` (default 1 s) join window → `abort_all` → drain
   (`client.rs:1093-1108`).
4. Reconnect loop observes `cancel`, breaks without incrementing
   `reconnects` (`client/reconnect.rs:139-142`, `client/reconnect.rs:177-179`).
5. `Client::shutdown` awaits the reconnect task.

Server-initiated `Drain` flips it: session cancels children and breaks
immediately (`client.rs:987-992`), then the same grace applies.
The asymmetry (`policy.timeouts.relay_drain` default 15 s advertised to the server vs 1 s local
grace) is intentional: the server gives the client time, but a locally
shutting-down client does not wait long.

---

## 6. Timeouts / resource limits

There are no `MAX_SERVICES` / `MAX_OPEN_TASKS` / `CONTROL_QUEUE` constants
in `client.rs` anymore. Finite ceilings and lifecycle timeouts come from
`RuntimePolicy` (`common.rs:321-333`), carried by `Counters.policy` and
read at `client.rs`/`client/open.rs` use sites. Defaults preserve the
pre-split runtime (`common.rs:249-263` limits, `common.rs:305-319`
timeouts):

| Policy field | Default | Read via | Meaning |
|---|---|---|---|
| `limits.services_per_session` | 64 | `client.rs:623-627` (initial `validate_config`), `client.rs:1001-1009` (dynamic `Register` gate) | Empty initial set is valid; `len() > limit` rejected. The dynamic gate is stricter — `desired.len() + unacknowledged >= limit` — and so counts every in-flight transaction. |
| `limits.client_open_tasks` | 128 | `client.rs:816` (semaphore), `client/reconnect.rs:440` (QUIC `max_concurrent_streams`, seeded at `client.rs:356`) | Concurrent `handle_open` tasks. |
| `limits.control_queue` | 128 | `client.rs:817` | Outbound control `mpsc` depth. |
| `limits.client_command_queue` | 32 | `client.rs:343,454`, `client.rs:754-757` (correlated ceiling) | `ClientHandle →` loop command depth. |
| `timeouts.connect` | 10 s | `client/reconnect.rs:295-298` (TCP/proxy dial), `client/open.rs:30,39,66`, `client/reconnect.rs:461-463` (QUIC) | TCP connect (direct or proxy), target `connect()`, QUIC connect/get/open. |
| `timeouts.handshake` | 10 s | `client/reconnect.rs:299-304` (TLS), `client/reconnect.rs:314-323` (WSS), `client.rs:714-748` (control handshake), `client.rs:1027-1031,1055-1056` (dynamic `Register` write + ack deadline), `client.rs:1171-1181` `write_control` (in-loop `Unregister` + `out_rx` drain), `client/open.rs:70-71` (`DataHello`) | TLS handshake, WSS upgrade, every control handshake read/write, dynamic registration round-trip, `DataHello` write, and every in-loop control write so a wedged peer cannot stall the loop. |
| `timeouts.relay_drain` | 15 s | `client/open.rs:72`, `client.rs:836-837` | `RelayOptions` drain bound and advertised `Drain.deadline_ms` on local shutdown. |
| `timeouts.shutdown_grace` | 1 s | `client.rs:1098-1102` | Local join window for open tasks after session break. |
| `timeouts.reconnect_initial` → `reconnect_max` | 500 ms → 30 s, ×2 + jitter | `client/reconnect.rs:139-158`, `client/reconnect.rs:226-245` | Reset to initial on a ready Session (`client.rs:809`). |
| `timeouts.heartbeat_interval` | 20 s | `client.rs:821-824` | One-outstanding-probe `Ping`; matching `Pong` updates RTT, mismatched/stale nonces ignored (`client.rs:910-915`). Unanswered ticks increment saturating `missed_heartbeats`. |
| `timeouts.control_idle` | 90 s | `client/reconnect.rs:439` (QUIC `idle_timeout` only) | QUIC connection idle timeout; the TCP/TLS path has no read deadline. |
| Terminal `Drain` write cap | 250 ms | `client.rs:837` | Best-effort courtesy on local shutdown. |

`Counters::snapshot()` echoes `policy.limits` as `Snapshot.resource_limits`
(`common.rs:394`) and `HeartbeatSnapshot` as `Snapshot.heartbeat`
(`common.rs:395-409`). `handshake_read`/`handshake_write` map expiry to
`Timeout` (`client.rs:1183-1200`) — reviewers tracing `last_termination`
should expect `Timeout` for a stalled control handshake or dynamic-register
round-trip.

---

## 7. Validation matrix

| Check | Where | Behavior |
|---|---|---|
| Service count vs policy | `client.rs:623-627` | Empty initial set is valid; `len() > policy.limits.services_per_session` → `Configuration("service count exceeds the configured per-session limit")` before any I/O. Dynamic `Register` gates on `desired.len() + unacknowledged >= limit` (`client.rs:1001-1009`). |
| Unique service IDs + names | `client.rs:628-636` (+ `service_state.rs:187-200` for dynamic) | `Configuration("service IDs and names must be unique")`. Prevents `RegisterAck` aliasing (match on `service_id`, `client.rs:917`). Dynamic duplicates → `ServiceAlreadyExists`. |
| `server_addr` shape | `client.rs:618-619`, `endpoint.rs:42-86` | `Configuration("server_addr must be a host:port endpoint")`. IPv6 bracket-aware; rejects whitespace hosts, zero ports. |
| `tls_server_name` non-empty, ≤253 | `client.rs:620-622` | `Configuration("TLS server name is invalid")`. |
| `ca_pem` ≤ 1 MiB | `client.rs:637-645` | `Configuration("custom CA bundle exceeds configured size limit")`. |
| QUIC + custom CA / mTLS / proxy | `client.rs:587-594` | `Configuration("Eggress QUIC currently supports platform roots and bearer auth only")` via `validate_client_profile` — all three offenders share one guard. Bearer token still required inside the encrypted control stream (`docs/SECURITY.md:67-68`). Legacy per-path QUIC CA guard remains at `start_quic_profile` (`client.rs:334-338`). |
| WSS + mTLS | `client.rs:595-600` | `Configuration("WebSocket transport currently does not support mTLS")` (`docs/SUPPORT.md:23`). No WSS+mTLS constructor exists. |
| Proxy + mTLS | `client.rs:601-606` | `Configuration("outbound proxy mode currently does not support mTLS")`. No `start_with_mtls`+proxy constructor exists — the combination is unconstructable in-process too. |
| Proxy chain syntax | `client.rs:520-527` | `OutboundConnector::from_pproxy_uri` failure → `Configuration("invalid outbound proxy chain")`. Validated in `validate_client_profile` (`client.rs:607-610`) and surfaced through `validate_outbound_proxy` for `check`. |
| QUIC + proxy | `client.rs:587-594`; `docs/SUPPORT.md:24,30-31` | Rejected as part of the QUIC bearer-only guard (proxy traversal unsupported for QUIC). |
| Server + proxy (CLI) | `main.rs:668-673` | `"outbound_proxy is only valid in client mode"`. Proxy types are client-side only (`docs/SECURITY.md:93`). |
| **No silent proxy fallback** | `client/reconnect.rs:352-380`; `docs/SECURITY.md:98-99` | Proxy errors map to typed variants (`Authentication`/`Authorization`/`Timeout`/`Disconnected` via `OutboundConnectErrorKind`, `client/reconnect.rs:364-369`); `connect_tcp` returns early on the proxy branch, so direct TCP is never attempted when a proxy is configured. |
| Typed termination | `common.rs:502-518` | Every supervisor outcome records `termination_category()` (`client/reconnect.rs:119,124`), but `handle_open` does not (`client/open.rs:84-92` only traces it): `Cancelled/Timeout/Target/ResourceExhausted/PeerClosed/Auth*/Protocol/Transport/Internal`. `Configuration` → `Internal` and is Terminal for the supervisor (`client/reconnect.rs:112-117`), so it can be a session outcome, not just start-time. `ServiceAlreadyExists` → `Authorization`. |

mTLS specifics (`client.rs:659-682`, `docs/SECURITY.md:52-59`): system or
configured roots for the server + required client cert/key; empty cert
chain → `Tls`; key/cert parse failures → `Tls` (no detail — avoids
oracle). Private key is zeroized on drop (`client.rs:555-561`) and
redacted in `Debug` (`client.rs:545-553`).

---

## 8. Observability

`ClientHandle::snapshot()` (`client.rs:83-85`) returns
`Counters::snapshot()` (`common.rs:372-415`). Fields the client actually
drives:

| Snapshot field | Updated by client at | Notes |
|---|---|---|
| `connected` | `client.rs:799-801` (set 1), cleared `client/reconnect.rs:97-99` (set 0) | Bool view over atomic. |
| `registered_services` (`services`) | `client.rs:802-805`, `client.rs:931,1073`, cleared `client/reconnect.rs:100-102` | Set from `active.len()` on initial ready + dynamic commits; decremented on `Unregister`; zeroed on reconnect. High-water via `high_water_services` (`client.rs:932`). |
| `active_sessions` (`sessions`) | `client.rs:806-808` + `CounterGuard` zero on exit | Always 0/1 for a client (single session). |
| `effective_binds` | `client.rs:782-784,927-929`, pruned `client.rs:1074`, cleared `client/reconnect.rs:103` | `(SessionId, ServiceId, EffectiveBind)` — the only place the client learns server-chosen addresses. Copy-on-write behind the mutex (`common.rs:376-388`), so the snapshot copies outside the lock. |
| `reconnects` | `client/reconnect.rs:150-152` (shared `backoff`, all transports) | Incremented per failed session (not on clean cancel). `client_reconnects_and_restores_services_in_a_new_session_generation` (`server_tests/tcp.rs:1180`) asserts `>0`. |
| `rejected_connections` (`rejected`) | `client.rs:873,881` (Open-path unknown-service + overload), `client/open.rs:96-97` (`OpenReject` that could not be queued) | A target/data-dial failure still does *not* bump `rejected` — it traces and sends `OpenReject code=1`. The one exception is delivery failure: a full control queue means the server keeps its pending entry and admission permit until `pending_connection` expires, so that drop is now counted (and logged as `open_reject_dropped`) rather than silent. A *delivered* reject is not counted. |
| `bytes_upstream/downstream` | `client/open.rs:74-79` (both success and failure reports) | Bounded `u64` totals; asserted `>0` in roundtrip tests. |
| `active_client_open_tasks` + high-water | `OpenTaskGuard` (constructed `client.rs:891-894`, guard `client.rs:1209-1226`) | `fetch_add` + `fetch_max`; asserted `>=1` after relay. |
| `heartbeat` (`HeartbeatSnapshot`) | `record_heartbeat_missed` (`client.rs:842, 848`), `record_heartbeat_pong` (`client.rs:913`), reset per generation in `begin_session` (`common.rs:441`) | Bounded per-Session view only: `session_generation`, `last_pong_age_ms`, `latest_rtt_ms`, `missed_heartbeats` (`common.rs:179-186`, `common.rs:395-409`). At most one Ping outstanding (`client/heartbeat.rs:5-43`). `docs/EMBEDDING.md:80-82` states the same contract. |
| `last_termination` | `record_termination` at `client.rs:876, 884` (Open reject causes) and `client/reconnect.rs:119, 124` (every session outcome) | Includes `Authorization` for unknown-service `Open` and `ResourceExhausted` for overload — reviewers can distinguish the two reject causes here (wire codes 1/2 are not surfaced in `Snapshot`). Data-path failures from `handle_open` never reach this field. |
| `task_panics` | `record_join_result` (`client.rs:1088-1090` → `common.rs:463-468`) | `Internal` termination on panic. |
| `resource_limits` | `policy.limits` (`common.rs:394`) | Live ceilings (`sessions:128, services:64, …, client_open_tasks:128, control_queue:128, client_command_queue:32` by default) echoed for operators. |

Fields the client never meaningfully drives: `pending_connections`,
`active_connections`, `active_handshakes` (+ their high-waters) stay 0 —
they are server-side. `high_water_sessions` is likewise never bumped (no
per-session high-water on the client), though `high_water_services` is
(`client.rs:932`). This is expected but worth knowing when comparing client
vs server snapshots.

---

## 9. Test inventory for client behavior

Client-side validation is independently qualified in `client/tests.rs`
(endpoint shape without the `server` feature, duplicate Service identity,
token redaction, builder/profile rejection cases, plus dynamic
`Register`/`Unregister` generation gating, correlated vs legacy in-flight
rules, ack-deadline timeout, drain-deadline, bounded control writes, and
tombstone cases over a duplex control pair).
`client/qualification_tests.rs` runs the deterministic 10k-step seeded
`ServiceState` sequence (`deterministic_service_state_sequence_preserves_invariants_for_10000_steps`,
begin/ack/reject/unregister/disconnect invariants).
`client/service_state.rs:394-595` and `client/heartbeat.rs:45-78` hold
focused unit tests (single-flight ack commit, disconnect exclusion,
pending tombstone → `Abandoned`, duplicate id/name rejection, stale ack,
correlated ceiling and out-of-order correlation, outstanding-probe
matching). `client/reconnect.rs:477-602` holds the reconnect unit tests
(backoff progression, cancel-during-sleep not counted, terminal
Auth/Authz/`ResourceExhausted` disposition, jitter bound).
Cross-transport integration tests are organized
under `server_tests/` by TCP/lifecycle, mTLS, QUIC, WebSocket, and
outbound-proxy behavior. The shared test fixtures live in
`server_tests.rs`; QUIC-only test seams (`start_quic_insecure_for_test`,
`quic_client_for_test`) are `#[cfg(all(test, feature = "quic-client"))]`.

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
| Reconnect storms | `client/reconnect.rs:139-158, 226-245` | Backoff `reconnect_initial→reconnect_max` + jitter bounds a single client, but valid-credential flapping reconnects forever. Confirm deployment guidance (no circuit breaker by design). Auth-fail hard-stop (`client/reconnect.rs:108-122`) prevents credential-spray loops — verify callers surface `last_termination=Authentication` rather than retrying with new tokens silently. |
| Task leaks | `client.rs:819, 903-907, 1093-1108` | `JoinSet` reaped in-loop + `shutdown_grace` + `abort_all`. `OpenTaskGuard`/`CounterGuard` are RAII. `repeated_client_server_start_stop_returns_runtime_counts_to_zero` (`server_tests/tcp.rs:1377`) asserts zero. Ask: is 1 s grace vs 15 s `relay_drain` the intended asymmetry (fast local exit, slow server courtesy)? Yes per §5, but confirm operators expect truncated relays on Ctrl-C. |
| `Open` flood | `client.rs:816, 881-888` | `try_acquire_owned` → `code=2` + `ResourceExhausted`; unknown service → `code=1` + `Authorization`. Both `try_send` — a full control queue silently drops the reject, leaving the server pending entry to expire (`pending_connection`, 30 s). Confirm that tradeoff is acceptable; consider `rejected++` already covers observability even when the frame is dropped — but `last_termination` is overwritten per event, so burst cause is lossy. |
| Target confusion | `client/config.rs:27-49`, `client/reconnect.rs:337-344`, `client/open.rs:22-34` | Server cannot select/rewrite targets; `active` lookup is by `service_id` with `OpenReject code=1` fallback. `DuplexEchoConnector` shows name-checking is the embedder's job. Confirm: `TargetError::Refused` vs `Failed` collapse to one wire code but not to one category (`Target` vs `Transport`) — and neither reaches `last_termination`, so neither is visible in `Snapshot`. |
| Credential handling | `common.rs:19-52`, `client.rs:649-657`, `client/reconnect.rs:299-304` | Token redacted + zeroized; `expose()` crate-only; key zeroized. Proxy creds from env, redacted (`docs/SECURITY.md:93-97`). `handshake_write(Auth)` copies token bytes into one frame — confirm no logging of `Message::Auth` anywhere (wire path uses `write_message`/`write_boxed` with no debug of payload — verified `wire_io.rs:21-77`). |
| Control-queue silence | `client.rs:846, 878, 886, 909`, `client/open.rs:87` | All `try_send` sites ignore full-queue errors. `Ping` loss records `missed_heartbeats` (benign); `Pong`/`OpenReject` loss delays server cleanup to timeouts. Consider a `rejected`-adjacent counter for dropped control frames if this ever matters in review. |
| Timeout categorization | `client.rs:1183-1200` | Handshake/registration stall → `Timeout`. Target/data dial stalls → `Timeout`. Intentional — document when triaging `last_termination`. Legacy ack-deadline expiry breaks the Session with `Timeout` (`client.rs:861-865`, `client/service_state.rs:155-168`); a correlated expiry fails only its own caller (`client.rs:866-869`). |
| QUIC/WSS/mTLS rejections | `client/config.rs:136-146`, `client.rs:569-612`, `main.rs:921-922` | Fail-closed with `Configuration` via `ClientBuilder::validate`. Confirm no code path constructs the rejected combos in-process (no WSS+mTLS or proxy+mTLS constructors exist — only builder guards + QUIC CA guard in `start_quic_profile`). |
| `CounterGuard` always-zero | `client.rs:1202-1231` | Drops store 0 unconditionally. Safe today (single session), but a future concurrent-session refactor would silently zero a live counter. Flag if sessions ever multiplex. |
| Target-before-data ordering | `client/open.rs:22-34` | Slow target holds an open-task slot before server state is touched — correct for server protection, but a hung connector starves legitimate `Open`s (`client_open_tasks` slots, `connect` timeout each). `PendingConnector` (`server_tests.rs:305-310`) proves cancel works; confirm the policy timeout is the right bound for slow app targets. |
| Dynamic registration single-flight | `client/service_state.rs:3-16`, `client/service_state.rs:204-215`, `client.rs:916-954` | Wire `Error` has no `ServiceId`, so in legacy-serial mode only one dynamic ack may be outstanding; second `begin` → `ResourceExhausted`. With capability 1 the ceiling is `client_command_queue` and transactions correlate by `ServiceId` via `RegisterReject`. Stale-generation acks → `Disconnected`; tombstones → `Abandoned` + wire `Unregister`. Confirm operators expect `Timeout` (not `Disconnected`) when the ack deadline fires mid-session. |

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
  │── RegisterService × N ─────────────────────────►│  sequential, both modes
  │◄── RegisterAck(effective_bind) × N ──────────────│  else Authorization (no retry)
  │                                                  │
  │◄── Open(service, connection) ────────────────────│  per external accept
  │   ├── unknown service → OpenReject(code=1) ─────►│
  │   ├── overloaded → OpenReject(code=2) ──────────►│
  │   └── spawn handle_open:                        │
  │        target dial ──► local target              │  timeouts.connect 10s default
  │        data TLS dial ──────────────────────────►│  (+TLS/handshake 10s / QUIC stream 10s defaults)
  │        DataHello(session, svc, conn) ──────────►│  single-use correlation, handshake-capped
  │        relay opaque bytes ◄────────────────────►│  16 KiB bound, drain 15s default
  │        on failure → OpenReject(code=1) ────────►│
  │                                                  │
  │◄── Ping ─── Pong ──► / ── Ping ──► ◄── Pong ─────│  heartbeat_interval 20s default; one outstanding probe, matching Pong updates RTT, stale ignored
  │── Drain(deadline=relay_drain) ──► / ◄── Drain ───│  local(250 ms cap) / remote(break)
  │── RegisterService/UnregisterService ───────────►│  dynamic Register generation-gated; legacy 1 in flight, correlated bounded by ServiceId; Unregister persists across reconnect
```

Wire framing for every control/data-hello frame: `wire_io.rs:21-77`
(header-first read, length pre-check vs 1 MiB, exact-consumption check).
Relay bytes bypass framing entirely.

### Runtime policy and composition (M008)

`ClientBuilder` is the canonical typed composition path for TCP/TLS, QUIC,
WebSocket, custom connectors, mTLS identity, outbound proxy, and
`RuntimePolicy`. The builder validates profile combinations before startup
(`config.rs:136-146` → `client.rs:569-612`);
`Client::start_*` functions delegate through it. Runtime ceilings and
timeouts are read from the policy carried by `Counters`. The
`policy.limits.control_queue` (default 128) protocol-control and
`policy.limits.client_command_queue` (default 32) handle-command queues
remain separate finite limits.

### Dynamic Services and heartbeat health (M009)

`ClientHandle::register_service` sends a typed command through the bounded
command channel and waits for the current Session's RegisterAck. The Session
generation is stamped by the worker at `begin()` time, not snapshotted in the
handle, so a rotation between the connected check and command processing
cannot spuriously fail a registration a live Session could serve; stale
generations are still rejected. Only a matching successful acknowledgement
appends the Service to the bounded desired-state vector used by subsequent
reconnects. In legacy-serial mode there is one dynamic registration request
in flight at a time because wire Error messages carry no ServiceId;
RegisterAck does carry the ID, and with capability 1 the client permits a
finite bounded number correlated by ServiceId via RegisterReject. Unregister
removes desired state and is sent for the current Session when present. A
cancelled legacy pending registration keeps a bounded tombstone until its
response so its late acknowledgement can be unregistered without mutating
desired state; the same tombstone is used for a correlated transaction whose
write timed out.

Heartbeat health stores one outstanding `(nonce, monotonic send time)` per
Session. Matching Pong updates RTT and last-success time and clears consecutive
misses. Unanswered intervals increment a saturating count without sending
additional probes. `Snapshot.heartbeat` contains only the current generation,
last-Pong age, latest RTT, and missed count; no unbounded history is kept.
Tracing events use typed IDs/categories and never format credentials, proxy
chains, or full config objects. The library emits events only and leaves
subscriber setup to the embedder.
