# Proto wire protocol — deep dive

Back to [Architecture Overview](overview.md) §1.

Sources: `crates/eggtunnel-proto/src/lib.rs` (886 lines, wire v1.1), `crates/eggtunnel-proto/Cargo.toml`,
`docs/PROTOCOL.md`, `crates/eggtunnel/src/wire_io.rs` (58 lines).
Cross-references below use `file:line` anchors. All claims were read from code; no invented behavior.

---

## 1. Purpose, scope, non-goals

**Purpose.** `eggtunnel-proto` is the runtime-neutral, bounded native Eggtunnel control-plane
wire protocol. It owns:

- bounded wire DTOs and their validation (`crates/eggtunnel-proto/src/lib.rs:13-22`, `crates/eggtunnel-proto/src/lib.rs:37-227`),
- the 14-byte framing (`crates/eggtunnel-proto/src/lib.rs:2-7`, `docs/PROTOCOL.md:12-21`),
- `encode_frame` / `decode_frame` (`crates/eggtunnel-proto/src/lib.rs:457-525`),
- the 15 stable message IDs (`crates/eggtunnel-proto/src/lib.rs:264-282`, `docs/PROTOCOL.md:32-37`).

**Scope.**

- Framing: `ETUN` magic + major/minor `u16 BE` + message-ID `u16 BE` + payload-len `u32 BE` +
  exactly one `postcard` payload (`crates/eggtunnel-proto/src/lib.rs:457-470`, `crates/eggtunnel-proto/src/lib.rs:474-525`).
- Validation: length/charset pre-checks before copy or deserialize
  (`crates/eggtunnel-proto/src/lib.rs:91-103`, `crates/eggtunnel-proto/src/lib.rs:161-179`,
  `crates/eggtunnel-proto/src/lib.rs:185-191`, `crates/eggtunnel-proto/src/lib.rs:209-215`,
  `crates/eggtunnel-proto/src/lib.rs:287-309`, `crates/eggtunnel-proto/src/lib.rs:488-496`).
- Version gate: wire `1.1` (major 1 keeps the 1.0 boundary), major mismatch rejected, minor informational; extensions gated by capability intersection only (ADR-0002)
  (`crates/eggtunnel-proto/src/lib.rs:21-35`, `crates/eggtunnel-proto/src/lib.rs:481-485`,
  `docs/PROTOCOL.md:1-10`).
- Typed errors for every hostile-input class (`crates/eggtunnel-proto/src/lib.rs:433-455`).

**Non-goals (explicitly out of this crate).**

| Non-goal | Where it lives instead | Evidence |
|---|---|---|
| No sockets / async / timers / tasks | `crates/eggtunnel/src/wire_io.rs`, `client.rs`, `server.rs` via Eggress transports | `crates/eggtunnel-proto/Cargo.toml:15-19` depends only on `serde`, `postcard` (`alloc`), `thiserror`, `getrandom`; `lib.rs:1` is `#![forbid(unsafe_code)]` and uses `core::fmt` (`crates/eggtunnel-proto/src/lib.rs:9`) |
| No TLS / QUIC / WebSocket / proxy | `crates/eggtunnel/src/wire_io.rs:7-58`, feature gates in `crates/eggtunnel/Cargo.toml` | proto never imports `tokio`, `rustls`, `eggress-*` |
| No session state machine | `crates/eggtunnel/src/client.rs:709-740` (handshake) + `crates/eggtunnel/src/client.rs:802-981` (control loop), `crates/eggtunnel/src/server/control.rs:148-234` (control loop) + first-frame gates `crates/eggtunnel/src/server/accept.rs:387-392`, `crates/eggtunnel/src/server/accept.rs:470-478`, `crates/eggtunnel/src/server/accept.rs:249-275` enforce ordering with `UnexpectedMessage` | `decode_frame` never returns `UnexpectedMessage` (`crates/eggtunnel-proto/src/lib.rs:474-525`); the variant is only constructed by client/server (`crates/eggtunnel/src/client.rs:723`, `crates/eggtunnel/src/client.rs:886`, `crates/eggtunnel/src/client.rs:918`, `crates/eggtunnel/src/server/accept.rs:389`, `crates/eggtunnel/src/server/accept.rs:472`, `crates/eggtunnel/src/server/accept.rs:261`, `crates/eggtunnel/src/server/accept.rs:279`, `crates/eggtunnel/src/server/control.rs:201`) |
| No application data framing | opaque bytes after `DataHello` | `docs/PROTOCOL.md:36-39`: “After DataHello, data streams carry opaque application bytes; application payload frames are not part of the control protocol.” |
| No credential transport security | caller must establish secure transport first | `docs/PROTOCOL.md:36-37`: “Credentials … must only be sent after a secure transport is established.” |

Related overview: [Architecture Overview](overview.md) §1 summarizes this crate as
“Runtime-neutral, `forbid(unsafe_code)`, no socket/async/timer/task dependencies.”

---

## 2. Frame layout + `encode_frame` / `decode_frame` semantics

### 2.1 Layout

Defined at `crates/eggtunnel-proto/src/lib.rs:13-22` and documented at `docs/PROTOCOL.md:12-21`:

| Offset | Size | Field | Encoding | Constant / code |
|---:|---:|---|---|---|
| 0 | 4 | magic | ASCII `ETUN` | `MAGIC` (`crates/eggtunnel-proto/src/lib.rs:13`) |
| 4 | 2 | major version | `u16 BE` (`1`) | `PROTOCOL_MAJOR` (`crates/eggtunnel-proto/src/lib.rs:21`) |
| 6 | 2 | minor version | `u16 BE` (`1`) | `PROTOCOL_MINOR` (`crates/eggtunnel-proto/src/lib.rs:22`) |
| 8 | 2 | message ID | `u16 BE`, explicit discriminant 1–14 | `MessageType` `#[repr(u16)]` (`crates/eggtunnel-proto/src/lib.rs:229-246`) |
| 10 | 4 | payload length | `u32 BE`, bytes of the one postcard payload | written at `crates/eggtunnel-proto/src/lib.rs:467`, read at `crates/eggtunnel-proto/src/lib.rs:487` |
| 14 | variable | payload | `postcard` encoding of the message DTO | `Message::encode_payload` (`crates/eggtunnel-proto/src/lib.rs:408-431`) |

`HEADER_LEN = 14` (`crates/eggtunnel-proto/src/lib.rs:14`).
Payload cap `MAX_FRAME_BYTES = 1 MiB` (`crates/eggtunnel-proto/src/lib.rs:15`, `docs/PROTOCOL.md:23-29`).

### 2.2 `encode_frame` (`crates/eggtunnel-proto/src/lib.rs:457-470`)

