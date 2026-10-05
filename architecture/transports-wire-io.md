# Transports + Wire I/O — Deep Dive

Back to [Architecture Overview](overview.md) §5. This file is the review-oriented
deep dive for `crates/eggtunnel/src/wire_io.rs` and the feature-gated transport
stack (TCP+TLS baseline, QUIC, WebSocket/WSS, outbound proxy) built on pinned
Eggress 1.0.8 primitives.

Primary sources (line anchors are load-bearing for review):

- `crates/eggtunnel/src/wire_io.rs` (209 lines: framing `1-77`, in-file tests `79-209`)
- `crates/eggtunnel/Cargo.toml`, `Cargo.toml` (workspace)
- `crates/eggtunnel/src/client.rs`, `crates/eggtunnel/src/client/config.rs`,
  `crates/eggtunnel/src/client/open.rs`, `crates/eggtunnel/src/client/reconnect.rs`,
  `crates/eggtunnel/src/server.rs`, `crates/eggtunnel/src/server/config.rs`
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
> (`client/config.rs:75-161`, `server/config.rs:52-131`). `Client::start*` /
> `Server::bind*` are conveniences that delegate to the builders.
> Finite ceilings/timeouts come from `RuntimePolicy`
> (`common.rs:213-333`); `MAX_SESSIONS`/`MAX_HANDSHAKES` in `server_tests.rs:35-36`
> are `#[cfg(test)]`-only. QUIC `max_concurrent_streams` is policy-derived
> (server: `active_connections_per_session + 1`, `server.rs:253-261`; client:
> `client_open_tasks` verbatim, 128 by default, `client.rs:356` →
> `client/reconnect.rs:391,403,411`), `idle_timeout` is
> `policy.timeouts.control_idle` (`server.rs:252`, `client/reconnect.rs:439`), and
> both relays use `RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain)` at
> `client/open.rs:72` and `server/service.rs:124`. WSS sets 1 MiB caps via
> `WebSocketTunnel{Client,Server}::new(MAX_WEBSOCKET_FRAME_SIZE)`
> (`common.rs:15` = `MAX_FRAME_BYTES`) plus matching `tokio-tungstenite`
> `WebSocketConfig` limits.

---

## 1. `wire_io.rs`: bounded framing over any `AsyncRead/AsyncWrite`

File: `crates/eggtunnel/src/wire_io.rs:1-77` (framing), with an in-file test
module at `wire_io.rs:79-209`.

`wire_io.rs` is deliberately thin: it adapts the runtime-neutral
`eggtunnel-proto` codec (`encode_frame` / `decode_frame`,
`crates/eggtunnel-proto/src/lib.rs:565-579` and `583-635`) to Tokio I/O and to
Eggress's boxed stream type. All Eggtunnel control-plane messages (`ClientHello` …
`DataHello`) flow through these four functions. Opaque post-`DataHello` relay
bytes do **not** flow through `wire_io`; they go to
`eggress-relay::relay_with_options` (`crates/eggtunnel/src/client/open.rs:72`,
`crates/eggtunnel/src/server/service.rs:124`).

The file defines no production `AsyncRead`/`AsyncWrite` adapter. The only such
impls are the `#[cfg(test)]` doubles `FailingReader` (`wire_io.rs:86-96`) and
`FailingWriter` (`wire_io.rs:98-122`); the WebSocket and QUIC byte-stream
adapters that must preserve stream semantics live in Eggress (§5, §4.3).

### 1.1 `read_message` — header-first, length pre-check, incremental payload, exact consumption

`crates/eggtunnel/src/wire_io.rs:21-58`:

1. **Header-first read (14 bytes).**
   `reader.read_exact(&mut header)` (`wire_io.rs:25`), with failures classified by
   `io_error` (`wire_io.rs:13-19`): `ErrorKind::UnexpectedEof` becomes
   `ProtocolError::TruncatedFrame`; every other `std::io::Error` (reset,
   refused, TLS alert, broken pipe) becomes `TunnelError::Io`, whose
   `termination_category()` is `Transport` rather than `Protocol`
   (`common.rs:513`). Without this split a transport fault was laundered into a
   protocol error and skewed `last_termination` and reconnect accounting.
2. **Header-only `decode_frame` probe.** `wire_io.rs:26-33` calls
   `decode_frame(&header)` expecting one of two outcomes:
   - `Err(TruncatedFrame)` → expected; a full frame cannot fit in 14 bytes, so
     continue to the payload read.
   - Any other `Err` (`InvalidMagic`, `UnsupportedVersion`, `UnknownMessage`,
     `FrameTooLarge` from the length word) → return immediately **before**
     buffering or reading payload. This is the hostile-header fast reject.
   - `Ok(_)` → `InvalidPayload`, fail closed. The arm is reachable in principle
     if a future message type ever encodes to zero bytes, and it must not be a
     panic: a crafted `len = 0` header would otherwise kill the control or data
     task (`JoinSet` records it as `Internal`).
3. **Length pre-check before payload copy.**
   `wire_io.rs:34-37` extracts
   `u32::from_be_bytes([header[10], header[11], header[12], header[13]])` and
   rejects `len > MAX_FRAME_BYTES` (`1 MiB`,
   `crates/eggtunnel-proto/src/lib.rs:15`) with `FrameTooLarge`. This mirrors
   the identical check inside `decode_frame`
   (`crates/eggtunnel-proto/src/lib.rs:597-599`) but happens **before** any
   payload buffering, so a lying length word cannot force a large allocation.
   Note the cap is on the **postcard payload**, not the total on-wire bytes
   (`HEADER_LEN + len`).
4. **Incremental bounded payload read.** `wire_io.rs:38-49` reads the payload
   with `(&mut *reader).take(len as u64).read_to_end(&mut payload)`, so the
   buffer grows only as bytes actually arrive. A peer that announces the
   1 MiB maximum and then stalls holds what it sent instead of a pre-committed
   1 MiB allocation, and a short payload is rejected as `TruncatedFrame`.
5. **Exact-consumption check.** `wire_io.rs:50-57`:
   ```rust
   let (message, consumed) = decode_frame(&frame)?;
   if consumed != frame.len() {
       return Err(TunnelError::Protocol(ProtocolError::InvalidPayload));
   }
   ```
    `decode_frame` itself is exactly-one-frame and trailing-tolerant — it returns
   the frame it consumed as the second tuple element
   (`crates/eggtunnel-proto/src/lib.rs:600-634`, `Ok((message, total))`), and
    additionally rejects trailing bytes **inside** the postcard payload
    (`crates/eggtunnel-proto/src/lib.rs:607-616`). The
    `consumed != frame.len()` guard in `wire_io` closes the remaining gap: the
    caller passed exactly `HEADER_LEN + len` bytes, so any mismatch means the
    declared length and the decoded payload disagree → `InvalidPayload`. There
    is no concatenated-frame fast path here; each `read_message` consumes
    exactly one frame. Control loops that `split()` the stream rely on this 1:1
    property.

### 1.2 `write_message` — encode-then-`write_all`

`crates/eggtunnel/src/wire_io.rs:60-66`: `encode_frame(message)?` (which itself
enforces `payload.len() > MAX_FRAME_BYTES → FrameTooLarge`,
`crates/eggtunnel-proto/src/lib.rs:567-569`), then `write_all(&frame)` with the
same `io_error` classification: a broken pipe is `TunnelError::Io` →
`TerminationCategory::Transport`, never a protocol truncation. There is no
partial-frame resume; failure tears down the session or data stream. Call sites
that must not block forever wrap this in their own budget (server
`write_bounded`, `server/control.rs:41-49`; client `write_control`,
`client.rs:1173-1181`, and `handshake_write`, `client.rs:1192-1200`).

### 1.3 Error mapping table

| Site | Input condition | Result |
|---|---|---|
| `wire_io.rs:25` + `io_error` | `< HEADER_LEN` bytes available then EOF | `Protocol(TruncatedFrame)` |
| `wire_io.rs:25` + `io_error` | header read reset / refused / TLS alert | `Io(..)` → `TerminationCategory::Transport` |
| `wire_io.rs:26-33` probe | header-only slice | expected `TruncatedFrame` → continue; any other decode error → return it; `Ok` → `InvalidPayload` |
| `wire_io.rs:34-37` | declared `len > 1 MiB` | `FrameTooLarge` (not `TruncatedFrame`) |
| `wire_io.rs:38-49` | header OK but payload short | `Protocol(TruncatedFrame)` |
| `wire_io.rs:50-57` | `consumed != frame.len()` | `InvalidPayload` |
| `wire_io.rs:60-66` | `write_all` failure | `Io(..)` → `TerminationCategory::Transport` |

