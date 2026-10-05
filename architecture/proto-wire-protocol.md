# Proto wire protocol — deep dive

Back to [Architecture Overview](overview.md) §1.

Sources: `crates/eggtunnel-proto/src/lib.rs` (991 lines, wire v1.1), `crates/eggtunnel-proto/Cargo.toml`,
`docs/PROTOCOL.md`, `crates/eggtunnel/src/wire_io.rs` (209 lines; adapter code at `:1-77`).
Cross-references below use `file:line` anchors. All claims were read from code; no invented behavior.

---

## 1. Purpose, scope, non-goals

**Purpose.** `eggtunnel-proto` is the runtime-neutral, bounded native Eggtunnel control-plane
wire protocol. It owns:

- bounded wire DTOs and their validation (`crates/eggtunnel-proto/src/lib.rs:13-22`, `crates/eggtunnel-proto/src/lib.rs:95-474`),
- the 14-byte framing (`crates/eggtunnel-proto/src/lib.rs:2-7`, `docs/PROTOCOL.md:17-26`),
- `encode_frame` / `decode_frame` (`crates/eggtunnel-proto/src/lib.rs:565-635`),
- the 15 stable message IDs (`crates/eggtunnel-proto/src/lib.rs:314-332`, `docs/PROTOCOL.md:36-39`).

**Scope.**

- Framing: `ETUN` magic + major/minor `u16 BE` + message-ID `u16 BE` + payload-len `u32 BE` +
  exactly one `postcard` payload (`crates/eggtunnel-proto/src/lib.rs:565-579`, `crates/eggtunnel-proto/src/lib.rs:583-635`).
- Validation: length/charset pre-checks before copy or deserialize
  (`crates/eggtunnel-proto/src/lib.rs:100-109`, `crates/eggtunnel-proto/src/lib.rs:169-177`,
  `crates/eggtunnel-proto/src/lib.rs:230-242`, `crates/eggtunnel-proto/src/lib.rs:285-312`,
  `crates/eggtunnel-proto/src/lib.rs:394-403`, `crates/eggtunnel-proto/src/lib.rs:584-605`).
- Version gate: wire `1.1` (major 1 keeps the 1.0 boundary), major mismatch rejected, minor informational; extensions gated by capability intersection only (ADR-0002)
  (`crates/eggtunnel-proto/src/lib.rs:21-22`, `crates/eggtunnel-proto/src/lib.rs:590-594`,
  `docs/PROTOCOL.md:1-15`).
- Typed errors for every hostile-input class (`crates/eggtunnel-proto/src/lib.rs:541-563`).

**Non-goals (explicitly out of this crate).**

| Non-goal | Where it lives instead | Evidence |
|---|---|---|
| No sockets / async / timers / tasks | `crates/eggtunnel/src/wire_io.rs`, `client.rs`, `server.rs` via Eggress transports | `crates/eggtunnel-proto/Cargo.toml:15-20` depends only on `serde`, `postcard` (`alloc`), `thiserror`, `getrandom`, `zeroize`; `lib.rs:1` is `#![forbid(unsafe_code)]` and uses `core::fmt` (`crates/eggtunnel-proto/src/lib.rs:9`) |
| No TLS / QUIC / WebSocket / proxy | `crates/eggtunnel/src/wire_io.rs:1-77`, feature gates in `crates/eggtunnel/Cargo.toml` | proto never imports `tokio`, `rustls`, `eggress-*` |
| No session state machine | `crates/eggtunnel/src/client.rs:697-746` (handshake) + `crates/eggtunnel/src/client.rs:828-1000` (control loop), `crates/eggtunnel/src/server/control.rs:182-253` (control loop) + first-frame gates `crates/eggtunnel/src/server/accept.rs:416-427`, `crates/eggtunnel/src/server/accept.rs:507-515`, `crates/eggtunnel/src/server/accept.rs:267-313` enforce ordering with `UnexpectedMessage` | `decode_frame` never returns `UnexpectedMessage` (`crates/eggtunnel-proto/src/lib.rs:583-635`); the variant is only constructed by client/server (`crates/eggtunnel/src/client.rs:732`, `crates/eggtunnel/src/client.rs:918`, `crates/eggtunnel/src/client.rs:961`, `crates/eggtunnel/src/client.rs:982`, `crates/eggtunnel/src/client.rs:993`, `crates/eggtunnel/src/server/accept.rs:310`, `crates/eggtunnel/src/server/accept.rs:386`, `crates/eggtunnel/src/server/accept.rs:425`, `crates/eggtunnel/src/server/accept.rs:512`, `crates/eggtunnel/src/server/control.rs:244`) |
| No application data framing | opaque bytes after `DataHello` | `docs/PROTOCOL.md:44-46`: “After DataHello, data streams carry opaque application bytes; application payload frames are not part of the control protocol.” |
| No credential transport security | caller must establish secure transport first | `docs/PROTOCOL.md:43-44`: “Credentials … must only be sent after a secure transport is established.” |

Related overview: [Architecture Overview](overview.md) §1 summarizes this crate as
“Runtime-neutral, `forbid(unsafe_code)`, no socket/async/timer/task dependencies.”

---

## 2. Frame layout + `encode_frame` / `decode_frame` semantics

### 2.1 Layout

Defined at `crates/eggtunnel-proto/src/lib.rs:13-22` and documented at `docs/PROTOCOL.md:17-26`:

| Offset | Size | Field | Encoding | Constant / code |
|---:|---:|---|---|---|
| 0 | 4 | magic | ASCII `ETUN` | `MAGIC` (`crates/eggtunnel-proto/src/lib.rs:13`) |
| 4 | 2 | major version | `u16 BE` (`1`) | `PROTOCOL_MAJOR` (`crates/eggtunnel-proto/src/lib.rs:21`) |
| 6 | 2 | minor version | `u16 BE` (`1`) | `PROTOCOL_MINOR` (`crates/eggtunnel-proto/src/lib.rs:22`) |
| 8 | 2 | message ID | `u16 BE`, explicit discriminant 1–15 | `MessageType` `#[repr(u16)]` (`crates/eggtunnel-proto/src/lib.rs:314-332`) |
| 10 | 4 | payload length | `u32 BE`, bytes of the one postcard payload | written at `crates/eggtunnel-proto/src/lib.rs:576`, read at `crates/eggtunnel-proto/src/lib.rs:596` |
| 14 | variable | payload | `postcard` encoding of the message DTO | `Message::encode_payload` (`crates/eggtunnel-proto/src/lib.rs:515-538`) |

`HEADER_LEN = 14` (`crates/eggtunnel-proto/src/lib.rs:14`).
Payload cap `MAX_FRAME_BYTES = 1 MiB` (`crates/eggtunnel-proto/src/lib.rs:15`, `docs/PROTOCOL.md:28-34`).

### 2.2 `encode_frame` (`crates/eggtunnel-proto/src/lib.rs:565-579`)

1. `message.encode_payload()` serializes via `postcard::to_allocvec`, mapping any serializer
   failure to `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:515-538`, macro at `crates/eggtunnel-proto/src/lib.rs:516-520`).
2. Length pre-check: `payload.len() > MAX_FRAME_BYTES` → `Err(FrameTooLarge)` **before**
   allocating the output frame (`crates/eggtunnel-proto/src/lib.rs:567-569`).
