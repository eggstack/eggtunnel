# Proto wire protocol — deep dive

Back to [Architecture Overview](overview.md) §1.

Sources: `crates/eggtunnel-proto/src/lib.rs` (754 lines), `crates/eggtunnel-proto/Cargo.toml`,
`docs/PROTOCOL.md`, `crates/eggtunnel/src/wire_io.rs` (58 lines).
Cross-references below use `file:line` anchors. All claims were read from code; no invented behavior.

---

## 1. Purpose, scope, non-goals

**Purpose.** `eggtunnel-proto` is the runtime-neutral, bounded native Eggtunnel control-plane
wire protocol. It owns:

- bounded wire DTOs and their validation (`crates/eggtunnel-proto/src/lib.rs:13-22`, `crates/eggtunnel-proto/src/lib.rs:37-227`),
- the 14-byte framing (`crates/eggtunnel-proto/src/lib.rs:2-7`, `docs/PROTOCOL.md:12-21`),
- `encode_frame` / `decode_frame` (`crates/eggtunnel-proto/src/lib.rs:457-525`),
- the 14 stable message IDs (`crates/eggtunnel-proto/src/lib.rs:229-246`, `docs/PROTOCOL.md:31-34`).

**Scope.**

- Framing: `ETUN` magic + major/minor `u16 BE` + message-ID `u16 BE` + payload-len `u32 BE` +
  exactly one `postcard` payload (`crates/eggtunnel-proto/src/lib.rs:457-470`, `crates/eggtunnel-proto/src/lib.rs:474-525`).
- Validation: length/charset pre-checks before copy or deserialize
  (`crates/eggtunnel-proto/src/lib.rs:91-103`, `crates/eggtunnel-proto/src/lib.rs:161-179`,
  `crates/eggtunnel-proto/src/lib.rs:185-191`, `crates/eggtunnel-proto/src/lib.rs:209-215`,
  `crates/eggtunnel-proto/src/lib.rs:287-309`, `crates/eggtunnel-proto/src/lib.rs:488-496`).
- Version gate: wire `1.0`, major mismatch rejected, minor informational
  (`crates/eggtunnel-proto/src/lib.rs:21-35`, `crates/eggtunnel-proto/src/lib.rs:481-485`,
  `docs/PROTOCOL.md:1-10`).
- Typed errors for every hostile-input class (`crates/eggtunnel-proto/src/lib.rs:433-455`).

**Non-goals (explicitly out of this crate).**

| Non-goal | Where it lives instead | Evidence |
|---|---|---|
| No sockets / async / timers / tasks | `crates/eggtunnel/src/wire_io.rs`, `client.rs`, `server.rs` via Eggress transports | `crates/eggtunnel-proto/Cargo.toml:15-19` depends only on `serde`, `postcard` (`alloc`), `thiserror`, `getrandom`; `lib.rs:1` is `#![forbid(unsafe_code)]` and uses `core::fmt` (`crates/eggtunnel-proto/src/lib.rs:9`) |
| No TLS / QUIC / WebSocket / proxy | `crates/eggtunnel/src/wire_io.rs:7-58`, feature gates in `crates/eggtunnel/Cargo.toml` | proto never imports `tokio`, `rustls`, `eggress-*` |
| No session state machine | `crates/eggtunnel/src/client.rs:968-1012`, `crates/eggtunnel/src/server.rs:981-1041` enforce ordering with `UnexpectedMessage` | `decode_frame` never returns `UnexpectedMessage` (`crates/eggtunnel-proto/src/lib.rs:474-525`); the variant is only consumed by client/server (`crates/eggtunnel/src/client.rs:901`, `crates/eggtunnel/src/client.rs:1010`, `crates/eggtunnel/src/server.rs:598`, `crates/eggtunnel/src/server.rs:1039`) |
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
| 6 | 2 | minor version | `u16 BE` (`0`) | `PROTOCOL_MINOR` (`crates/eggtunnel-proto/src/lib.rs:22`) |
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
4. Always stamps the current version (`1.0`); there is no API to encode an older/newer
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

### 2.4 `wire_io.rs` transport adapter (`crates/eggtunnel/src/wire_io.rs:7-58`)

`eggtunnel-proto` itself does no I/O. The thin async adapter in the parent crate preserves
the same safety order:

| Function | Behavior |
|---|---|
| `read_message` (`crates/eggtunnel/src/wire_io.rs:7-36`) | `read_exact` 14-byte header; `decode_frame(&header)` must yield `TruncatedFrame` (any other `Err` is returned immediately, `Ok` is `unreachable!` — `crates/eggtunnel/src/wire_io.rs:15-19`); parse `len` from `header[10..14]` and reject `len > MAX_FRAME_BYTES` **before** allocating (`crates/eggtunnel/src/wire_io.rs:20-23`); `resize(HEADER_LEN + len)` + `read_exact` payload (`crates/eggtunnel/src/wire_io.rs:24-30`, short read → `TruncatedFrame`); final `decode_frame(&frame)` + exact-consumption check `consumed != frame.len()` → `InvalidPayload` (`crates/eggtunnel/src/wire_io.rs:31-34`) |
| `write_message` (`crates/eggtunnel/src/wire_io.rs:38-47`) | `encode_frame` then `write_all`; I/O failure mapped to `TruncatedFrame` (`crates/eggtunnel/src/wire_io.rs:46`) |
| `read_boxed` / `write_boxed` (`crates/eggtunnel/src/wire_io.rs:49-58`) | same logic over Eggress `BoxStream` (delegates to `read_message` / `write_message`) |

Review note: `write_message` mapping a failed `write_all` to `TruncatedFrame` is a deliberate
narrowing to `ProtocolError` (no `std::io::Error` in the signature); reviewers should check
callers do not misinterpret a write failure as “peer needs more bytes.”

---

## 3. Every message type 1–14

IDs are explicit `#[repr(u16)]` discriminants (`crates/eggtunnel-proto/src/lib.rs:229-246`),
parsed by total `TryFrom<u16>` (`crates/eggtunnel-proto/src/lib.rs:248-269`), pinned by test
(`crates/eggtunnel-proto/src/lib.rs:607-639`), and documented at `docs/PROTOCOL.md:31-34`.
“Direction” below is the conventional control/data-plane direction as used by
`crates/eggtunnel/src/client.rs` and `crates/eggtunnel/src/server.rs`; the proto crate itself
does **not** enforce direction or ordering.