1. `message.encode_payload()` serializes via `postcard::to_allocvec`, mapping any serializer
   failure to `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:408-431`, macro at `crates/eggtunnel-proto/src/lib.rs:409-413`).
2. Length pre-check: `payload.len() > MAX_FRAME_BYTES` → `Err(FrameTooLarge)` **before**
   allocating the output frame (`crates/eggtunnel-proto/src/lib.rs:459-461`).
3. Header-first construction: `MAGIC` + `PROTOCOL_MAJOR BE` + `PROTOCOL_MINOR BE` +
   `message.kind() as u16 BE` + `payload.len() as u32 BE` + payload
   (`crates/eggtunnel-proto/src/lib.rs:462-469`).
4. Always stamps the current version (`1.1`); there is no API to encode an older/newer
   version. `kind()` is a total match over the 14 variants (`crates/eggtunnel-proto/src/lib.rs:389-407`).

### 2.3 `decode_frame` (`crates/eggtunnel-proto/src/lib.rs:474-525`)

Header-first, length pre-check, exact-one-frame:

| Step | Code | Semantics |
|---|---|---|
| Short header | `crates/eggtunnel-proto/src/lib.rs:475-477` | `input.len() < HEADER_LEN` → `TruncatedFrame` (caller should read more; **not** a hard error) |
| Magic | `crates/eggtunnel-proto/src/lib.rs:478-480` | `input[..4] != MAGIC` → `InvalidMagic` |
| Version | `crates/eggtunnel-proto/src/lib.rs:481-485` | major `!= PROTOCOL_MAJOR` → `UnsupportedVersion(major, minor)`; minor is read but **not enforced** (informational) |
| Message ID | `crates/eggtunnel-proto/src/lib.rs:486` + `crates/eggtunnel-proto/src/lib.rs:248-269` | unknown `u16` → `UnknownMessage(value)` |
| Length pre-check | `crates/eggtunnel-proto/src/lib.rs:487-493` | `len > MAX_FRAME_BYTES` → `FrameTooLarge` **before** slicing/copying/deserializing; `HEADER_LEN.checked_add(len)` overflow → `FrameTooLarge` |
| Truncated payload | `crates/eggtunnel-proto/src/lib.rs:494-496` | `input.len() < total` → `TruncatedFrame` |
| Payload decode | `crates/eggtunnel-proto/src/lib.rs:497-524` | `postcard::take_from_bytes` per `kind`; deserializer error → `InvalidPayload`; **trailing bytes inside the declared payload → `InvalidPayload`** (`crates/eggtunnel-proto/src/lib.rs:502-504`) |
| Return | `crates/eggtunnel-proto/src/lib.rs:524` | `Ok((message, total))`; bytes after `total` are left for the caller |

Key properties:

- **Concatenated frames:** decoder consumes exactly one frame and reports `total` bytes consumed
  (`crates/eggtunnel-proto/src/lib.rs:472-473` doc comment, `crates/eggtunnel-proto/src/lib.rs:524`).
  Callers loop with `rest = &rest[used..]` (test demonstration at
  `crates/eggtunnel-proto/src/lib.rs:596-603`).
- **No over-read:** if `input` holds `frame + extra`, the extra is untouched; `used == encoded.len()`
  is asserted at `crates/eggtunnel-proto/src/lib.rs:593`.
- **Error variants exercised:** `TruncatedFrame`, `InvalidMagic`, `UnsupportedVersion`,
  `UnknownMessage`, `FrameTooLarge`, `InvalidPayload` — see test at
  `crates/eggtunnel-proto/src/lib.rs:642-677`. `InvalidName` / `InvalidTarget` /
  `InvalidPayload` surface from nested DTO validation during `take_from_bytes`
  (via `serde(try_from)` — §4).

### 2.4 `wire_io.rs` transport adapter (`crates/eggtunnel/src/wire_io.rs:11-74`)

`eggtunnel-proto` itself does no I/O. The thin async adapter in the parent crate preserves
the same safety order:

| Function | Behavior |
|---|---|
| `io_error` (`crates/eggtunnel/src/wire_io.rs:11-17`) | classifies a transport I/O failure: only `ErrorKind::UnexpectedEof` is a `ProtocolError::TruncatedFrame`, everything else becomes `TunnelError::Io`, whose `termination_category()` is `Transport` |
| `read_message` (`crates/eggtunnel/src/wire_io.rs:19-56`) | `read_exact` 14-byte header; `decode_frame(&header)` must yield `TruncatedFrame` (any other `Err` is returned immediately, a header that decodes to a complete frame is `InvalidPayload` — fail closed, never a panic — `crates/eggtunnel/src/wire_io.rs:24-31`); parse `len` from `header[10..14]` and reject `len > MAX_FRAME_BYTES` **before** buffering (`crates/eggtunnel/src/wire_io.rs:32-36`); buffer the payload incrementally with `take(len).read_to_end` so a peer that announces the maximum frame and stalls holds only what it sent, not a pre-committed 1 MiB allocation (`crates/eggtunnel/src/wire_io.rs:37-47`, short payload → `TruncatedFrame`); final `decode_frame(&frame)` + exact-consumption check `consumed != frame.len()` → `InvalidPayload` (`crates/eggtunnel/src/wire_io.rs:48-54`) |
| `write_message` (`crates/eggtunnel/src/wire_io.rs:58-64`) | `encode_frame` then `write_all`; I/O failure classified by `io_error`, so a reset/refused/broken pipe is `Transport`, not `Protocol` |
| `read_boxed` / `write_boxed` (`crates/eggtunnel/src/wire_io.rs:66-74`) | same logic over Eggress `BoxStream` (delegates to `read_message` / `write_message`) |

All four return `TunnelError`, not `ProtocolError`: the adapter is the I/O boundary, so a
transport fault must be able to report itself as `Transport` instead of being laundered into
`Protocol` and skewing `last_termination` and reconnect accounting.

---

## 3. Every message type 1–15

IDs are explicit `#[repr(u16)]` discriminants (`crates/eggtunnel-proto/src/lib.rs:229-246`),
parsed by total `TryFrom<u16>` (`crates/eggtunnel-proto/src/lib.rs:248-269`), pinned by test
(`crates/eggtunnel-proto/src/lib.rs:607-639`), and documented at `docs/PROTOCOL.md:31-34`.
“Direction” below is the conventional control/data-plane direction as used by
`crates/eggtunnel/src/client.rs` and `crates/eggtunnel/src/server.rs`; the proto crate itself
does **not** enforce direction or ordering.