3. Header-first construction: `MAGIC` + `PROTOCOL_MAJOR BE` + `PROTOCOL_MINOR BE` +
   `message.kind() as u16 BE` + `payload.len() as u32 BE` + payload
   (`crates/eggtunnel-proto/src/lib.rs:571-577`).
4. Always stamps the current version (`1.1`); there is no API to encode an older/newer
   version. `kind()` is a total match over the 15 variants (`crates/eggtunnel-proto/src/lib.rs:495-514`).

### 2.3 `decode_frame` (`crates/eggtunnel-proto/src/lib.rs:583-635`)

Header-first, length pre-check, exact-one-frame:

| Step | Code | Semantics |
|---|---|---|
| Short header | `crates/eggtunnel-proto/src/lib.rs:584-586` | `input.len() < HEADER_LEN` → `TruncatedFrame` (caller should read more; **not** a hard error) |
| Magic | `crates/eggtunnel-proto/src/lib.rs:587-589` | `input[..4] != MAGIC` → `InvalidMagic` |
| Version | `crates/eggtunnel-proto/src/lib.rs:590-594` | major `!= PROTOCOL_MAJOR` → `UnsupportedVersion(major, minor)`; minor is read but **not enforced** (informational) |
| Message ID | `crates/eggtunnel-proto/src/lib.rs:595` + `crates/eggtunnel-proto/src/lib.rs:334-356` | unknown `u16` → `UnknownMessage(value)` |
| Length pre-check | `crates/eggtunnel-proto/src/lib.rs:596-602` | `len > MAX_FRAME_BYTES` → `FrameTooLarge` **before** slicing/copying/deserializing; `HEADER_LEN.checked_add(len)` overflow → `FrameTooLarge` |
| Truncated payload | `crates/eggtunnel-proto/src/lib.rs:603-605` | `input.len() < total` → `TruncatedFrame` |
| Payload decode | `crates/eggtunnel-proto/src/lib.rs:606-633` | `postcard::take_from_bytes` per `kind`; deserializer error → `InvalidPayload`; **trailing bytes inside the declared payload → `InvalidPayload`** (`crates/eggtunnel-proto/src/lib.rs:611-613`) |
| Return | `crates/eggtunnel-proto/src/lib.rs:634` | `Ok((message, total))`; bytes after `total` are left for the caller |

Key properties:

- **Concatenated frames:** decoder consumes exactly one frame and reports `total` bytes consumed
  (`crates/eggtunnel-proto/src/lib.rs:581-582` doc comment, `crates/eggtunnel-proto/src/lib.rs:634`).
  Callers loop with `rest = &rest[used..]` (test demonstration at
  `crates/eggtunnel-proto/src/lib.rs:711-717`).
- **No over-read:** if `input` holds `frame + extra`, the extra is untouched; `used == encoded.len()`
  is asserted at `crates/eggtunnel-proto/src/lib.rs:708`.
- **Error variants exercised:** `TruncatedFrame`, `InvalidMagic`, `UnsupportedVersion`,
  `UnknownMessage`, `FrameTooLarge`, `InvalidPayload` — see test at
  `crates/eggtunnel-proto/src/lib.rs:831-867`. `InvalidName` / `InvalidTarget` /
  `InvalidPayload` surface from nested DTO validation during `take_from_bytes`
  (via `serde(try_from)` — §4).

### 2.4 `wire_io.rs` transport adapter (`crates/eggtunnel/src/wire_io.rs:13-77`)

`eggtunnel-proto` itself does no I/O. The thin async adapter in the parent crate preserves
the same safety order:

| Function | Behavior |
|---|---|
| `io_error` (`crates/eggtunnel/src/wire_io.rs:13-19`) | classifies a transport I/O failure: only `ErrorKind::UnexpectedEof` is a `ProtocolError::TruncatedFrame`, everything else becomes `TunnelError::Io`, whose `termination_category()` is `Transport` |
| `read_message` (`crates/eggtunnel/src/wire_io.rs:21-58`) | `read_exact` 14-byte header; `decode_frame(&header)` must yield `TruncatedFrame` (any other `Err` is returned immediately, a header that decodes to a complete frame is `InvalidPayload` — fail closed, never a panic — `crates/eggtunnel/src/wire_io.rs:24-33`); parse `len` from `header[10..14]` and reject `len > MAX_FRAME_BYTES` **before** buffering (`crates/eggtunnel/src/wire_io.rs:34-37`); buffer the payload incrementally with `take(len).read_to_end` so a peer that announces the maximum frame and stalls holds only what it sent, not a pre-committed 1 MiB allocation (`crates/eggtunnel/src/wire_io.rs:38-49`, short payload → `TruncatedFrame`); final `decode_frame(&frame)` + exact-consumption check `consumed != frame.len()` → `InvalidPayload` (`crates/eggtunnel/src/wire_io.rs:50-56`) |
| `write_message` (`crates/eggtunnel/src/wire_io.rs:60-66`) | `encode_frame` then `write_all`; I/O failure classified by `io_error`, so a reset/refused/broken pipe is `Transport`, not `Protocol` |
| `read_boxed` / `write_boxed` (`crates/eggtunnel/src/wire_io.rs:68-77`) | same logic over Eggress `BoxStream` (delegates to `read_message` / `write_message`) |

All four return `TunnelError`, not `ProtocolError`: the adapter is the I/O boundary, so a
transport fault must be able to report itself as `Transport` instead of being laundered into
`Protocol` and skewing `last_termination` and reconnect accounting.

---

## 3. Every message type 1–15

IDs are explicit `#[repr(u16)]` discriminants (`crates/eggtunnel-proto/src/lib.rs:314-332`),
parsed by total `TryFrom<u16>` (`crates/eggtunnel-proto/src/lib.rs:334-356`), pinned by test
(`crates/eggtunnel-proto/src/lib.rs:721-756`), and documented at `docs/PROTOCOL.md:36-39`.
“Direction” below is the conventional control/data-plane direction as used by
`crates/eggtunnel/src/client.rs` and `crates/eggtunnel/src/server.rs`; the proto crate itself
does **not** enforce direction or ordering.