Review note: `TruncatedFrame` means two wire realities (clean peer close and
network truncation); a mid-handshake timeout is collapsed by the call site
(which knows whether a `timeout()` fired) into `TunnelError::Timeout`.

### 1.4 Why `BoxStream` matters — transport neutrality

`crates/eggtunnel/src/wire_io.rs:1,68-77`:

```rust
use eggress_core::BoxStream;
pub(crate) async fn read_boxed(stream: &mut BoxStream) -> Result<Message, TunnelError>
pub(crate) async fn write_boxed(stream: &mut BoxStream, message: &Message) -> Result<(), TunnelError>
```

`read_boxed` / `write_boxed` are one-line delegates to `read_message` /
`write_message`, but the type is the point: `BoxStream` (from
`eggress-core = "=1.0.8"`, `Cargo.toml:23`) is the **single control-plane I/O
type** across all transports. Every handshake path converges on it:

- Baseline / mTLS: `Box::new(tcp)` → `tls_accept` / `tls_connect` returns a
  `BoxStream` (`server/accept.rs:317-365`, `client/reconnect.rs:299-304`;
  `connect_tcp` boxes the direct TCP stream at `client/reconnect.rs:379`).
- WebSocket: TLS `BoxStream` → `WebSocketTunnelServer/Client` adapter returns a
  `BoxStream` (`server/accept.rs:368-397`, `client/reconnect.rs:306-334`,
  `client/open.rs:46-57`).
- QUIC: `QuicConnection::accept_stream` / `open_stream` returns a `BoxStream`
  directly (`server/accept.rs:412-415` and `462-463`, `client/reconnect.rs:455-459`,
  `client/open.rs:62-68`).
- Outbound proxy: `OutboundConnector::connect_tcp_timeout_detailed` returns a
  `BoxStream` that is then fed into `tls_connect`
  (`client/reconnect.rs:352-380`).

Consequences for review:

- Session logic (`run_session`, `client.rs:697`; `serve_control`,
  `server/control.rs:82`; `accept_data_hello`, `server/pending.rs:29`) never
  branches on socket type; transport selection ends before the first
  `read_boxed`. The `websocket: bool` flag and the private
  `ClientDataTransport` enum (`client.rs:42-54`) are construction-time only, as
  are the private `Transport` trait and its two adapters
  (`client/reconnect.rs:45-52`, `StreamTransport` at `248-350`, `QuicTransport`
  at `385-475`).
- `tokio::io::split(stream)` on a `BoxStream`
  (`client.rs:818`, `server/control.rs:170`) requires `BoxStream: AsyncRead +
  AsyncWrite`; the WebSocket and QUIC adapters must therefore preserve
  byte-stream semantics (see §5 on the half-close caveat — byte-stream does
  **not** imply half-close equivalence).
- Public API leakage is contained: `lib.rs:19-35` re-exports
  `Client/Server/Config/Handle`, builders, profiles, and `proto`, never
  `BoxStream`, `rustls`, `quinn`, or `tungstenite` types (per
  `ADR-0001` public-API consequence,
  `plans/adrs/ADR-0001-session-transport-and-egress-boundary.md:151`).

### 1.5 In-file framing tests (`wire_io.rs:79-209`)

Five `#[tokio::test]`s pin the behavior above; they are the regression net for
any framing edit:

- `transport_io_failures_are_not_reported_as_protocol_truncation`
  (`wire_io.rs:125`) — `FailingReader` (ConnectionReset) and `FailingWriter`
  (BrokenPipe) both yield `TunnelError::Io`, and `Io` maps to
  `TerminationCategory::Transport`.
- `a_peer_that_closes_mid_frame_is_a_truncation` (`wire_io.rs:142`) — a frame
  cut one byte short, and an empty reader, both yield
  `Protocol(TruncatedFrame)`.
- `a_zero_length_payload_fails_closed_instead_of_panicking`
  (`wire_io.rs:157`) — for every known message type, a hand-built header with
  `len = 0` fails closed (`InvalidPayload`/`UnknownMessage`) instead of
  panicking the control or data task.
- `an_oversized_announced_length_is_rejected_before_buffering`
  (`wire_io.rs:180`) — announced `MAX_FRAME_BYTES + 1` with no payload bytes
  yields `FrameTooLarge`, proving the pre-check precedes allocation.
- `consecutive_frames_decode_one_at_a_time` (`wire_io.rs:194`) — three
  concatenated `Ping` frames decode one per `read_message`, pinning the 1:1
  consumption property that `split()`-based control loops depend on.

---

## 2. Feature matrix: what each flag pulls in and gates

Crate features: `crates/eggtunnel/Cargo.toml:15-32`. Workspace pins:
`Cargo.toml:13-36`.

Transport selection is via Builder profiles, not direct constructors:
`ClientTransportProfile::{TcpTls, Quic, WebSocket}`
(`client/config.rs:75-81`) consumed by `Client::start_profile`
(`client.rs:142-216`), and `ServerTransportProfile::{TcpTls, Quic, WebSocket}`
(`server/config.rs:52-58`) consumed by `ServerBuilder::bind`
(`server/config.rs:119-130`) → `Server::bind_profile` (`server.rs:204-237`)
→ `bind_with_tls_profile` (`server.rs:289-323`) or `bind_quic_profile`
(`server.rs:240-287`). `ClientDataTransport` (`client.rs:42-54`) is a private
enum handed to `drive`; `start_quic_profile` (`client.rs:324-389`) and
`start_with_tls_config` (`client.rs:436-500`) are private late-stage workers
reached only after `validate_client_profile` (`client.rs:150-158`) /
`validate_server_profile` (`server/config.rs:108-117`).