| ID | Variant (code) | Key fields | Direction | Place in session lifecycle |
|---:|---|---|---|---|
| 1 | `ClientHello` (`crates/eggtunnel-proto/src/lib.rs:232`, `crates/eggtunnel-proto/src/lib.rs:271-275`) | `version: ProtocolVersion`, `capabilities: Capabilities` | client → server | Handshake opener. Client sends first (`crates/eggtunnel/src/client.rs:709-717`); server requires it as the first frame on a control stream (`crates/eggtunnel/src/server/accept.rs:387-392` QUIC, `crates/eggtunnel/src/server/accept.rs:249-261` TCP/TLS dispatch). |
| 2 | `ServerHello` (`crates/eggtunnel-proto/src/lib.rs:233`, `crates/eggtunnel-proto/src/lib.rs:276-280`) | `version`, `capabilities` (same shape as `ClientHello`) | server → client | Handshake answer (`crates/eggtunnel/src/server/control.rs:94-101` sends; `crates/eggtunnel/src/client.rs:719-723` validates major version). |
| 3 | `Auth` (`crates/eggtunnel-proto/src/lib.rs:234`, `crates/eggtunnel-proto/src/lib.rs:281-285`) | `token: Vec<u8>` (private, `bounded_bytes`, ≤4096 B; redacted `Debug`) | client → server | Credential presentation inside the already-established secure transport (`docs/PROTOCOL.md:36-37`; sent at `crates/eggtunnel/src/client.rs:727-733`). |
| 4 | `AuthOk` (`crates/eggtunnel-proto/src/lib.rs:235`, `crates/eggtunnel-proto/src/lib.rs:317-320`) | `session_id: SessionId` | server → client | Authentication success + session binding (`crates/eggtunnel/src/server/control.rs:143` sends; `crates/eggtunnel/src/client.rs:735` extracts `session_id`, else `TunnelError::Authentication`). |
| 5 | `RegisterService` (`crates/eggtunnel-proto/src/lib.rs:236`, `crates/eggtunnel-proto/src/lib.rs:321-327`) | `service_id: ServiceId`, `name: ServiceName`, `requested_bind: RequestedBind`, `target: TcpTarget` | client → server | Registration request, one per service (initial loop at `crates/eggtunnel/src/client.rs:744-770`, dynamic at `crates/eggtunnel/src/client.rs:941-960`; server validates, binds, spawns `run_service` at `crates/eggtunnel/src/server/control.rs:164-177` via `register_service` at `crates/eggtunnel/src/server/control.rs:235-343`). Note: `target` is client-owned metadata; server never dials it (comment at `crates/eggtunnel/src/server/control.rs:272`). |
| 6 | `RegisterAck` (`crates/eggtunnel-proto/src/lib.rs:237`, `crates/eggtunnel-proto/src/lib.rs:328-332`) | `service_id`, `effective_bind: EffectiveBind { address: [u8;16], port: u16 }` | server → client | Registration success with server-chosen bind (`crates/eggtunnel/src/server/control.rs:333-339` sends; initial client path requires `ack.service_id == service.id` at `crates/eggtunnel/src/client.rs:759-765`, dynamic path correlates via `take_ack` at `crates/eggtunnel/src/client.rs:883-901`). |
| 7 | `UnregisterService` (`crates/eggtunnel-proto/src/lib.rs:238`, `crates/eggtunnel-proto/src/lib.rs:333-336`) | `service_id` | client → server | Deregistration (`crates/eggtunnel/src/client.rs:972` sends on `ClientCommand::Unregister`; `crates/eggtunnel/src/server/control.rs:178-187` cancels service, removes pending). Unknown IDs are silently tolerated server-side (remove-if-present). |
| 8 | `Open` (`crates/eggtunnel-proto/src/lib.rs:239`, `crates/eggtunnel-proto/src/lib.rs:337-341`) | `service_id`, `connection_id: ConnectionId` | server → client (control plane) | Per-external-connection demand: server listener accepted, server queues pending and sends `Open` (`crates/eggtunnel/src/server/service.rs:103` via `open_tx`, forwarded at `crates/eggtunnel/src/server/control.rs:205-210`); client receives at `crates/eggtunnel/src/client.rs:840-856`, resolves `service_id`, else `OpenReject(code 1)` at `crates/eggtunnel/src/client.rs:845`; semaphore-full → `OpenReject(code 2)` at `crates/eggtunnel/src/client.rs:853`. |
| 9 | `OpenReject` (`crates/eggtunnel-proto/src/lib.rs:240`, `crates/eggtunnel-proto/src/lib.rs:342-346`) | `connection_id`, `code: u16` | client → server | Negative answer to `Open` (control-loop rejects at `crates/eggtunnel/src/client.rs:845`, `crates/eggtunnel/src/client.rs:853`; data-path failure at `crates/eggtunnel/src/client/open.rs:83-86`; `crates/eggtunnel/src/server/control.rs:189-194` drops the pending entry). Codes are untyped `u16` (1 = unknown service, 2 = resource-exhausted in current client). |
| 10 | `Ping` (`crates/eggtunnel-proto/src/lib.rs:241`, `crates/eggtunnel-proto/src/lib.rs:347-350`) | `nonce: u64` | either direction (keepalive) | Client heartbeat ticker (`crates/eggtunnel/src/client.rs:795-819`; 20 s default at `crates/eggtunnel/src/common.rs:299`); each side answers `Ping` with `Pong{nonce}` (`crates/eggtunnel/src/client.rs:876`, `crates/eggtunnel/src/server/control.rs:196-198`). |
| 11 | `Pong` (`crates/eggtunnel-proto/src/lib.rs:242`, `crates/eggtunnel-proto/src/lib.rs:351-354`) | `nonce: u64` | either direction (answer) | Echoes the `Ping` nonce; client matches it against the outstanding ping and ignores non-matching `Pong`s (`crates/eggtunnel/src/client.rs:877-881`). |
| 12 | `Drain` (`crates/eggtunnel-proto/src/lib.rs:243`, `crates/eggtunnel-proto/src/lib.rs:355-358`) | `deadline_ms: u32` | either direction (graceful shutdown) | Client sends on cancellation (`crates/eggtunnel/src/client.rs:806-808`); server sends on the shutdown drain (`crates/eggtunnel/src/server/accept.rs:77-95`); receipt breaks the control loop on both sides (`crates/eggtunnel/src/client.rs:913-916`, `crates/eggtunnel/src/server/control.rs:200`, forwarded `Drain` breaks at `crates/eggtunnel/src/server/control.rs:205-210`). |
| 13 | `Error` / `ErrorMessage` (`crates/eggtunnel-proto/src/lib.rs:244`, `crates/eggtunnel-proto/src/lib.rs:359-363`) | `code: u16`, `diagnostic: BoundedDiagnostic` (≤256 B) | server → client | Terminal/negative ack: auth failure (`crates/eggtunnel/src/server/control.rs:111-117`, code 4) and registration failures via `write_registration_error` (`crates/eggtunnel/src/server/control.rs:344-352`, call sites at `crates/eggtunnel/src/server/control.rs:259` code 5, `:270` code 1, `:284` code 2, `:298` code 3); client maps handshake-time `Error` to `TunnelError::Authorization` (`crates/eggtunnel/src/client.rs:767`), and dynamic `Error` with a pending registration to `registration_error()` (`crates/eggtunnel/src/client.rs:903-911`, mapping at `crates/eggtunnel/src/client.rs:1026-1032`), or to `TunnelError::Authorization` when nothing is pending (`crates/eggtunnel/src/client.rs:909-912`); other unexpected messages hit `UnexpectedMessage` (`crates/eggtunnel/src/client.rs:918`). Note the enum variant is `Message::Error` but the struct is `ErrorMessage` (`crates/eggtunnel-proto/src/lib.rs:385`, `crates/eggtunnel-proto/src/lib.rs:521`). |
| 14 | `DataHello` (`crates/eggtunnel-proto/src/lib.rs:245`, `crates/eggtunnel-proto/src/lib.rs:364-369`) | `session_id: SessionId`, `service_id: ServiceId`, `connection_id: ConnectionId` | client → server (data plane) | First frame on each **data** connection, then opaque relay bytes (`docs/PROTOCOL.md:36-39`; client sends at `crates/eggtunnel/src/client/open.rs:67`; server requires it first on data streams at `crates/eggtunnel/src/server/accept.rs:470-478` QUIC / `crates/eggtunnel/src/server/accept.rs:249-260` TCP dispatch, correlated jointly at `crates/eggtunnel/src/server/pending.rs:30-97`). Wrong-session / unknown-connection / service-or-expiry mismatches are rejected and counted (`crates/eggtunnel/src/server/pending.rs:52-86`), covered by `crates/eggtunnel/src/server_tests/tcp.rs:1248`, `crates/eggtunnel/src/server_tests/tcp.rs:1309`, `crates/eggtunnel/src/server_tests/quic.rs:287`, `crates/eggtunnel/src/server_tests/quic.rs:406`, `crates/eggtunnel/src/server_tests/quic.rs:530`. |
| 15 | `RegisterReject` (`crates/eggtunnel-proto/src/lib.rs:284`, `crates/eggtunnel-proto/src/lib.rs:410-419`) | `service_id: ServiceId`, `code: u16` (same registration vocabulary as `Error`: 1 duplicate, 2 bind, 3 listener, 5 admission), `diagnostic: BoundedDiagnostic` | server → client (only with negotiated capability 1) | Correlated registration failure: server sends via `write_registration_response` (`crates/eggtunnel/src/server/control.rs:378-399`); client correlates by ServiceId+generation (`take_reject` at `crates/eggtunnel/src/client/service_state.rs:257-268`), mapping codes through `registration_error_code` (`crates/eggtunnel/src/client.rs:1150-1156`). Unknown/stale rejects and unnegotiated receipt fail closed. Codec round-trip + hostile-diagnostic tests at `crates/eggtunnel-proto/src/lib.rs:742-771`. |

