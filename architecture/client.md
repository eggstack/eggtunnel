# Reverse-session client — deep dive

> Parent: [Architecture Overview](overview.md) §3. This file is the
> review-oriented deep dive for `crates/eggtunnel/src/client.rs`
> (1181 lines). For the server half see `server.md`; for framing see
> `proto-wire-protocol.md`; for shared types see `common-core.md`;
> for transports see `transports-wire-io.md`.

Scope: the private-side, outbound-only initiator. The client owns one
authenticated control stream per session, registers local services, and
dials one data connection per server `Open`. It never listens. The server
owns listeners and picks effective binds; the client owns local targets
and picks where bytes land.

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

Defined at `crates/eggtunnel/src/client.rs:88-97`, redacted `Debug` at
`crates/eggtunnel/src/client.rs:99-109`:

| Field | Type | Notes |
|---|---|---|
| `server_addr` | `String` | `host:port`; DNS via Tokio on connect. Validated by `valid_endpoint` (`client.rs:500-518`): bracketed IPv6 or `rsplit_once(':')`, non-empty host without whitespace, nonzero `u16` port. |
| `tls_server_name` | `String` | SNI + cert verification name; must be `1..=253` bytes (`client.rs:473`). |
| `ca_pem` | `Option<Vec<u8>>` | Custom CA bundle; if absent, system/platform roots. Size-capped at `MAX_FRAME_BYTES` (1 MiB) at `client.rs:488-496`. Rejected for QUIC (see §7). |
| `token` | `SecretToken` | `1..=4096` B, redacted `Debug`, `zeroize` on drop (`common.rs:17-46`). `expose()` is `pub(crate)` only (`common.rs:31-33`). |
| `services` | `Vec<ClientService>` | Must be `1..=64`, unique IDs and names (`client.rs:476-487`). |

`ClientService` (`common.rs:48-71`) is the client view: `id: ServiceId`
+ `name: ServiceName` + `requested_bind: RequestedBind` +
`target: TcpTarget`. Contrast `ServiceSpec` (server view, no target —
server never learns the local destination).

### 2.2 `Client::start*` variants

All constructors require a caller-owned Tokio runtime
(`client.rs:266-268`, `client.rs:365-367`) and call `validate_config`
(`client.rs:368`, `client.rs:269`).

| Entry point | Line | Transport | Notes |
|---|---|---|---|
| `start` | `client.rs:156-158` | TCP+TLS | Default; `TcpTargetConnector`. |
| `start_with_connector` | `client.rs:161-175` | TCP+TLS | Custom `TargetConnector`; builds TLS via `build_tls_config` (`client.rs:520-528`). |
| `start_websocket` / `start_websocket_with_connector` | `client.rs:177-197` | WSS | `websocket=true`; verified TLS → binary WS upgrade, 1 MiB caps (`client.rs:601-613`). |
| `start_with_outbound_proxy` / `..._and_connector` | `client.rs:199-221` | TCP+TLS over proxy | Parses `pproxy` URI chain via `parse_outbound_proxy` (`client.rs:418-425`); `OutboundConnector::from_pproxy_uri`, typed `Configuration("invalid outbound proxy chain")` on failure. |
| `start_websocket_with_outbound_proxy` / `..._and_connector` | `client.rs:223-245` | WSS over proxy | Composes both adapters. |
| `start_quic` / `start_quic_with_connector` → `start_quic_profile` | `client.rs:247-304` | QUIC | Separate `quic_reconnect_loop`; platform roots only, rejects `ca_pem` (`client.rs:270-274`). Test-only `start_quic_insecure_for_test` / `..._with_connector_for_test` (`client.rs:306-319`) set `insecure=true`. |
| `start_with_mtls` / `start_with_mtls_and_connector` | `client.rs:321-354` | TCP+TLS + client cert | `build_mtls_tls_config` (`client.rs:530-562`); bearer token still required (see §7). |

Internal fan-in: TLS/WS/proxy variants converge on `start_with_tls_config`
(`client.rs:356-399`), which spawns `reconnect_loop` (`client.rs:564-685`).
QUIC converges on `start_quic_profile`, which spawns `quic_reconnect_loop`
(`client.rs:716-806`). Both return immediately with an owner `Client` +
cloneable `ClientHandle`; the background task owns config, connector,
cancel token, counters, and command channel.

`validate_outbound_proxy` (`client.rs:413-416`) is re-exported for CLI
`check` (`lib.rs:18-19`).

### 2.3 `Client` / `ClientHandle`

- `Client` (`client.rs:111-115`): `{ cancel, task: Option<JoinHandle<()>>, handle }`. `Drop` cancels (`client.rs:461-465`).
  - `handle()` (`client.rs:401-403`) clones the handle.
  - `shutdown(mut self)` (`client.rs:405-410`) cancels then awaits the
    reconnect task (join, ignore result). This is the graceful path
    embedders must call (`docs/EMBEDDING.md:16`).