| ID | Variant (code) | Key fields | Direction | Place in session lifecycle |
|---:|---|---|---|---|
| 1 | `ClientHello` (`crates/eggtunnel-proto/src/lib.rs:232`, `crates/eggtunnel-proto/src/lib.rs:271-275`) | `version: ProtocolVersion`, `capabilities: Capabilities` | client → server | Handshake opener. Client sends first (`crates/eggtunnel/src/client.rs:888-890`); server requires it as the first frame (`crates/eggtunnel/src/server.rs:596`). |
| 2 | `ServerHello` (`crates/eggtunnel-proto/src/lib.rs:233`, `crates/eggtunnel-proto/src/lib.rs:276-280`) | `version`, `capabilities` (same shape as `ClientHello`) | server → client | Handshake answer (`crates/eggtunnel/src/server.rs:904` sends; `crates/eggtunnel/src/client.rs:897` validates version). |
| 3 | `Auth` (`crates/eggtunnel-proto/src/lib.rs:234`, `crates/eggtunnel-proto/src/lib.rs:281-285`) | `token: Vec<u8>` (private, `bounded_bytes`, ≤4096 B; redacted `Debug`) | client → server | Credential presentation inside the already-established secure transport (`docs/PROTOCOL.md:36-37`; sent at `crates/eggtunnel/src/client.rs:906`). |
| 4 | `AuthOk` (`crates/eggtunnel-proto/src/lib.rs:235`, `crates/eggtunnel-proto/src/lib.rs:317-320`) | `session_id: SessionId` | server → client | Authentication success + session binding (`crates/eggtunnel/src/server.rs:968` sends; `crates/eggtunnel/src/client.rs:907-910` extracts `session_id`, else `TunnelError::Authentication`). |
| 5 | `RegisterService` (`crates/eggtunnel-proto/src/lib.rs:236`, `crates/eggtunnel-proto/src/lib.rs:321-327`) | `service_id: ServiceId`, `name: ServiceName`, `requested_bind: RequestedBind`, `target: TcpTarget` | client → server | Registration request, one per service (`crates/eggtunnel/src/client.rs:912-919` loop; `crates/eggtunnel/src/server.rs:983-1015` validates, binds, spawns `run_service`). Note: `target` is client-owned metadata; server never dials it (comment at `crates/eggtunnel/src/server.rs:996`). |
| 6 | `RegisterAck` (`crates/eggtunnel-proto/src/lib.rs:237`, `crates/eggtunnel-proto/src/lib.rs:328-332`) | `service_id`, `effective_bind: EffectiveBind { address: [u8;16], port: u16 }` | server → client | Registration success with server-chosen bind (`crates/eggtunnel/src/server.rs:1015` sends; `crates/eggtunnel/src/client.rs:921-927` requires `ack.service_id == service.id`). |
| 7 | `UnregisterService` (`crates/eggtunnel-proto/src/lib.rs:238`, `crates/eggtunnel-proto/src/lib.rs:333-336`) | `service_id` | client → server | Deregistration (`crates/eggtunnel/src/client.rs:1017-1023` sends on `ClientCommand::Unregister`; `crates/eggtunnel/src/server.rs:1017-1026` cancels service, removes pending). Unknown IDs are silently tolerated server-side (remove-if-present). |
| 8 | `Open` (`crates/eggtunnel-proto/src/lib.rs:239`, `crates/eggtunnel-proto/src/lib.rs:337-341`) | `service_id`, `connection_id: ConnectionId` | server → client (control plane) | Per-external-connection demand: server listener accepted, server queues pending and sends `Open` (`crates/eggtunnel/src/server.rs:1043-1045` via `open_rx`); client receives at `crates/eggtunnel/src/client.rs:970`, resolves `service_id`, else `OpenReject(code 1)`; semaphore-full → `OpenReject(code 2)` (`crates/eggtunnel/src/client.rs:971-983`). |
| 9 | `OpenReject` (`crates/eggtunnel-proto/src/lib.rs:240`, `crates/eggtunnel-proto/src/lib.rs:342-346`) | `connection_id`, `code: u16` | client → server | Negative answer to `Open` (`crates/eggtunnel/src/client.rs:974`, `crates/eggtunnel/src/client.rs:981`, `crates/eggtunnel/src/client.rs:1144` send; `crates/eggtunnel/src/server.rs:1027-1033` drops the pending entry). Codes are untyped `u16` (1 = unknown service, 2 = resource-exhausted in current client). |
| 10 | `Ping` (`crates/eggtunnel-proto/src/lib.rs:241`, `crates/eggtunnel-proto/src/lib.rs:347-350`) | `nonce: u64` | either direction (keepalive) | Client heartbeat every 20 s (`crates/eggtunnel/src/client.rs:951-967`); each side answers `Ping` with `Pong{nonce}` (`crates/eggtunnel/src/client.rs:1004`, `crates/eggtunnel/src/server.rs:1034-1036`). |
| 11 | `Pong` (`crates/eggtunnel-proto/src/lib.rs:242`, `crates/eggtunnel-proto/src/lib.rs:351-354`) | `nonce: u64` | either direction (answer) | Echoes the `Ping` nonce; client ignores bare `Pong` (`crates/eggtunnel/src/client.rs:1005`). |
| 12 | `Drain` (`crates/eggtunnel-proto/src/lib.rs:243`, `crates/eggtunnel-proto/src/lib.rs:355-358`) | `deadline_ms: u32` | either direction (graceful shutdown) | Client sends on cancellation (`crates/eggtunnel/src/client.rs:958-962`); server sends on shutdown paths (`crates/eggtunnel/src/server.rs:466`, `crates/eggtunnel/src/server.rs:569`); receipt breaks the control loop on both sides (`crates/eggtunnel/src/client.rs:1006-1009`, `crates/eggtunnel/src/server.rs:1038`, `crates/eggtunnel/src/server.rs:1044-1046`). |
| 13 | `Error` / `ErrorMessage` (`crates/eggtunnel-proto/src/lib.rs:244`, `crates/eggtunnel-proto/src/lib.rs:359-363`) | `code: u16`, `diagnostic: BoundedDiagnostic` (≤256 B) | server → client | Terminal/negative ack: auth failure (`crates/eggtunnel/src/server.rs:925`) and registration failures via `write_registration_error` (`crates/eggtunnel/src/server.rs:1122-1127`, call sites `crates/eggtunnel/src/server.rs:988`, `crates/eggtunnel/src/server.rs:993`, `crates/eggtunnel/src/server.rs:999`, `crates/eggtunnel/src/server.rs:1003`); client maps registration-time `Error` to `TunnelError::Authorization` (`crates/eggtunnel/src/client.rs:928`) and post-registration `Error` to `UnexpectedMessage` (`crates/eggtunnel/src/client.rs:1010`). Note the enum variant is `Message::Error` but the struct is `ErrorMessage` (`crates/eggtunnel-proto/src/lib.rs:385`, `crates/eggtunnel-proto/src/lib.rs:521`). |
| 14 | `DataHello` (`crates/eggtunnel-proto/src/lib.rs:245`, `crates/eggtunnel-proto/src/lib.rs:364-369`) | `session_id: SessionId`, `service_id: ServiceId`, `connection_id: ConnectionId` | client → server (data plane) | First frame on each **data** connection, then opaque relay bytes (`docs/PROTOCOL.md:36-39`; client sends at `crates/eggtunnel/src/client.rs:1128`; server requires it at `crates/eggtunnel/src/server.rs:677` / `crates/eggtunnel/src/server.rs:806-827`). Wrong-session / replay / stale `DataHello` is rejected and counted (`crates/eggtunnel/src/server.rs:3456-3474`, `crates/eggtunnel/src/server.rs:3981-4016`, etc.). |

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
| `TcpTarget` (`crates/eggtunnel-proto/src/lib.rs:141-179`) | `new` rejects empty host, `len() > MAX_TARGET_HOST_BYTES`, any `char::is_control` in host, or `port == 0` (`crates/eggtunnel-proto/src/lib.rs:164-168` → `InvalidTarget`); `#[serde(try_from = "WireTcpTarget")]` (`crates/eggtunnel-proto/src/lib.rs:142`) + `TryFrom<WireTcpTarget>` (`crates/eggtunnel-proto/src/lib.rs:154-159`) revalidate on decode; private `host` field with `host()`/`port()` accessors (`crates/eggtunnel-proto/src/lib.rs:173-178`) | derived `Debug` prints host/port in clear — treated as config metadata, not a secret |
| `Capabilities` (`crates/eggtunnel-proto/src/lib.rs:181-203`) | `new` rejects `ids.len() > MAX_CAPABILITIES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:186-189`); `#[serde(try_from = "Vec<u16>")]` (`crates/eggtunnel-proto/src/lib.rs:182`) + `TryFrom<Vec<u16>>` (`crates/eggtunnel-proto/src/lib.rs:198-203`) revalidate on decode; `Default` is empty (`crates/eggtunnel-proto/src/lib.rs:181`) | plain `Debug`; currently exchanged as empty set (see §5) |
| `Auth` (`crates/eggtunnel-proto/src/lib.rs:281-316`) | `new` rejects `token.len() > MAX_AUTH_TOKEN_BYTES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:288-292`); wire decode uses `#[serde(deserialize_with = "bounded_bytes")]` (`crates/eggtunnel-proto/src/lib.rs:283`) + `bounded_bytes` (`crates/eggtunnel-proto/src/lib.rs:300-309`) which rejects oversize tokens even if constructed by hand-rolled postcard bytes | custom `Debug` prints `Auth { token: "[REDACTED]" }` (`crates/eggtunnel-proto/src/lib.rs:310-316`); accessor is `token() -> &[u8]` (`crates/eggtunnel-proto/src/lib.rs:295-297`), field is private |
| `BoundedDiagnostic` (`crates/eggtunnel-proto/src/lib.rs:205-227`) | `new` rejects `len() > MAX_DIAGNOSTIC_BYTES` → `InvalidPayload` (`crates/eggtunnel-proto/src/lib.rs:211-214`); `#[serde(try_from = "String")]` (`crates/eggtunnel-proto/src/lib.rs:206`) + `TryFrom<String>` (`crates/eggtunnel-proto/src/lib.rs:222-227`) revalidate on decode | plain `Debug`; length cap bounds log amplification |
| `RequestedBind` (`crates/eggtunnel-proto/src/lib.rs:129-133`) | no validation in proto (`Loopback{port}` / `Ip{address:[u8;16],port}` are structurally deserialized); policy enforcement is in the server (`bind_to_socket`, `BindPolicy`) | derived `Debug` |
| `EffectiveBind` (`crates/eggtunnel-proto/src/lib.rs:135-139`) | no validation in proto; server-derived from `TcpListener::local_addr` | derived `Debug` |

