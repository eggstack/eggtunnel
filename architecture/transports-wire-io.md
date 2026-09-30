# Transports + Wire I/O — Deep Dive

Back to [Architecture Overview](overview.md) §5. This file is the review-oriented
deep dive for `crates/eggtunnel/src/wire_io.rs` and the feature-gated transport
stack (TCP+TLS baseline, QUIC, WebSocket/WSS, outbound proxy) built on pinned
Eggress 1.0.8 primitives.

Primary sources (line anchors are load-bearing for review):

- `crates/eggtunnel/src/wire_io.rs` (full, 58 LOC)
- `crates/eggtunnel/Cargo.toml`, `Cargo.toml` (workspace)
- `crates/eggtunnel/src/client.rs`, `crates/eggtunnel/src/client/config.rs`,
  `crates/eggtunnel/src/client/open.rs`, `crates/eggtunnel/src/server.rs`
- `crates/eggtunnel/src/common.rs`, `crates/eggtunnel/src/lib.rs`,
  `crates/eggtunnel/src/pem.rs`
- `crates/eggtunnel-proto/src/lib.rs`
- `crates/eggtunnel-cli/src/main.rs`
- `docs/SUPPORT.md`, `docs/SECURITY.md`, `docs/ARCHITECTURE.md`
- `plans/adrs/ADR-0001-session-transport-and-egress-boundary.md`,
  `plans/subsystems/reverse-session-roadmap.md`

Related overview sections: [wire protocol](proto-wire-protocol.md) (framing),
[client](client.md), [server](server.md).