- `ClientHandle` (`client.rs:117-124`): `{ cancel, counters, commands,
  quic_client }` (last field only with `quic`).
  - `snapshot()` (`client.rs:131-133`) → `Counters::snapshot()`.
  - `shutdown(&self)` (`client.rs:134-136`) → `cancel.cancel()` (no join;
    fire-and-forget vs `Client::shutdown` which joins).
  - `unregister_service(id)` (`client.rs:139-144`) → `mpsc::send`
    `ClientCommand::Unregister(id)`; `Err(Cancelled)` if the loop is gone.
    Semantics (see `client.rs:1017-1025`): removes from `active_services`
    *and* from `config.services` (via `apply_client_command`), so it
    survives reconnect; prunes `services` counter and `binds`; writes
    `UnregisterService` on the wire. No-op if the id is unknown
    (`was_present` guard).
  - `ClientCommand` (`client.rs:126-128`): currently only
    `Unregister(ServiceId)`.
  - `quic_client_for_test` (`client.rs:146-153`): `#[cfg(all(test,
    feature="quic"))]` accessor for assertions.

Command channel depth is 32 (`client.rs:371`, `client.rs:277` for QUIC) —
distinct from `CONTROL_QUEUE=128` (wire-bound outbound queue).

### 2.4 Target abstraction

| Type | Line | Role |
|---|---|---|
| `ApplicationStream` | `client.rs:46-48` | Blanket impl over `AsyncRead+AsyncWrite+Send+Unpin`. Any embedder byte stream qualifies. |
| `TargetStream` | `client.rs:51` | `Box<dyn ApplicationStream>` — transport-neutral return type. |
| `TargetContext` | `client.rs:53-58` | `{ session_id, connection_id, cancellation: CancellationToken }`. Built per-`Open` in `handle_open` (`client.rs:1084-1088`). Child token of the session token, so session teardown cancels in-flight target dials. |
| `TargetError` | `client.rs:60-66` | `Refused` (default TCP maps dial failure here) vs `Failed`. Both map to `TunnelError::Target` at `client.rs:1091` — reviewer note: the distinction is lost on the wire (both become `OpenReject code=1`). |
| `TargetFuture` | `client.rs:68` | Pinned boxed future for `connect`. |
| `TargetConnector` | `client.rs:70-73` | `fn connect(&self, service: ClientService, context: TargetContext) -> TargetFuture`. Receives the *trusted client-owned* service; server cannot rewrite it. |
| `TcpTargetConnector` | `client.rs:75-86` | Default: `TcpStream::connect((host, port))`, `Refused` on error. |

`docs/EMBEDDING.md:25-31` + `fixtures/embedder`: direct in-process
connectors skip loopback TCP entirely (no socket). The `DuplexEchoConnector`
test double (`server.rs:1434-1451`) is the canonical example: refuses
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

Normal path through `run_session` (`client.rs:877-1043`):

1. `ClientHello{ version: CURRENT, capabilities: default }` —
   `handshake_write` (`client.rs:888-894`). Each handshake
   read/write is wrapped in `HANDSHAKE_TIMEOUT` (`client.rs:1051-1063`);
   timeout maps to `TunnelError::Disconnected` (deliberately transport,
   not `Timeout` — see §10).
2. `ServerHello{ version }` — major must equal
   `ProtocolVersion::CURRENT.major`, else `Protocol(UnexpectedMessage)`
   (`client.rs:896-904`).
3. `Auth::new(token.expose().to_vec())?` → `Message::Auth`
   (`client.rs:905-906`). Token bytes copied out of the redacted
   `SecretToken` only for this frame.
4. `AuthOk{ session_id }` — anything else is `Authentication`
   (`client.rs:907-910`).
5. Per service: `RegisterService{ service_id, name, requested_bind,
   target }` → expect `RegisterAck{ service_id }` with matching id;
   push `(session_id, service_id, effective_bind)` to `counters.binds`
   (`client.rs:912-931`). `Message::Error(_)` or any other message →
   `Authorization`. Note: the client sends its `target` on the wire but
   the server must ignore it (target-confusion boundary).
6. Mark connected: `connected=1`, `services=len`,
   `sessions=1`, reset `reconnect_delay=500ms`, install
   `CounterGuard(sessions)` which zeroes `sessions` on exit
   (`client.rs:933-943`, `client.rs:1152-1157`).