Lifecycle summary (control path per [Architecture Overview](overview.md) §8):
`ClientHello → ServerHello → Auth → AuthOk → (RegisterService → RegisterAck | Error)* →
(Open → DataHello → opaque relay | OpenReject)*`, with `Ping/Pong`, `Drain`, `Error`,
`UnregisterService` interleaved. `DataHello` is the only message that appears on data
connections; the other 13 are control-plane.

---

## 4. Bounded types

Limits (`crates/eggtunnel-proto/src/lib.rs:15-20`):

| Constant (code) | Value | Applies to |
|---|---|---|
| `MAX_FRAME_BYTES` (`crates/eggtunnel-proto/src/lib.rs:15`) | `1024 * 1024` (1 MiB) | whole postcard payload per frame; checked on encode (`crates/eggtunnel-proto/src/lib.rs:459-461`) and on decode before slice/deserialize (`crates/eggtunnel-proto/src/lib.rs:488-493`) and again in `wire_io` before allocation (`crates/eggtunnel/src/wire_io.rs:20-23`) |
| `MAX_AUTH_TOKEN_BYTES` (`crates/eggtunnel-proto/src/lib.rs:20`) | 4096 | `Auth.token` bytes |
| `MAX_NAME_BYTES` (`crates/eggtunnel-proto/src/lib.rs:16`) | 128 | `ServiceName` byte length |
| `MAX_DIAGNOSTIC_BYTES` (`crates/eggtunnel-proto/src/lib.rs:17`) | 256 | `BoundedDiagnostic` byte length |
| `MAX_CAPABILITIES` (`crates/eggtunnel-proto/src/lib.rs:18`) | 32 | `Capabilities` entry count |
| `MAX_TARGET_HOST_BYTES` (`crates/eggtunnel-proto/src/lib.rs:19`) | 253 | `TcpTarget.host` byte length (DNS-name max) |

Validation rules + redaction:

| Type (code) | Validation | Redaction / display |
|---|---|---|
| `ServiceName` (`crates/eggtunnel-proto/src/lib.rs:87-127`) | `new` rejects empty, `len() > MAX_NAME_BYTES`, or any byte outside ASCII alphanumeric + `-_.` (`crates/eggtunnel-proto/src/lib.rs:94-98` → `InvalidName`); `#[serde(try_from = "String")]` (`crates/eggtunnel-proto/src/lib.rs:88`) + `TryFrom<String>` (`crates/eggtunnel-proto/src/lib.rs:110-115`) force revalidation on deserialize, so hostile postcard strings cannot bypass `new` | `Debug`/`Display` print the name in clear (`crates/eggtunnel-proto/src/lib.rs:117-127`) — names are non-secret routing labels |
| `TcpTarget` (`crates/eggtunnel-proto/src/lib.rs:152-197`) | `new` delegates the host to the shared `validate_target_host` (`crates/eggtunnel-proto/src/lib.rs:202-214`: empty, `len() > MAX_TARGET_HOST_BYTES`, whitespace, `char::is_control`, or any of `/?#@[]\\"'<>` — the shapes ambiguous in a URL authority) and rejects `port == 0` (`crates/eggtunnel-proto/src/lib.rs:177-184` → `InvalidTarget`). `Endpoint::parse` in the parent crate calls the same validator, so one host shape governs both the wire target and the server endpoint; a colon is accepted because the host travels without a port, so IPv6 literals stay valid; `#[serde(try_from = "WireTcpTarget")]` (`crates/eggtunnel-proto/src/lib.rs:142`) + `TryFrom<WireTcpTarget>` (`crates/eggtunnel-proto/src/lib.rs:154-159`) revalidate on decode; private `host` field with `host()`/`port()` accessors (`crates/eggtunnel-proto/src/lib.rs:173-178`) | derived `Debug` prints host/port in clear — treated as config metadata, not a secret |
| `Capabilities` (`crates/eggtunnel-proto/src/lib.rs:181-203`) | `new` rejects `ids.len() > MAX_CAPABILITIES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:186-189`); `#[serde(try_from = "Vec<u16>")]` (`crates/eggtunnel-proto/src/lib.rs:182`) + `TryFrom<Vec<u16>>` (`crates/eggtunnel-proto/src/lib.rs:198-203`) revalidate on decode; `Default` is empty (`crates/eggtunnel-proto/src/lib.rs:181`) | plain `Debug`; currently exchanged as empty set (see §5) |
| `Auth` (`crates/eggtunnel-proto/src/lib.rs:281-316`) | `new` rejects `token.len() > MAX_AUTH_TOKEN_BYTES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:288-292`); wire decode uses `#[serde(deserialize_with = "bounded_bytes")]` (`crates/eggtunnel-proto/src/lib.rs:283`) + `bounded_bytes` (`crates/eggtunnel-proto/src/lib.rs:300-309`) which rejects oversize tokens even if constructed by hand-rolled postcard bytes | custom `Debug` prints `Auth { token: "[REDACTED]" }` (`crates/eggtunnel-proto/src/lib.rs:310-316`); accessor is `token() -> &[u8]` (`crates/eggtunnel-proto/src/lib.rs:295-297`), field is private |
| `BoundedDiagnostic` (`crates/eggtunnel-proto/src/lib.rs:205-227`) | `new` rejects `len() > MAX_DIAGNOSTIC_BYTES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:211-214`); `#[serde(try_from = "String")]` (`crates/eggtunnel-proto/src/lib.rs:206`) + `TryFrom<String>` (`crates/eggtunnel-proto/src/lib.rs:222-227`) revalidate on decode | plain `Debug`; length cap bounds log amplification |
| `RequestedBind` (`crates/eggtunnel-proto/src/lib.rs:129-133`) | no validation in proto (`Loopback{port}` / `Ip{address:[u8;16],port}` are structurally deserialized); policy enforcement is in the server (`bind_to_socket`, `BindPolicy`) | derived `Debug` |
| `EffectiveBind` (`crates/eggtunnel-proto/src/lib.rs:135-139`) | no validation in proto; server-derived from `TcpListener::local_addr` | derived `Debug` |

ID semantics:

| ID (code) | Representation | Generation / equality | Debug |
|---|---|---|---|
| `SessionId` (`crates/eggtunnel-proto/src/lib.rs:47-64`) | `pub struct SessionId(pub [u8;16])`, `Copy`, `Eq`/`Hash`, `Serialize`/`Deserialize` | `generate()` via `getrandom::fill` (`crates/eggtunnel-proto/src/lib.rs:50-56`); 128-bit random; `PartialEq` is derived (non-constant-time) | fully redacted: `SessionId([REDACTED])` (`crates/eggtunnel-proto/src/lib.rs:58-64`) — a Session ID is a capability in `DataHello`, so no prefix is recoverable from logs |
| `ConnectionId` (`crates/eggtunnel-proto/src/lib.rs:61-85`) | `pub struct ConnectionId(pub [u8;16])`, same derives | `generate()` via `getrandom` (`crates/eggtunnel-proto/src/lib.rs:64-69`); `constant_time_eq` folds `a ^ b` with `\|` (`crates/eggtunnel-proto/src/lib.rs:71-78`) “useful when IDs are treated as capabilities” | fully redacted: `ConnectionId([REDACTED])` (`crates/eggtunnel-proto/src/lib.rs:81-85`) |
| `ServiceId` (`crates/eggtunnel-proto/src/lib.rs:58-59`) | `pub struct ServiceId(pub u64)`, `Copy`, `Debug`, `Eq`/`Hash` | no generation in proto; client-chosen per service (initial `crates/eggtunnel/src/client.rs:746-750`, dynamic `crates/eggtunnel/src/client.rs:941-945` copy `service.id`); server treats duplicate IDs/names as errors (`crates/eggtunnel/src/server/control.rs:261-270`) | transparent `u64` — non-secret multiplexing key |

Measurement notes: all `len()` checks are **byte** lengths (`String::len` / `Vec::len`), not
grapheme/char counts; `ServiceName` charset is checked per **byte**
(`value.bytes().all(...)` at `crates/eggtunnel-proto/src/lib.rs:96-98`), which for UTF-8
multibyte input fails closed (non-ASCII bytes are rejected). The `TcpTarget` host check is per
`char` (`host.chars().any(...)` at `crates/eggtunnel-proto/src/lib.rs:206`).

---

## 5. Versioning policy

| Item | Rule | Code / doc |
|---|---|---|
| Wire version | `1.1` (major-1 boundary preserves 1.0 interop) | `PROTOCOL_MAJOR = 1`, `PROTOCOL_MINOR = 1` (`crates/eggtunnel-proto/src/lib.rs:21-22`); `ProtocolVersion::CURRENT` (`crates/eggtunnel-proto/src/lib.rs:37-44`); `docs/PROTOCOL.md:1` |
| Crate version | workspace `0.2.0` line (`crates/eggtunnel-proto/Cargo.toml:4` inherits `version.workspace`; root `Cargo.toml:6` sets `version = "0.2.0"`) | `docs/PROTOCOL.md:3-6` states the split: current crate line `0.2.0`, wire `1.1` with 1.0 fallback |
| Major | reject on mismatch | `decode_frame` returns `UnsupportedVersion(major, minor)` if `major != PROTOCOL_MAJOR` (`crates/eggtunnel-proto/src/lib.rs:483-485`); tested with major 2 at `crates/eggtunnel-proto/src/lib.rs:660-666` |
| Minor | informational only | minor is decoded but never compared; `encode_frame` always stamps the current minor (`crates/eggtunnel-proto/src/lib.rs:518-519`); extensions are never inferred from minor — only from negotiated capabilities (`docs/PROTOCOL.md:44-60`). |
| Capabilities | negotiated intersection (ADR-0002) | `ClientHello`/`ServerHello` carry `Capabilities`; client advertises `Capabilities::supported()` (`crates/eggtunnel-proto/src/lib.rs:207-213`), server returns `supported().intersect(&hello.capabilities)`, client intersects again (`has` at `:227-229`, `intersect` at `:215-225`); unknown IDs ignored, emission sorted/unique. Registry: 1 = correlated rejection, 2 = drain deadline (`:25-31`); pinned by `capability_registry_is_pinned_and_intersection_is_a_set` (`:706-740`). 1.0 (empty) peers negotiate nothing. |
| Message IDs | stable, explicit, pinned | IDs 1–14 listed at `docs/PROTOCOL.md:31-34`; discriminants are explicit, “not derived from enum order” (`docs/PROTOCOL.md:33-34`); test `documented_wire_version_and_message_ids_are_pinned` asserts every discriminant and round-trips `TryFrom` (`crates/eggtunnel-proto/src/lib.rs:607-639`), including rejection of `0` and `15` (`crates/eggtunnel-proto/src/lib.rs:637-638`) |