| ID | Variant (code) | Key fields | Direction | Place in session lifecycle |
|---:|---|---|---|---|
| 1 | `ClientHello` (`crates/eggtunnel-proto/src/lib.rs:317`, `crates/eggtunnel-proto/src/lib.rs:358-362`) | `version: ProtocolVersion`, `capabilities: Capabilities` | client → server | Handshake opener. Client sends first (`crates/eggtunnel/src/client.rs:713-722`); server requires it as the first frame on a control stream (`crates/eggtunnel/src/server/accept.rs:416-427` QUIC, `crates/eggtunnel/src/server/accept.rs:287-303` TCP/TLS dispatch). |
| 2 | `ServerHello` (`crates/eggtunnel-proto/src/lib.rs:318`, `crates/eggtunnel-proto/src/lib.rs:363-367`) | `version`, `capabilities` (same shape as `ClientHello`) | server → client | Handshake answer (`crates/eggtunnel/src/server/control.rs:112-120` sends; `crates/eggtunnel/src/client.rs:724-737` validates major version and re-intersects capabilities). |
| 3 | `Auth` (`crates/eggtunnel-proto/src/lib.rs:319`, `crates/eggtunnel-proto/src/lib.rs:368-372`) | `token: Vec<u8>` (private, `bounded_bytes`, ≤4096 B; redacted `Debug`) | client → server | Credential presentation inside the already-established secure transport (`docs/PROTOCOL.md:43-44`; sent at `crates/eggtunnel/src/client.rs:739-743`). |
| 4 | `AuthOk` (`crates/eggtunnel-proto/src/lib.rs:320`, `crates/eggtunnel-proto/src/lib.rs:411-414`) | `session_id: SessionId` | server → client | Authentication success + session binding (`crates/eggtunnel/src/server/control.rs:164-169` sends; `crates/eggtunnel/src/client.rs:745-747` extracts `session_id`, else `TunnelError::Authentication`). |
| 5 | `RegisterService` (`crates/eggtunnel-proto/src/lib.rs:321`, `crates/eggtunnel-proto/src/lib.rs:415-421`) | `service_id: ServiceId`, `name: ServiceName`, `requested_bind: RequestedBind`, `target: TcpTarget` | client → server | Registration request, one per service (initial loop at `crates/eggtunnel/src/client.rs:767-796`, dynamic at `crates/eggtunnel/src/client.rs:1021-1032`; server dispatches at `crates/eggtunnel/src/server/control.rs:192-207` and admits via `register_service` at `crates/eggtunnel/src/server/control.rs:295-448`, spawning `run_service` at `crates/eggtunnel/src/server/control.rs:415-422`). Note: `target` is client-owned metadata; server never dials it (comment at `crates/eggtunnel/src/server/control.rs:291-293`). |
| 6 | `RegisterAck` (`crates/eggtunnel-proto/src/lib.rs:322`, `crates/eggtunnel-proto/src/lib.rs:422-426`) | `service_id`, `effective_bind: EffectiveBind { address: [u8;16], port: u16 }` | server → client | Registration success with server-chosen bind (`crates/eggtunnel/src/server/control.rs:438-446` sends; initial client path requires `ack.service_id == service.id` at `crates/eggtunnel/src/client.rs:781`, dynamic path correlates via `take_ack` at `crates/eggtunnel/src/client/service_state.rs:256-283` consumed at `crates/eggtunnel/src/client.rs:916-927`). |
| 7 | `UnregisterService` (`crates/eggtunnel-proto/src/lib.rs:323`, `crates/eggtunnel-proto/src/lib.rs:427-430`) | `service_id` | client → server | Deregistration (`crates/eggtunnel/src/client.rs:1070-1082` sends on `ClientCommand::Unregister`; `crates/eggtunnel/src/server/control.rs:208-218` cancels service, removes pending). Unknown IDs are silently tolerated server-side (remove-if-present). |
| 8 | `Open` (`crates/eggtunnel-proto/src/lib.rs:324`, `crates/eggtunnel-proto/src/lib.rs:431-435`) | `service_id`, `connection_id: ConnectionId` | server → client (control plane) | Per-external-connection demand: server listener accepted, server queues pending and sends `Open` (`crates/eggtunnel/src/server/service.rs:102` via `open_tx`, forwarded at `crates/eggtunnel/src/server/control.rs:248-253`); client receives at `crates/eggtunnel/src/client.rs:873`, resolves `service_id`, else `OpenReject(code 1)` at `crates/eggtunnel/src/client.rs:878`; semaphore-full → `OpenReject(code 2)` at `crates/eggtunnel/src/client.rs:886`. |
| 9 | `OpenReject` (`crates/eggtunnel-proto/src/lib.rs:325`, `crates/eggtunnel-proto/src/lib.rs:436-440`) | `connection_id`, `code: u16` | client → server | Negative answer to `Open` (control-loop rejects at `crates/eggtunnel/src/client.rs:878`, `crates/eggtunnel/src/client.rs:886`; data-path failure at `crates/eggtunnel/src/client/open.rs:87-90`; `crates/eggtunnel/src/server/control.rs:219-230` drops the pending entry). Codes are untyped `u16` (1 = unknown service, 2 = resource-exhausted in current client). |
| 10 | `Ping` (`crates/eggtunnel-proto/src/lib.rs:326`, `crates/eggtunnel-proto/src/lib.rs:441-444`) | `nonce: u64` | either direction (keepalive) | Client heartbeat ticker (`crates/eggtunnel/src/client.rs:840-852`; 20 s default at `crates/eggtunnel/src/common.rs:316`); each side answers `Ping` with `Pong{nonce}` (`crates/eggtunnel/src/client.rs:909`, `crates/eggtunnel/src/server/control.rs:231-239`). |
| 11 | `Pong` (`crates/eggtunnel-proto/src/lib.rs:327`, `crates/eggtunnel-proto/src/lib.rs:445-448`) | `nonce: u64` | either direction (answer) | Echoes the `Ping` nonce; client matches it against the outstanding ping and ignores non-matching `Pong`s (`crates/eggtunnel/src/client.rs:910-914`). |
| 12 | `Drain` (`crates/eggtunnel-proto/src/lib.rs:328`, `crates/eggtunnel-proto/src/lib.rs:449-452`) | `deadline_ms: u32` | either direction (graceful shutdown) | Client sends on cancellation (`crates/eggtunnel/src/client.rs:836`); server sends on the shutdown drain (`crates/eggtunnel/src/server/accept.rs:79-100`); receipt breaks the control loop on both sides (`crates/eggtunnel/src/client.rs:987-992`, `crates/eggtunnel/src/server/control.rs:240-243`, forwarded `Drain` breaks at `crates/eggtunnel/src/server/control.rs:248-253`). |
| 13 | `Error` / `ErrorMessage` (`crates/eggtunnel-proto/src/lib.rs:329`, `crates/eggtunnel-proto/src/lib.rs:453-457`) | `code: u16`, `diagnostic: BoundedDiagnostic` (≤256 B) | server → client | Terminal/negative ack: auth failure (`crates/eggtunnel/src/server/auth.rs:127-132`, code 4 defined at `crates/eggtunnel/src/server/auth.rs:28`) and registration failures via `write_registration_response` (`crates/eggtunnel/src/server/control.rs:454-480`, call sites at `crates/eggtunnel/src/server/control.rs:321` code 5, `:339` code 1, `:360` code 2, `:381` code 3, `:397` code 3); client maps handshake-time `Error` to `TunnelError::Authorization` (`crates/eggtunnel/src/client.rs:791-794`), and dynamic `Error` with a pending registration to `registration_error()` (`crates/eggtunnel/src/client.rs:938-953`, mapping at `crates/eggtunnel/src/client.rs:1157-1159`), or to `TunnelError::Authorization` when nothing is pending (`crates/eggtunnel/src/client.rs:951-953`); other unexpected messages hit `UnexpectedMessage` (`crates/eggtunnel/src/client.rs:993`). Note the enum variant is `Message::Error` but the struct is `ErrorMessage` (`crates/eggtunnel-proto/src/lib.rs:490`, `crates/eggtunnel-proto/src/lib.rs:630`). |
| 14 | `DataHello` (`crates/eggtunnel-proto/src/lib.rs:330`, `crates/eggtunnel-proto/src/lib.rs:469-474`) | `session_id: SessionId`, `service_id: ServiceId`, `connection_id: ConnectionId` | client → server (data plane) | First frame on each **data** connection, then opaque relay bytes (`docs/PROTOCOL.md:44-46`; client sends at `crates/eggtunnel/src/client/open.rs:70`; server requires it first on data streams at `crates/eggtunnel/src/server/accept.rs:507-515` QUIC / `crates/eggtunnel/src/server/accept.rs:271-286` TCP dispatch, correlated jointly at `crates/eggtunnel/src/server/pending.rs:29-100`). Wrong-session / unknown-connection / service-or-expiry mismatches are rejected and counted (`crates/eggtunnel/src/server/pending.rs:39-91`), covered by `crates/eggtunnel/src/server_tests/tcp.rs:1248`, `crates/eggtunnel/src/server_tests/tcp.rs:1309`, `crates/eggtunnel/src/server_tests/quic.rs:361`, `crates/eggtunnel/src/server_tests/quic.rs:480`, `crates/eggtunnel/src/server_tests/quic.rs:604`. |
| 15 | `RegisterReject` (`crates/eggtunnel-proto/src/lib.rs:331`, `crates/eggtunnel-proto/src/lib.rs:458-468`) | `service_id: ServiceId`, `code: u16` (same registration vocabulary as `Error`: 1 duplicate, 2 bind, 3 listener, 5 admission), `diagnostic: BoundedDiagnostic` | server → client (only with negotiated capability 1) | Correlated registration failure: server sends via `write_registration_response` (`crates/eggtunnel/src/server/control.rs:454-480`); client correlates by ServiceId+generation (`take_reject` at `crates/eggtunnel/src/client/service_state.rs:284-300`), mapping codes through `registration_error_code` (`crates/eggtunnel/src/client.rs:1163-1169`). Unknown/stale rejects and unnegotiated receipt fail closed (`crates/eggtunnel/src/client.rs:955-983`). Codec round-trip + hostile-diagnostic tests at `crates/eggtunnel-proto/src/lib.rs:803-829`. |