7. Split control stream (`client.rs:948`), spawn data tasks into
   `JoinSet opens` (`client.rs:949`), then `select!` loop
   (`client.rs:956-1032`):
   - `cancel` → send `Drain{ deadline_ms: RELAY_DRAIN }` with 250 ms
     cap, then break (`client.rs:958-963`).
   - 20 s heartbeat → `try_send(Ping{ nonce++ })`
     (`client.rs:964-967`). Fire-and-forget; full queue drops the ping.
   - `read_message` → `Open` / `Ping`→`Pong` / `Pong` (ignore) /
     `Drain` (cancel session, break) / anything else →
     `Protocol(UnexpectedMessage)` which tears down the session
     (`client.rs:968-1013`).
   - `out_rx.recv` (the `CONTROL_QUEUE=128` queue) → `write_message`
     (`client.rs:1014`). `?` propagates write errors → session teardown.
   - `commands.recv` → `Unregister` handling (`client.rs:1015-1027`).
   - `opens.join_next()` → `record_join_result` (panic accounting)
     (`client.rs:1028-1030`).
8. Teardown: `timeout(SERVER_DRAIN_GRACE=1s)` joining open tasks, then
   `abort_all` + drain (`client.rs:1033-1038`), `connected=0`
   (`client.rs:1039-1041`), return `Ok(())` (reconnectable) or `Err`
   (categorized).

`Open` dispatch detail (`client.rs:970-1002`):

- Unknown `service_id` → `rejected++`,
  `record_termination(Authorization)`,
  `try_send(OpenReject{ connection_id, code: 1 })`, continue
  (`client.rs:971-976`).
- `semaphore.try_acquire_owned()` fails (128 tasks busy) →
  `rejected++`, `record_termination(ResourceExhausted)`,
  `try_send(OpenReject{ code: 2 })`, continue (`client.rs:977-983`).
- Else spawn `handle_open` with child cancel token, cloned `out` sender,
  `OpenTaskGuard` (bumps `open_tasks` + high-water, decrements on drop),
  and `OpenContext` (`client.rs:984-1002`).

`OpenReject` codes are client-originated advisory signals (1 = refused /
unknown / target failure; 2 = overloaded). The client also *receives* no
`OpenReject` — it only sends them.

### 3.3 Reconnect behavior

`reconnect_loop` (`client.rs:564-685`) and `quic_reconnect_loop`
(`client.rs:716-806`):

- Pre-dial: drain pending `Unregister` commands
  (`client.rs:580-582`, `client.rs:736-738`) so reconnect re-registers
  the pruned set.
- Dial: `connect_server` (proxy-aware) → TLS (10 s cap) → optional WSS
  upgrade (10 s cap) → `run_session` (`client.rs:583-640`). QUIC:
  `QuicClient::connect` (10 s) → `get_connection` (10 s) →
  `open_stream` for control (10 s) → `run_session`
  (`client.rs:746-782`).
- **Auth/Authz failures do not reconnect** — `break` after recording
  termination (`client.rs:649-657`, `client.rs:788-796`). Everything else
  records termination, zeroes `connected/services/binds`
  (`client.rs:661-671`), increments `reconnects`
  (`client.rs:675-677`), sleeps `delay + jitter`, doubles `delay`
  (`500ms → 30s` cap, `client.rs:683`).
- Jitter: `random_jitter_ms` (`client.rs:853-864`) = uniform
  `[0, min(delay/4, 7500)ms]` via `getrandom`; returns 0 if `max==0` or
  RNG fails (no panic path).
- QUIC teardown closes the QUIC connection and clears
  `handle.quic_client` (`client.rs:787`, `client.rs:804`).
- Successful session resets `delay` to 500 ms (`client.rs:942`).

Review implication: reconnect storms are bounded by backoff + jitter, but
there is no global circuit breaker — a flapping server with valid
credentials reconnects forever until `cancel`. Auth failures are the only
hard stop. See §10.

---

## 4. Data path per `Open`

`handle_open` (`client.rs:1074-1150`) runs once per spawned task (permit +
`OpenTaskGuard` held by the parent future at `client.rs:998-1002`):

1. **Target dial first.** Build `TargetContext{ session_id,
   connection_id, cancellation }`, then
   `timeout(CONNECT_TIMEOUT, connector.connect(...))`
   (`client.rs:1089-1092`). Timeout → `Timeout`; connector `Refused` /
   `Failed` → `Target`. All races also select on `cancel` →
   `Cancelled`. Ordering note: the target is dialed *before* the data
   connection — a slow/malicious target holds one of the 128 open-task
   slots without yet consuming server pending state.
2. **Data dial.** TCP/TLS profile (`client.rs:1094-1119`):
   `connect_server` (proxy-aware, cancellable) → `tls_connect` (10 s,
   cancellable) → optional WSS upgrade (10 s, cancellable). QUIC profile
   (`client.rs:1120-1127`): `connection.open_stream()` (10 s,
   cancellable). No custom-CA/mTLS/proxy knobs here beyond what the
   session already validated — the stored `ClientDataTransport`
   (`client.rs:24-36`) carries them.