Practical consequence for reviewers: any change to a discriminant, to `HEADER_LEN`/field order,
or to a DTO’s postcard shape is a compatibility event and must update `docs/PROTOCOL.md`
alongside the constants (the test comment says exactly this at
`crates/eggtunnel-proto/src/lib.rs:608-610`).

---

## 6. Security properties

| Property | Mechanism | Code |
|---|---|---|
| Constant-time `ConnectionId` comparison | `constant_time_eq` accumulates `diff \| (a ^ b)` over all 16 bytes, single `== 0` at the end; no early exit | `crates/eggtunnel-proto/src/lib.rs:71-78` |
| Redacted `Debug` for secrets/capabilities | `Auth` prints `[REDACTED]` instead of token bytes; `ConnectionId` prints `[REDACTED]`; `SessionId` prints only a 4-byte prefix | `crates/eggtunnel-proto/src/lib.rs:310-316`, `crates/eggtunnel-proto/src/lib.rs:81-85`, `crates/eggtunnel-proto/src/lib.rs:48-56` |
| Hostile-input revalidation (deserialize ≠ constructor bypass) | `ServiceName`, `TcpTarget`, `Capabilities`, `BoundedDiagnostic` all use `serde(try_from = …)` so `postcard::from_bytes` re-runs the validating constructor; `Auth` uses a custom `bounded_bytes` deserializer | `crates/eggtunnel-proto/src/lib.rs:88`, `crates/eggtunnel-proto/src/lib.rs:142`, `crates/eggtunnel-proto/src/lib.rs:182`, `crates/eggtunnel-proto/src/lib.rs:206`, `crates/eggtunnel-proto/src/lib.rs:283`, `crates/eggtunnel-proto/src/lib.rs:300-309`; negative tests at `crates/eggtunnel-proto/src/lib.rs:699-711` |
| Pre-copy / pre-deserialize length checks | `decode_frame` rejects `len > MAX_FRAME_BYTES` before slicing the payload (`FrameTooLarge`); `wire_io::read_message` rejects before `Vec::resize`/payload `read_exact`; `encode_frame` rejects oversize payloads before framing | `crates/eggtunnel-proto/src/lib.rs:488-493`, `crates/eggtunnel/src/wire_io.rs:20-23`, `crates/eggtunnel-proto/src/lib.rs:459-461`; tests at `crates/eggtunnel-proto/src/lib.rs:674-676`, `crates/eggtunnel-proto/src/lib.rs:680-696` |
| Strict payload consumption | `postcard::take_from_bytes` + `trailing.is_empty()` check rejects smuggled trailing bytes inside the declared length | `crates/eggtunnel-proto/src/lib.rs:498-507`; negative test crafts a 1-byte trailer with adjusted length at `crates/eggtunnel-proto/src/lib.rs:652-656` |
| `forbid(unsafe_code)` | whole crate refuses `unsafe` | `crates/eggtunnel-proto/src/lib.rs:1` |
| Random IDs via OS RNG | `SessionId::generate` / `ConnectionId::generate` use `getrandom::fill`, propagate `getrandom::Error` | `crates/eggtunnel-proto/src/lib.rs:40-46`, `crates/eggtunnel-proto/src/lib.rs:64-69` |
| Secret hygiene boundary | proto redacts but does **not** zeroize; zeroization lives one layer up (`SecretToken` in `common.rs` — see [Architecture Overview](overview.md) §2) | proto `Auth::token()` returns `&[u8]` (`crates/eggtunnel-proto/src/lib.rs:295-297`) with no `zeroize`; reviewers must confirm callers drop/clone minimally |

Caveats a reviewer should carry into `client.rs` / `server.rs`:

- `SessionId` uses derived `PartialEq`, not constant-time, while `ConnectionId` offers
  `constant_time_eq` — check each comparison site uses the intended one.
- `SessionId`’s `Debug` leaks 4 prefix bytes by design (correlation vs. secrecy trade-off).
- `ServiceName`, `TcpTarget`, diagnostics, and `ServiceId` are non-redacted by design.
- Auth secrecy depends on the transport: `docs/PROTOCOL.md:36-37` requires secure transport
  before `Auth`; enforcement is outside this crate.

---

## 7. Test inventory in `lib.rs` (`crates/eggtunnel-proto/src/lib.rs:527-754`)