Lifecycle summary (control path per [Architecture Overview](overview.md) §8):
`ClientHello → ServerHello → Auth → AuthOk → (RegisterService → RegisterAck | Error | RegisterReject)* →
(Open → DataHello → opaque relay | OpenReject)*`, with `Ping/Pong`, `Drain`, `Error`,
`UnregisterService` interleaved. `DataHello` is the only message that appears on data
connections; the other 14 are control-plane.

---

## 4. Bounded types

Limits (`crates/eggtunnel-proto/src/lib.rs:15-20`):

| Constant (code) | Value | Applies to |
|---|---|---|
| `MAX_FRAME_BYTES` (`crates/eggtunnel-proto/src/lib.rs:15`) | `1024 * 1024` (1 MiB) | whole postcard payload per frame; checked on encode (`crates/eggtunnel-proto/src/lib.rs:567-569`) and on decode before slice/deserialize (`crates/eggtunnel-proto/src/lib.rs:596-602`) and again in `wire_io` before allocation (`crates/eggtunnel/src/wire_io.rs:34-37`) |
| `MAX_AUTH_TOKEN_BYTES` (`crates/eggtunnel-proto/src/lib.rs:20`) | 4096 | `Auth.token` bytes |
| `MAX_NAME_BYTES` (`crates/eggtunnel-proto/src/lib.rs:16`) | 128 | `ServiceName` byte length |
| `MAX_DIAGNOSTIC_BYTES` (`crates/eggtunnel-proto/src/lib.rs:17`) | 256 | `BoundedDiagnostic` byte length |
| `MAX_CAPABILITIES` (`crates/eggtunnel-proto/src/lib.rs:18`) | 32 | `Capabilities` entry count |
| `MAX_TARGET_HOST_BYTES` (`crates/eggtunnel-proto/src/lib.rs:19`) | 253 | `TcpTarget.host` byte length (DNS-name max) |

Validation rules + redaction:

| Type (code) | Validation | Redaction / display |
|---|---|---|
| `ServiceName` (`crates/eggtunnel-proto/src/lib.rs:95-135`) | `new` rejects empty, `len() > MAX_NAME_BYTES`, or any byte outside ASCII alphanumeric + `-_.` (`crates/eggtunnel-proto/src/lib.rs:100-109` → `InvalidName`); `#[serde(try_from = "String")]` (`crates/eggtunnel-proto/src/lib.rs:96`) + `TryFrom<String>` (`crates/eggtunnel-proto/src/lib.rs:118-123`) force revalidation on deserialize, so hostile postcard strings cannot bypass `new` | `Debug`/`Display` print the name in clear (`crates/eggtunnel-proto/src/lib.rs:125-135`) — names are non-secret routing labels |
| `TcpTarget` (`crates/eggtunnel-proto/src/lib.rs:149-184`) | `new` delegates the host to the shared `validate_target_host` (`crates/eggtunnel-proto/src/lib.rs:198-211`: empty, `len() > MAX_TARGET_HOST_BYTES`, whitespace, `char::is_control`, Unicode format chars, or any of `/?#@[]\"'<>` — the shapes ambiguous in a URL authority) and rejects `port == 0` (`crates/eggtunnel-proto/src/lib.rs:173-175` → `InvalidTarget`). `Endpoint::parse` in the parent crate calls the same validator, so one host shape governs both the wire target and the server endpoint; a colon is accepted **only** when the whole host parses as an IPv6 literal (`crates/eggtunnel-proto/src/lib.rs:207-209`), so `":“`, `":::"`, and `"foo:bar"` fail closed; `#[serde(try_from = "WireTcpTarget")]` (`crates/eggtunnel-proto/src/lib.rs:150`) + `TryFrom<WireTcpTarget>` (`crates/eggtunnel-proto/src/lib.rs:162-167`) revalidate on decode; private `host` field with `host()`/`port()` accessors (`crates/eggtunnel-proto/src/lib.rs:178-183`) | derived `Debug` prints host/port in clear — treated as config metadata, not a secret |
| `Capabilities` (`crates/eggtunnel-proto/src/lib.rs:226-279`) | `new` rejects `ids.len() > MAX_CAPABILITIES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:232-234`) and then sorts + dedups, so `new(vec![2,1]) == new(vec![1,2])` (`crates/eggtunnel-proto/src/lib.rs:238-241`); `#[serde(try_from = "Vec<u16>")]` (`crates/eggtunnel-proto/src/lib.rs:227`) + `TryFrom<Vec<u16>>` (`crates/eggtunnel-proto/src/lib.rs:274-279`) revalidate on decode; `Default` is empty (`crates/eggtunnel-proto/src/lib.rs:226`) | plain `Debug`; exchanged as the negotiated intersection (see §5) |
| `Auth` (`crates/eggtunnel-proto/src/lib.rs:368-410`) | `new` rejects `token.len() > MAX_AUTH_TOKEN_BYTES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:376-378`); wire decode uses `#[serde(deserialize_with = "bounded_bytes")]` (`crates/eggtunnel-proto/src/lib.rs:370`) + `bounded_bytes` (`crates/eggtunnel-proto/src/lib.rs:394-403`) which rejects oversize tokens even if constructed by hand-rolled postcard bytes | custom `Debug` prints `Auth { token: "[REDACTED]" }` (`crates/eggtunnel-proto/src/lib.rs:404-410`); accessor is `token() -> &[u8]` (`crates/eggtunnel-proto/src/lib.rs:382-384`), field is private; `Drop` zeroizes the buffer via `zeroize` (`crates/eggtunnel-proto/src/lib.rs:387-392`) |
| `BoundedDiagnostic` (`crates/eggtunnel-proto/src/lib.rs:281-312`) | `new` rejects `len() > MAX_DIAGNOSTIC_BYTES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:288-290`) and any `char::is_control()` or Unicode format char (`crates/eggtunnel-proto/src/lib.rs:294-296`) so ANSI/control/newline/bidi injection into a log sink fails closed; `#[serde(try_from = "String")]` (`crates/eggtunnel-proto/src/lib.rs:282`) + `TryFrom<String>` (`crates/eggtunnel-proto/src/lib.rs:307-312`) revalidate on decode | plain `Debug`; length cap bounds log amplification, charset cap bounds log spoofing |
| `RequestedBind` (`crates/eggtunnel-proto/src/lib.rs:137-141`) | no validation in proto (`Loopback{port}` / `Ip{address:[u8;16],port}` are structurally deserialized); policy enforcement is in the server (`bind_to_socket`, `BindPolicy`) | derived `Debug` |
| `EffectiveBind` (`crates/eggtunnel-proto/src/lib.rs:143-147`) | no validation in proto; server-derived from `TcpListener::local_addr` | derived `Debug` |