ID semantics:

| ID (code) | Representation | Generation / equality | Debug |
|---|---|---|---|
| `SessionId` (`crates/eggtunnel-proto/src/lib.rs:37-56`) | `pub struct SessionId(pub [u8;16])`, `Copy`, `Eq`/`Hash`, `Serialize`/`Deserialize` | `generate()` via `getrandom::fill` (`crates/eggtunnel-proto/src/lib.rs:40-46`); 128-bit random; `PartialEq` is derived (non-constant-time) | truncated prefix only: first 4 bytes hex + `…` (`crates/eggtunnel-proto/src/lib.rs:48-56`) — aids log correlation without printing the full bearer |
| `ConnectionId` (`crates/eggtunnel-proto/src/lib.rs:61-85`) | `pub struct ConnectionId(pub [u8;16])`, same derives | `generate()` via `getrandom` (`crates/eggtunnel-proto/src/lib.rs:64-69`); `constant_time_eq` folds `a ^ b` with `\|` (`crates/eggtunnel-proto/src/lib.rs:71-78`) “useful when IDs are treated as capabilities” | fully redacted: `ConnectionId([REDACTED])` (`crates/eggtunnel-proto/src/lib.rs:81-85`) |
| `ServiceId` (`crates/eggtunnel-proto/src/lib.rs:58-59`) | `pub struct ServiceId(pub u64)`, `Copy`, `Debug`, `Eq`/`Hash` | no generation in proto; client-chosen per service (`crates/eggtunnel/src/client.rs:912-918` copies `service.id`); server treats duplicate IDs/names as errors (`crates/eggtunnel/src/server.rs:991-994`) | transparent `u64` — non-secret multiplexing key |