| Feature | Default? | Pulls in (crate deps) | Gates in code |
|---|---|---|---|
| `client` | ✅ (`default = ["client","tls"]`, `crates/eggtunnel/Cargo.toml:16`) | `tokio`, `tokio-util`, `eggress-core`, `eggress-transport-tls`, `eggress-relay`, `getrandom`, `rustls`, `tracing`, + `tls` (`:17`) | `mod client` (`lib.rs:14-15`); `Client`, `ClientBuilder`, `ClientTransportProfile` (`client/config.rs:75-161`), `TargetConnector`, all `Client::start*` conveniences (`client.rs:218-321`) |
| `tls` | ✅ | `eggress-transport-tls` (`crates/eggtunnel/Cargo.toml:19`) | Baseline path; `TlsClientConfigBuilder` / `TlsServerConfigBuilder` (`client.rs:5,649-657`; `server/tls.rs:9,23-33`), `tls_connect` / `tls_accept` |
| `server` | ❌ | `tokio`, `tokio-util`, `eggress-core`, `eggress-transport-tls`, `eggress-relay`, `rustls`, `tracing`, + `tls` (`:18`) | `mod server` (`lib.rs:16-17`); `Server`, `ServerBuilder`, `ServerTransportProfile` (`server/config.rs:52-131`), private `ServerTls` enum (`server/tls.rs:16-21`), all `Server::bind*` conveniences (`server.rs:64-202`) |
| `quic` (+ role slices `quic-client` / `quic-server`, `crates/eggtunnel/Cargo.toml:23-26`) | ❌ | umbrella `quic` = both roles + `quic-transport` (`dep:eggress-transport-quic`); role slices isolate client/server builds | `ClientTransportProfile::Quic` (`client/config.rs:77-78`); `start_profile` Quic arm (`client.rs:192-195`) → `start_quic_profile` (`client.rs:324-389`), `drive` + `QuicTransport` (`client/reconnect.rs:171-222,385-475`); `ServerTransportProfile::Quic` (`server/config.rs:54-55`); `Server::bind_quic[_with_policy]` (`server.rs:86-104`), `bind_quic_profile` (`server.rs:240-287`), `quic_server_loop*` (`server/accept.rs:169-237`), `handle_quic_connection` (`server/accept.rs:401-493`), `handle_quic_data_stream` (`server/accept.rs:496-516`); QUIC stream budget via `max_active_data_streams` (`server/accept.rs:176-177,430`) and client `QuicTransport` (`client/reconnect.rs:385-475`) |
| `websocket` (+ role slices `websocket-client` / `websocket-server`, `crates/eggtunnel/Cargo.toml:27-30`) | ❌ | umbrella `websocket` = both roles + `websocket-transport` (`dep:eggress-protocol-websocket`, `dep:tokio-tungstenite`); role slices isolate client/server builds | `ClientTransportProfile::WebSocket` (`client/config.rs:79-80`); `start_profile` WebSocket arm (`client.rs:196-214`) → `start_with_tls_config(..., websocket: true)` (`client.rs:436-500`), `StreamTransport` upgrade (`client/reconnect.rs:306-334`), `handle_open` data-path upgrade (`client/open.rs:46-57`); server `ServerTransportProfile::WebSocket` (`server/config.rs:56-57`) → `bind_profile` WebSocket arm (`server.rs:227-231`), `handle_connection` upgrade (`server/accept.rs:266`); `Server::bind_websocket` (`server.rs:79-84`), `Client::start_websocket*` (`client.rs:234-251`) |
| `outbound-proxy` | ❌ | `client` + `eggress-outbound` with `pproxy-compat` (`crates/eggtunnel/Cargo.toml:31`, `Cargo.toml:28`) | `outbound: Option<Arc<OutboundConnector>>` on the `#[cfg]`-gated field of private `ClientDataTransport::TcpTls` (`client.rs:49-50`), on `StreamTransport` (`client/reconnect.rs:255-256,278-279,342-343`), and on `start_with_tls_config` (`client.rs:441-443`); `ClientBuilder::outbound_proxy` (`client/config.rs:130-134`); `parse_outbound_proxy` via `OutboundConnector::from_pproxy_uri` (`client.rs:521-527`); `connect_tcp` proxy branch (`client/reconnect.rs:357-371`); `validate_outbound_proxy` re-export (`lib.rs:21-22`); `start_with_outbound_proxy*`, `start_websocket_with_outbound_proxy*` builder conveniences (`client.rs:253-301`) |
| `mtls` | ❌ | `tls` + `tokio-rustls`, `webpki-roots`, `sha2` (`crates/eggtunnel/Cargo.toml:32`) | `mod pem` (`lib.rs:9-10`); `ServerTls::Mutual` (`server/tls.rs:19-20`); `Server::bind_mtls*` (`server.rs:175-202`), `build_mtls_server_config` (`server/tls.rs:35-58`), `certificate_principal` SHA-256 (`server/tls.rs:63-67`); `Client::start_with_mtls*` builder conveniences (`client.rs:412-434`), `ClientIdentity` + redacted `Debug` + `zeroize` drop (`client.rs:529-561`), `build_mtls_tls_config` (`client.rs:659-682`), `ClientBuilder::with_identity` (`client/config.rs:124-128`) |

Notes:

- `client` implies `tls` (`crates/eggtunnel/Cargo.toml:17`: `client = [... "tls"]`).
  Role slices `quic-client` / `quic-server` and `websocket-client` /
  `websocket-server` exist precisely for single-role builds: with
  `--no-default-features`, `quic-client` resolves to `client` + `quic-transport` +
  `tls` and pulls only `eggress-transport-quic`; `quic-server` to `server` +
  `quic-transport` + `tls`; `websocket-client` to `client` + `websocket-transport` +
  `tls`; `websocket-server` to `server` + `websocket-transport` + `tls`. None
  enables the opposite role, `quic`/`websocket` umbrella, `outbound-proxy`, or
  `mtls` (verified with `cargo tree --locked -p eggtunnel
  --no-default-features --features <slice> -e features -i eggtunnel`, mirroring
  `.github/workflows/ci.yml:62-81`). Transport
  profile validation lives in `validate_client_profile`
  (`client.rs:569-612`) and `validate_server_profile`
  (`server/config.rs:149-179`), not in `endpoint.rs` (which owns only
  `Endpoint::parse` at `endpoint.rs:42-86` plus `websocket_url` at `103-106`);
  the CLI enforces the same coupling at config-check time (§6).
- `mtls` deliberately composes with neither role: it pulls `tls` but not
  `client`/`server`, so an `mtls`-only build has `mod pem` and the TLS adapter
  dep but no socket layer and no `mod wire_io` (`lib.rs:11-12` needs `client` or
  `server`). Both mTLS convenience APIs sit behind the role features that reach
  them. A `proto`-only build has no socket dependency, per
  `docs/ARCHITECTURE.md:3-6`.
- `mod wire_io` exists iff `client` or `server` is enabled
  (`lib.rs:11-12`).
- Dev-only: `crates/eggtunnel/Cargo.toml:57` enables
  `eggress-transport-quic/insecure-quic` for tests. That feature must never
  leak into non-test builds; production QUIC goes through
  `QuicClientConfig { insecure: false }` (`insecure` is the `false` literal
  threaded from `start_profile`, `client.rs:194`, into
  `QuicClientConfig { insecure: self.insecure, .. }`,
  `client/reconnect.rs:436-442`). The insecure
  path is reachable only via `start_quic_insecure_for_test`
  (`client.rs:391-410`).
- Workspace pins every Eggress crate to exactly `=1.0.8`
  (`Cargo.toml:23-28`; `Cargo.lock` confirms `eggress-core/-relay/
  -transport-tls/-transport-quic/-protocol-websocket/-outbound 1.0.8`).
  `tokio-tungstenite 0.26.2` (`Cargo.toml:29`), `rustls 0.23` (`ring`, `std`,
  `tls12`) (`Cargo.toml:30`), `tokio-rustls 0.26` (`ring`, `tls12`)
  (`Cargo.toml:31`) are also pinned in the workspace.

---

## 3. Baseline TCP+TLS: Eggress builders, ring provider, TLS 1.2+, caller runtime

### 3.1 Construction

- Server: `Server::bind` (`server.rs:64-66`) → `ServerBuilder::bind`
  (`server/config.rs:119-130`) → `Server::bind_profile` (`server.rs:204-237`)
  builds either
  `ServerTls::Mutual(build_mtls_server_config(...))` or
  `ServerTls::Eggress(build_server_tls(...))` (`server.rs:216-222`) and hands it
  to `bind_with_tls_profile` (`server.rs:225,289-323`), which binds
  `TcpListener::bind(config.listen_addr)` (`server.rs:299`).
  `build_server_tls` = `TlsServerConfigBuilder::new().with_certificate_pem(...).with_key_pem(...).build()`,
  `server/tls.rs:23-33`. Every accepted `TcpStream` is boxed then passed to
  `eggress_transport_tls::tls_accept(stream, tls)` under
  `policy.timeouts.handshake` (`server/accept.rs:326-331` on the mTLS-enabled
  build, `358-363` otherwise).
- Client: `Client::start` → `ClientBuilder::start` → `Client::start_profile`
  (`client.rs:218-220`, `client/config.rs:148-160`, `client.rs:142-216`):
  `build_tls_config` (`client.rs:649-657`):
  `TlsClientConfigBuilder::new().with_system_roots()` or
  `.with_custom_ca_pem(pem)`, then `.build()`. The TCP dial happens in
  `StreamTransport::establish` (`client/reconnect.rs:293-347`):
  `connect_tcp` under `policy.timeouts.connect` (`client/reconnect.rs:295-298`,
  `352-380`), then `tls_connect(stream, tls, &server_name)`
  under `policy.timeouts.handshake` (`client/reconnect.rs:299-304`). The data
  path repeats the same two steps per `Open` (`client/open.rs:37-45`).
- Validation before any socket: `validate_client_profile` (`client.rs:569-612`)
  runs `RuntimePolicy::validate`, then `validate_config` (`client.rs:614-647`):
  endpoint shape via `Endpoint::parse` (`endpoint.rs:42-86`), non-empty ≤253 B
  server name (`client.rs:620-622`), service count against
  `policy.limits.services_per_session` with unique IDs/names
  (`client.rs:623-636`), CA size cap (`client.rs:637-645`), then the transport
  rejections (`client.rs:587-610`); server checks
  `runtime_policy.validate()` + `bind_policy.validate()` + cert/key presence and
  size cap via `validate_server_profile` (`server/config.rs:149-179`) and
  `validate_config` (`server/config.rs:133-147`), plus
  `BindPolicy::validate` (`common.rs:116-130`).