ID semantics:

| ID (code) | Representation | Generation / equality | Debug |
|---|---|---|---|
| `SessionId` (`crates/eggtunnel-proto/src/lib.rs:47-64`) | `pub struct SessionId(pub [u8;16])`, `Copy`, `Eq`/`Hash`, `Serialize`/`Deserialize` | `generate()` via `getrandom::fill` (`crates/eggtunnel-proto/src/lib.rs:50-56`); 128-bit random; `PartialEq` is derived (non-constant-time) | fully redacted: `SessionId([REDACTED])` (`crates/eggtunnel-proto/src/lib.rs:58-64`) — a Session ID is a capability in `DataHello`, so no prefix is recoverable from logs |
| `ConnectionId` (`crates/eggtunnel-proto/src/lib.rs:69-93`) | `pub struct ConnectionId(pub [u8;16])`, same derives | `generate()` via `getrandom` (`crates/eggtunnel-proto/src/lib.rs:72-77`); `constant_time_eq` folds `a ^ b` with `\|` (`crates/eggtunnel-proto/src/lib.rs:79-86`) “useful when IDs are treated as capabilities” | fully redacted: `ConnectionId([REDACTED])` (`crates/eggtunnel-proto/src/lib.rs:89-93`) |
| `ServiceId` (`crates/eggtunnel-proto/src/lib.rs:66-67`) | `pub struct ServiceId(pub u64)`, `Copy`, `Debug`, `Eq`/`Hash` | no generation in proto; client-chosen per service (initial `crates/eggtunnel/src/client.rs:767-773`, dynamic `crates/eggtunnel/src/client.rs:1021-1026` copy `service.id`); server treats duplicate IDs/names as errors (`crates/eggtunnel/src/server/control.rs:330-346`) | transparent `u64` — non-secret multiplexing key |

Measurement notes: all `len()` checks are **byte** lengths (`String::len` / `Vec::len`), not
grapheme/char counts; `ServiceName` charset is checked per **byte**
(`value.bytes().all(...)` at `crates/eggtunnel-proto/src/lib.rs:104-106`), which for UTF-8
multibyte input fails closed (non-ASCII bytes are rejected). The `TcpTarget` host check is per
`char` (`host.chars().any(...)` at `crates/eggtunnel-proto/src/lib.rs:201-203`), with the shared
format-char predicate at `crates/eggtunnel-proto/src/lib.rs:217-224`.

---

## 5. Versioning policy

| Item | Rule | Code / doc |
|---|---|---|
| Wire version | `1.1` (major-1 boundary preserves 1.0 interop) | `PROTOCOL_MAJOR = 1`, `PROTOCOL_MINOR = 1` (`crates/eggtunnel-proto/src/lib.rs:21-22`); `ProtocolVersion::CURRENT` (`crates/eggtunnel-proto/src/lib.rs:40-45`); `docs/PROTOCOL.md:1` |
| Crate version | workspace `0.2.0` line (`crates/eggtunnel-proto/Cargo.toml:4` inherits `version.workspace`; root `Cargo.toml:6` sets `version = "0.2.0"`) | `docs/PROTOCOL.md:3-6` states the split: current crate line `0.2.0`, wire `1.1` with 1.0 fallback |
| Major | reject on mismatch | `decode_frame` returns `UnsupportedVersion(major, minor)` if `major != PROTOCOL_MAJOR` (`crates/eggtunnel-proto/src/lib.rs:592-594`); tested with major 2 at `crates/eggtunnel-proto/src/lib.rs:850-856` |
| Minor | informational only | minor is decoded but never compared; `encode_frame` always stamps the current minor (`crates/eggtunnel-proto/src/lib.rs:574`); a 1.0 frame fixture (minor 0) still decodes, pinned at `crates/eggtunnel-proto/src/lib.rs:815-820`; extensions are never inferred from minor — only from negotiated capabilities (`docs/PROTOCOL.md:56-74`). |
| Capabilities | negotiated intersection (ADR-0002) | `ClientHello`/`ServerHello` carry `Capabilities`; client advertises `Capabilities::supported()` (`crates/eggtunnel-proto/src/lib.rs:244-248`), server returns `supported().intersect(&hello.capabilities)` (`crates/eggtunnel/src/server/control.rs:110`), client intersects again (`has` at `crates/eggtunnel-proto/src/lib.rs:265-267`, `intersect` at `crates/eggtunnel-proto/src/lib.rs:250-263`, applied at `crates/eggtunnel/src/client.rs:1153-1155`); unknown IDs ignored, emission sorted/unique. Registry: 1 = correlated rejection, 2 = drain deadline (`crates/eggtunnel-proto/src/lib.rs:24-32`); pinned by `capability_registry_is_pinned_and_intersection_is_a_set` (`crates/eggtunnel-proto/src/lib.rs:758-801`). 1.0 (empty) peers negotiate nothing. |
| Message IDs | stable, explicit, pinned | IDs 1–15 listed at `docs/PROTOCOL.md:36-38`; discriminants are explicit, “not derived from enum order” (`docs/PROTOCOL.md:39`); test `documented_wire_version_and_message_ids_are_pinned` asserts every discriminant and round-trips `TryFrom` (`crates/eggtunnel-proto/src/lib.rs:721-756`), including rejection of `0` and `16` (`crates/eggtunnel-proto/src/lib.rs:754-755`) |

Practical consequence for reviewers: any change to a discriminant, to `HEADER_LEN`/field order,
or to a DTO’s postcard shape is a compatibility event and must update `docs/PROTOCOL.md`
alongside the constants (the test comment says exactly this at
`crates/eggtunnel-proto/src/lib.rs:723-726`).

---

## 6. Security properties