| Test (anchor) | What it does | What it guards |
|---|---|---|
| `every_message_round_trips_and_concatenation_is_exact` (`crates/eggtunnel-proto/src/lib.rs:587-604`) | builds one sample of all 14 variants (`crates/eggtunnel-proto/src/lib.rs:531-584`: `web-main`, `ConnectionId([7;16])`, `SessionId([1;16])`, `ServiceId(1)`, …), asserts `decode(encode(m)) == m` and `used == encoded.len()`, then concatenates all frames and decodes in a loop asserting count == 14 | DTO/postcard symmetry for every type; exactly-one-frame + concatenated-stream contract; `kind()` ↔ `MessageType` ↔ decode-match alignment (`crates/eggtunnel-proto/src/lib.rs:389-407` vs `crates/eggtunnel-proto/src/lib.rs:508-523`) |
| `documented_wire_version_and_message_ids_are_pinned` (`crates/eggtunnel-proto/src/lib.rs:607-639`) | asserts `PROTOCOL_MAJOR == 1`, `PROTOCOL_MINOR == 0`, `CURRENT == {1,0}`, each `MessageType as u16` equals its documented ID and `TryFrom` round-trips, and `0`/`15` are rejected | wire-compat tripwire: any discriminant/version drift fails loudly; comment (`crates/eggtunnel-proto/src/lib.rs:608-610`) requires updating `docs/PROTOCOL.md` with the constants |
| `rejects_bad_headers_lengths_and_unknown_ids` (`crates/eggtunnel-proto/src/lib.rs:642-677`) | on a valid `Ping` frame: 3-byte prefix → `TruncatedFrame`; 1-byte-short frame → `TruncatedFrame`; appended trailer byte with bumped length → `InvalidPayload`; zeroed magic → `InvalidMagic`; major 2 → `UnsupportedVersion(2,0)`; ID `0xffff` → `UnknownMessage(65535)`; length `MAX+1` → `FrameTooLarge` | each header-check branch (`crates/eggtunnel-proto/src/lib.rs:475-489`); trailing-byte strictness (`crates/eggtunnel-proto/src/lib.rs:502-504`); unknown-ID path (`crates/eggtunnel-proto/src/lib.rs:248-269`) |
| `maximum_frame_length_is_checked_before_payload_decode` (`crates/eggtunnel-proto/src/lib.rs:680-696`) | hand-builds `Ping`-tagged headers with lengths `MAX-1`, `MAX` (garbage payload → `InvalidPayload`, proving the length gate passed and decode was attempted) and a header-only frame with `MAX+1` → `FrameTooLarge` (proving rejection precedes payload read/deserialize) | pre-copy / pre-deserialize length gate (`crates/eggtunnel-proto/src/lib.rs:488-493`); distinguishes “length OK but payload bad” from “length itself rejected”; note the `MAX`-length cases allocate ~1 MiB each — acceptable in unit tests but not a pattern to copy into hot paths |
| `hostile_wire_strings_vectors_and_tokens_are_revalidated` (`crates/eggtunnel-proto/src/lib.rs:699-711`) | postcard-encodes over-limit name (129 B), diagnostic (257 B), `MAX_CAPABILITIES+1` caps, `MAX_AUTH+1` token and asserts direct `from_bytes` / `new` fail | `serde(try_from)` + `bounded_bytes` revalidation cannot be bypassed by crafting wire bytes (`crates/eggtunnel-proto/src/lib.rs:88`, `crates/eggtunnel-proto/src/lib.rs:142`, `crates/eggtunnel-proto/src/lib.rs:182`, `crates/eggtunnel-proto/src/lib.rs:206`, `crates/eggtunnel-proto/src/lib.rs:300-309`) |
| `validates_bounded_types_and_redacts_secrets` (`crates/eggtunnel-proto/src/lib.rs:714-722`) | empty name rejected; 128 B name accepted; 129 B rejected; `TcpTarget("host", 0)` rejected; 257 B diagnostic rejected; `Debug(Auth("secret"))` and `Debug(ConnectionId)` do not contain secret bytes | boundary values (empty / max / max+1); port-0 rule (`crates/eggtunnel-proto/src/lib.rs:167`); redaction (`crates/eggtunnel-proto/src/lib.rs:310-316`, `crates/eggtunnel-proto/src/lib.rs:81-85`) |
| `ids_have_separate_types_and_connection_comparison_is_correct` (`crates/eggtunnel-proto/src/lib.rs:725-734`) | two generated `SessionId`s differ; `constant_time_eq` is reflexive and rejects zeros; `size_of::<ConnectionId>() == 16`; `ServiceId(42)` type-checks | RNG uniqueness smoke test; constant-time comparator correctness; 16-byte wire size; nominal typing prevents accidental `SessionId`/`ConnectionId`/`ServiceId` mixing |
| `arbitrary_input_never_panics` (`crates/eggtunnel-proto/src/lib.rs:737-753`) | xorshift-64 PRNG (`0xD1CE_BA5E_F00D` seed), 10 000 samples of length `0..4097`, `let _ = decode_frame(&bytes)` ignoring the result | fuzz-style never-panics gate over header + small-payload inputs; guards indexing (`input[..4]`, `[input[4], input[5]]`, …), `u32→usize` conversion, `checked_add`, and postcard error paths. Limitation: lengths cap at 4097 so the `FrameTooLarge` branch via huge declared lengths is covered by the dedicated max-length tests above, not here; no coverage requirement on output correctness for random bytes |

---

## 8. Review checklist

### Compatibility risks

- [ ] **Discriminant / DTO drift.** IDs are explicit but postcard field order/types are implicit.
  Adding/removing/reordering a struct field, changing `RequestedBind`/`EffectiveBind` layout, or
  reusing an ID breaks peers with no runtime fallback. The pinned-ID test
  (`crates/eggtunnel-proto/src/lib.rs:607-639`) catches discriminant changes, **not** DTO-shape
  changes — require golden-vector / cross-version tests before any DTO edit.
- [x] **Minor-version blindness resolved by ADR-0002.** `decode_frame` still ignores minor for framing, but extensions are negotiated capabilities, never minor inference (`docs/PROTOCOL.md:44-60`).
- [ ] **Wire vs. crate version confusion.** Wire `1.1` ≠ crate `0.2.0`
  (root `Cargo.toml:6`; see `docs/PROTOCOL.md:3-6`). Do not gate wire behavior on
  `CARGO_PKG_VERSION`; gate only on negotiated capabilities (never minor alone).
- [x] **`Capabilities` negotiated (ADR-0002).** Non-empty sets intersect bilaterally; unknown IDs ignored; IDs 1–2 pinned with `RegisterReject` (ID 15) as the only extension-only message.
- [ ] **Untyped codes.** `OpenReject.code: u16` (`crates/eggtunnel-proto/src/lib.rs:345`) and
  `ErrorMessage.code: u16` (`crates/eggtunnel-proto/src/lib.rs:361`) have no enum; client/server
  assign meaning ad hoc (e.g. codes 1/2 at `crates/eggtunnel/src/client/reconnect.rs:170`,
  `crates/eggtunnel/src/client/reconnect.rs:163` plus `crates/eggtunnel/src/client/open.rs:83-86`; codes 5/1/2/3 at `crates/eggtunnel/src/server/control.rs:42`,
  `crates/eggtunnel/src/server/control.rs:39`, `crates/eggtunnel/src/server/control.rs:178`,
  `crates/eggtunnel/src/server/control.rs:181`). Document new codes centrally or risk silent
  misinterpretation across versions.

### Bound-bypass risks

- [ ] **Construction vs. deserialization.** Every bounded string/vector/token type must keep its
  `serde(try_from)` / `deserialize_with` attribute in sync with `new()`. Removing
  `#[serde(try_from = "String")]` from `ServiceName` (`crates/eggtunnel-proto/src/lib.rs:88`),
  `TcpTarget` (`crates/eggtunnel-proto/src/lib.rs:142`), `Capabilities`
  (`crates/eggtunnel-proto/src/lib.rs:182`), `BoundedDiagnostic`
  (`crates/eggtunnel-proto/src/lib.rs:206`), or `bounded_bytes` from `Auth`
  (`crates/eggtunnel-proto/src/lib.rs:283`) would open a bypass the unit tests at
  `crates/eggtunnel-proto/src/lib.rs:699-711` are designed to catch — run them after any serde
  refactor.