### 3.2 TLS properties relevant to review

- **Builders are Eggress-owned.** `TlsClientConfigBuilder` /
  `TlsServerConfigBuilder` come from `eggress-transport-tls 1.0.8`. Eggtunnel
  never constructs `rustls::{Client,Server}Config` directly on this path
  (contrast the mTLS path, `client.rs:659-682` and `server/tls.rs:35-58`, which
  drops to `rustls` + `tokio-rustls` directly; PEM decoding uses the Rustls
  pki-types parser).
- **Ring provider.** Per `docs/SECURITY.md:61-63`, the Eggress 1.0.8 builders
  "install the Rustls ring provider as the process default if no provider has
  been set". Workspace features corroborate: `rustls` with `ring/std/tls12`,
  `tokio-rustls` with `ring/tls12` (`Cargo.toml:30-31`). Review implication:
  first TLS build in the process wins the global provider; embedding two TLS
  stacks with different providers in one process can surprise. Eggtunnel itself
  "does not install a runtime or tracing subscriber" (`docs/SECURITY.md:63`,
  `lib.rs:2-5`, roadmap §2 Runtime invariants,
  `plans/subsystems/reverse-session-roadmap.md:85-93`).
- **TLS 1.2+.** The `tls12` feature on both `rustls` and `tokio-rustls`
  preserves TLS 1.2 floor alongside 1.3. There is no Eggtunnel-side
  version/cipher allowlist; that policy is delegated to Eggress + rustls
  defaults. Any future requirement to pin 1.3-only must be expressed as a
  builder option or workspace feature change, not a `wire_io` change.
- **SNI + verification.** Client passes `server_name` (validated
  non-empty, ≤253 B) as the SNI/verification name on every control **and**
  data connection (`client/reconnect.rs:301`, `client/open.rs:44`; the same
  `server_name` is cloned into `ClientDataTransport::TcpTls`,
  `client/reconnect.rs:337-344`). Server-name verification uses
  system roots by default or the explicit `ca_pem` bundle; `ClientConfig`'s
  `Debug` redacts the token and collapses `ca_pem` to `[configured]`
  (`client/config.rs:62-72`). Auth (`Auth` bearer token) always runs **inside**
  the verified channel (`docs/SECURITY.md:3-6`).

### 3.3 Caller-owned Tokio runtime invariant

Every public entrypoint asserts a caller-owned runtime before doing I/O:

- `Client::start_with_tls_config` (`client.rs:446-448`),
  `Client::start_quic_profile` (`client.rs:330-332`);
- `Server::bind_with_tls_profile` (`server.rs:296`),
  `Server::bind_quic_with_admission_for_test` (`server.rs:113`, test-only).

Both go through one predicate: `tokio::runtime::Handle::try_current`, wrapped on
the server side as `require_caller_runtime` (`server.rs:348-353`).

`Server::bind_quic_profile` (`server.rs:240-287`) performs no separate
`try_current` gate; it runs inside the caller's `bind().await` future, which
reaches the runtime-gated `bind_with_tls_profile` only on the TCP/WSS path.

Failure is `TunnelError::Configuration("... requires a caller-owned Tokio
runtime")` → `TerminationCategory::Internal` (`common.rs:514`). The library
never calls `#[tokio::main]`, `Runtime::new`, or installs a global tracing
subscriber (`lib.rs:2-5`, roadmap §2 runtime invariants).

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

Control uses `read_boxed`/`write_boxed` throughout (`server/control.rs:82-171`
for the server side, `client.rs:697-828` for the client side; both `split()` the
stream at `server/control.rs:170` and `client.rs:818`). Version check is
major-only and fail-closed: `server/control.rs:98-106` rejects
`hello.version.major != ProtocolVersion::CURRENT.major`, and the client accepts
`ServerHello` only on a major match (`client.rs:723-735`). Data connections
branch on the first message in `handle_connection` (`server/accept.rs:270-313`):
`DataHello` → relay, `ClientHello` → control session, anything else →
`UnexpectedMessage`.

---

## 4. QUIC: UDP control endpoint, one bidi stream per path

Feature: `quic` (`crates/eggtunnel/Cargo.toml:23-26`). Docs:
`docs/SUPPORT.md:22`, `docs/SECURITY.md:65-81`, `docs/ARCHITECTURE.md:12-13`.

### 4.1 What changes vs baseline

| Aspect | TCP+TLS baseline | QUIC profile |
|---|---|---|
| Listener | `TcpListener::bind` (`server.rs:299`) | `QuicListener::bind(config.listen_addr, QuicServerConfig { certificate_pem, private_key_pem, idle_timeout: policy.timeouts.control_idle, max_concurrent_streams: policy.limits.active_connections_per_session + 1, alpn_protocols: [] })` (`server.rs:247-266`) |
| Control transport | one TLS-over-TCP connection | one UDP QUIC connection per session on `listen_addr`; first inbound bidi stream is the control stream (`server/accept.rs:412-427`) |
| Data transport | one TCP+TLS connection per external conn | one bidi stream per external conn on the **same** QUIC connection (`server/accept.rs:462-476`, `client/open.rs:62-68`) |
| Service listeners | TCP (`TcpListener` accept in `run_service`, `server/service.rs:66-79`) | **retained as TCP** — "Service listeners remain TCP even when the control Session uses QUIC over UDP" (`docs/SUPPORT.md:28-29`) |
| Trust | system roots or custom CA | **platform roots only, bearer only** — custom CA / mTLS / proxy rejected, not ignored (`client.rs:334-338`, `client.rs:587-594`) |
| Client entry | `ClientBuilder` + `ClientTransportProfile::TcpTls` | `ClientBuilder.transport(ClientTransportProfile::Quic)` → `start_profile` → `start_quic_profile` (`client.rs:142-216`, `324-389`); conveniences `Client::start_quic[_with_connector]` (`client.rs:303-321`) |
| Server entry | `ServerBuilder` + `ServerTransportProfile::TcpTls` | `ServerBuilder.transport(ServerTransportProfile::Quic)` → `bind` → `bind_profile` → `bind_quic_profile` (`server/config.rs:119-130`, `server.rs:232-235,240-287`); conveniences `Server::bind_quic[_with_policy]` (`server.rs:86-104`) |

### 4.2 Handshake sequence (QUIC)

```text
client (QuicClient)                       server (QuicListener, UDP)
→  | QuicClient::connect(host, port, QuicClientConfig{server_name, insecure:false, idle = policy.timeouts.control_idle, max_streams = policy.limits.client_open_tasks}) [policy.timeouts.connect]
→  |--------------------------------------->| accept_connection(&cancel)
→  | get_connection() [connect] → open_stream() [connect] (control)
→  |--------------------------------------->| accept_stream() [handshake] → read_boxed → expect ClientHello
→  | ... same Session handshake as baseline (ServerHello/Auth/AuthOk/Register*) over control stream ... |
→  |                                        | external TCP accept → PendingEntry → Open over control stream
→  | connection.open_stream() [connect] (data)  |
→  |--------------------------------------->| accept_stream() → read_boxed → expect DataHello
→  | DataHello(session, service, connection) | accept_data_hello (wrong-session/stale/replay → Auth error)
→  |--------------------------------------->| relay_with_options (opaque bytes over bidi stream)
```

Key call sites: client `QuicTransport::establish`
(`client/reconnect.rs:424-467`): config build with `insecure` from the profile
and `max_concurrent_streams` from the passed budget (`428-444`), connect +
`get_connection` + control `open_stream` inside one `policy.timeouts.connect`
budget (`432-463`), returning `(control, ClientDataTransport::Quic(connection))`
(`465`). `drive` then runs the shared `run_session` with that data transport
(`client/reconnect.rs:195-212`). Data dial is `connection.open_stream()` under
`policy.timeouts.connect` (`client/open.rs:62-68`) followed by `DataHello` +
relay (`client/open.rs:70-72`).

Server `quic_server_loop[_with_admission]` (`server/accept.rs:169-237`) mirrors
`server_loop` admission accounting, then `handle_quic_connection`
(`server/accept.rs:401-493`): accept first stream with
`policy.timeouts.handshake` (`412-415`), `read_boxed` expecting `ClientHello`
(`416-427`), spawn `serve_control` (`432-445`), then loop `accept_stream` for
data streams (`462-476`) each handled by `handle_quic_data_stream`
(`server/accept.rs:496-516`), which expects `DataHello` under
`policy.timeouts.handshake` and delegates to the shared `accept_data_hello`.