| Property | Mechanism | Code |
|---|---|---|
| Constant-time `ConnectionId` comparison | `constant_time_eq` accumulates `diff \| (a ^ b)` over all 16 bytes, single `== 0` at the end; no early exit | `crates/eggtunnel-proto/src/lib.rs:79-86` |
| Redacted `Debug` for secrets/capabilities | `Auth` prints `[REDACTED]` instead of token bytes; `ConnectionId` prints `[REDACTED]`; `SessionId` prints `[REDACTED]` with **no** recoverable prefix | `crates/eggtunnel-proto/src/lib.rs:404-410`, `crates/eggtunnel-proto/src/lib.rs:89-93`, `crates/eggtunnel-proto/src/lib.rs:58-64` |
| Secret-buffer zeroization | `impl Drop for Auth` calls `zeroize::Zeroize` on the token buffer, so a dropped `Auth` does not leave the bearer token in freed memory | `crates/eggtunnel-proto/src/lib.rs:387-392`; `zeroize` dependency at `crates/eggtunnel-proto/Cargo.toml:20` |
| Hostile-input revalidation (deserialize ≠ constructor bypass) | `ServiceName`, `TcpTarget`, `Capabilities`, `BoundedDiagnostic` all use `serde(try_from = …)` so `postcard::from_bytes` re-runs the validating constructor; `Auth` uses a custom `bounded_bytes` deserializer | `crates/eggtunnel-proto/src/lib.rs:96`, `crates/eggtunnel-proto/src/lib.rs:150`, `crates/eggtunnel-proto/src/lib.rs:227`, `crates/eggtunnel-proto/src/lib.rs:282`, `crates/eggtunnel-proto/src/lib.rs:370`, `crates/eggtunnel-proto/src/lib.rs:394-403`; negative tests at `crates/eggtunnel-proto/src/lib.rs:888-901` |
| Pre-copy / pre-deserialize length checks | `decode_frame` rejects `len > MAX_FRAME_BYTES` before slicing the payload (`FrameTooLarge`); `wire_io::read_message` rejects before buffering the payload; `encode_frame` rejects oversize payloads before framing | `crates/eggtunnel-proto/src/lib.rs:596-602`, `crates/eggtunnel/src/wire_io.rs:34-37`, `crates/eggtunnel-proto/src/lib.rs:567-569`; tests at `crates/eggtunnel-proto/src/lib.rs:865-866`, `crates/eggtunnel-proto/src/lib.rs:869-886` |
| Strict payload consumption | `postcard::take_from_bytes` + `trailing.is_empty()` check rejects smuggled trailing bytes inside the declared length | `crates/eggtunnel-proto/src/lib.rs:606-616`; negative test crafts a 1-byte trailer with adjusted length at `crates/eggtunnel-proto/src/lib.rs:842-846` |
| Log-spoofing charset gate | `BoundedDiagnostic` and the shared host validator reject `Cc`/`Cf`/`Zl`/`Zp` characters, not just over-length text | `crates/eggtunnel-proto/src/lib.rs:294-296`, `crates/eggtunnel-proto/src/lib.rs:201-203`, `crates/eggtunnel-proto/src/lib.rs:217-224` |
| `forbid(unsafe_code)` | whole crate refuses `unsafe` | `crates/eggtunnel-proto/src/lib.rs:1` |
| Random IDs via OS RNG | `SessionId::generate` / `ConnectionId::generate` use `getrandom::fill`, propagate `getrandom::Error` | `crates/eggtunnel-proto/src/lib.rs:50-56`, `crates/eggtunnel-proto/src/lib.rs:72-77` |
| Secret hygiene boundary | proto redacts and zeroizes its own copies; the caller-owned `SecretToken` in `common.rs` is the outer boundary (see [Architecture Overview](overview.md) §2) | proto `Auth::token()` returns `&[u8]` (`crates/eggtunnel-proto/src/lib.rs:382-384`); reviewers must confirm callers drop/clone minimally |

Caveats a reviewer should carry into `client.rs` / `server.rs`:

- `SessionId` uses derived `PartialEq`, not constant-time, while `ConnectionId` offers
  `constant_time_eq` — check each comparison site uses the intended one.
- `SessionId` and `ConnectionId` are both fully redacted, so correlating logs across a Session
  needs an explicit non-secret correlation field rather than a `Debug` prefix.
- `ServiceName`, `TcpTarget`, diagnostics, and `ServiceId` are non-redacted by design.
- Auth secrecy depends on the transport: `docs/PROTOCOL.md:43-44` requires secure transport
  before `Auth`; enforcement is outside this crate.

---

## 7. Test inventory in `lib.rs` (`crates/eggtunnel-proto/src/lib.rs:637-991`)