Measurement notes: all `len()` checks are **byte** lengths (`String::len` / `Vec::len`), not
grapheme/char counts; `ServiceName` charset is checked per **byte**
(`value.bytes().all(...)` at `crates/eggtunnel-proto/src/lib.rs:96-98`), which for UTF-8
multibyte input fails closed (non-ASCII bytes are rejected). `TcpTarget` control check is per
`char` (`host.chars().any(char::is_control)` at `crates/eggtunnel-proto/src/lib.rs:166`).

---

## 5. Versioning policy

| Item | Rule | Code / doc |
|---|---|---|
| Wire version | `1.0` | `PROTOCOL_MAJOR = 1`, `PROTOCOL_MINOR = 0` (`crates/eggtunnel-proto/src/lib.rs:21-22`); `ProtocolVersion::CURRENT` (`crates/eggtunnel-proto/src/lib.rs:30-35`); `docs/PROTOCOL.md:1` |
| Crate version | workspace `0.1.0` line (`crates/eggtunnel-proto/Cargo.toml:4` inherits `version.workspace`; root `Cargo.toml:6` sets `version = "0.1.0"`) | `docs/PROTOCOL.md:3-6`: “The wire version is distinct from the Rust crate version. The current crate release line is Eggtunnel `0.1.x` and uses wire version `1.0`. The crates are pre-1.0: Rust API compatibility is not promised across minor releases until a 1.0 library release.” |
| Major | reject on mismatch | `decode_frame` returns `UnsupportedVersion(major, minor)` if `major != PROTOCOL_MAJOR` (`crates/eggtunnel-proto/src/lib.rs:483-485`); tested with major 2 at `crates/eggtunnel-proto/src/lib.rs:660-666` |
| Minor | informational only | minor is decoded (`crates/eggtunnel-proto/src/lib.rs:482`) but never compared; `encode_frame` always stamps `1.0` (`crates/eggtunnel-proto/src/lib.rs:464-465`); `docs/PROTOCOL.md:6-10`: “Minor versions are currently informational; there is no backward-peer support window … Do not infer a long-term 1.x protocol guarantee.” |
| Capabilities | exchanged but not negotiated | `ClientHello`/`ServerHello` carry `Capabilities` (`crates/eggtunnel-proto/src/lib.rs:271-280`); doc says “no … capability negotiation beyond exchanging the current empty capability set” (`docs/PROTOCOL.md:7-9`); tests use `Capabilities::default()` (`crates/eggtunnel-proto/src/lib.rs:537`, `crates/eggtunnel-proto/src/lib.rs:541`) |
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
- [ ] **Minor-version blindness.** `decode_frame` ignores minor
  (`crates/eggtunnel-proto/src/lib.rs:481-485`). A future `1.1` peer’s new optional fields would
  decode as `InvalidPayload` today, not negotiate. Confirm product decision in
  `docs/PROTOCOL.md:6-10` still holds before relying on minor for features.