3. **`DataHello{ session_id, service_id, connection_id }`** via
   `write_boxed` — no timeout wrapper (relies on session cancel +
   transport timeouts), `?` propagates (`client.rs:1128`). This is the
   last structured message; everything after is opaque bytes.
4. **Relay.** `relay_with_options(target, data,
   RelayOptions::bounded(16 KiB, RELAY_DRAIN=15s))`
   (`client.rs:1129`). Both `Ok(report)` and `Err(failure)` still credit
   `bytes_upstream/downstream` (`client.rs:1130-1137`) — partial-byte
   accounting survives failures. Relay is full-duplex until EOF/error;
   half-close semantics are transport-dependent (WSS full-close only —
   see `docs/SUPPORT.md:16-20`).
5. **Failure → `OpenReject`.** Any `Err` records its termination
   category and, unless cancelled, `try_send(OpenReject{
   connection_id, code: 1 })` (`client.rs:1141-1149`). If the session is
   already gone the reject is silently dropped — correct, since the
   server has already reaped the pending entry.

Cancellation threads through every await (`cancel.cancelled()` arms at
`client.rs:1090,1096,1101,1112,1123`), and `TargetContext.cancellation`
lets embedder connectors abort early. Session teardown cancels
`session_cancel`, whose children are the per-`Open` tokens
(`client.rs:984`), so all in-flight dials/relays observe cancellation
promptly.

---

## 5. Concurrency model

| Primitive | Where | Purpose |
|---|---|---|
| Control `split` reader/writer | `client.rs:948` | Full-duplex control: `reader` only in the `read_message` arm, `writer` only for `out_rx` drain + `Unregister` + terminal `Drain`. No lock; single owner task. |
| `JoinSet opens` | `client.rs:949` | Data-plane tasks (`handle_open`). Reaped via `join_next` inside the loop (`client.rs:1028-1030`) and at teardown (`client.rs:1033-1038`). Panics counted via `record_join_result` → `task_panics++` + `Internal` (`common.rs:265-270`). |
| `Semaphore(MAX_OPEN_TASKS=128)` + `try_acquire_owned` | `client.rs:39`, `client.rs:946`, `client.rs:977-983` | Admission for `Open` flood. Non-blocking: overload → immediate `OpenReject code=2`, no queueing. Permit moved into the task (`client.rs:999`). |
| `mpsc CONTROL_QUEUE=128` (`out_tx/out_rx`) | `client.rs:40`, `client.rs:947` | Outbound control queue (`Pong`, `Ping`, `OpenReject`). All producers use `try_send` (never block the data plane); drops on full are silent (`let _ =`). |
| `mpsc channel(32)` commands | `client.rs:371` | `ClientHandle → reconnect_loop` (`Unregister`). Drained pre-dial and polled in-session. `send` (async) from the handle; `try_recv` pre-dial, `recv` in-session. |
| `CancellationToken` tree | `client.rs:369`, `client.rs:950`, `client.rs:984` | Root `cancel` (handle + owner) → `session_cancel` per session → per-`Open` child. `Drop for Client` cancels root (`client.rs:461-465`). |
| `OpenTaskGuard` / `CounterGuard` | `client.rs:1152-1181` | RAII counters: `open_tasks` inc/high-water + dec on drop; `sessions` zeroed on session exit. Note `CounterGuard::new` ignores its inner value except on drop (`client.rs:1152-1157`) — it always stores 0, even if nested (no nesting occurs today). |
| Heartbeat `interval_at(20s)` | `client.rs:951-955` | Keepalive `Ping`; first tick 20 s after session start. Nonce wraps (`wrapping_add`). |

Shutdown ordering (graceful):

1. `Client::shutdown` cancels root (`client.rs:405-410`) *or*
   `ClientHandle::shutdown` cancels without joining
   (`client.rs:134-136`).
2. Session loop observes `cancel` → cancels `session_cancel` (data tasks
   see it), sends `Drain` (250 ms cap), breaks (`client.rs:958-963`).
3. `SERVER_DRAIN_GRACE=1s` join window → `abort_all` → drain
   (`client.rs:1033-1038`).
4. Reconnect loop observes `cancel`, breaks without incrementing
   `reconnects` (`client.rs:672-674`).
5. `Client::shutdown` awaits the reconnect task.

Server-initiated `Drain` flips it: session cancels children and breaks
immediately (`client.rs:1006-1009`), then the same 1 s grace applies.
The asymmetry (`RELAY_DRAIN=15s` advertised to the server vs 1 s local
grace) is intentional: the server gives the client time, but a locally
shutting-down client does not wait long.