| Test (anchor) | What it does | What it guards |
|---|---|---|
| `every_message_round_trips_and_concatenation_is_exact` (`crates/eggtunnel-proto/src/lib.rs:701-719`) | builds one sample of all 15 variants (`crates/eggtunnel-proto/src/lib.rs:641-699`: `web-main`, `ConnectionId([7;16])`, `SessionId([1;16])`, `ServiceId(1)`, …), asserts `decode(encode(m)) == m` and `used == encoded.len()`, then concatenates all frames and decodes in a loop asserting the count equals the sample length | DTO/postcard symmetry for every type; exactly-one-frame + concatenated-stream contract; `kind()` ↔ `MessageType` ↔ decode-match alignment (`crates/eggtunnel-proto/src/lib.rs:495-514` vs `crates/eggtunnel-proto/src/lib.rs:617-633`) |
| `documented_wire_version_and_message_ids_are_pinned` (`crates/eggtunnel-proto/src/lib.rs:721-756`) | asserts `PROTOCOL_MAJOR == 1`, `PROTOCOL_MINOR == 1`, `CURRENT == {major: 1, minor: 1}`, each `MessageType as u16` equals its documented ID and `TryFrom` round-trips, and `0`/`16` are rejected | wire-compat tripwire: any discriminant/version drift fails loudly; comment (`crates/eggtunnel-proto/src/lib.rs:723-726`) requires updating `docs/PROTOCOL.md` with the constants |
| `capability_registry_is_pinned_and_intersection_is_a_set` (`crates/eggtunnel-proto/src/lib.rs:758-801`) | pins `CAPABILITY_REGISTER_REJECT == 1` / `CAPABILITY_DRAIN_DEADLINE == 2` and `supported() == [1, 2]`; intersects a client set carrying unknown ID 9 against a server set with a duplicate, asserting only capability 1 survives; asserts empty `Default` (a 1.0 peer) intersects to nothing in either direction; asserts emission order is deterministic | capability IDs are never silently reassigned (ADR-0002); intersection is a set, not a multiset; unknown IDs never negotiate |
| `register_reject_round_trips_and_oversized_diagnostics_fail_closed` (`crates/eggtunnel-proto/src/lib.rs:803-829`) | codec round-trip for `RegisterReject` (kind + `used == len`), a hand-patched minor-0 (1.0) `Ping` frame still decodes, and a hostile payload carrying a `MAX_DIAGNOSTIC_BYTES + 1` diagnostic is rejected both by `BoundedDiagnostic::new` and by `postcard::from_bytes::<RegisterReject>` | extension message 15 stays decodable on the wire; the 1.0 baseline is preserved; `serde(try_from)` revalidation blocks a smuggled over-length diagnostic |
| `rejects_bad_headers_lengths_and_unknown_ids` (`crates/eggtunnel-proto/src/lib.rs:831-867`) | on a valid `Ping` frame: 3-byte prefix → `TruncatedFrame`; 1-byte-short frame → `TruncatedFrame`; appended trailer byte with bumped length → `InvalidPayload`; zeroed magic → `InvalidMagic`; major 2 → `UnsupportedVersion(2, PROTOCOL_MINOR)`; ID `0xffff` → `UnknownMessage(65535)`; length `MAX+1` → `FrameTooLarge` | each header-check branch (`crates/eggtunnel-proto/src/lib.rs:584-596`); trailing-byte strictness (`crates/eggtunnel-proto/src/lib.rs:611-613`); unknown-ID path (`crates/eggtunnel-proto/src/lib.rs:334-356`) |
| `maximum_frame_length_is_checked_before_payload_decode` (`crates/eggtunnel-proto/src/lib.rs:869-886`) | hand-builds `Ping`-tagged headers with lengths `MAX-1`, `MAX` (garbage payload → `InvalidPayload`, proving the length gate passed and decode was attempted) and a header-only frame with `MAX+1` → `FrameTooLarge` (proving rejection precedes payload read/deserialize) | pre-copy / pre-deserialize length gate (`crates/eggtunnel-proto/src/lib.rs:596-602`); distinguishes “length OK but payload bad” from “length itself rejected”; note the `MAX`-length cases allocate ~1 MiB each — acceptable in unit tests but not a pattern to copy into hot paths |
| `hostile_wire_strings_vectors_and_tokens_are_revalidated` (`crates/eggtunnel-proto/src/lib.rs:888-901`) | postcard-encodes over-limit name (129 B), diagnostic (257 B), `MAX_CAPABILITIES+1` caps, `MAX_AUTH+1` token and asserts direct `from_bytes` / `new` fail | `serde(try_from)` + `bounded_bytes` revalidation cannot be bypassed by crafting wire bytes (`crates/eggtunnel-proto/src/lib.rs:96`, `crates/eggtunnel-proto/src/lib.rs:150`, `crates/eggtunnel-proto/src/lib.rs:227`, `crates/eggtunnel-proto/src/lib.rs:282`, `crates/eggtunnel-proto/src/lib.rs:394-403`) |
| `target_host_validation_is_shared_with_endpoint_parsing` (`crates/eggtunnel-proto/src/lib.rs:903-943`) | accepts `localhost`, `example.internal`, `127.0.0.1`, `::1`, `a-b_c.d`; rejects empty, whitespace (incl. NBSP), `/ ? # @ [ ] \ " ' <`, and `Cc` control; checks the 253/254-byte boundary; confirms a hostile host and `port == 0` in a wire payload both fail revalidation | one shared `validate_target_host` (`crates/eggtunnel-proto/src/lib.rs:198-211`) governs both the wire target and the client endpoint; the IPv6-literal-only colon rule (`crates/eggtunnel-proto/src/lib.rs:207-209`) is enforced on decode |
| `validates_bounded_types_and_redacts_secrets` (`crates/eggtunnel-proto/src/lib.rs:945-959`) | empty name rejected; 128 B name accepted; 129 B rejected; `TcpTarget("host", 0)` rejected; 257 B diagnostic rejected; `Debug(Auth("secret"))` and `Debug(ConnectionId)` do not contain secret bytes; `Debug(SessionId)` equals exactly `SessionId([REDACTED])` | boundary values (empty / max / max+1); port-0 rule (`crates/eggtunnel-proto/src/lib.rs:173`); redaction (`crates/eggtunnel-proto/src/lib.rs:404-410`, `crates/eggtunnel-proto/src/lib.rs:89-93`, `crates/eggtunnel-proto/src/lib.rs:58-64`) |
| `ids_have_separate_types_and_connection_comparison_is_correct` (`crates/eggtunnel-proto/src/lib.rs:961-971`) | two generated `SessionId`s differ; `constant_time_eq` is reflexive and rejects zeros; `size_of::<ConnectionId>() == 16`; `ServiceId(42)` type-checks | RNG uniqueness smoke test; constant-time comparator correctness; 16-byte wire size; nominal typing prevents accidental `SessionId`/`ConnectionId`/`ServiceId` mixing |
| `arbitrary_input_never_panics` (`crates/eggtunnel-proto/src/lib.rs:973-990`) | xorshift-64 PRNG (`0xD1CE_BA5E_F00D` seed), 10 000 samples of length `0..4097`, `let _ = decode_frame(&bytes)` ignoring the result | fuzz-style never-panics gate over header + small-payload inputs; guards indexing (`input[..4]`, `[input[4], input[5]]`, …), `u32→usize` conversion, `checked_add`, and postcard error paths. Limitation: lengths cap at 4097 so the `FrameTooLarge` branch via huge declared lengths is covered by the dedicated max-length tests above, not here; no coverage requirement on output correctness for random bytes |

---

## 8. Review checklist

### Compatibility risks

- [ ] **Discriminant / DTO drift.** IDs are explicit but postcard field order/types are implicit.
  Adding/removing/reordering a struct field, changing `RequestedBind`/`EffectiveBind` layout, or
  reusing an ID breaks peers with no runtime fallback. The pinned-ID test
  (`crates/eggtunnel-proto/src/lib.rs:721-756`) catches discriminant changes, **not** DTO-shape
  changes — require golden-vector / cross-version tests before any DTO edit.
- [x] **Minor-version blindness resolved by ADR-0002.** `decode_frame` still ignores minor for framing, but extensions are negotiated capabilities, never minor inference (`docs/PROTOCOL.md:56-74`).
- [ ] **Wire vs. crate version confusion.** Wire `1.1` ≠ crate `0.2.0`
  (root `Cargo.toml:6`; see `docs/PROTOCOL.md:3-6`). Do not gate wire behavior on
  `CARGO_PKG_VERSION`; gate only on negotiated capabilities (never minor alone).
- [x] **`Capabilities` negotiated (ADR-0002).** Non-empty sets intersect bilaterally; unknown IDs ignored; IDs 1–2 pinned with `RegisterReject` (ID 15) as the only extension-only message.
- [ ] **Untyped codes.** `OpenReject.code: u16` (`crates/eggtunnel-proto/src/lib.rs:439`) and
  `ErrorMessage.code: u16` (`crates/eggtunnel-proto/src/lib.rs:455`) have no enum; client/server
  assign meaning ad hoc (codes 1/2 at `crates/eggtunnel/src/client.rs:878`,
  `crates/eggtunnel/src/client.rs:886` plus `crates/eggtunnel/src/client/open.rs:87-90`; the
  registration vocabulary 1/2/3/5 is centralized at `crates/eggtunnel/src/server/control.rs:54-57`
  and applied at call sites `crates/eggtunnel/src/server/control.rs:321`, `:339`, `:360`,
  `:381`, `:397`). Document new codes centrally or risk silent
  misinterpretation across versions.

### Bound-bypass risks

- [ ] **Construction vs. deserialization.** Every bounded string/vector/token type must keep its
  `serde(try_from)` / `deserialize_with` attribute in sync with `new()`. Removing
  `#[serde(try_from = "String")]` from `ServiceName` (`crates/eggtunnel-proto/src/lib.rs:96`),
  `TcpTarget` (`crates/eggtunnel-proto/src/lib.rs:150`), `Capabilities`
  (`crates/eggtunnel-proto/src/lib.rs:227`), `BoundedDiagnostic`
  (`crates/eggtunnel-proto/src/lib.rs:282`), or `bounded_bytes` from `Auth`
  (`crates/eggtunnel-proto/src/lib.rs:370`) would open a bypass the unit tests at
  `crates/eggtunnel-proto/src/lib.rs:888-901` are designed to catch — run them after any serde
  refactor.