- [ ] **Wire vs. crate version confusion.** Wire `1.0` ≠ crate `0.1.x`
  (`docs/PROTOCOL.md:1-10`, root `Cargo.toml:6`). Do not gate wire behavior on
  `CARGO_PKG_VERSION`; gate only on `PROTOCOL_MAJOR` / `MessageType`.
- [ ] **`Capabilities` is currently a placeholder.** Default/empty is the only exercised value
  (`crates/eggtunnel-proto/src/lib.rs:537`, `crates/eggtunnel-proto/src/lib.rs:541`). Any
  non-empty semantics need a negotiation rule that does not exist yet.
- [ ] **Untyped codes.** `OpenReject.code: u16` (`crates/eggtunnel-proto/src/lib.rs:345`) and
  `ErrorMessage.code: u16` (`crates/eggtunnel-proto/src/lib.rs:361`) have no enum; client/server
  assign meaning ad hoc (e.g. codes 1/2 at `crates/eggtunnel/src/client.rs:974`,
  `crates/eggtunnel/src/client.rs:981`; codes 1/2/3/5 at `crates/eggtunnel/src/server.rs:988`,
  `crates/eggtunnel/src/server.rs:993`, `crates/eggtunnel/src/server.rs:999`,
  `crates/eggtunnel/src/server.rs:1003`). Document new codes centrally or risk silent
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
  `TcpTarget`’s control check is `chars().any(char::is_control)`
  (`crates/eggtunnel-proto/src/lib.rs:166`). Non-`control` Unicode (e.g. bidi overrides,
  zero-width) in hosts/diagnostics passes validation — confirm upper layers normalize or reject
  where display/lookup matters.
- [ ] **`RequestedBind` / `EffectiveBind` have no proto-level validation**
  (`crates/eggtunnel-proto/src/lib.rs:129-139`). `InvalidBind`
  (`crates/eggtunnel-proto/src/lib.rs:449-450`) is defined but never constructed in the
  workspace — dead variant today. Policy lives server-side (`bind_to_socket`, `BindPolicy`).
  Either wire the variant or remove it; a reviewer should not assume the proto rejects bad binds.
- [ ] **Double length gate.** Both `decode_frame`
  (`crates/eggtunnel-proto/src/lib.rs:488-493`) and `wire_io::read_message`
  (`crates/eggtunnel/src/wire_io.rs:20-23`) enforce `MAX_FRAME_BYTES`. Keep both: the first
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
  (`Cargo.toml:15` workspace). A major postcard encoding change would silently break interop
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
  or replay confusion. Server-side tests already cover wrong-session/replay/stale `DataHello`
  (e.g. `crates/eggtunnel/src/server.rs:3456-3474`, `crates/eggtunnel/src/server.rs:3981-4016`) —
  keep them green when touching correlation logic.
- [ ] **`ServiceId` collisions.** Proto does not allocate or deduplicate `ServiceId`; server
  rejects duplicates per session (`crates/eggtunnel/src/server.rs:991-994`). Client-side ID
  reuse across reconnects/generations is a caller bug the proto cannot catch — check embedders
  (`fixtures/embedder`, `examples/`) generate fresh IDs per registration.
- [ ] **`UnexpectedMessage` is a protocol-state signal, not a decode error.**
  Defined at `crates/eggtunnel-proto/src/lib.rs:453-454`, raised only by client/server state
  machines (`crates/eggtunnel/src/client.rs:901`, `crates/eggtunnel/src/client.rs:1010`,
  `crates/eggtunnel/src/server.rs:598`, `crates/eggtunnel/src/server.rs:679`,
  `crates/eggtunnel/src/server.rs:1039`). Fuzzing `decode_frame` alone
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