---

## 6. Timeouts / constants

| Constant | Value | Line | Meaning |
|---|---|---|---|
| `MAX_SERVICES` | 64 | `client.rs:38` | `services.len()` must be `1..=64`. Mirrors `BindPolicy::max_services_per_session=64` and `ResourceLimits::services_per_session`. |
| `MAX_OPEN_TASKS` | 128 | `client.rs:39` | Concurrent `handle_open` tasks. Matches `ResourceLimits::client_open_tasks`. |
| `CONTROL_QUEUE` | 128 | `client.rs:40` | Outbound control `mpsc` depth. Matches `ResourceLimits::control_queue`. |
| `CONNECT_TIMEOUT` | 10 s | `client.rs:41` | TCP connect (direct or proxy, `client.rs:687-714`), target `connect()` (`client.rs:1091`), QUIC connect/get/open (`client.rs:748-767`, `client.rs:1124`). |
| `HANDSHAKE_TIMEOUT` | 10 s | `client.rs:42` | TLS handshake (`client.rs:591`), WSS upgrade (`client.rs:604-613`, `client.rs:1113`), every control handshake read/write (`client.rs:1051-1063`). |
| `RELAY_DRAIN` | 15 s | `client.rs:43` | `RelayOptions` drain bound (`client.rs:1129`) and advertised `Drain.deadline_ms` on local shutdown (`client.rs:960`). |
| `SERVER_DRAIN_GRACE` | 1 s | `client.rs:44` | Local join window for open tasks after session break (`client.rs:1033-1036`). |
| Reconnect backoff | 500 ms → 30 s, ×2 + jitter | `client.rs:575`, `client.rs:678-683` | Reset to 500 ms on successful registration (`client.rs:942`). |
| Heartbeat | 20 s interval | `client.rs:951-955` | `Ping` keepalive; `Pong` ignored (`client.rs:1005`). |
| Terminal `Drain` write cap | 250 ms | `client.rs:961` | Best-effort courtesy on local shutdown. |

All timeouts are hard-coded; no per-call overrides. `handshake_read` maps
elapsed to `Disconnected` rather than `Timeout` (`client.rs:1051-1056`) —
reviewers tracing `last_termination` should expect `Transport` (not
`Timeout`) for a stalled control handshake.

---

## 7. Validation matrix

| Check | Where | Behavior |
|---|---|---|
| Service count `1..=64` | `client.rs:476-478` | `Configuration("service count must be 1..=64")` before any I/O. |
| Unique service IDs + names | `client.rs:479-487` | `Configuration("service IDs and names must be unique")`. Prevents `RegisterAck` aliasing (match on `service_id`, `client.rs:921`). |
| `server_addr` shape | `client.rs:468-472`, `client.rs:500-518` | `Configuration("server_addr must be a host:port endpoint")`. IPv6 bracket-aware; rejects whitespace hosts, zero ports. |
| `tls_server_name` non-empty, ≤253 | `client.rs:473-475` | `Configuration("TLS server name is invalid")`. |
| `ca_pem` ≤ 1 MiB | `client.rs:488-496` | `Configuration("custom CA bundle exceeds configured size limit")`. |
| QUIC + custom CA | `client.rs:270-274` | `Configuration("Eggress QUIC currently uses platform roots; custom CA bundles are unsupported")`. Bearer token still required inside the encrypted control stream (`docs/SECURITY.md:45-48`). |
| QUIC + CA / mTLS / proxy (CLI) | `main.rs:160-169` | `check` rejects with platform-roots/bearer-only message. |
| Proxy + mTLS (CLI) | `main.rs:185-189` | `check` rejects `"outbound proxy mode currently does not support mTLS"`. No `start_with_mtls`+proxy constructor exists — the combination is unconstructable in-process too. |
| WSS + mTLS (CLI + lib) | `main.rs:190-194`; no WSS+mTLS constructor | `check` rejects `"WebSocket transport currently does not support mTLS"` (`docs/SUPPORT.md:7`). |
| Server + proxy (CLI) | `main.rs:229-231` | `"outbound_proxy is only valid in client mode"`. Proxy types are client-side only (`docs/SECURITY.md:74`). |
| Proxy chain syntax | `client.rs:418-425` | `OutboundConnector::from_pproxy_uri` failure → `Configuration("invalid outbound proxy chain")`. Surfaced through `validate_outbound_proxy` for `check`. |
| QUIC + proxy (docs/CLI) | `docs/SUPPORT.md:8`; `main.rs:160-169` | Rejected (proxy traversal unsupported for QUIC). |
| **No silent proxy fallback** | `client.rs:687-714`; `docs/SECURITY.md:79-81` | Proxy errors map to typed variants (`Authentication`/`Authorization`/`Timeout`/`Disconnected` via `OutboundConnectErrorKind`, `client.rs:701-706`); direct TCP is never attempted when a proxy is configured. |
| Typed termination | `common.rs:159-172`, `common.rs:301-316` | Every reconnect-loop and `handle_open` error records `termination_category()` (`client.rs:654,659,793,798,1142`): `Cancelled/Timeout/Target/ResourceExhausted/PeerClosed/Auth*/Protocol/Transport/Internal`. `Configuration` → `Internal` (start-time only, never a session outcome). |