- [ ] **Byte vs. char.** `ServiceName`/`BoundedDiagnostic`/host limits use byte `len()`.
  The `TcpTarget` host check is `chars().any(...)` over whitespace, control, Unicode format
  characters, and URL-ambiguous characters (`crates/eggtunnel-proto/src/lib.rs:201-203`,
  predicate at `crates/eggtunnel-proto/src/lib.rs:217-224`). `BoundedDiagnostic` bounds charset
  the same way (`crates/eggtunnel-proto/src/lib.rs:294-296`), so ANSI/control/bidi text in
  diagnostics is rejected rather than logged; the `as_str()` doc comment
  (`crates/eggtunnel-proto/src/lib.rs:299-301`) still asks sinks not to log it verbatim without
  sanitization.
- [ ] **`RequestedBind` / `EffectiveBind` have no proto-level validation**
  (`crates/eggtunnel-proto/src/lib.rs:137-147`). `InvalidBind`
  (`crates/eggtunnel-proto/src/lib.rs:557-558`) is defined but never constructed in the
  workspace — dead variant today. Policy lives server-side (`bind_to_socket`, `BindPolicy`).
  Either wire the variant or remove it; a reviewer should not assume the proto rejects bad binds.
- [ ] **Double length gate.** Both `decode_frame`
  (`crates/eggtunnel-proto/src/lib.rs:596-602`) and `wire_io::read_message`
  (`crates/eggtunnel/src/wire_io.rs:34-37`) enforce `MAX_FRAME_BYTES`. Keep both: the first
  protects pure-decode callers, the second protects the allocating network path. Removing either
  re-opens allocation-before-check for that caller.
- [ ] **`u32 → usize` and `checked_add`.** Length is `u32 BE`
  (`crates/eggtunnel-proto/src/lib.rs:596`); `checked_add` guards theoretical 32-bit overflow
  (`crates/eggtunnel-proto/src/lib.rs:600-602`). On 16-bit targets `u32 as usize` truncation
  would be a concern — out of scope for the declared `rust-version = 1.89` tier but worth a
  comment if portability is ever claimed.

### Postcard trailing-bytes strictness

- [ ] **Strictness is load-bearing.** `dec!` rejects any trailing bytes inside the declared
  payload (`crates/eggtunnel-proto/src/lib.rs:606-616`). Without the
  `trailing.is_empty()` check, a sender could smuggle a second logical message inside one
  frame’s length prefix, breaking the exactly-one-frame invariant and confusing
  concatenation loops. The trailer test (`crates/eggtunnel-proto/src/lib.rs:842-846`) must keep
  failing if strictness regresses.
- [ ] **`wire_io` exact-consumption check is the second half.**
  `consumed != frame.len()` → `InvalidPayload` (`crates/eggtunnel/src/wire_io.rs:53-56`) defends
  the `read_exact`-assembled path even though `decode_frame` already enforces intra-payload
  strictness. Both layers should stay strict.
- [ ] **Postcard upgrade risk.** `postcard` is `version = "1"` with `alloc`
  (root `Cargo.toml:16`). A major postcard encoding change would silently break interop
  despite identical Rust types — pin/audit postcard upgrades as wire-compat events, and prefer
  golden byte-vectors over round-trip-only tests for long-term stability.

### ID-confusion risks

- [ ] **Three ID types, three secrecy levels.** `ServiceId(u64)` is transparent and
  client-chosen; `SessionId` and `ConnectionId` are both fully redacted in `Debug`, with
  `ConnectionId` additionally offering constant-time eq. Review every log line (add an
  explicit non-secret correlation field — neither ID is recoverable from `Debug`) and every
  `==` vs `constant_time_eq` call site: using derived `==` on `ConnectionId` (available via
  `PartialEq`) instead of `constant_time_eq`
  (`crates/eggtunnel-proto/src/lib.rs:79-86`) loses the timing property the
  doc comment promises.
- [ ] **`SessionId` copy-paste across planes.** The same `SessionId` appears in `AuthOk`
  (`crates/eggtunnel-proto/src/lib.rs:411-414`) and `DataHello`
  (`crates/eggtunnel-proto/src/lib.rs:469-474`). Server must verify all three `DataHello`
  components jointly (session + service + connection); partial matching enables cross-service
  or replay confusion (joint verification at `crates/eggtunnel/src/server/pending.rs:29-100`). Server-side tests already cover wrong-session/replay/stale `DataHello`
  (e.g. `crates/eggtunnel/src/server_tests/tcp.rs:1248`, `crates/eggtunnel/src/server_tests/tcp.rs:1309`,
  `crates/eggtunnel/src/server_tests/quic.rs:361`, `crates/eggtunnel/src/server_tests/quic.rs:480`,
  `crates/eggtunnel/src/server_tests/quic.rs:604`) —
  keep them green when touching correlation logic.
- [ ] **`ServiceId` collisions.** Proto does not allocate or deduplicate `ServiceId`; server
  rejects duplicates per session (`crates/eggtunnel/src/server/control.rs:330-346`). Client-side ID
  reuse across reconnects/generations is a caller bug the proto cannot catch — check embedders
  (`fixtures/embedder`, `examples/`) generate fresh IDs per registration.
- [ ] **`UnexpectedMessage` is a protocol-state signal, not a decode error.**
  Defined at `crates/eggtunnel-proto/src/lib.rs:561-562`, raised only by client/server state
  machines (`crates/eggtunnel/src/client.rs:732`, `crates/eggtunnel/src/client.rs:918`,
  `crates/eggtunnel/src/client.rs:961`, `crates/eggtunnel/src/client.rs:982`,
  `crates/eggtunnel/src/client.rs:993`, `crates/eggtunnel/src/server/accept.rs:310`,
  `crates/eggtunnel/src/server/accept.rs:386`, `crates/eggtunnel/src/server/accept.rs:425`,
  `crates/eggtunnel/src/server/accept.rs:512`, `crates/eggtunnel/src/server/control.rs:244`). Fuzzing `decode_frame` alone
  (`crates/eggtunnel-proto/src/lib.rs:973-990`) never exercises it — state-machine
  testing belongs in `client.rs`/`server.rs`.

---

*See also: [Architecture Overview](overview.md) §§1–2 and §8 for session-lifecycle context;
`docs/PROTOCOL.md` for the normative wire statement; `crates/eggtunnel/src/wire_io.rs:1-77`
for the header-first network adapter.*

### Session-time registration and heartbeat (M009)

The established Session accepts RegisterService and UnregisterService
repeatedly. RegisterAck correlates by ServiceId; the generic Error carries only a code,
so without capability 1 the client permits only one dynamic registration request in flight and
correlates that response to the sole pending registration; with capability 1 the correlated
`RegisterReject` (message 15) carries the ServiceId so several bounded transactions are in
flight at once (`crates/eggtunnel/src/client.rs:736-737`, `crates/eggtunnel/src/client/service_state.rs:284-300`).
Ping and Pong may
repeat during the Session and correlate by nonce. M009 does not change these
wire messages or add a message type.