### 4.3 Limits and their interaction (read carefully — three layers)

All Eggtunnel-side ceilings come from `RuntimePolicy`
(`common.rs:213-333`). `ResourceLimits` is an 8-field struct
(`common.rs:213-222`: `sessions`, `services_per_session`,
`pending_per_session`, `active_connections_per_session`,
`accepted_handshakes`, `client_open_tasks`, `control_queue`,
`client_command_queue`; each `1..=65536`, `common.rs:225-246`) with defaults
`128/64/128/128/64/128/128/32` (`common.rs:249-262`). `TimeoutPolicy`
(`common.rs:267-277`) defaults to `connect/handshake 10 s`, `control_idle 90 s`,
`pending_connection 30 s`, `relay_drain 15 s`, `shutdown_grace 1 s`,
`reconnect_initial 500 ms`, `reconnect_max 30 s`, `heartbeat_interval 20 s`
(`common.rs:305-319`), with validation (`common.rs:280-302`: nonzero, ≤24 h,
`reconnect_initial ≤ reconnect_max`, `heartbeat_interval < control_idle`).

1. **Eggress QUIC adapter (per-connection/task fan-out).**
   `docs/SECURITY.md:71-73`: `MAX_CONCURRENT_CONNECTION_TASKS=1024` and
   `MAX_CONCURRENT_STREAM_TASKS=4096`. These bound Eggress-internal task
   spawning per connection/stream **before** Eggtunnel authentication. They are
   not Eggtunnel constants; no Eggtunnel source defines them.
2. **Eggtunnel pre-session admission (`accepted_handshakes`, default 64).**
   `Semaphore::new(counters.policy.limits.accepted_handshakes)` in
   `AcceptContext::new` (`server/accept.rs:49`), taken by `admit()`
   (`server/accept.rs:66-75`) from both `server_loop`
   (`server/accept.rs:133`) and `quic_server_loop_with_admission`
   (`server/accept.rs:204-207`), plus a `HandshakeGuard` active-handshake
   counter (`server/session.rs:140-163`). (`MAX_SESSIONS`
   / `MAX_HANDSHAKES` at `server_tests.rs:35-36` are `#[cfg(test)]`-only and play
   no role in production.) On exhaustion: the TCP path drops the accept after
   `record_saturation`; the QUIC path additionally
   `connection.close("handshake limit reached")` (`server/accept.rs:204-207`).
   Per `docs/SECURITY.md:69-75`, "pre-session UDP/TLS handshake work runs
   inside the adapter before Eggtunnel's semaphore is acquired", so the
   residual pre-auth admission risk is documented as observable, not
   eliminated — no vendoring required.
3. **Per-session stream admission (default 128).**
   `counters.policy.limits.active_connections_per_session` (default 128),
   threaded as `max_active_data_streams` from `quic_server_loop`
   (`server/accept.rs:176-177`) and instantiated per QUIC connection as
   `Semaphore::new(max_active_data_streams)` (`server/accept.rs:430`).
   Each accepted data stream does `try_acquire_owned`; on failure:
   `rejected+1` + `ResourceExhausted`, stream dropped without a response
   (`server/accept.rs:464-468`). The semaphore saturates, recovers on stream close
   (permit is held by the spawned task, `server/accept.rs:472-475`), and rejects
   beyond the policy limit — qualified by the stream-saturation test
   (`docs/SECURITY.md:76-79`; test-only override
   `bind_quic_with_admission_for_test`, `server.rs:107-149`, and the
   saturation test `quic_stream_saturation_recovers_capacity_and_keeps_unrelated_streams_alive`,
   `server_tests/quic.rs:715`).

Interaction mental model for review: Eggress 1024/4096 caps adapter-internal
fan-out; Eggtunnel `accepted_handshakes` (default 64) caps
**unauthenticated handshakes process-wide**
(both TCP and QUIC); `active_connections_per_session` (default 128) caps
**active data streams/connections per session** (QUIC `stream_admission`,
TCP `connection_admission` `server/control.rs:151-153`, `server/service.rs:81`,
client open-task semaphore
`policy.limits.client_open_tasks` at `client.rs:816`) ≠
`control_queue` (`policy.limits.control_queue`: `client.rs:817`,
`server/control.rs:171`) ≠ `client_command_queue`
(`policy.limits.client_command_queue`: `client.rs:343`, `454`; correlated
registration in-flight ceiling derived from it at `client.rs:756`). Any log,
metric, or doc that says "connection limit" must say which one. Do not conflate
"connection" (QUIC UDP 4-tuple ≈ session transport) with "stream" (one
external TCP connection) or with "handshake task" (pre-auth work unit).

Additional QUIC specifics:

- The transport knob is asymmetric on purpose.
  `QuicServerConfig { idle_timeout: policy.timeouts.control_idle,
  max_concurrent_streams: u32::try_from(limits.active_connections_per_session
  .saturating_add(1))? }` (`server.rs:249-263`) — 129 by default, i.e. the
  128 data streams plus exactly one control stream. The client mirrors
  `idle_timeout: policy.timeouts.control_idle, max_concurrent_streams:
  policy.limits.client_open_tasks` (`client.rs:356` →
  `client/reconnect.rs:391,403,411` → `428-440`) — 128 by default, the budget
  passed verbatim, so the server's `+ 1` is what leaves room for the control
  stream and Eggtunnel admission (`stream_admission`, layer 3) still rejects
  first.
- Test-only admission override: `bind_quic_with_admission_for_test` sets
  `max_concurrent_streams = u32::try_from(n.max(1)).saturating_mul(2)`
  (`server.rs:142-146`) — test-only, `#[cfg(all(test, feature = "quic-server"))]`.
- Bearer token still required inside the encrypted control stream
  (`docs/SECURITY.md:68`); QUIC provides confidentiality + SNI verification,
  not authentication. Platform roots + verified SNI; no custom-root or
  client-cert knobs exist on the adapter, hence fail-closed rejects (§6).
- Session/stream correlation (wrong-session, stale, replay, half-close) is
  qualified end-to-end via C001 (`docs/SECURITY.md:79-81`,
  `docs/SUPPORT.md:22`).

---

## 5. WebSocket (WSS): verified-TLS-then-upgrade, binary 1 MiB, whole-connection close

Feature: `websocket` (`crates/eggtunnel/Cargo.toml:27-30`). Docs:
`docs/SUPPORT.md:23`, `docs/SECURITY.md:83-91`, `docs/ARCHITECTURE.md:13-14`.

### 5.1 Construction

- Client control: after `tls_connect` succeeds, if `websocket == true`,
  `WebSocketTunnelClient::new(MAX_WEBSOCKET_FRAME_SIZE).connect_over_stream_with_config(
  &url, stream, ws_config)` under `policy.timeouts.handshake`, where
  `url = endpoint.websocket_url()` (`client/reconnect.rs:306-334`, with the
  1 MiB `WebSocketConfig` at `310-313`). Failure maps to `Timeout` (outer) or
  `Tls` (inner) (`client/reconnect.rs:320-323`). A build without
  `websocket-client` fails closed with
  `Configuration("WebSocket transport is not enabled in this build")`
  (`client/reconnect.rs:325-331`). Data connections repeat the identical upgrade
  per `Open` (`client/open.rs:46-57`).
- Server: after `accept_tls` (or mTLS accept — but WSS+mTLS is rejected in
  `validate_server_profile` before this point, §6), `handle_connection` calls
  `upgrade_websocket` (`server/accept.rs:266`), which on
  `websocket == true` runs
  `WebSocketTunnelServer::new(MAX_WEBSOCKET_FRAME_SIZE)
  .accept_upgrade_with_config_over_stream(stream, ws_config)`
  under `policy.timeouts.handshake` with the same 1 MiB caps
  (`server/accept.rs:368-387`, caps at `376-382`). Upgrade failure maps to
  `Timeout` (outer) or
  `Protocol(UnexpectedMessage)` (inner) — note asymmetry with the client side
  (§8.1). A build without `websocket-server` returns the TLS stream unchanged
  (`server/accept.rs:389-397`), which is unreachable because the profile variant
  itself is feature-gated.