mTLS specifics (`client.rs:530-562`, `docs/SECURITY.md:32-39`): system or
configured roots for the server + required client cert/key; empty cert
chain → `Tls`; key/cert parse failures → `Tls` (no detail — avoids
oracle). Private key is zeroized on drop (`client.rs:453-459`) and
redacted in `Debug` (`client.rs:443-451`).

---

## 8. Observability

`ClientHandle::snapshot()` (`client.rs:131-133`) returns
`Counters::snapshot()` (`common.rs:227-256`). Fields the client actually
drives:

| Snapshot field | Updated by client at | Notes |
|---|---|---|
| `connected` | `client.rs:933-935` (set 1), `client.rs:664,816,1041` (set 0) | Bool view over atomic. |
| `registered_services` (`services`) | `client.rs:936-938`, `client.rs:1021`, cleared `client.rs:665` | Decremented on `Unregister`; zeroed on reconnect. |
| `active_sessions` (`sessions`) | `client.rs:939-941` + `CounterGuard` zero on exit | Always 0/1 for a client (single session). |
| `effective_binds` | `client.rs:922-927`, pruned `client.rs:1022`, cleared `client.rs:668-671` | `(SessionId, ServiceId, EffectiveBind)` — the only place the client learns server-chosen addresses. CLI/`check` flows poll this. |
| `reconnects` | `client.rs:675-677`, `client.rs:828-830` | Incremented per failed session (not on clean cancel). `client_reconnects…` test asserts `>0`. |
| `rejected_connections` (`rejected`) | `client.rs:972,979` (Open-path), indirectly via `handle_open` failures? No — `handle_open` failures record termination but do *not* bump `rejected` | Gap: target/data failures are visible only in `last_termination`, not the reject counter. |
| `bytes_upstream/downstream` | `client.rs:1130-1137` (both success and failure reports) | Bounded `u64` totals; asserted `>0` in roundtrip tests. |
| `active_client_open_tasks` + high-water | `OpenTaskGuard` (`client.rs:1159-1176`) | `fetch_add` + `fetch_max`; asserted `>=1` after relay. |
| `last_termination` | `record_termination` at `client.rs:654,659,793,798,973,980,1142` | Includes `Authorization` for unknown-service `Open` and `ResourceExhausted` for overload — reviewers can distinguish the two reject causes here (wire codes 1/2 are not surfaced in `Snapshot`). |
| `task_panics` | `record_join_result` (`client.rs:1028-1030` → `common.rs:265-270`) | `Internal` termination on panic. |
| `resource_limits` | `ResourceLimits::default()` (`common.rs:249`) | Static ceilings (`sessions:128, services:64, …, client_open_tasks:128, control_queue:128`) echoed for operators. |

Fields the client never meaningfully drives: `pending_connections`,
`active_connections`, `active_handshakes` (+ high-waters) stay 0 —
they are server-side. `high_water_services/sessions` likewise. This is
expected but worth knowing when comparing client vs server snapshots.

---

## 9. Test inventory for client behavior

`client.rs` itself contains **no `#[cfg(test)]` module** — only
test-gated helpers: `quic_client_for_test` (`client.rs:146-153`),
`start_quic_insecure_for_test` (`client.rs:306-312`), and
`start_quic_insecure_with_connector_for_test` (`client.rs:313-319`).
All client behavior tests live in `server.rs:1306-4513` (`#[cfg(all(test,
feature="client"))]`) as end-to-end client↔server sessions. Grouped by
what each proves about the client:

**Happy path + API surface.**

- `tcp_tls_reverse_session_registers_and_relays_data` (`server.rs:2835`):
  two services register, both relay (`roundtrip`), client+server byte
  counters `>0`, resource-limit/high-water assertions
  (`high_water_client_open_tasks >= 1`), then `unregister_service(1)`
  shrinks server binds `2→1`, then shutdown drains the active external
  to EOF with zero pending/active. Covers §§2–4, 8 in one test.
- `application_target_connector_relays_without_loopback_target`
  (`server.rs:1462`): custom `TargetConnector` (`DuplexEchoConnector`,
  `server.rs:1434-1451`) proves in-process targets work and that unknown
  service names yield `TargetError::Refused`.