- [ ] **Byte vs. char.** `ServiceName`/`BoundedDiagnostic`/host limits use byte `len()`.
  The `TcpTarget` host check is `chars().any(...)` over whitespace, control, and URL-ambiguous
  characters (`crates/eggtunnel-proto/src/lib.rs:206`). Non-`control` Unicode (e.g. bidi
  overrides, zero-width) in hosts/diagnostics passes validation — confirm upper layers
  normalize or reject where display/lookup matters.
- [ ] **`RequestedBind` / `EffectiveBind` have no proto-level validation**
  (`crates/eggtunnel-proto/src/lib.rs:129-139`). `InvalidBind`
  (`crates/eggtunnel-proto/src/lib.rs:449-450`) is defined but never constructed in the
  workspace — dead variant today. Policy lives server-side (`bind_to_socket`, `BindPolicy`).
  Either wire the variant or remove it; a reviewer should not assume the proto rejects bad binds.
- [ ] **Double length gate.** Both `decode_frame`
  (`crates/eggtunnel-proto/src/lib.rs:557-560`) and `wire_io::read_message`
  (`crates/eggtunnel/src/wire_io.rs:32-36`) enforce `MAX_FRAME_BYTES`. Keep both: the first
  protects pure-decode callers, the second protects the allocating network path. Removing either
  re-opens allocation-before-check for that caller.
- [ ] **`u32 → usize` and `checked_add`.** Length is `u32 BE`
  (`crates/eggtunnel-proto/src/lib.rs:487`); `checked_add` guards theoretical 32-bit overflow
  (`crates/eggtunnel-proto/src/lib.rs:491-493`). On 16-bit targets `u32 as usize` truncation
  would be a concern — out of scope for the declared `rust-version = 1.89` tier but worth a
  comment if portability is ever claimed.

### Postcard trailing-bytes strictness

- [ ] **Strictness is load-bearing.** `dec!` rejects any trailing bytes inside the declared
  payload (`crates/eggtunnel-proto/src/lib.rs:498-507`). Without the
  `trailing.is_empty()` check, a sender could smuggle a second logical message inside one
  frame’s length prefix, breaking the exactly-one-frame invariant and confusing
  concatenation loops. The trailer test (`crates/eggtunnel-proto/src/lib.rs:652-656`) must keep
  failing if strictness regresses.
- [ ] **`wire_io` exact-consumption check is the second half.**
  `consumed != frame.len()` → `InvalidPayload` (`crates/eggtunnel/src/wire_io.rs:31-34`) defends
  the `read_exact`-assembled path even though `decode_frame` already enforces intra-payload
  strictness. Both layers should stay strict.
- [ ] **Postcard upgrade risk.** `postcard` is `version = "1"` with `alloc`
  (root `Cargo.toml:15`). A major postcard encoding change would silently break interop
  despite identical Rust types — pin/audit postcard upgrades as wire-compat events, and prefer
  golden byte-vectors over round-trip-only tests for long-term stability.

### ID-confusion risks

- [ ] **Three ID types, three secrecy levels.** `ServiceId(u64)` is transparent and
  client-chosen; `SessionId` leaks a 4-byte prefix in `Debug`; `ConnectionId` is fully redacted
  with constant-time eq. Review every log line and every `==` vs `constant_time_eq` call site:
  using derived `==` on `ConnectionId` (available via `PartialEq`) instead of
  `constant_time_eq` (`crates/eggtunnel-proto/src/lib.rs:71-78`) loses the timing property the
  doc comment promises.
- [ ] **`SessionId` copy-paste across planes.** The same `SessionId` appears in `AuthOk`
  (`crates/eggtunnel-proto/src/lib.rs:317-320`) and `DataHello`
  (`crates/eggtunnel-proto/src/lib.rs:364-369`). Server must verify all three `DataHello`
  components jointly (session + service + connection); partial matching enables cross-service
  or replay confusion (joint verification at `crates/eggtunnel/src/server/pending.rs:30-95`). Server-side tests already cover wrong-session/replay/stale `DataHello`
  (e.g. `crates/eggtunnel/src/server_tests/tcp.rs:1248`, `crates/eggtunnel/src/server_tests/tcp.rs:1309`,
  `crates/eggtunnel/src/server_tests/quic.rs:287`, `crates/eggtunnel/src/server_tests/quic.rs:406`,
  `crates/eggtunnel/src/server_tests/quic.rs:530`) —
  keep them green when touching correlation logic.
- [ ] **`ServiceId` collisions.** Proto does not allocate or deduplicate `ServiceId`; server
  rejects duplicates per session (`crates/eggtunnel/src/server/control.rs:261-264`). Client-side ID
  reuse across reconnects/generations is a caller bug the proto cannot catch — check embedders
  (`fixtures/embedder`, `examples/`) generate fresh IDs per registration.
- [ ] **`UnexpectedMessage` is a protocol-state signal, not a decode error.**
  Defined at `crates/eggtunnel-proto/src/lib.rs:453-454`, raised only by client/server state
  machines (`crates/eggtunnel/src/client/reconnect.rs:287`, `crates/eggtunnel/src/client/reconnect.rs:413`,
  `crates/eggtunnel/src/client/reconnect.rs:431`, `crates/eggtunnel/src/server/accept.rs:389`,
  `crates/eggtunnel/src/server/accept.rs:472`, `crates/eggtunnel/src/server/accept.rs:261`,
  `crates/eggtunnel/src/server/accept.rs:279`, `crates/eggtunnel/src/server/control.rs:201`). Fuzzing `decode_frame` alone
  (`crates/eggtunnel-proto/src/lib.rs:737-753`) never exercises it — state-machine
  testing belongs in `client.rs`/`server.rs`.

---

*See also: [Architecture Overview](overview.md) §§1–2 and §8 for session-lifecycle context;
`docs/PROTOCOL.md` for the normative wire statement; `crates/eggtunnel/src/wire_io.rs:7-58`
for the header-first network adapter.*

### Session-time registration and heartbeat (M009)

The established Session accepts RegisterService and UnregisterService
repeatedly. RegisterAck correlates by ServiceId; Error carries only a code,
so the client permits only one dynamic registration request in flight and
correlates that response to the sole pending registration. Ping and Pong may
repeat during the Session and correlate by nonce. M009 does not change these
wire messages or add a message type.