- Entry points: `ClientBuilder.transport(ClientTransportProfile::WebSocket)` with
  conveniences `Client::start_websocket[_with_connector]`
  (`client.rs:234-251`), `ServerBuilder.transport(ServerTransportProfile::WebSocket)`
  with convenience `Server::bind_websocket` (`server.rs:79-84`), CLI
  `transport = "websocket_tls"` mapped to profiles by `client_transport` /
  `server_transport` and applied in `client_builder` / `server_builder`
  (`crates/eggtunnel-cli/src/main.rs:495-518`, `721-760`).

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
  (`docs/SECURITY.md:83-84`, `docs/SUPPORT.md:23`).
- **Binary messages, 1 MiB cap.** `MAX_WEBSOCKET_FRAME_SIZE` is
  `eggtunnel_proto::MAX_FRAME_BYTES` (`common.rs:15`), and both
  `WebSocketTunnel{Client,Server}::new(MAX_WEBSOCKET_FRAME_SIZE)` and the
  `tokio-tungstenite` `WebSocketConfig`
  (`max_message_size = Some(1 MiB)`, `max_frame_size = Some(1 MiB)`) agree on
  1 MiB (`client/reconnect.rs:310-318`, `client/open.rs:49-52`,
  `server/accept.rs:376-382`).
  Multi-frame bounded backpressure round-trips within the caps per C001
  (`docs/SECURITY.md:90-91`).
- **Non-browser endpoint.** "Intended for non-browser tunnel clients; the
  adapter does not validate Origin and makes no browser cross-site security
  claim" (`docs/SECURITY.md:84-86`). Do not review this as a browser WebSocket
  server; there is no Origin allowlist to audit because none is claimed.
- **Close = whole-connection close.** "Its close operation closes the
  WebSocket connection as a whole, so TCP half-close equivalence is not
  promised" (`docs/SECURITY.md:86-88`; `docs/SUPPORT.md:31-32`, `41-45`).
  There is no write-half-close signal across the WS byte stream. C001 confirms
  the narrower property that actually holds: peer close during active relay
  terminates the underlying TCP connection promptly without dangling relay
  halves (test `wss_peer_close_during_active_relay_terminates_cleanly`,
  `server_tests/websocket.rs:253`; session/data round-trip
  `websocket_tls_session_registers_and_relays_data_paths`,
  `server_tests/websocket.rs:75`). Application code must not depend on observing a
  TCP-style `shutdown(Write)` through WSS.
- **Session semantics unchanged.** QUIC/WS "do not change Service,
  authorization, TargetConnector, or ConnectionId semantics"
  (`docs/SUPPORT.md:26-27`).
- **Support-table caveat.** `docs/SUPPORT.md:64-68` documents that crates
  `eggtunnel-proto` and `eggtunnel` are published at `0.2.0`, matching the
  workspace `version = "0.2.0"` (`Cargo.toml:6`). The wire version is
  independent of the crate version (major 1, minor 1 advertised with capability
  intersection, `docs/SUPPORT.md:12-15`).

---

## 6. Outbound proxy (client-only): CONNECT/SOCKS5 + `__` chains, env credentials

Feature: `outbound-proxy` (`crates/eggtunnel/Cargo.toml:31`, workspace
`eggress-outbound = "=1.0.8"` with `pproxy-compat`, `Cargo.toml:28`). Docs:
`docs/SUPPORT.md:24`, `docs/SECURITY.md:93-104`, `docs/ARCHITECTURE.md:14-20`.

### 6.1 Shape