- `websocket_tls_session_registers_and_relays_data_paths`
  (`server.rs:1517`): WSS control + data upgrade registers and relays.
- `quic_session_multiplexes_isolated_data_streams_for_two_services`
  (`server.rs:2969`): one QUIC connection, two services, isolated
  streams.

**Failure / resilience.**

- `bad_token_does_not_create_a_registered_session` (`server.rs:3167`):
  wrong bearer → no session; exercises the no-reconnect auth path (§3.3).
- `wrong_tls_server_name_is_rejected_before_authentication`
  (`server.rs:3212`): SNI verification fires before any `Auth`.
- `refused_target_rejects_external_connection_and_releases_pending_capacity`
  (`server.rs:3303`): dial-refused target → external sees EOF/empty,
  server pending returns to 0 — proves `OpenReject` + capacity release.
- `client_reconnects_and_restores_services_in_a_new_session_generation`
  (`server.rs:3371`): server restart → client re-registers with a *new*
  `SessionId`, `reconnects > 0`.
- `client_cancellation_releases_pending_direct_connector_and_external_peer`
  (`server.rs:1915`): `PendingConnector` (never resolves,
  `server.rs:1453-1459`) + `client.shutdown()` → server pending/active
  return to 0, external drains. Proves cancellation tears down
  in-flight `Open` tasks (§§4–5).
- `repeated_client_server_start_stop_returns_runtime_counts_to_zero`
  (`server.rs:3622`): start/stop cycles leave counters at zero — no
  task/counter leak.
- `data_hello_is_session_service_bound_and_single_use` (`server.rs:3439`)
  and `wrong_service_and_expired_data_hellos_consume_and_reject_pending_state`
  (`server.rs:3500`): server-side correlation, but they pin the contract
  the client's `DataHello` must satisfy.
- `owned_task_panic_is_counted_as_internal_termination`
  (`server.rs:3672`): `record_join_result` path (`task_panics`,
  `Internal`).
- `server_shutdown_cancels_incomplete_tls_and_authentication_handshakes`
  (`server.rs:3685`) and
  `unauthenticated_handshake_admission_caps_at_limit_and_recovers`
  (`server.rs:3740`): server admission, but bound client handshake
  timeouts (10 s) are the client-visible counterpart.

**Transports / proxy / mTLS.**

- `outbound_http_connect_keeps_eggtunnel_tls_end_to_end`
  (`server.rs:1732`), `outbound_socks5_keeps_eggtunnel_tls_end_to_end`
  (`server.rs:1818`): proxy traversal preserves end-to-end TLS.
- `outbound_proxy_refused_endpoint_terminates_without_secret_in_diagnostic`
  (`server.rs:1984`), `outbound_proxy_handshake_timeout_tears_down_bounded`
  (`server.rs:2048`), `outbound_proxy_cancellation_terminates_in_progress_handshake`
  (`server.rs:2118`): typed, bounded, secret-free proxy failures — the
  no-fallback claim with evidence.
- `outbound_http_connect_auth_success…` (`server.rs:2176`) /
  `…_failure_rejects_without_secret_leak` (`server.rs:2278`) and the
  SOCKS5 pair (`server.rs:2358`, `server.rs:2487`): URI-userinfo auth
  qualified both ways.
- `outbound_two_hop_socks5_then_http_connect_routes_end_to_end`
  (`server.rs:2568`): canonical `__`-separated multi-hop chain.
- `mtls_requires_trusted_client_certificate_and_keeps_server_name_validation`
  (`server.rs:2704`) and `mtls_principal_mismatch_cannot_attach_data_stream`
  (`server.rs:3569`): bearer-still-required + leaf-identity binding.
- WSS edge: `wss_peer_close_during_active_relay_terminates_cleanly`
  (`server.rs:1576`), `wss_payload_larger_than_message_cap_roundtrips_multiple_frames`
  (`server.rs:1655`).
- QUIC edge: `quic_connection_replacement_creates_new_session_and_reregisters_services`
  (`server.rs:3056`),
  `production_quic_profile_rejects_untrusted_server_certificate`
  (`server.rs:3125`), wrong-session/replay/stale/saturation/half-close
  suite (`server.rs:3906,4025,4149,4260,4435`).

Gaps inherited from `docs/SUPPORT.md:21-34`: WSS is not TCP-half-close
equivalent; multi-hop beyond SOCKS5+HTTP is unverified. No client-unit
tests for `validate_config`/`valid_endpoint` in isolation — covered only
indirectly via CLI `check` and end-to-end failures.

---

## 10. Review checklist