> Canonical composition is `ClientBuilder` / `ServerBuilder` +
> `Client/ServerTransportProfile` + `RuntimePolicy`
> (`client/config.rs:68-156`, `server/config.rs:51-130`). `Client::start*` /
> `Server::bind*` are conveniences that delegate to the builders.
> Finite ceilings/timeouts come from `RuntimePolicy`
> (`common.rs:194-316`); `MAX_SESSIONS`/`MAX_HANDSHAKES` in `server_tests.rs:35`
> are `#[cfg(test)]`-only. QUIC `max_concurrent_streams` is policy-derived
> (server: `active_connections_per_session + 1`, client:
> `client_open_tasks` verbatim on the client (128 by default), `idle_timeout` is
> `policy.timeouts.control_idle`, and both relays use
> `RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain)` at
> `client/open.rs:67` and `server/service.rs:121`. WSS sets 1 MiB caps via
> `WebSocketTunnel{Client,Server}::new(1024*1024)` plus matching
> `tokio-tungstenite` `WebSocketConfig` limits.

---

## 1. `wire_io.rs`: bounded framing over any `AsyncRead/AsyncWrite`

File: `crates/eggtunnel/src/wire_io.rs:1-58`.

`wire_io.rs` is deliberately thin: it adapts the runtime-neutral
`eggtunnel-proto` codec (`encode_frame` / `decode_frame`,
`crates/eggtunnel-proto/src/lib.rs:457-504`) to Tokio I/O and to Eggress's
boxed stream type. All Eggtunnel control-plane messages (`ClientHello` … `DataHello`)
flow through these four functions. Opaque post-`DataHello` relay bytes do **not**
flow through `wire_io`; they go to `eggress-relay::relay_with_options`
(`crates/eggtunnel/src/client/open.rs:67`,
`crates/eggtunnel/src/server/service.rs:121`).

### 1.1 `read_message` — header-first, length pre-check, exact consumption

`crates/eggtunnel/src/wire_io.rs:7-36`:

1. **Header-first read (14 bytes).**
   `reader.read_exact(&mut header)` (`wire_io.rs:11-14`). Short read / EOF /
    reset maps to `ProtocolError::TruncatedFrame` via
    `.map_err(|_| ProtocolError::TruncatedFrame)`. There is no distinction
    between "peer closed cleanly" and "peer sent a short header" at this layer;
    both become `TruncatedFrame`, which `common.rs:466-482` later maps to
    `TunnelError::Protocol` → `TerminationCategory::Protocol` on the control path
    (via `From<ProtocolError>`), or to `Transport`/`Timeout` wrappers where the
    call site adds `timeout(...).map_err(|_| TunnelError::Disconnected/Timeout)`.
2. **Header-only `decode_frame` probe.** `wire_io.rs:15-19` calls
   `decode_frame(&header)` expecting one of two outcomes:
   - `Err(TruncatedFrame)` → expected; a full frame cannot fit in 14 bytes, so
     continue to the payload read.
   - Any other `Err` (`InvalidMagic`, `UnsupportedVersion`, `UnknownMessage`,
     `FrameTooLarge` from the length word) → return immediately **before**
     allocating or reading payload. This is the hostile-header fast reject.
     The `Ok(_) => unreachable!(...)` arm documents the invariant that a
     14-byte input can never decode to a complete frame.
3. **Length pre-check before payload copy.**
   `wire_io.rs:20-23` extracts
   `u32::from_be_bytes([header[10], header[11], header[12], header[13]])` and
   rejects `len > MAX_FRAME_BYTES` (`1 MiB`,
   `crates/eggtunnel-proto/src/lib.rs:15`) with `FrameTooLarge`. This mirrors
   the identical check inside `decode_frame`
   (`crates/eggtunnel-proto/src/lib.rs:488-490`) but happens **before**
   `Vec::with_capacity(HEADER_LEN + len)` / `resize`, so a lying length word
   cannot force a large allocation. Note the cap is on the **postcard payload**,
   not the total on-wire bytes (`HEADER_LEN + len`).
4. **Bounded payload read.** `wire_io.rs:24-30` extends the buffer with the
   already-read header, resizes to `HEADER_LEN + len`, and `read_exact`s the
   remainder. Short payload again maps to `TruncatedFrame`.
5. **Exact-consumption check.** `wire_io.rs:31-34`:
   ```rust
   let (message, consumed) = decode_frame(&frame)?;
   if consumed != frame.len() {
       return Err(ProtocolError::InvalidPayload);
   }
   ```
    `decode_frame` itself is exactly-one-frame and trailing-tolerant
    (`crates/eggtunnel-proto/src/lib.rs:472-473`: "bytes after that frame are
    left for the caller"), and additionally rejects trailing bytes **inside**
    the postcard payload (`crates/eggtunnel-proto/src/lib.rs:500-504`). The
    `consumed != frame.len()` guard in `wire_io` closes the remaining gap: the
    caller passed exactly `HEADER_LEN + len` bytes, so any mismatch means the
    declared length and the decoded payload disagree → `InvalidPayload`. There
    is no concatenated-frame fast path here; each `read_message` consumes
    exactly one frame. Control loops that `split()` the stream
    (`client/reconnect.rs:87`, `server/control.rs:144`) rely on this 1:1 property.

### 1.2 `write_message` — encode-then-`write_all`

`crates/eggtunnel/src/wire_io.rs:38-47`: `encode_frame(message)?` (which itself
enforces `payload.len() > MAX_FRAME_BYTES →
FrameTooLarge`, `crates/eggtunnel-proto/src/lib.rs:459-461`), then
`write_all(&frame)` with I/O failure mapped to `TruncatedFrame`. The mapping is
lossy by design — a failed write surfaces as a protocol error, and the caller's
`TunnelError::From<ProtocolError>` / timeout wrapper decides the termination
category. There is no partial-frame resume; failure tears down the session or
data stream.

### 1.3 `TruncatedFrame` mapping table

| Site | Input condition | Result |
|---|---|---|
| `wire_io.rs:11-14` | `< HEADER_LEN` bytes available then EOF/error | `TruncatedFrame` |
| `wire_io.rs:15-19` probe | header-only slice | expected `TruncatedFrame` → continue; any other decode error → return that error |
| `wire_io.rs:21-23` | declared `len > 1 MiB` | `FrameTooLarge` (not `TruncatedFrame`) |
| `wire_io.rs:27-30` | header OK but payload short | `TruncatedFrame` |
| `wire_io.rs:31-34` | `consumed != frame.len()` | `InvalidPayload` |

Review note: `TruncatedFrame` therefore means three different wire realities
(clean peer close, network truncation, mid-handshake timeout collapsed by the
caller). If finer diagnostics are ever needed, the distinction must be added at
the call site (which knows whether a `timeout()` fired), not in `wire_io`.

### 1.4 Why `BoxStream` matters — transport neutrality

`crates/eggtunnel/src/wire_io.rs:1,49-57`:

```rust
use eggress_core::BoxStream;
pub(crate) async fn read_boxed(stream: &mut BoxStream) -> Result<Message, ProtocolError>
pub(crate) async fn write_boxed(stream: &mut BoxStream, message: &Message) -> Result<(), ProtocolError>
```

`read_boxed` / `write_boxed` are one-line delegates to `read_message` /
write_message, but the type is the point: `BoxStream` (from
`eggress-core = "=1.0.8"`, `Cargo.toml:22`) is the **single control-plane I/O
type** across all transports. Every handshake path converges on it:

- Baseline / mTLS: `Box::new(tcp)` → `tls_accept` / `tls_connect` returns a
  `BoxStream` (`server/accept.rs:235-242`, `client/reconnect.rs:269-272` + `829-833`).
- WebSocket: TLS `BoxStream` → `WebSocketTunnelServer/Client` adapter returns a
  `BoxStream` (`server/accept.rs:222-284`, `client/reconnect.rs:276-300`, `client/open.rs:42-53`).
- QUIC: `QuicConnection::accept_stream` / `open_stream` returns a `BoxStream`
  directly (`server/accept.rs:380-383`, `client/reconnect.rs:416-423`, `client/open.rs:59-64`).
- Outbound proxy: `OutboundConnector::connect_tcp_timeout_detailed` returns a
  `BoxStream` that is then fed into `tls_connect`
  (`client/reconnect.rs:320-353`).

Consequences for review:

- Session logic (`run_session`, `serve_control`, `accept_data_hello`) never
  branches on socket type; transport selection ends before the first
  `read_boxed`. The `websocket: bool` flag and the private
  `ClientDataTransport` enum (`client.rs:47-60`) are construction-time only.
- `tokio::io::split(stream)` on a `BoxStream`
  (`client/reconnect.rs:87`, `server/control.rs:144`) requires `BoxStream: AsyncRead +
  AsyncWrite`; the WebSocket and QUIC adapters must therefore preserve
  byte-stream semantics (see §5 on the half-close caveat — byte-stream does
  **not** imply half-close equivalence).
- Public API leakage is contained: `lib.rs:22-33` re-exports
  `Client/Server/Config/Handle`, builders, profiles, and `proto`, never
  `BoxStream`, `rustls`, `quinn`, or `tungstenite` types (per
  `ADR-0001` public-API consequence).

---

## 2. Feature matrix: what each flag pulls in and gates

Crate features: `crates/eggtunnel/Cargo.toml:15-23`. Workspace pins:
`Cargo.toml:13-35`.

Transport selection is via Builder profiles, not direct constructors:
`ClientTransportProfile::{TcpTls, Quic, WebSocket}`
(`client/config.rs:68-75`) consumed by `Client::start_profile`
(`client.rs:347-421`), and `ServerTransportProfile::{TcpTls, Quic, WebSocket}`
(`server/config.rs:51-58`) consumed by `Server::bind_profile` (`server.rs:180-213`)
→ `bind_with_tls_profile` (`server.rs:260-294`) or `bind_quic_profile`
(`server.rs:216-257`). `ClientDataTransport` (`client.rs:43-55`) is a private
enum threaded into `drive` / `handle_open`; `start_with_tls_config`
(`client.rs:437-501`) and `bind_with_tls_profile` are private late-stage
workers reached only after `validate_client_profile` / `validate_server_profile`.

| Feature | Default? | Pulls in (crate deps) | Gates in code |
|---|---|---|---|
| `client` | ✅ (`default = ["client","tls"]`, `crates/eggtunnel/Cargo.toml:16`) | `tokio`, `tokio-util`, `eggress-core`, `eggress-transport-tls`, `eggress-relay`, `getrandom`, `rustls`, + `tls` | `mod client` (`lib.rs:13-14`); `Client`, `ClientBuilder`, `ClientTransportProfile` (`client/config.rs:68-156`), `TargetConnector`, all `Client::start*` conveniences (`client.rs:224-234`) |
| `tls` | ✅ | `eggress-transport-tls` (`crates/eggtunnel/Cargo.toml:19`) | Baseline path; `TlsClientConfigBuilder` / `TlsServerConfigBuilder`, `tls_connect` / `tls_accept` |
| `server` | ❌ | `tokio`, `tokio-util`, `eggress-core`, `eggress-transport-tls`, `eggress-relay`, `rustls`, + `tls` | `mod server` (`lib.rs:15-16`); `Server`, `ServerBuilder`, `ServerTransportProfile` (`server/config.rs:51-130`), private `ServerTls` enum (`server/tls.rs:16-22`), all `Server::bind*` conveniences (`server/config.rs:35-75`) |
| `quic` (+ role slices `quic-client` / `quic-server`, `crates/eggtunnel/Cargo.toml:20-26`) | ❌ | umbrella `quic` = both roles + `eggress-transport-quic`; role slices isolate client/server builds | `ClientTransportProfile::Quic` (`client/config.rs:71-72`); `start_profile` Quic arm (`client.rs:194-196`) → `start_quic_profile` (`client.rs:325-390`), `drive` + `QuicTransport` (`client/reconnect.rs:158-203,354-437`); `ServerTransportProfile::Quic` (`server/config.rs:52-57`); `Server::bind_quic[_with_policy]` (`server.rs:87-104`), `bind_quic_profile` (`server.rs:216-257`), `quic_server_loop*`, `handle_quic_connection`, `handle_quic_data_stream` (`server/accept.rs:147-180,369-480`); QUIC stream budget via `max_active_data_streams` (`server/accept.rs:160-180`) and client `QuicTransport` (`client/reconnect.rs:354-437`) |
| `websocket` (+ role slices `websocket-client` / `websocket-server`, `crates/eggtunnel/Cargo.toml:27-30`) | ❌ | umbrella `websocket` = both roles + `eggress-protocol-websocket` + `tokio-tungstenite`; role slices isolate client/server builds | `ClientTransportProfile::WebSocket` (`client/config.rs:73-74`); `start_profile` WebSocket arm (`client.rs:198-200`) → `start_with_tls_config(..., websocket: true)` (`client.rs:437-501`), `StreamTransport` upgrade (`client/reconnect.rs:276-300`), `handle_open` data-path upgrade (`client/open.rs:42-53`); server `ServerTransportProfile::WebSocket` (`server/config.rs:52-57`) → `bind_profile` WebSocket arm (`server.rs:203-206`), `handle_connection` upgrade (`server/accept.rs:336-357`); `Server::bind_websocket` (`server.rs:79-84`), `Client::start_websocket*` (`client.rs:235-252`) |
| `outbound-proxy` | ❌ | `client` + `eggress-outbound` with `pproxy-compat` (`crates/eggtunnel/Cargo.toml:31`, `Cargo.toml:43`) | `outbound: Option<Arc<OutboundConnector>>` on private `ClientDataTransport::TcpTls` (`client.rs:43-55`) and `start_with_tls_config` (`client.rs:437-501`); `ClientBuilder::outbound_proxy` (`client/config.rs:124-128`); `parse_outbound_proxy` via `OutboundConnector::from_pproxy_uri` (`client.rs:522-528`); `connect_tcp` proxy branch (`client/reconnect.rs:320-353`); `validate_outbound_proxy` re-export (`lib.rs:22`); `start_with_outbound_proxy*`, `start_websocket_with_outbound_proxy*` builder conveniences (`client.rs:255-302`) |
| `mtls` | ❌ | `tls` + `tokio-rustls`, `webpki-roots`, `sha2` (`crates/eggtunnel/Cargo.toml:32`) | `ServerTls::Mutual` (`server/tls.rs:14-19`); `Server::bind_mtls*` (`server.rs:152-178`), `build_mtls_server_config` (`server/tls.rs:36-62`), `certificate_principal` SHA-256 (`server/tls.rs:64-67`); `Client::start_with_mtls*` builder conveniences (`client.rs:414-435`), `ClientIdentity` + redacted `Debug` + `zeroize` drop (`client.rs:531-562`), `build_mtls_tls_config` (`client.rs:661-683`), `ClientBuilder::with_identity` (`client/config.rs:118-122`) |

Notes:

- `client` implies `tls` (`crates/eggtunnel/Cargo.toml:17`: `client = [... "tls"]`).
  Role slices `quic-client` / `quic-server` and `websocket-client` /
  `websocket-server` exist precisely for single-role builds. Transport
  profile validation lives in `validate_client_profile`
  (`client.rs:570-613`), not in `endpoint.rs` (which owns only
  `Endpoint::parse`); the CLI enforces the same coupling at config-check time (§6).
- `mod wire_io` exists iff `client` or `server` is enabled
  (`lib.rs:10-11`). A `proto`-only build has no socket dependency, per
  `docs/ARCHITECTURE.md:3-6`.
- Dev-only: `crates/eggtunnel/Cargo.toml:46-48` enables
  `eggress-transport-quic/insecure-quic` for tests. That feature must never
  leak into non-test builds; production QUIC goes through
  `QuicClientConfig { insecure: false }` (default `..QuicClientConfig::default()`
  with `insecure` param `false` from `start_profile`, `client/reconnect.rs:401-408`). The insecure
  path is reachable only via `start_quic_insecure_for_test`
  (`client.rs:345-413`).
- Workspace pins every Eggress crate to exactly `=1.0.8`
  (`Cargo.toml:22-27`; `Cargo.lock` confirms `eggress-core/-relay/
  -transport-tls/-transport-quic/-protocol-websocket/-outbound 1.0.8`).
  `tokio-tungstenite 0.26.2`, `rustls 0.23` (`ring`, `std`, `tls12`),
  `tokio-rustls 0.26` (`ring`, `tls12`) are also pinned in the workspace
  (`Cargo.toml:28-30`).

---

## 3. Baseline TCP+TLS: Eggress builders, ring provider, TLS 1.2+, caller runtime

### 3.1 Construction

- Server: `Server::bind` → `ServerBuilder::bind` → `Server::bind_profile`
  (`server.rs:180-213`) builds either
  `ServerTls::Mutual(build_mtls_server_config(...))` or
  `ServerTls::Eggress(build_server_tls(...))` (`server.rs:194`; profile dispatch at `server.rs:180-213`;
  `build_server_tls` = `TlsServerConfigBuilder::new().with_certificate_pem(...).with_key_pem(...).build()`,
  `server/tls.rs:23-35`) and hands it to `bind_with_tls_profile`
  (`server/config.rs:109-145`). Every accepted `TcpStream` is boxed then passed to
  `eggress_transport_tls::tls_accept(stream, tls)` under
  `policy.timeouts.handshake` (`server/accept.rs:235-242`).
- Client: `Client::start` → `ClientBuilder::start` → `Client::start_profile`
  (`client.rs:224-234`, `client/config.rs:142-155`, `client.rs:347-421`):
  `build_tls_config` (`client/reconnect.rs:186-194`):
  `TlsClientConfigBuilder::new().with_system_roots()` or
  `.with_custom_ca_pem(pem)`, then `.build()`. `reconnect_loop`
  (`client/reconnect.rs:158-298`) dials via `connect_server` (`client/reconnect.rs:320-353`,
  `policy.timeouts.connect`), then `tls_connect(stream, tls, &tls_server_name)`
  under `policy.timeouts.handshake` (`client/reconnect.rs:269-272`). The data path repeats
  the same two steps per `Open` (`client/open.rs:31-41`).
- Validation before any socket: `validate_client_profile` checks the
  `RuntimePolicy`, endpoint shape, non-empty ≤253 B server name, service count
  against `policy.limits.services_per_session` with unique IDs/names, CA size cap,
  and transport rejections (`endpoint.rs:58-140`); server checks
  `runtime_policy.validate()` + `bind_policy.validate()` + cert/key presence and
  size cap via `validate_server_profile` (`server/config.rs:148-173`,
  `443-457`) plus `BindPolicy::validate` (`common.rs:100-112`).

### 3.2 TLS properties relevant to review

- **Builders are Eggress-owned.** `TlsClientConfigBuilder` /
  `TlsServerConfigBuilder` come from `eggress-transport-tls 1.0.8`. Eggtunnel
  never constructs `rustls::{Client,Server}Config` directly on this path
  (contrast the mTLS path, §3.4 of the server/client dives, which drops to
  `rustls` + `tokio-rustls` directly; PEM decoding uses the Rustls pki-types parser).
- **Ring provider.** Per `docs/SECURITY.md:41-43`, the Eggress 1.0.8 builders
  "install the Rustls ring provider as the process default if no provider has
  been set". Workspace features corroborate: `rustls` with `ring/std/tls12`,
  `tokio-rustls` with `ring/tls12` (`Cargo.toml:29-30`). Review implication:
  first TLS build in the process wins the global provider; embedding two TLS
  stacks with different providers in one process can surprise. Eggtunnel itself
  "does not install a runtime or tracing subscriber" (`docs/SECURITY.md:43`,
  `lib.rs:3-5`).
- **TLS 1.2+.** The `tls12` feature on both `rustls` and `tokio-rustls`
  preserves TLS 1.2 floor alongside 1.3. There is no Eggtunnel-side
  version/cipher allowlist; that policy is delegated to Eggress + rustls
  defaults. Any future requirement to pin 1.3-only must be expressed as a
  builder option or workspace feature change, not a `wire_io` change.
- **SNI + verification.** Client passes `tls_server_name` (validated
  non-empty, ≤253 B) as the SNI/verification name on every control **and**
  data connection (`client/reconnect.rs:271`, `client/open.rs:40`). Server-name verification uses
  system roots by default or the explicit `ca_pem` bundle; `ClientConfig`'s
  `Debug` redacts the token and collapses `ca_pem` to `[configured]`
  (`client/config.rs:56-66`). Auth (`Auth` bearer token) always runs **inside** the
  verified channel (`docs/SECURITY.md:3-7`).

### 3.3 Caller-owned Tokio runtime invariant

Every public entrypoint asserts a caller-owned runtime before doing I/O:

- `Client::start_with_tls_config` (`client.rs:350-352`),
  `Client::start_quic_profile` (`client.rs:325-390`);
- `Server::bind_with_tls_profile` (`server.rs:260-294`),
  `Server::bind_quic_with_admission_for_test` (`server.rs:107-149`, test-only).

`Server::bind_quic_profile` (`server/config.rs:31-66`) performs no separate
`try_current` gate; it runs inside the caller's `bind().await` future, which
reaches the runtime-gated `bind_with_tls_profile` only on the TCP/WSS path.

Failure is `TunnelError::Configuration("... requires a caller-owned Tokio
runtime")` → `TerminationCategory::Internal`. The library never calls
`#[tokio::main]`, `Runtime::new`, or installs a global tracing subscriber
(`lib.rs:3-5`, roadmap §2 runtime invariants).

### 3.4 Baseline handshake sequence (TCP+TLS)

```text
client                                    server
  | TcpStream::connect(server_addr) [policy.timeouts.connect]  |
  |--------------------------------------->| listener.accept()
  | tls_connect(stream, tls, server_name)  | tls_accept(Box(tcp), tls) [policy.timeouts.handshake each]
  |<=========== verified TLS =============>|
  | ClientHello(version, caps) via write_boxed
  |--------------------------------------->| read_boxed → check major
  |                          ServerHello(current) via write_boxed
  |<---------------------------------------|
  | Auth(token)                              | verify_token (constant-time) +
  |--------------------------------------->| 100ms delay + Error(4) on failure
  |                          AuthOk(session_id)
  |<---------------------------------------|
  | RegisterService × N  ←——————→  RegisterAck(effective_bind) / Error
  | ... control loop: Ping/Pong, Open/OpenReject, Drain, Error ...   |
  |                                        | accept external TCP → PendingEntry
  |                          Open(service, connection_id)
  |<---------------------------------------|
  | dial target + dial NEW TcpStream + tls_connect + DataHello(session, service, connection)
  |--------------------------------------->| atomic consume PendingEntry → relay_with_options
  |<=========== opaque bytes =============>|
```

Control uses `read_boxed`/`write_boxed` throughout (`server/control.rs:94-164`;
`client/reconnect.rs:277-338`). Data connections branch on the first
message in `handle_connection` (`server/accept.rs:248-267`): `DataHello` → relay,
`ClientHello` → control session, anything else → `UnexpectedMessage`.

---

## 4. QUIC: UDP control endpoint, one bidi stream per path

Feature: `quic` (`crates/eggtunnel/Cargo.toml:20`). Docs:
`docs/SUPPORT.md:6`, `docs/SECURITY.md:45-62`, `docs/ARCHITECTURE.md:12-13`.

### 4.1 What changes vs baseline

| Aspect | TCP+TLS baseline | QUIC profile |
|---|---|---|
| Listener | `TcpListener::bind` (`server/config.rs:119`) | `QuicListener::bind(addr, QuicServerConfig { cert, key, idle_timeout: policy.timeouts.control_idle, max_concurrent_streams: policy.limits.active_connections_per_session + 1, alpn: [] })` (`server/config.rs:74-86`) |
| Control transport | one TLS-over-TCP connection | one UDP QUIC connection per session; first inbound bidi stream is the control stream (`server/accept.rs:380-391`) |
| Data transport | one TCP+TLS connection per external conn | one bidi stream per external conn on the **same** QUIC connection (`server/accept.rs:426-447`, `client/open.rs:59-64`) |
| Service listeners | TCP (`TcpListener` accept in `run_service`, `server/service.rs:66-111`; bound at `:1174`) | **retained as TCP** — "Service listeners remain TCP even when the control Session uses QUIC over UDP" (`docs/SUPPORT.md:12-13`) |
| Trust | system roots or custom CA | **platform roots only, bearer only** — custom CA / mTLS rejected, not ignored |
| Client entry | `ClientBuilder` + `ClientTransportProfile::TcpTls` | `ClientBuilder.transport(ClientTransportProfile::Quic)` → `start_profile` → `start_quic_profile` (`client.rs:347-422`, `320-364`); conveniences `Client::start_quic[_with_connector]` (`client.rs:305-312`) |
| Server entry | `ServerBuilder` + `ServerTransportProfile::TcpTls` | `ServerBuilder.transport(ServerTransportProfile::Quic)` → `bind_profile` → `bind_quic_profile` (`server/config.rs:31-66`, `337-380`); conveniences `Server::bind_quic[_with_policy]` (`server/config.rs:49-57`) |

### 4.2 Handshake sequence (QUIC)

```text
client (QuicClient)                       server (QuicListener, UDP)
  | QuicClient::connect(host, port, QuicClientConfig{server_name, insecure:false, idle = policy.timeouts.control_idle, max_streams = policy.limits.client_open_tasks}) [policy.timeouts.connect]
  |--------------------------------------->| accept_connection(&cancel)
  | get_connection() [connect] → open_stream() [connect] (control)
  |--------------------------------------->| accept_stream() [handshake] → read_boxed → expect ClientHello
  | ... same Session handshake as baseline (ServerHello/Auth/AuthOk/Register*) over control stream ... |
  |                                        | external TCP accept → PendingEntry → Open over control stream
  | connection.open_stream() [connect] (data)  |
  |--------------------------------------->| accept_stream() → read_boxed → expect DataHello
  | DataHello(session, service, connection) | accept_data_hello (wrong-session/stale/replay → Auth error)
  |--------------------------------------->| relay_with_options (opaque bytes over bidi stream)
```

Key call sites: client `quic_reconnect_loop` (`client/reconnect.rs:158-260`):
config build (`867-874`), connect with `policy.timeouts.connect` (`875-887`),
`get_connection` (`890-893`), control `open_stream` (`894-897`), then the
shared `run_session` with `ClientDataTransport::Quic(connection)`
(`899-912`). Data dial is `connection.open_stream()` under
`policy.timeouts.connect` (`client/open.rs:59-64`) followed by `DataHello` + relay
(`client/open.rs:66-67`).

Server `quic_server_loop[_with_admission]` (`server/accept.rs:96-146`) mirrors
`server_loop` admission accounting, then `handle_quic_connection`
(`server/accept.rs:159-222`): accept first stream with `policy.timeouts.handshake`
(`716-719`), `read_boxed` expecting `ClientHello` (`720-727`), spawn
`serve_control` (`742`), then loop `accept_stream` for data streams
(`760-782`) each handled by `handle_quic_data_stream` (`800-816`), which
expects `DataHello` under `policy.timeouts.handshake` and delegates to the shared
`accept_data_hello`.

### 4.3 Limits and their interaction (read carefully — three layers)

All Eggtunnel-side ceilings come from `RuntimePolicy`
(`common.rs:194-316`). `ResourceLimits` is an 8-field struct
(`common.rs:196-205`: `sessions`, `services_per_session`,
`pending_per_session`, `active_connections_per_session`,
`accepted_handshakes`, `client_open_tasks`, `control_queue`,
`client_command_queue`; each `1..=65536`, `common.rs:208-230`) with defaults
`128/64/128/128/64/128/128/32` (`common.rs:232-245`). `TimeoutPolicy`
(`common.rs:249-302`) defaults to `connect/handshake 10 s`, `control_idle 90 s`,
`pending_connection 30 s`, `relay_drain 15 s`, `shutdown_grace 1 s`,
`reconnect_initial 500 ms`, `reconnect_max 30 s`, `heartbeat_interval 20 s`,
with validation (`common.rs:263-286`: nonzero, ≤24 h,
`reconnect_initial ≤ reconnect_max`, `heartbeat_interval < control_idle`).

1. **Eggress QUIC adapter (per-connection/task fan-out).**
   `docs/SECURITY.md:51-53`: `MAX_CONCURRENT_CONNECTION_TASKS=1024` and
   `MAX_CONCURRENT_STREAM_TASKS=4096`. These bound Eggress-internal task
   spawning per connection/stream **before** Eggtunnel authentication. They are
   not Eggtunnel constants; no Eggtunnel source defines them.
2. **Eggtunnel pre-session admission (`accepted_handshakes`, default 64).**
   `Semaphore::new(counters.policy.limits.accepted_handshakes)` in both
   `server_loop` (`server/accept.rs:102`, `550-555`) and
   `quic_server_loop_with_admission` (`server/accept.rs:167`, `654-659`), plus a
   `HandshakeGuard` active-handshake counter (`server/session.rs:135-156`). (`MAX_SESSIONS`
   / `MAX_HANDSHAKES` at `server_tests.rs:35` are `#[cfg(test)]`-only and play no
   role in production.) On exhaustion: TCP path drops the accept + `rejected+1` +
   `ResourceExhausted` (`server/accept.rs:112-117`); QUIC path additionally
   `connection.close("handshake limit reached")` (`server/accept.rs:112-117`).
   Per `docs/SECURITY.md:50-57`, "pre-session UDP/TLS handshake work runs
   inside the adapter before Eggtunnel's semaphore is acquired", so the
   residual pre-auth admission risk is documented as observable, not
   eliminated — no vendoring required.
3. **Per-session stream admission (default 128).**
   `counters.policy.limits.active_connections_per_session` (default 128),
   instantiated per QUIC connection as
   `Semaphore(max_active_data_streams.unwrap_or(policy...))` (`server/accept.rs:388-396`).
   Each accepted data stream does `try_acquire_owned`; on failure:
   `rejected+1` + `ResourceExhausted`, stream dropped without a response
   (`server/accept.rs:428-432`). The semaphore saturates, recovers on stream close
   (permit is held by the spawned task, `server/accept.rs:436-439`), and rejects
   beyond the policy limit — qualified by the stream-saturation test
   (`docs/SECURITY.md:57-60`; test-only override
   `bind_quic_with_admission_for_test`, `server/config.rs:65-77`, and the
   saturation test `quic_stream_saturation_recovers_capacity_and_keeps_unrelated_streams_alive`,
   `server_tests/quic.rs:641`).

Interaction mental model for review: Eggress 1024/4096 caps adapter-internal
fan-out; Eggtunnel `accepted_handshakes` (default 64) caps
**unauthenticated handshakes process-wide**
(both TCP and QUIC); `active_connections_per_session` (default 128) caps
**active data streams/connections per session** (QUIC `stream_admission`,
TCP `connection_admission` `server/control.rs:130-132`, client open-task semaphore
`policy.limits.client_open_tasks` at `client/reconnect.rs:197`) ≠
`control_queue` (`policy.limits.control_queue`: `client/reconnect.rs:130`,
`server/control.rs:145`) ≠ `client_command_queue`
(`policy.limits.client_command_queue`: `client.rs:385`, `427`). Any log, metric,
or doc that says "connection limit" must say which one. Do not conflate
"connection" (QUIC UDP 4-tuple ≈ session transport) with "stream" (one
external TCP connection) or with "handshake task" (pre-auth work unit).

Additional QUIC specifics:

- `QuicServerConfig { idle_timeout: policy.timeouts.control_idle,
  max_concurrent_streams: policy.limits.active_connections_per_session + 1 }`
  (`server/config.rs:79-83`); client mirrors `idle_timeout:
  policy.timeouts.control_idle, max_concurrent_streams:
  policy.limits.client_open_tasks` (`client.rs:357` → `client/reconnect.rs:371-405`) — 128 by default
  on the client, i.e. the client passes the budget verbatim while the server
  adds one control stream (`active_connections_per_session + 1` = 129), so
  Eggtunnel admission rejects first.
- Test-only admission override: `bind_quic_with_admission_for_test` scales
  `max_concurrent_streams = max(1, n) * 2` (`server/config.rs:93`) — test-only,
  `#[cfg(all(test, feature = "quic"))]`.
- Bearer token still required inside the encrypted control stream
  (`docs/SECURITY.md:48-49`); QUIC provides confidentiality + SNI verification,
  not authentication. Platform roots + verified SNI; no custom-root or
  client-cert knobs exist on the adapter, hence fail-closed rejects (§6).
- Session/stream correlation (wrong-session, stale, replay, half-close) is
  qualified end-to-end via C001 (`docs/SECURITY.md:60-62`,
  `docs/SUPPORT.md:6`).

---

## 5. WebSocket (WSS): verified-TLS-then-upgrade, binary 1 MiB, whole-connection close

Feature: `websocket` (`crates/eggtunnel/Cargo.toml:21`). Docs:
`docs/SUPPORT.md:7`, `docs/SECURITY.md:64-72`, `docs/ARCHITECTURE.md:13-14`.

### 5.1 Construction

- Client control: after `tls_connect` succeeds, if `websocket == true`,
  `WebSocketTunnelClient::new(1024*1024).connect_over_stream_with_config(
  &url, stream, ws_config)` under `policy.timeouts.handshake`, where
  `url = format!("wss://{}", server_addr)` and `ws_config` sets
  `max_message_size = Some(1 MiB)` and `max_frame_size = Some(1 MiB)`
  (`client/reconnect.rs:158-203`). Failure maps to `Timeout` (outer) or `Tls` (inner).
  Data connections repeat the identical upgrade per `Open`
  (`client/open.rs:42-53`).
- Server: after `tls_accept` (or mTLS accept — but WSS+mTLS is rejected in
  `validate_server_profile` before this point, §6), if `websocket == true`,
  `WebSocketTunnelServer::new(1024*1024)
  .accept_upgrade_with_config_over_stream(stream, ws_config)` under
  `policy.timeouts.handshake` with the same 1 MiB caps (`server/accept.rs:222-284`).
  Upgrade failure maps to `Timeout` (outer) or
  `Protocol(UnexpectedMessage)` (inner) — note asymmetry with the client side
  (§8.1).
- Entry points: `ClientBuilder.transport(ClientTransportProfile::WebSocket)` with
  conveniences `Client::start_websocket[_with_connector]`
  (`client.rs:229-239`), `ServerBuilder.transport(ServerTransportProfile::WebSocket)`
  with convenience `Server::bind_websocket` (`server/config.rs:39-49`), CLI
  `transport = "websocket_tls"` mapped to profiles in
  `client_builder` / `server_builder`
  (`crates/eggtunnel-cli/src/main.rs:138-144`, `178-184`).

### 5.2 Handshake sequence (WSS, control and each data connection)

```text
client                                    server
  | TCP connect → tls_connect (verified, SNI) — identical to baseline
  |<=========== verified TLS =============>|
  | WS upgrade: WebSocketTunnelClient.connect_over_stream_with_config(wss://addr, tls_stream) [policy.timeouts.handshake]
  |--------------------------------------->| WebSocketTunnelServer.accept_upgrade_with_config_over_stream(tls_stream) [policy.timeouts.handshake]
  |<=========== binary WS byte stream (1 MiB cap) =============>|
  | ... identical Session handshake (ClientHello/Auth/...) via read/write_boxed over WS stream ... |
  | per-Open: NEW TCP → NEW tls_connect → NEW WS upgrade → DataHello → relay |
```

Properties to hold in review:

- **Verified TLS first.** The upgrade runs over the already-verified TLS
  stream; there is no `ws://` plaintext mode. Trust (system vs custom CA) and
  SNI verification are inherited from the TLS layer
  (`docs/SECURITY.md:64-65`, `docs/SUPPORT.md:7`).
- **Binary messages, 1 MiB cap.** Both `WebSocketTunnel{Client,Server}::new(
  1024*1024)` and the `tokio-tungstenite` `WebSocketConfig`
  (`max_message_size = Some(1 MiB)`, `max_frame_size = Some(1 MiB)`) agree on
  1 MiB (`client/reconnect.rs:282-289`, `client/open.rs:45-48`, `server/accept.rs:254-259`).
  Multi-frame bounded backpressure round-trips within the caps per C001
  (`docs/SECURITY.md:70-72`).
- **Non-browser endpoint.** "Intended for non-browser tunnel clients; the
  adapter does not validate Origin and makes no browser cross-site security
  claim" (`docs/SECURITY.md:65-67`). Do not review this as a browser WebSocket
  server; there is no Origin allowlist to audit because none is claimed.
- **Close = whole-connection close.** "Its close operation closes the
  WebSocket connection as a whole, so TCP half-close equivalence is not
  promised" (`docs/SECURITY.md:67-68`; `docs/SUPPORT.md:15-16`, `26-30`).
  There is no write-half-close signal across the WS byte stream. C001 confirms
  the narrower property that actually holds: peer close during active relay
  terminates the underlying TCP connection promptly without dangling relay
  halves (test `wss_peer_close_during_active_relay_terminates_cleanly`,
  `server_tests/websocket.rs:195`; session/data round-trip
  `websocket_tls_session_registers_and_relays_data_paths`,
  `server_tests/websocket.rs:17`). Application code must not depend on observing a
  TCP-style `shutdown(Write)` through WSS.
- **Session semantics unchanged.** QUIC/WS "do not change Service,
  authorization, TargetConnector, or ConnectionId semantics"
  (`docs/SUPPORT.md:10`).
- **Support-table caveat.** `docs/SUPPORT.md:48-51` documents that crates
  `eggtunnel-proto` and `eggtunnel` are published at `0.2.0`, matching the
  workspace `version = "0.2.0"` (`Cargo.toml:6`). The transport profile
  matrix is unchanged from `0.1.0`.

---

## 6. Outbound proxy (client-only): CONNECT/SOCKS5 + `__` chains, env credentials

Feature: `outbound-proxy` (`crates/eggtunnel/Cargo.toml:22`, workspace
`eggress-outbound = "=1.0.8"` with `pproxy-compat`, `Cargo.toml:27`). Docs:
`docs/SUPPORT.md:8`, `docs/SECURITY.md:74-85`, `docs/ARCHITECTURE.md:14-20`.

### 6.1 Shape

- **Client-only, listener-free.** `OutboundConnector` dials each client
  connection **before** Eggtunnel TLS; it "does not start a local proxy
  listener or change the Session protocol" (`docs/ARCHITECTURE.md:16-17`).
  Server + proxy is rejected at CLI check
  (`crates/eggtunnel-cli/src/main.rs:269-271`: "outbound_proxy is only valid
  in client mode"). There is no `Server::bind_*_with_proxy`.
- **Profiles.** Direct, HTTP CONNECT, SOCKS5 single-hop, plus multi-hop chains
  through the canonical `__`-separated pproxy URI syntax
  (`docs/ARCHITECTURE.md:17-20`, `docs/SUPPORT.md:8`). Parsing is a single
  delegation: `OutboundConnector::from_pproxy_uri(chain)`
  (`client.rs:577-581`); invalid chains →
  `TunnelError::Configuration("invalid outbound proxy chain")`. Public
  pre-flight: `validate_outbound_proxy` (`client.rs:598-607`,
  re-exported `lib.rs:20-21`); `eggtunnel check` additionally validates the
  full builder via `client_builder(config)?.validate()`
  (`crates/eggtunnel-cli/src/main.rs:245`).
- **Per-connection use.** `connect_server(endpoint,
  connect_timeout, outbound.as_deref())` (`client/reconnect.rs:320-353`): with a proxy,
  `split_endpoint` → `outbound.connect_tcp_timeout_detailed(host, port,
  connect_timeout)` (`client/reconnect.rs:330-342`); without, direct
  `TcpStream::connect` (`829-833`). Both control (`client/reconnect.rs:265-268`) and
  every data dial (`client/open.rs:33-36`) traverse the same proxy path, so a
  session over proxy opens N proxied data connections.
- **Auth.** HTTP CONNECT Basic and SOCKS5 username/password via URI userinfo
  (`docs/SECURITY.md:81-83`). `OutboundConnectErrorKind::Authentication /
  Policy / Timeout` map to `Authentication / Authorization / Timeout`,
  everything else to `Disconnected` (`client/reconnect.rs:332-337`) — hence proxy auth
  failure is typed, never a silent direct fallback.

### 6.2 Credential handling (env var + redaction)

- Credentials "should be placed in the environment variable named by
  `outbound_proxy_env`" (`docs/SECURITY.md:76-78`). CLI `check` verifies the
  variable exists, is non-empty, and parses
  (`crates/eggtunnel-cli/src/main.rs:226-239`); `client_builder` reads it once
  with `env::var` (`main.rs:133-137`). The proxy URI (possibly containing
  userinfo) never comes from the TOML file itself.
- Redaction: "redacted from Eggtunnel diagnostics and the public `Snapshot`
  view" (`docs/SECURITY.md:77-78`, `docs/SUPPORT.md:17-19`). Structurally:
  `Snapshot` (`common.rs:136-160`) has no proxy/credential fields at all;
  `ClientConfig::Debug` (`client/config.rs:56-66`) prints no proxy material (proxy
  lives on the private `ClientDataTransport`, which derives only `Clone` and
  has no `Debug` impl); outbound failure
  tests assert no secret in diagnostics, e.g.
  `outbound_proxy_refused_endpoint_terminates_without_secret_in_diagnostic`
  (`server_tests/proxy.rs:189`), `outbound_http_connect_auth_failure_rejects_without_secret_leak`
  (`server_tests/proxy.rs:483`), `outbound_socks5_auth_failure_rejects_without_secret_leak`
  (`server_tests/proxy.rs:692`). Failures surface only as typed termination
  categories (`docs/SUPPORT.md:18-19`).
- Multi-hop evidence: one two-hop SOCKS5→HTTP CONNECT end-to-end test
  (`server_tests/proxy.rs:773`,
  `outbound_two_hop_socks5_then_http_connect_routes_end_to_end`);
  "additional protocol combinations are unverified beyond the Eggress 1.0.8
  public API's typed compatibility layer" (`docs/SUPPORT.md:31-34`).

### 6.3 TLS+SNI end-to-end, no silent fallback, rejected combos

- "Eggtunnel TLS and server-name verification run over the established proxy
  path, protecting authentication from a proxy that only forwards CONNECT or
  SOCKS traffic" (`docs/SECURITY.md:74-76`). Concretely: proxy yields a raw
  `BoxStream`, then the **same** `tls_connect(stream, tls, &tls_server_name)`
  + optional WSS upgrade runs on top (`client/reconnect.rs:269-307` control,
  `client/open.rs:31-53` data). End-to-end TLS tests:
  `outbound_http_connect_keeps_eggtunnel_tls_end_to_end`
  (`server_tests/proxy.rs:5`),
  `outbound_socks5_keeps_eggtunnel_tls_end_to_end` (`server_tests/proxy.rs:91`).
- "The client does not silently fall back to direct networking when a proxy
  path fails" (`docs/SECURITY.md:79-81`): refusal, handshake timeout, and
  cancellation each produce typed termination categories, covered by
  `outbound_proxy_refused_endpoint_...` (`server_tests/proxy.rs:189`),
  `outbound_proxy_handshake_timeout_...` (`server_tests/proxy.rs:253`),
  `outbound_proxy_cancellation_...` (`server_tests/proxy.rs:323`).
- **Rejected combos (fail-closed, at both library and CLI layers):**
  enforcement lives in `validate_client_profile` (`endpoint.rs:58-102`) and
  `validate_server_profile` (`server/config.rs:148-172`); the CLI delegates through
  `client_builder(config)?.validate()` / `server_builder(config)?.validate()`
  inside `check_config` (`main.rs:245`, `272`), after its own structural checks
  (`main.rs:198-244`, `247-273`):

  | Combination | Library behavior | CLI `check` behavior |
  |---|---|---|
  | proxy + QUIC | `validate_client_profile` rejects proxy/identity/CA with `Quic` (`endpoint.rs:58-102`); `start_profile` Quic arm takes no proxy (`client.rs:347-422`) | structural transport check (`main.rs:200-205`) + builder `validate()` (`main.rs:245`) → error |
  | proxy + mTLS | `validate_client_profile` rejects identity + proxy (`endpoint.rs:58-102`); mTLS arm passes `None` as outbound (`client.rs:347-422`) | `client_cert`/`client_key` pairing + file checks (`main.rs:223-244`) + builder `validate()` (`main.rs:245`) → error |
  | QUIC + custom CA / mTLS | `validate_client_profile` rejects `ca_pem.is_some()` / identity with `Quic` (`endpoint.rs:58-102`); `start_quic_profile` also rejects `ca_pem` (`client.rs:329-397`) | builder `validate()` (`main.rs:245`); server `client_ca` + non-TCP profile rejected by `validate_server_profile` (`server/config.rs:148-174`, via `main.rs:272`) |
  | WSS + mTLS | `validate_client_profile` rejects identity with `WebSocket` (`endpoint.rs:58-102`); `validate_server_profile` rejects `client_ca` with non-`TcpTls` (`server/config.rs:148-174`) | builder `validate()` (`main.rs:245`, `272`) → error |
  | server + proxy | No server proxy API | `main.rs:269-271`: `outbound_proxy_env` in server mode → error |

  WSS **over** proxy (`start_websocket_with_outbound_proxy[+connector]`,
  `client.rs:280-293`) is the one allowed composition: proxy → TLS → WSS
  upgrade, selected by `transport = "websocket_tls"` + `outbound_proxy_env`
  in `client_builder` (`main.rs:138-164`).

---

## 7. Eggress dependency direction: what Eggtunnel owns vs provides (pinned =1.0.8)

Per `ADR-0001` (decision: Eggtunnel owns reverse-session semantics,
Eggress supplies narrow primitives; public-API and dependency consequences):

**Eggtunnel owns** (never delegated): native protocol/version negotiation,
framing bounds, authenticated Session lifecycle, multi-Service registration,
server-authoritative bind allocation (`BindPolicy`), Pending Connection
registry, `ConnectionId` generation/expiry/single-use consumption,
`Open`/`DataHello` orchestration, reconnect + registration restoration,
service/bind/admission ceilings, shutdown/drain, transport **adapter selection**
(not implementation), secret-free snapshots/diagnostics, embedding API.

**Eggress 1.0.8 provides** (generic primitives, narrow crates only):

| Eggress crate (=1.0.8) | Supplies | Used at |
|---|---|---|
| `eggress-core` | `BoxStream` stream representation | `wire_io.rs:1`, all `read/write_boxed` call sites |
| `eggress-relay` | `relay_with_options` + `RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain)` opaque byte copy | `client/open.rs:67`, `server/service.rs:121` |
| `eggress-transport-tls` | `TlsClient/ServerConfigBuilder`, `tls_connect`, `tls_accept` | `client.rs:5,650-660,269-272`; `client/open.rs:40`; `server/tls.rs:23-35,235-242` |
| `eggress-transport-quic` | `QuicListener/Client/Connection`, `QuicClient/ServerConfig`, stream open/accept | `client/reconnect.rs:401-431`; `client/open.rs:59-64`; `server/config.rs:74-86,131-322` |
| `eggress-protocol-websocket` | `WebSocketTunnelClient/Server`, `connect_over_stream_with_config`, `accept_upgrade_with_config_over_stream` | `client/reconnect.rs:276-286`; `client/open.rs:45-52`; `server/accept.rs:257-262` |
| `eggress-outbound` (+ `pproxy-compat`) | `OutboundConnector::from_pproxy_uri`, `connect_tcp_timeout_detailed`, `OutboundConnectErrorKind` | `client.rs:577-581,330-397` |

Also: `tokio-tungstenite 0.26.2` supplies only the `WebSocketConfig`
(1 MiB `max_message_size` + `max_frame_size`) passed into the Eggress adapter
(`client/reconnect.rs:282-284`, `client/open.rs:46-48`; `server/accept.rs:254-256`) — the tunnel framing itself is Eggress's.

Boundaries worth asserting in review:

- No `eggress-embed`, no pproxy reverse protocol as product, no Synvoid/i2pr
  production dependency (`ADR-0001` alternatives-rejected + dependency
  consequence). The pproxy URI syntax is reused for proxy chains
  only, via the `pproxy-compat` feature.
- Narrow crates + `default-features = false` where applicable
  (`Cargo.toml:25-27`) keep the minimal client slice (`client` + `tls`) free
  of QUIC/WebSocket/proxy code (`ADR-0001` dependency consequence).
- All Eggress versions are exact (`=`), locked in `Cargo.lock` at 1.0.8.
  Bumping Eggress is a deliberate compat event: re-verify ring-provider
  behavior, QUIC task caps, WS message caps, and outbound error-kind mapping.

---

## 8. Review checklist

Use these as accept/reject prompts on any change touching `wire_io.rs`,
feature gates, or transport construction. Each item names the file:line that
pins current behavior.

### 8.1 Downgrade / negotiation risks

- [ ] **No plaintext fallback.** Control and every data connection establish
      TLS before the first Eggtunnel byte on all profiles
      (`client/reconnect.rs:265-274`, `client/open.rs:31-41`; `server/accept.rs:234-242`). WS upgrade
      happens only over verified TLS (`client/reconnect.rs:158-203`,
      `client/open.rs:42-53`, `server/accept.rs:222-284`). There is no `ws://` or bare-TCP session path.
      Reject any change that adds one without an ADR.
- [ ] **Version check is major-only, fail-closed.** `serve_control` rejects
      `hello.version.major != CURRENT.major` (`server/control.rs:86-93`);
      `decode_frame` rejects bad magic / unknown major (`proto:475-484`) and unknown
      message IDs (`proto:266`) / oversize frames (`proto:488-489`). Client checks `ServerHello` major
      (`client/reconnect.rs:283-291`). Minor is informational. Confirm tests still pin
      wire `CURRENT` + message-ID coverage (`proto:609-...`; see `proto` tests).
- [ ] **Error-kind collapse on WS.** Client maps WS upgrade failure to
      `Tls`/`Timeout` (`client/reconnect.rs:289-291`, `client/open.rs:51`); server maps it to
      `Timeout`/`Protocol(UnexpectedMessage)` (`server/accept.rs:259-262`). Same
      wire event yields `Transport` on one side and `Protocol` on the other
      (`common.rs:466-482`). Acceptable today (typed, no silent retry
      difference — auth failures still break the reconnect loop,
      `client/reconnect.rs:106-114`), but do not "fix" one side without updating
      dashboards/tests that key on termination categories.
- [ ] **Auth-failure loop break preserved.** Only
      `Authentication`/`Authorization` break `reconnect_loop`/`quic_reconnect_loop`
      (`client/reconnect.rs:106-114`, `920-928`); `Tls`/`Timeout`/`Disconnected` retry
      with backoff. A downgrade attacker forcing TLS failures must not be
      reclassified into a loop-breaking category that bricks reconnect, nor
      into a retried category for auth failures.

### 8.2 SNI / verification gaps

- [ ] **SNI on every connection, not just control.** `tls_server_name` is
      cloned into `ClientDataTransport::TcpTls` and reused per data dial
      (`client/reconnect.rs:305-312`, `client/open.rs:31-41`). Verify any new data-path constructor
      threads `server_name` through; a missing SNI on data connections is a
      finding.
- [ ] **Custom CA only where supported.** Baseline + WSS + mTLS honor
      `ca_pem` (`client/reconnect.rs:186-219`); QUIC rejects it
      (`validate_client_profile`, `endpoint.rs:58-102`, plus `start_quic_profile`,
      `client.rs:329-397`; CLI delegates via builder `validate()`,
      `main.rs:245`). Do not add a QUIC custom-CA
      knob by silently ignoring the PEM — the current fail-closed reject is
      the safe behavior until the adapter supports it
      (`docs/SECURITY.md:46-49`).
- [ ] **mTLS principal binding on data.** Server captures leaf SHA-256 at
      accept (`server/accept.rs:236-249`, `certificate_principal` `server/tls.rs:35-63`
      via `sha2`, PEM parsed with the rustls `pki-types` parser
      `pem.rs:5-22`), stores it on the session (`serve_control`
      `server/control.rs:125-135`), and `accept_data_hello` requires
      `session.principal == principal` (`server/pending.rs:52-61`). Bearer token is
      still required alongside the certificate. Any new
      transport must carry the principal through `ConnectionContext`
      (`server/control.rs:46-66`) or be rejected alongside mTLS (§6 table).
      `ClientIdentity` redacts + zeroizes (`client.rs:634-649`); `ServerConfig`
      redacts + zeroizes (`server/config.rs:29-49`).
- [ ] **Ring-provider global.** First Eggress TLS builder call installs the
      process-default provider (`docs/SECURITY.md:41-43`). Embedding tests
      that construct two different TLS stacks in one process should assert no
      provider conflict; changing `ring`/`tls12` workspace features
      (`Cargo.toml:29-30`) is a security-relevant change.

### 8.3 Proxy credential leaks

- [ ] **No credential in file, logs, or Snapshot.** Proxy URI comes from
      `outbound_proxy_env`, never TOML (`main.rs:133-137`, `226-239`);
      `Snapshot` has no proxy fields (`common.rs:136-160`); `ClientConfig`
      and `ServerConfig`/`ClientIdentity` `Debug` impls redact
      (`client/config.rs:56-66`, `client.rs:634-641`; `server/config.rs:36-49`). Run the
      `*_without_secret_leak` / `*_without_secret_in_diagnostic` tests on any
      change to error formatting (`server_tests/proxy.rs:189`, `483`,
      `692`).
- [ ] **Typed proxy errors, no fallback.** `OutboundConnectErrorKind` →
      `TunnelError` mapping (`client/reconnect.rs:332-337`) must stay total; adding a
      new Eggress error kind that hits the `_ => Disconnected` arm is fine,
      mapping it to silent direct-dial is a finding. Confirm refusal/timeout/
      cancellation tests still pass (`server_tests/proxy.rs:189`, `253`,
      `323`).
- [ ] **`__` chain parsing stays delegated.** `parse_outbound_proxy` is a thin
      wrapper over `from_pproxy_uri` (`client.rs:577-581`). Do not hand-roll
      URI splitting (userinfo `@`, IPv6 `[]`, multi-hop `__`) in Eggtunnel;
      divergence from `pproxy-compat` semantics is a finding.

### 8.4 Stream-vs-connection limit confusion

- [ ] **Name the layer.** `accepted_handshakes` (pre-auth, process-wide,
      default 64; `common.rs:232-245`, enforced `server/accept.rs:102,167`) ≠
      `active_connections_per_session` (per-session, default 128;
      `common.rs:232-245`) ≠ Eggress `1024` connection-tasks /
      `4096` stream-tasks (adapter-internal, `docs/SECURITY.md:51-53`) ≠
      `max_concurrent_streams` (QUIC transport knob, policy-derived 129 by
      default; `server/config.rs:80-83`, `client/reconnect.rs:405-406`) ≠ client
      `client_open_tasks` (default 128, `client/reconnect.rs:197`) ≠
      `control_queue` (default 128: `client/reconnect.rs:130`, `server/control.rs:145`) ≠
      `client_command_queue` (default 32: `client.rs:385,385`).
      (`MAX_SESSIONS`/`MAX_HANDSHAKES` at `server_tests.rs:35` are
      `#[cfg(test)]`-only.) Any log, metric,
      or doc that says "connection limit" must say which one.
- [ ] **Admission release paths.** Pre-auth `admission` permit + `handshake_guard`
      are dropped exactly once on auth success/failure/serve entry
      (`server/control.rs:112-113`, `1094-1095`; data-fast-path `server/accept.rs:251-252`,
      `handle_quic_data_stream` `server/accept.rs:474`). QUIC
      `stream_admission` permits are held by the spawned data task
      (`server/accept.rs:436-439`). Leaking either permit under a new early-return is
      a resource-exhaustion finding; the saturation/recovery test
      (`server_tests/quic.rs:641`) is the regression net.
- [ ] **Pre-session UDP work is outside the accepted-handshakes cap.** QUIC handshake/CPU cost
      inside `eggress-transport-quic` precedes `admission.try_acquire`
      (`docs/SECURITY.md:50-51`). Do not claim the 64-default cap bounds unauthenticated
      UDP packet processing; it bounds post-accept handshake tasks.

### 8.5 Half-close / relay mismatches

- [ ] **No half-close through WSS.** WS close is whole-connection
      (`docs/SECURITY.md:67-68`); `relay_with_options` over a WS `BoxStream`
      cannot observe TCP write-half-close. The qualified property is narrower:
      peer close terminates the underlying TCP promptly with no dangling halves
      (`docs/SECURITY.md:69-70`, test `server_tests/websocket.rs:195`). Do not add
      application framing that depends on half-close over WSS or QUIC streams
      without a new correlation test.
- [ ] **Relay bounds identical on both ends.** Both relays use
      `RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain)` (`client/open.rs:67`,
      `server/service.rs:121`). Changing one side's buffer/drain without the other
      alters backpressure behavior under the 1 MiB WS caps (§5.2); the bounded
      backpressure C001 case is the gate.
- [ ] **`DataHello`-then-opaque invariant.** `DataHello` is the last
      Eggtunnel message on a data stream (`docs/SECURITY.md:23`); after
      `accept_data_hello` both sides hand the stream to `relay_with_options`
      and never `read_boxed` again. Any post-`DataHello` framing change breaks
      relay pairing — review data-path changes with `handle_quic_data_stream`
      (`server/accept.rs:458-474`), `handle_connection` data branch
      (`server/accept.rs:248-267`), and `handle_open` (`client/open.rs:12-89`) together.

### 8.6 Quick file:line index for reviewers

| Question | Answer at |
|---|---|
| Framing semantics | `wire_io.rs:7-47`, `proto:457-504` |
| Transport neutrality | `wire_io.rs:49-57`, `lib.rs:22-33` |
| Feature gates | `crates/eggtunnel/Cargo.toml:15-23`, `Cargo.toml:22-30` |
| Builder profiles | `client/config.rs:68-156`, `server/config.rs:51-130`, `client.rs:143-217`, `server.rs:180-213` |
| TLS baseline build | `client.rs:650-659`, `server/tls.rs:23-35`, `client/reconnect.rs:269-275`, `server/accept.rs:285-331` |
| Runtime invariant | `client.rs:335-342`, `server.rs:107-112`, `lib.rs:3-5` |
| RuntimePolicy limits/timeouts | `common.rs:194-316` |
| QUIC dial/accept | `client/reconnect.rs:158-203,392-428`, `client/open.rs:59-67`, `server/config.rs:52-60,61-130` |
| QUIC limits | `common.rs:194-245`, `server/accept.rs:388-396,428-432`, `server/config.rs:79-83`, `client/reconnect.rs:401-408`, `docs/SECURITY.md:46-62` |
| WSS upgrade | `client/reconnect.rs:158-203`, `client/open.rs:42-53`, `server/accept.rs:222-284` |
| WSS close semantics | `docs/SECURITY.md:64-72`, `docs/SUPPORT.md:15-16,26-30`, `server_tests/websocket.rs:17,195` |
| Proxy dial/auth | `client.rs:577-581,325-392`, `server_tests/proxy.rs:5,91,381-...` |
| Proxy redaction | `common.rs:136-160`, `client/config.rs:56-66`, `server_tests/proxy.rs:189,483,692`, `docs/SECURITY.md:74-78` |
| Rejected combos | `endpoint.rs:58-102`, `server/config.rs:148-172`, `main.rs:198-273` |
| mTLS identity/principal | `client.rs:612-621,82-104`, `server/tls.rs:23-35,898-911,52-61,125-135`, `pem.rs:5-22` |
| CLI builder delegation | `main.rs:132-164,166-196,198-277` |
| Ownership boundary | `ADR-0001` (decision + consequences), `docs/ARCHITECTURE.md:1-21` |