- **Client-only, listener-free.** `OutboundConnector` dials each client
  connection **before** Eggtunnel TLS; it "does not start a local proxy
  listener or change the Session protocol" (`docs/ARCHITECTURE.md:14-16`).
  Server + proxy is rejected at CLI check
  (`crates/eggtunnel-cli/src/main.rs:668-673`: "outbound_proxy is only valid
  in client mode"). There is no `Server::bind_*_with_proxy`, and no
  `ServerBuilder` proxy method.
- **Profiles.** Direct, HTTP CONNECT, SOCKS5 single-hop, plus multi-hop chains
  through the canonical `__`-separated pproxy URI syntax
  (`docs/ARCHITECTURE.md:16-20`, `docs/SUPPORT.md:24`). Parsing is a single
  delegation: `OutboundConnector::from_pproxy_uri(chain)`
  (`client.rs:521-527`); invalid chains →
  `TunnelError::Configuration("invalid outbound proxy chain")`. Public
  pre-flight: `validate_outbound_proxy` (`client.rs:516-518`,
  re-exported `lib.rs:21-22`); `eggtunnel check` additionally validates the
  full builder via `client_builder(config)?.validate()`
  (`crates/eggtunnel-cli/src/main.rs:921-923`).
- **Per-connection use.** `connect_tcp(endpoint, connect_timeout, outbound.as_deref())`
  (`client/reconnect.rs:352-380`): with a proxy,
  `outbound.connect_tcp_timeout_detailed(host, port, connect_timeout)`
  (`client/reconnect.rs:357-371`); without, direct
  `TcpStream::connect` (`client/reconnect.rs:372-379`). Both control
  (`client/reconnect.rs:295-298`) and every data dial
  (`client/open.rs:37-40`) traverse the same proxy path, so a session over
  proxy opens N proxied data connections.
- **Auth.** HTTP CONNECT Basic and SOCKS5 username/password via URI userinfo
  (`docs/SECURITY.md:100-102`). `OutboundConnectErrorKind::Authentication /
  Policy / Timeout` map to `Authentication / Authorization / Timeout`,
  everything else to `Disconnected` (`client/reconnect.rs:364-369`) — hence proxy auth
  failure is typed, never a silent direct fallback.

### 6.2 Credential handling (env var + redaction)

- Credentials "should be placed in the environment variable named by
  `outbound_proxy_env`" (`docs/SECURITY.md:95-97`). The CLI resolves the name
  structurally (rejects an empty name, `main.rs:599-608`), then reads the
  variable exactly once, rejecting a missing or blank value
  (`main.rs:613-633`). The proxy URI (possibly containing
  userinfo) never comes from the TOML file itself; `client_builder`
  (`main.rs:721-739`) only forwards the already-resolved string.
- Redaction: "redacted from Eggtunnel diagnostics and the public `Snapshot`
  view" (`docs/SECURITY.md:96-97`, `docs/SUPPORT.md:33-35`). Structurally:
  `Snapshot` (`common.rs:154-177`) has no proxy/credential fields at all;
  `ClientConfig::Debug` (`client/config.rs:62-72`) prints no proxy material (proxy
  lives on the private `ClientDataTransport`, which derives only `Clone` and
  has no `Debug` impl, `client.rs:42-54`); outbound failure
  tests assert no secret in diagnostics, e.g.
  `outbound_proxy_refused_endpoint_terminates_without_secret_in_diagnostic`
  (`server_tests/proxy.rs:189`), `outbound_http_connect_auth_failure_rejects_without_secret_leak`
  (`server_tests/proxy.rs:477`), `outbound_socks5_auth_failure_rejects_without_secret_leak`
  (`server_tests/proxy.rs:686`). Failures surface only as typed termination
  categories (`docs/SUPPORT.md:32-35`).
- Multi-hop evidence: one two-hop SOCKS5→HTTP CONNECT end-to-end test
  (`server_tests/proxy.rs:767`,
  `outbound_two_hop_socks5_then_http_connect_routes_end_to_end`);
  "additional protocol combinations are unverified beyond the Eggress 1.0.8
  public API's typed compatibility layer" (`docs/SUPPORT.md:46-49`).

### 6.3 TLS+SNI end-to-end, no silent fallback, rejected combos

- "Eggtunnel TLS and server-name verification run over the established proxy
  path, protecting authentication from a proxy that only forwards CONNECT or
  SOCKS traffic" (`docs/SECURITY.md:93-95`). Concretely: the proxy yields a raw
  `BoxStream`, then the **same** `tls_connect(stream, tls, &server_name)`
  + optional WSS upgrade runs on top (`client/reconnect.rs:299-334` control,
  `client/open.rs:37-57` data). End-to-end TLS tests:
  `outbound_http_connect_keeps_eggtunnel_tls_end_to_end`
  (`server_tests/proxy.rs:5`),
  `outbound_socks5_keeps_eggtunnel_tls_end_to_end` (`server_tests/proxy.rs:91`).
- "The client does not silently fall back to direct networking when a proxy
  path fails" (`docs/SECURITY.md:98-100`): refusal, handshake timeout, and
  cancellation each produce typed termination categories, covered by
  `outbound_proxy_refused_endpoint_...` (`server_tests/proxy.rs:189`),
  `outbound_proxy_handshake_timeout_...` (`server_tests/proxy.rs:247`),
  `outbound_proxy_cancellation_...` (`server_tests/proxy.rs:317`).
- **Rejected combos (fail-closed, at both library and CLI layers):**
  enforcement lives in `validate_client_profile` (`client.rs:569-612`) and
  `validate_server_profile` (`server/config.rs:149-179`); the CLI delegates
  through `client_builder(config)?.validate()` / `server_builder(config)?.validate()`
  inside `check` (`run_check`, `main.rs:912-936`), after its own structural checks
  in `resolve_client_with` / `resolve_server_with`
  (`main.rs:550-652`, `658-717`):

  | Combination | Library behavior | CLI `check` behavior |
  |---|---|---|
  | proxy + QUIC | `validate_client_profile` rejects proxy/identity/CA with `Quic` (`client.rs:587-594`); `start_profile` Quic arm takes no proxy (`client.rs:192-195`, `start_quic_profile` has no outbound parameter) | builder `validate()` (`main.rs:921-923`) → error |
  | proxy + mTLS | `validate_client_profile` rejects identity + proxy (`client.rs:601-606`); the identity arm passes `None` as outbound (`client.rs:159-172`) | `client_cert`/`client_key` pairing + file checks (`main.rs:589-594,634-641`) + builder `validate()` (`main.rs:921-923`) → error |
  | QUIC + custom CA / mTLS | `validate_client_profile` rejects `ca_pem.is_some()` / identity with `Quic` (`client.rs:587-594`); `start_quic_profile` also rejects `ca_pem` (`client.rs:334-338`) | builder `validate()` (`main.rs:921-923`); server `client_ca` + non-TCP profile rejected by `validate_server_profile` (`server/config.rs:164-176`, via `main.rs:932-934`) |
  | WSS + mTLS | `validate_client_profile` rejects identity with `WebSocket` (`client.rs:595-600`); `validate_server_profile` rejects `client_ca` with non-`TcpTls` (`server/config.rs:171-175`) | builder `validate()` (`main.rs:921-923`, `932-934`) → error |
  | server + proxy | No server proxy API | `main.rs:668-673`: `outbound_proxy_env` in server mode → error |

  WSS **over** proxy (`start_websocket_with_outbound_proxy[+connector]`,
  `client.rs:277-301`) is the one allowed composition: proxy → TLS → WSS
  upgrade, selected by `transport = "websocket_tls"` + `outbound_proxy_env`
  in `resolve_client_with` / `client_builder` (`main.rs:613-633,721-739`).

---

## 7. Eggress dependency direction: what Eggtunnel owns vs provides (pinned =1.0.8)

Per `plans/adrs/ADR-0001-session-transport-and-egress-boundary.md` (decision:
Eggtunnel owns reverse-session semantics, Eggress supplies narrow primitives;
`Public API consequence` at `:151`, `Dependency consequence` at `:165`):

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
| `eggress-relay` | `relay_with_options` + `RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain)` opaque byte copy | `client/open.rs:72`, `server/service.rs:124` |
| `eggress-transport-tls` | `TlsClient/ServerConfigBuilder`, `tls_connect`, `tls_accept` | `client.rs:5,649-657`; `client/reconnect.rs:301`; `client/open.rs:44`; `server/tls.rs:9,23-33`; `server/accept.rs:330,362` |
| `eggress-transport-quic` | `QuicListener/Client/Connection`, `QuicClient/ServerConfig`, stream open/accept | `client/reconnect.rs:426-459`; `client/open.rs:66`; `server.rs:245-266`; `server/accept.rs:170,402,414,463` |
| `eggress-protocol-websocket` | `WebSocketTunnelClient/Server`, `connect_over_stream_with_config`, `accept_upgrade_with_config_over_stream` | `client/reconnect.rs:316-319`; `client/open.rs:49,55`; `server/accept.rs:381-382` |
| `eggress-outbound` (+ `pproxy-compat`) | `OutboundConnector::from_pproxy_uri`, `connect_tcp_timeout_detailed`, `OutboundConnectErrorKind` | `client.rs:521-527`; `client/reconnect.rs:359-369` |

Also: `tokio-tungstenite 0.26.2` supplies only the `WebSocketConfig`
(1 MiB `max_message_size` + `max_frame_size`) passed into the Eggress adapter
(`client/reconnect.rs:311`, `client/open.rs:50-52`; `server/accept.rs:376-378`) —
the tunnel framing itself is Eggress's.

Boundaries worth asserting in review:

- No `eggress-embed`, no pproxy reverse protocol as product, no Synvoid/i2pr
  production dependency (`ADR-0001` alternatives-rejected + dependency
  consequence, `:64`, `:187-207`). The pproxy URI syntax is reused for proxy chains
  only, via the `pproxy-compat` feature.
- Narrow crates + `default-features = false` where applicable
  (`Cargo.toml:26-28`) keep the minimal client slice (`client` + `tls`) free
  of QUIC/WebSocket/proxy code (`ADR-0001` dependency consequence).
- All Eggress versions are exact (`=1.0.8`), locked in `Cargo.lock` at 1.0.8.
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
      (`client/reconnect.rs:295-304`, `client/open.rs:37-45`;
      `server/accept.rs:257,326-364`). WS upgrade
      happens only over verified TLS (`client/reconnect.rs:306-334`,
      `client/open.rs:46-57`, `server/accept.rs:266,368-387`). There is no `ws://`
      or bare-TCP session path.
      Reject any change that adds one without an ADR.
- [ ] **Version check is major-only, fail-closed.** `serve_control` rejects
      `hello.version.major != CURRENT.major` (`server/control.rs:98-106`);
      `decode_frame` rejects bad magic / unknown major (`proto:587-594`) and
      unknown message IDs (`proto:595`, `MessageType::try_from` at `proto:334-…`) /
      oversize frames (`proto:597-599`). The client accepts `ServerHello` only on
      a major match (`client.rs:723-735`). Minor is informational. Confirm tests
      still pin wire `CURRENT` + message-ID coverage (`proto:973-…`; see `proto`
      tests).
- [ ] **Error-kind collapse on WS.** Client maps WS upgrade failure to
      `Tls`/`Timeout` (`client/reconnect.rs:320-323`, `client/open.rs:55`); server maps it to
      `Timeout`/`Protocol(UnexpectedMessage)` (`server/accept.rs:385-386`). Same
      wire event yields `Transport` on one side and `Protocol` on the other
      (`common.rs:502-516`). Acceptable today (typed, no silent retry
      difference — auth failures still break the reconnect loop,
      `client/reconnect.rs:108-134`), but do not "fix" one side without updating
      dashboards/tests that key on termination categories.
- [ ] **Auth-failure loop break preserved.** Only
      `Authentication`/`Authorization` (plus `ResourceExhausted` and
      `Configuration`) break `drive`/the reconnect supervisor
      (`client/reconnect.rs:112-122`); `Tls`/`Timeout`/`Disconnected` retry
      with backoff. A downgrade attacker forcing TLS failures must not be
      reclassified into a loop-breaking category that bricks reconnect, nor
      into a retried category for auth failures.

### 8.2 SNI / verification gaps

- [ ] **SNI on every connection, not just control.** `server_name` is
      cloned into `ClientDataTransport::TcpTls` and reused per data dial
      (`client/reconnect.rs:337-344`, `client/open.rs:44`). Verify any new data-path constructor
      threads `server_name` through; a missing SNI on data connections is a
      finding.
- [ ] **Custom CA only where supported.** Baseline + WSS + mTLS honor
      `ca_pem` (`client.rs:649-657`, `659-682`); QUIC rejects it
      (`validate_client_profile`, `client.rs:587-594`, plus `start_quic_profile`,
      `client.rs:334-338`; CLI delegates via builder `validate()`,
      `main.rs:921-923`). Do not add a QUIC custom-CA
      knob by silently ignoring the PEM — the current fail-closed reject is
      the safe behavior until the adapter supports it
      (`docs/SECURITY.md:65-68`).
- [ ] **mTLS principal binding on data.** Server captures leaf SHA-256 at
      accept (`server/accept.rs:334-347`, `certificate_principal` `server/tls.rs:63-67`
      via `sha2`, PEM parsed with the rustls `pki-types` parser
      `pem.rs:1-17`), stores it on the session (`serve_control`
      `server/control.rs:146-156`), and `accept_data_hello` requires
      `session.principal == principal` (`server/pending.rs:51-60`). Bearer token is
      still required alongside the certificate. Any new
      transport must carry the principal through `ControlAdmission`
      (`server/control.rs:61-74`) or be rejected alongside mTLS (§6 table).
      `ClientIdentity` redacts + zeroizes (`client.rs:545-561`); `ServerConfig`
      redacts + zeroizes (`server/config.rs:29-49`).
- [ ] **Ring-provider global.** First Eggress TLS builder call installs the
      process-default provider (`docs/SECURITY.md:61-63`). Embedding tests
      that construct two different TLS stacks in one process should assert no
      provider conflict; changing `ring`/`tls12` workspace features
      (`Cargo.toml:30-31`) is a security-relevant change.

### 8.3 Proxy credential leaks

- [ ] **No credential in file, logs, or Snapshot.** Proxy URI comes from
      `outbound_proxy_env`, never TOML (`main.rs:599-633`);
      `Snapshot` has no proxy fields (`common.rs:154-177`); `ClientConfig`
      and `ServerConfig`/`ClientIdentity` `Debug` impls redact
      (`client/config.rs:62-72`, `client.rs:545-553`; `server/config.rs:36-49`). Run the
      `*_without_secret_leak` / `*_without_secret_in_diagnostic` tests on any
      change to error formatting (`server_tests/proxy.rs:189`, `477`,
      `686`).
- [ ] **Typed proxy errors, no fallback.** `OutboundConnectErrorKind` →
      `TunnelError` mapping (`client/reconnect.rs:364-369`) must stay total; adding a
      new Eggress error kind that hits the `_ => Disconnected` arm is fine,
      mapping it to silent direct-dial is a finding. Confirm refusal/timeout/
      cancellation tests still pass (`server_tests/proxy.rs:189`, `247`,
      `317`).
- [ ] **`__` chain parsing stays delegated.** `parse_outbound_proxy` is a thin
      wrapper over `from_pproxy_uri` (`client.rs:521-527`). Do not hand-roll
      URI splitting (userinfo `@`, IPv6 `[]`, multi-hop `__`) in Eggtunnel;
      divergence from `pproxy-compat` semantics is a finding.

### 8.4 Stream-vs-connection limit confusion

- [ ] **Name the layer.** `accepted_handshakes` (pre-auth, process-wide,
      default 64; `common.rs:256`, enforced `server/accept.rs:49,133,204`-
      `207`) ≠
      `active_connections_per_session` (per-session, default 128;
      `common.rs:255`) ≠ Eggress `1024` connection-tasks /
      `4096` stream-tasks (adapter-internal, `docs/SECURITY.md:71-73`) ≠
      `max_concurrent_streams` (QUIC transport knob, policy-derived: server
      `active_connections_per_session + 1` = 129 by default at
      `server.rs:253-261`; client passes `client_open_tasks` = 128 verbatim,
      `client/reconnect.rs:428-440`) ≠ client
      `client_open_tasks` (default 128, `client.rs:816`) ≠
      `control_queue` (default 128: `client.rs:817`, `server/control.rs:171`) ≠
      `client_command_queue` (default 32: `client.rs:343,454`).
      (`MAX_SESSIONS`/`MAX_HANDSHAKES` at `server_tests.rs:35-36` are
      `#[cfg(test)]`-only.) Any log, metric,
      or doc that says "connection limit" must say which one.
- [ ] **Admission release paths.** Pre-auth `admission` permit + `handshake_guard`
      are dropped exactly once on auth success/failure/serve entry
      (`server/control.rs:138-139`; data fast path `server/accept.rs:276-277`);
      QUIC `stream_admission` permits are held by the spawned data task
      (`server/accept.rs:472-475`). Leaking either permit under a new early-return is
      a resource-exhaustion finding; the saturation/recovery test
      (`server_tests/quic.rs:715`) is the regression net.
- [ ] **Pre-session UDP work is outside the accepted-handshakes cap.** QUIC handshake/CPU cost
      inside `eggress-transport-quic` precedes `admit().try_acquire`
      (`docs/SECURITY.md:69-75`). Do not claim the 64-default cap bounds unauthenticated
      UDP packet processing; it bounds post-accept handshake tasks.

### 8.5 Half-close / relay mismatches

- [ ] **No half-close through WSS.** WS close is whole-connection
      (`docs/SECURITY.md:86-88`); `relay_with_options` over a WS `BoxStream`
      cannot observe TCP write-half-close. The qualified property is narrower:
      peer close terminates the underlying TCP promptly with no dangling halves
      (`docs/SECURITY.md:88-91`, test `server_tests/websocket.rs:253`). Do not add
      application framing that depends on half-close over WSS or QUIC streams
      without a new correlation test.
- [ ] **Relay bounds identical on both ends.** Both relays use
      `RelayOptions::bounded(16 KiB, policy.timeouts.relay_drain)` (`client/open.rs:72`,
      `server/service.rs:124`). Changing one side's buffer/drain without the other
      alters backpressure behavior under the 1 MiB WS caps (§5.2); the bounded
      backpressure C001 case is the gate.
- [ ] **`DataHello`-then-opaque invariant.** `DataHello` is the last
      Eggtunnel message on a data stream (`docs/SECURITY.md:33-34`); after
      `accept_data_hello` both sides hand the stream to `relay_with_options`
      and never `read_boxed` again. Any post-`DataHello` framing change breaks
      relay pairing — review data-path changes with `handle_quic_data_stream`
      (`server/accept.rs:496-516`), `handle_connection` data branch
      (`server/accept.rs:270-313`), and `handle_open` (`client/open.rs:13-93`) together.

### 8.6 Quick file:line index for reviewers

| Question | Answer at |
|---|---|
| Framing semantics | `wire_io.rs:1-77`, `proto:565-635` |
| Framing regression tests | `wire_io.rs:79-209` |
| Transport neutrality | `wire_io.rs:68-77`, `lib.rs:19-35` |
| Feature gates | `crates/eggtunnel/Cargo.toml:15-32`, `Cargo.toml:23-31` |
| Role slices stay minimal | `.github/workflows/ci.yml:62-81` |
| Builder profiles | `client/config.rs:75-161`, `server/config.rs:52-131`, `client.rs:142-216`, `server.rs:204-237` |
| TLS baseline build | `client.rs:649-657`, `server/tls.rs:23-33`, `client/reconnect.rs:299-304`, `server/accept.rs:317-365` |
| Runtime invariant | `client.rs:330-332,446-448`, `server.rs:296,348-353`, `lib.rs:2-5` |
| RuntimePolicy limits/timeouts | `common.rs:213-333` |
| QUIC dial/accept | `client/reconnect.rs:424-467`, `client/open.rs:62-68`, `server.rs:247-266`, `server/accept.rs:401-516` |
| QUIC limits | `common.rs:213-262`, `server/accept.rs:176-177,430,464-475`, `server.rs:253-261`, `client/reconnect.rs:428-440`, `docs/SECURITY.md:65-81` |
| WSS upgrade | `client/reconnect.rs:306-334`, `client/open.rs:46-57`, `server/accept.rs:368-387` |
| WSS close semantics | `docs/SECURITY.md:83-91`, `docs/SUPPORT.md:31-32,41-45`, `server_tests/websocket.rs:75,253` |
| Proxy dial/auth | `client.rs:516-527`, `client/reconnect.rs:352-380`, `server_tests/proxy.rs:5,91` |
| Proxy redaction | `common.rs:154-177`, `client/config.rs:62-72`, `server_tests/proxy.rs:189,477,686`, `docs/SECURITY.md:93-97` |
| Rejected combos | `client.rs:587-606`, `server/config.rs:164-176`, `main.rs:550-717,912-936` |
| mTLS identity/principal | `client.rs:529-561,659-682`, `server/tls.rs:35-67`, `server/accept.rs:334-347`, `server/pending.rs:51-60`, `pem.rs:1-17` |
| CLI builder delegation | `main.rs:495-518,721-760,912-936` |
| Ownership boundary | `ADR-0001` (decision + consequences `:151,165`), `docs/ARCHITECTURE.md:1-23` |