| Risk | Where to look | Status / question for reviewer |
|---|---|---|
| Reconnect storms | `client.rs:575-684`, `client.rs:853-864` | Backoff 500 ms→30 s + jitter bounds a single client, but valid-credential flapping reconnects forever. Confirm deployment guidance (no circuit breaker by design). Auth-fail hard-stop (`client.rs:649-657`) prevents credential-spray loops — verify callers surface `last_termination=Authentication` rather than retrying with new tokens silently. |
| Task leaks | `client.rs:949,998-1002,1028-1038` | `JoinSet` reaped in-loop + 1 s grace + `abort_all`. `OpenTaskGuard`/`CounterGuard` are RAII. `repeated_client_server_start_stop…` test asserts zero. Ask: is 1 s grace vs 15 s `RELAY_DRAIN` the intended asymmetry (fast local exit, slow server courtesy)? Yes per §5, but confirm operators expect truncated relays on Ctrl-C. |
| `Open` flood | `client.rs:946,977-983`, `client.rs:947` | `try_acquire_owned` → `code=2` + `ResourceExhausted`; unknown service → `code=1` + `Authorization`. Both `try_send` — a full `CONTROL_QUEUE` silently drops the reject, leaving the server pending entry to expire (30 s). Confirm that tradeoff is acceptable; consider `rejected++` already covers observability even when the frame is dropped — but `last_termination` is overwritten per event, so burst cause is lossy. |
| Target confusion | `client.rs:70-86`, `client.rs:971-976`, `client.rs:1089-1092` | Server cannot select/rewrite targets; `active_services` lookup is by `service_id` with `Refused` fallback. `DuplexEchoConnector` shows name-checking is the embedder's job. Confirm: `TargetError::Refused` vs `Failed` collapse to one wire code — should operators distinguish "bad name" from "target down"? Currently only `last_termination=Target` either way. |
| Credential handling | `common.rs:17-46`, `client.rs:443-459`, `client.rs:905-906` | Token redacted + zeroized; `expose()` crate-only; key zeroized. Proxy creds from env, redacted (`docs/SECURITY.md:74-83`). `handshake_write(Auth)` copies token bytes into one frame — confirm no logging of `Message::Auth` anywhere (wire path uses `write_message`/`write_boxed` with no debug of payload — verified `wire_io.rs:38-57`). |
| Control-queue silence | `client.rs:966,974,981,1004,1144` | All `try_send` sites ignore full-queue errors. `Ping` loss is benign; `Pong`/`OpenReject` loss delays server cleanup to timeouts. Consider a `rejected`-adjacent counter for dropped control frames if this ever matters in review. |
| Timeout miscategorization | `client.rs:1051-1063` | Handshake stall → `Disconnected`/`Transport`, not `Timeout`. Target/data dial stalls → `Timeout`. Intentional but surprising — document when triaging `last_termination`. |
| QUIC/WSS/mTLS rejections | `client.rs:270-274`, `main.rs:160-194` | Fail-closed with `Configuration`. Confirm no code path constructs the rejected combos in-process (no WSS+mTLS or proxy+mTLS constructors exist — only CLI guards + QUIC CA guard in-library). |
| `CounterGuard` always-zero | `client.rs:1152-1157` | Drops store 0 unconditionally. Safe today (single session), but a future concurrent-session refactor would silently zero a live counter. Flag if sessions ever multiplex. |
| Target-before-data ordering | `client.rs:1089-1119` | Slow target holds an open-task slot before server state is touched — correct for server protection, but a hung connector starves legitimate `Open`s (128 slots, 10 s cap each). `PendingConnector` test proves cancel works; confirm 10 s is the right bound for slow app targets. |

---

## Appendix — control + data sequence (text diagram)

```text
client                                            server
  │                                                  │
  │── TCP connect ──────────────────────────────────►│  CONNECT_TIMEOUT 10s
  │── TLS handshake (SNI=server_name) ──────────────►│  HANDSHAKE_TIMEOUT 10s
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
  │        target dial ──► local target              │  CONNECT_TIMEOUT 10s
  │        data TLS dial ──────────────────────────►│  (+TLS 10s / QUIC stream 10s)
  │        DataHello(session, svc, conn) ──────────►│  single-use correlation
  │        relay opaque bytes ◄────────────────────►│  16 KiB bound, 15 s drain
  │        on failure → OpenReject(code=1) ────────►│
  │                                                  │
  │◄── Ping ─── Pong ──► / ── Ping ──► ◄── Pong ─────│  20 s heartbeat; Pong ignored
  │── Drain(deadline=15s) ──► / ◄── Drain ───────────│  local(250 ms cap) / remote(break)
  │── UnregisterService ───────────────────────────►│  persistent across reconnect
```

Wire framing for every control/data-hello frame: `wire_io.rs:7-57`
(header-first read, length pre-check vs 1 MiB, exact-consumption check).
Relay bytes bypass framing entirely.
