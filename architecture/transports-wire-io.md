# Transports + Wire I/O — Deep Dive

Back to [Architecture Overview](overview.md) §5. This file is the review-oriented
deep dive for `crates/eggtunnel/src/wire_io.rs` and the feature-gated transport
stack (TCP+TLS baseline, QUIC, WebSocket/WSS, outbound proxy) built on pinned
Eggress 1.0.8 primitives.

Primary sources (line anchors are load-bearing for review):

- `crates/eggtunnel/src/wire_io.rs` (full, 58 LOC)
- `crates/eggtunnel/Cargo.toml`, `Cargo.toml` (workspace)
- `crates/eggtunnel/src/client.rs`, `crates/eggtunnel/src/server.rs`
- `crates/eggtunnel/src/common.rs`, `crates/eggtunnel/src/lib.rs`
- `crates/eggtunnel-proto/src/lib.rs`
- `crates/eggtunnel-cli/src/main.rs`
- `docs/SUPPORT.md`, `docs/SECURITY.md`, `docs/ARCHITECTURE.md`
- `plans/adrs/ADR-0001-session-transport-and-egress-boundary.md`,
  `plans/subsystems/reverse-session-roadmap.md`

Related overview sections: [wire protocol](proto-wire-protocol.md) (framing),
[client](client.md), [server](server.md).

---

## 1. `wire_io.rs`: bounded framing over any `AsyncRead/AsyncWrite`

File: `crates/eggtunnel/src/wire_io.rs:1-58`.

`wire_io.rs` is deliberately thin: it adapts the runtime-neutral
`eggtunnel-proto` codec (`encode_frame` / `decode_frame`,
`crates/eggtunnel-proto/src/lib.rs:457-525`) to Tokio I/O and to Eggress's
boxed stream type. All Eggtunnel control-plane messages (`ClientHello` … `DataHello`)
flow through these four functions. Opaque post-`DataHello` relay bytes do **not**
flow through `wire_io`; they go to `eggress-relay::relay_with_options`
(`crates/eggtunnel/src/client.rs:1129`,
`crates/eggtunnel/src/server.rs:1190`).

### 1.1 `read_message` — header-first, length pre-check, exact consumption

`crates/eggtunnel/src/wire_io.rs:7-36`:

1. **Header-first read (14 bytes).**
   `reader.read_exact(&mut header)` (`wire_io.rs:11-14`). Short read / EOF /
   reset maps to `ProtocolError::TruncatedFrame` via
   `.map_err(|_| ProtocolError::TruncatedFrame)`. There is no distinction
   between "peer closed cleanly" and "peer sent a short header" at this layer;
   both become `TruncatedFrame`, which `common.rs:301-315` later maps to
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
   (`client.rs:948`, `server.rs:969`) rely on this 1:1 property.

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
`write_message`, but the type is the point: `BoxStream` (from
`eggress-core = "=1.0.8"`, `Cargo.toml:22`) is the **single control-plane I/O
type** across all transports. Every handshake path converges on it:

- Baseline / mTLS: `Box::new(tcp)` → `tls_accept` / `tls_connect` returns a
  `BoxStream` (`server.rs:757-762`, `client.rs:709-714` + `591`).
- WebSocket: TLS `BoxStream` → `WebSocketTunnelServer/Client` adapter returns a
  `BoxStream` (`server.rs:781-795`, `client.rs:595-618`, `1104-1115`).
- QUIC: `QuicConnection::accept_stream` / `open_stream` returns a `BoxStream`
  directly (`server.rs:589-592`, `client.rs:764-767`, `1121-1126`).
- Outbound proxy: `OutboundConnector::connect_tcp_timeout_detailed` returns a
  `BoxStream` that is then fed into `tls_connect`
  (`client.rs:698-708`).

Consequences for review:

- Session logic (`run_session`, `serve_control`, `accept_data_hello`) never
  branches on socket type; transport selection ends before the first
  `read_boxed`. The `websocket: bool` flag and `ClientDataTransport` enum are
  construction-time only.
- `tokio::io::split(stream)` on a `BoxStream`
  (`client.rs:948`, `server.rs:969`) requires `BoxStream: AsyncRead +
  AsyncWrite`; the WebSocket and QUIC adapters must therefore preserve
  byte-stream semantics (see §5 on the half-close caveat — byte-stream does
  **not** imply half-close equivalence).
- Public API leakage is contained: `lib.rs:1-31` re-exports
  `Client/Server/Config/Handle` and `proto`, never `BoxStream`, `rustls`,
  `quinn`, or `tungstenite` types (per
  `ADR-0001:151-164`).

---

## 2. Feature matrix: what each flag pulls in and gates

Crate features: `crates/eggtunnel/Cargo.toml:15-23`. Workspace pins:
`Cargo.toml:13-34`.

| Feature | Default? | Pulls in (crate deps) | Gates in code |
|---|---|---|---|
| `client` | ✅ (`default = ["client","tls"]`, `crates/eggtunnel/Cargo.toml:16`) | `tokio`, `tokio-util`, `eggress-core`, `eggress-transport-tls`, `eggress-relay`, `getrandom`, `rustls`, + `tls` | `mod client` (`lib.rs:11-12`); `Client`, `TargetConnector`, all `Client::start*` variants; `ClientDataTransport` enum (`client.rs:24-36`) |
| `tls` | ✅ | `eggress-transport-tls` (`crates/eggtunnel/Cargo.toml:19`) | Baseline path; `TlsClientConfigBuilder` / `TlsServerConfigBuilder`, `tls_connect` / `tls_accept` |
| `server` | ❌ | `tokio`, `tokio-util`, `eggress-core`, `eggress-transport-tls`, `eggress-relay`, `rustls`, + `tls` | `mod server` (`lib.rs:12-14`); `Server`, `ServerTls` enum (`server.rs:32-37`), all `Server::bind*` variants |
| `quic` | ❌ | `client` + `server` + `eggress-transport-quic` (`crates/eggtunnel/Cargo.toml:20`) | `ClientDataTransport::Quic` (`client.rs:34-35`); `Client::start_quic*` (`client.rs:247-319`), `quic_reconnect_loop` (`client.rs:716-837`); `Server::bind_quic*` (`server.rs:159-259`), `quic_server_loop*`, `handle_quic_connection`, `handle_quic_data_stream` (`server.rs:479-684`); `max_active_data_streams` field on `ConnectionContext` (`server.rs:701-702`) |
| `websocket` | ❌ | `client` + `server` + `eggress-protocol-websocket` + `tokio-tungstenite` (`crates/eggtunnel/Cargo.toml:21`) | `websocket: bool` threading through `start_with_tls_config` (`client.rs:356-399`), `reconnect_loop` upgrade (`client.rs:598-616`), `handle_open` data-path upgrade (`client.rs:1104-1115`); server `websocket: bool` through `bind_with_tls_profile` (`server.rs:283-318`), `handle_connection` upgrade (`server.rs:780-800`); `Server::bind_websocket`, `Client::start_websocket*` |
| `outbound-proxy` | ❌ | `client` + `eggress-outbound` with `pproxy-compat` (`crates/eggtunnel/Cargo.toml:22`, `Cargo.toml:27`) | `outbound: Option<Arc<OutboundConnector>>` on `ClientDataTransport::TcpTls` (`client.rs:31-32`) and `start_with_tls_config` (`client.rs:361-363`); `parse_outbound_proxy` via `OutboundConnector::from_pproxy_uri` (`client.rs:418-425`); `connect_server` proxy branch (`client.rs:687-714`); `validate_outbound_proxy` re-export (`lib.rs:18-19`); `start_with_outbound_proxy*`, `start_websocket_with_outbound_proxy*` (`client.rs:199-245`) |
| `mtls` | ❌ | `tls` + `tokio-rustls`, `rustls-pemfile`, `webpki-roots`, `sha2` (`crates/eggtunnel/Cargo.toml:23`) | `ServerTls::Mutual` (`server.rs:35-36`); `Server::bind_mtls*` (`server.rs:261-281`), `build_mtls_server_config` (`server.rs:358-389`), `certificate_principal` SHA-256 (`server.rs:391-395`); `Client::start_with_mtls*`, `ClientIdentity` + redacted `Debug` + `zeroize` drop (`client.rs:427-459`), `build_mtls_tls_config` (`client.rs:530-562`) |

Notes:

- `client` implies `tls` (`crates/eggtunnel/Cargo.toml:17`: `client = [... "tls"]`),
  and `quic` / `websocket` imply `client` + `server`. There is no supported
  `quic`-without-`server` or `websocket`-without-`tls` slice; the CLI enforces
  the same coupling at config-check time (§6).
- `mod wire_io` exists iff `client` or `server` is enabled
  (`lib.rs:8-9`). A `proto`-only build has no socket dependency, per
  `docs/ARCHITECTURE.md:3-6`.
- Dev-only: `crates/eggtunnel/Cargo.toml:46-48` enables
  `eggress-transport-quic/insecure-quic` for tests. That feature must never
  leak into non-test builds; production QUIC goes through
  `QuicClientConfig { insecure: false }` (`client.rs:739-745`). The insecure
  path is reachable only via `start_quic_insecure_for_test`
  (`client.rs:306-319`).
- Workspace pins every Eggress crate to exactly `=1.0.8`
  (`Cargo.toml:22-27`; `Cargo.lock` confirms `eggress-core/-relay/
  -transport-tls/-transport-quic/-protocol-websocket/-outbound 1.0.8`).
  `tokio-tungstenite 0.26.2`, `rustls 0.23` (`ring`, `std`, `tls12`),
  `tokio-rustls 0.26` (`ring`, `tls12`) are also pinned in the workspace
  (`Cargo.toml:28-30`).

---

## 3. Baseline TCP+TLS: Eggress builders, ring provider, TLS 1.2+, caller runtime

### 3.1 Construction

- Server: `Server::bind` → `bind_with_policy` builds
  `TlsServerConfigBuilder::new().with_certificate_pem(...).with_key_pem(...).build()`
  (`server.rs:126-132`) and hands the resulting `Arc<rustls::ServerConfig>` to
  `bind_with_tls_profile(..., ServerTls::Eggress(tls), false)`
  (`server.rs:133`). Every accepted `TcpStream` is boxed then passed to
  `eggress_transport_tls::tls_accept(stream, tls)` under a 10 s
  `HANDSHAKE_TIMEOUT` (`server.rs:755-762`, constants `server.rs:39-52`).
- Client: `Client::start[_with_connector]` builds via `build_tls_config`
  (`client.rs:520-528`):
  `TlsClientConfigBuilder::new().with_system_roots()` or
  `.with_custom_ca_pem(pem)`, then `.build()`. `reconnect_loop`
  (`client.rs:565-685`) dials `TcpStream::connect` (10 s `CONNECT_TIMEOUT`),
  then `tls_connect(stream, tls, &tls_server_name)` under 10 s
  `HANDSHAKE_TIMEOUT` (`client.rs:587-593`). The data path repeats the same two
  steps per `Open` (`client.rs:1094-1103`).
- Validation before any socket: `validate_config` checks endpoint shape,
  non-empty ≤253 B server name, 1–64 services with unique IDs/names, CA size cap
  (`client.rs:467-498`); server checks cert/key presence and size cap
  (`server.rs:342-356`) plus `BindPolicy::validate` (`server.rs:125`).

### 3.2 TLS properties relevant to review

- **Builders are Eggress-owned.** `TlsClientConfigBuilder` /
  `TlsServerConfigBuilder` come from `eggress-transport-tls 1.0.8`. Eggtunnel
  never constructs `rustls::{Client,Server}Config` directly on this path
  (contrast the mTLS path, §3.4 of the server/client dives, which drops to
  `rustls` + `tokio-rustls` + `rustls-pemfile` directly).
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
  data connection (`client.rs:591`, `1102`). Server-name verification uses
  system roots by default or the explicit `ca_pem` bundle; `ClientConfig`'s
  `Debug` redacts the token and collapses `ca_pem` to `[configured]`
  (`client.rs:99-109`). Auth (`Auth` bearer token) always runs **inside** the
  verified channel (`docs/SECURITY.md:3-7`).

### 3.3 Caller-owned Tokio runtime invariant

Every public entrypoint asserts a caller-owned runtime before doing I/O:

- `Client::start_with_tls_config` (`client.rs:365-367`),
  `Client::start_quic_profile` (`client.rs:266-268`);
- `Server::bind_with_policy` (`server.rs:121-123`),
  `Server::bind_websocket` (`server.rs:142-146`),
  `Server::bind_quic[_with_policy]` (`server.rs:175-177`, `221-223`),
  `Server::bind_with_tls_profile` (`server.rs:289-291`).

Failure is `TunnelError::Configuration("... requires a caller-owned Tokio
runtime")` → `TerminationCategory::Internal`. The library never calls
`#[tokio::main]`, `Runtime::new`, or installs a global tracing subscriber
(`lib.rs:3-5`, roadmap §2 runtime invariants).

### 3.4 Baseline handshake sequence (TCP+TLS)

```text
client                                    server
  | TcpStream::connect(server_addr) [10s]  |
  |--------------------------------------->| listener.accept()
  | tls_connect(stream, tls, server_name)  | tls_accept(Box(tcp), tls) [10s each]
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

Control uses `read_boxed`/`write_boxed` throughout (`server.rs:902-909`,
`910-916`, `968`; `client.rs:888-931`). Data connections branch on the first
message in `handle_connection` (`server.rs:802-823`): `DataHello` → relay,
`ClientHello` → control session, anything else → `UnexpectedMessage`.

---

## 4. QUIC: UDP control endpoint, one bidi stream per path

Feature: `quic` (`crates/eggtunnel/Cargo.toml:20`). Docs:
`docs/SUPPORT.md:6`, `docs/SECURITY.md:45-62`, `docs/ARCHITECTURE.md:12-13`.

### 4.1 What changes vs baseline

| Aspect | TCP+TLS baseline | QUIC profile |
|---|---|---|
| Listener | `TcpListener::bind` (`server.rs:294`) | `QuicListener::bind(addr, QuicServerConfig { cert, key, idle_timeout: 90s, max_concurrent_streams: 256, alpn: [] })` (`server.rs:180-191`) |
| Control transport | one TLS-over-TCP connection | one UDP QUIC connection per session; first inbound bidi stream is the control stream (`server.rs:589-592`) |
| Data transport | one TCP+TLS connection per external conn | one bidi stream per external conn on the **same** QUIC connection (`server.rs:629-651`, `client.rs:1121-1126`) |
| Service listeners | TCP (`TcpListener::bind` in `serve_control`, `server.rs:1001`) | **retained as TCP** — "Service listeners remain TCP even when the control Session uses QUIC over UDP" (`docs/SUPPORT.md:12-13`) |
| Trust | system roots or custom CA | **platform roots only, bearer only** — custom CA / mTLS rejected, not ignored |
| Client entry | `Client::start` | `Client::start_quic[_with_connector]` (`client.rs:247-259`) |
| Server entry | `Server::bind` | `Server::bind_quic[_with_policy]` (`server.rs:159-212`) |

### 4.2 Handshake sequence (QUIC)

```text
client (QuicClient)                       server (QuicListener, UDP)
  | QuicClient::connect(host, port, QuicClientConfig{server_name, idle 90s, max_streams 256}) [10s]
  |--------------------------------------->| accept_connection(&cancel)
  | get_connection() [10s] → open_stream() [10s] (control)
  |--------------------------------------->| accept_stream() [10s] → read_boxed → expect ClientHello
  | ... same Session handshake as baseline (ServerHello/Auth/AuthOk/Register*) over control stream ... |
  |                                        | external TCP accept → PendingEntry → Open over control stream
  | connection.open_stream() [10s] (data)  |
  |--------------------------------------->| accept_stream() → read_boxed → expect DataHello
  | DataHello(session, service, connection) | accept_data_hello (wrong-session/stale/replay → Auth error)
  |--------------------------------------->| relay_with_options (opaque bytes over bidi stream)
```

Key call sites: client `quic_reconnect_loop` (`client.rs:717-806`):
config build (`739-745`), connect with `CONNECT_TIMEOUT` (`746-757`),
`get_connection` (`760-763`), control `open_stream` (`764-767`), then the
shared `run_session` with `ClientDataTransport::Quic(connection)`
(`768-782`). Data dial is `connection.open_stream()` under `CONNECT_TIMEOUT`
(`client.rs:1121-1126`) followed by `DataHello` + relay (`1128-1129`).

Server `quic_server_loop[_with_admission]` (`server.rs:479-580`) mirrors
`server_loop` admission accounting, then `handle_quic_connection`
(`server.rs:582-666`): accept first stream with `HANDSHAKE_TIMEOUT`
(`589-592`), `read_boxed` expecting `ClientHello` (`593-600`), spawn
`serve_control` (`611`), then loop `accept_stream` for data streams
(`629-651`) each handled by `handle_quic_data_stream` (`668-684`), which
expects `DataHello` under `HANDSHAKE_TIMEOUT` and delegates to the shared
`accept_data_hello`.

### 4.3 Limits and their interaction (read carefully — three layers)

1. **Eggress QUIC adapter (per-connection/task fan-out).**
   `docs/SECURITY.md:51-53`: `MAX_CONCURRENT_CONNECTION_TASKS=1024` and
   `MAX_CONCURRENT_STREAM_TASKS=4096`. These bound Eggress-internal task
   spawning per connection/stream **before** Eggtunnel authentication. They are
   not Eggtunnel constants; no Eggtunnel source defines them.
2. **Eggtunnel pre-session admission (`MAX_HANDSHAKES=64`).**
   `server.rs:43` (`MAX_HANDSHAKES`), enforced by `admission: Semaphore(64)`
   in both `server_loop` (`server.rs:408`, `420-424`) and
   `quic_server_loop_with_admission` (`server.rs:509`, `528-533`), plus a
   `HandshakeGuard` active-handshake counter (`server.rs:705-726`). On
   exhaustion: TCP path drops the accept + `rejected+1` +
   `ResourceExhausted` (`server.rs:420-424`); QUIC path additionally
   `connection.close("handshake limit reached")` (`server.rs:528-533`).
   Per `docs/SECURITY.md:48-57`, "pre-session UDP/TLS handshake work runs
   inside the adapter before Eggtunnel's semaphore is acquired", so the
   residual pre-auth admission risk is documented as observable, not
   eliminated — no vendoring required.
3. **Per-session stream admission (`stream_admission`, 128).**
   `server.rs:41` (`MAX_ACTIVE_CONNECTIONS_PER_SESSION=128`),
   instantiated per QUIC connection as
   `Semaphore(max_active_data_streams.unwrap_or(128))` (`server.rs:602-606`).
   Each accepted data stream does `try_acquire_owned`; on failure:
   `rejected+1` + `ResourceExhausted`, stream dropped without a response
   (`server.rs:631-635`). The semaphore saturates, recovers on stream close
   (permit is held by the spawned task, `server.rs:647-650`), and rejects
   beyond 128 — qualified by the C001 stream-saturation test
   (`docs/SECURITY.md:57-60`; test helper
   `bind_quic_with_admission_for_test`, `server.rs:214-259`, and the
   saturation test near `server.rs:4304-4388`).

Interaction mental model for review: Eggress 1024/4096 caps adapter-internal
fan-out; Eggtunnel 64 caps **unauthenticated handshakes process-wide**
(both TCP and QUIC); 128 caps **active data streams/connections per
session** (QUIC `stream_admission`, TCP `connection_admission`
`server.rs:944`, client `MAX_OPEN_TASKS=128` `client.rs:39`). Do not conflate
"connection" (QUIC UDP 4-tuple ≈ session transport) with "stream" (one
external TCP connection) or with "handshake task" (pre-auth work unit).

Additional QUIC specifics:

- `QuicServerConfig { idle_timeout: 90s, max_concurrent_streams: 256 }`
  (`server.rs:185-186`); client mirrors `idle_timeout: 90s,
  max_concurrent_streams: 256` (`client.rs:743-744`). The 256 transport cap
  sits above the 128 Eggtunnel admission cap, so Eggtunnel rejects first.
- Test-only admission override: `bind_quic_with_admission_for_test` scales
  `max_concurrent_streams = max(1, n) * 2` (`server.rs:232`) — test-only,
  `#[cfg(all(test, feature = "quic"))]`.
- Bearer token still required inside the encrypted control stream
  (`docs/SECURITY.md:48`); QUIC provides confidentiality + SNI verification,
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
  &url, stream, ws_config)` under `HANDSHAKE_TIMEOUT`, where
  `url = format!("wss://{}", server_addr)` and `ws_config` sets
  `max_message_size = max_frame_size = 1 MiB`
  (`client.rs:598-614`). Failure maps to `Timeout` (outer) or `Tls` (inner).
  Data connections repeat the identical upgrade per `Open`
  (`client.rs:1104-1115`).
- Server: after `tls_accept` (or mTLS accept — but WSS+mTLS is rejected before
  this point, §6), if `websocket == true`,
  `WebSocketTunnelServer::new(1024*1024)
  .accept_upgrade_with_config_over_stream(stream, ws_config)` under
  `HANDSHAKE_TIMEOUT` with the same 1 MiB caps (`server.rs:780-795`).
  Upgrade failure maps to `Timeout` (outer) or
  `Protocol(UnexpectedMessage)` (inner) — note asymmetry with the client side
  (§8.1).
- Entry points: `Client::start_websocket[_with_connector]`
  (`client.rs:177-197`), `Server::bind_websocket` (`server.rs:136-157`), CLI
  `transport = "websocket_tls"` (`crates/eggtunnel-cli/src/main.rs:136-138`,
  `262-263`, `314-315`).

### 5.2 Handshake sequence (WSS, control and each data connection)

```text
client                                    server
  | TCP connect → tls_connect (verified, SNI) — identical to baseline
  |<=========== verified TLS =============>|
  | WS upgrade: WebSocketTunnelClient.connect_over_stream_with_config(wss://addr, tls_stream) [10s]
  |--------------------------------------->| WebSocketTunnelServer.accept_upgrade_with_config_over_stream(tls_stream) [10s]
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
  (`max_message_size`, `max_frame_size`) agree on 1 MiB. Multi-frame bounded
  backpressure round-trips within the caps per C001
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
  `server.rs:1574-...`; session/data round-trip
  `websocket_tls_session_registers_and_relays_data_paths`,
  `server.rs:1515-...`). Application code must not depend on observing a
  TCP-style `shutdown(Write)` through WSS.
- **Session semantics unchanged.** QUIC/WS "do not change Service,
  authorization, TargetConnector, or ConnectionId semantics"
  (`docs/SUPPORT.md:10`).

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
  (`crates/eggtunnel-cli/src/main.rs:229-231`: "outbound_proxy is only valid
  in client mode"). There is no `Server::bind_*_with_proxy`.
- **Profiles.** Direct, HTTP CONNECT, SOCKS5 single-hop, plus multi-hop chains
  through the canonical `__`-separated pproxy URI syntax
  (`docs/ARCHITECTURE.md:17-20`, `docs/SUPPORT.md:8`). Parsing is a single
  delegation: `OutboundConnector::from_pproxy_uri(chain)`
  (`client.rs:419-425`); invalid chains →
  `TunnelError::Configuration("invalid outbound proxy chain")`. Public
  pre-flight: `validate_outbound_proxy` (`client.rs:413-416`,
  re-exported `lib.rs:18-19`), called by `eggtunnel check`
  (`crates/eggtunnel-cli/src/main.rs:183`).
- **Per-connection use.** `connect_server(endpoint,
  outbound.as_deref())` (`client.rs:687-714`): with a proxy,
  `split_endpoint` → `outbound.connect_tcp_timeout_detailed(host, port,
  CONNECT_TIMEOUT)` (`client.rs:695-707`); without, direct
  `TcpStream::connect` (`709-713`). Both control (`client.rs:583-586`) and
  every data dial (`client.rs:1094-1098`) traverse the same proxy path, so a
  session over proxy opens N proxied data connections.
- **Auth.** HTTP CONNECT Basic and SOCKS5 username/password via URI userinfo
  (`docs/SECURITY.md:81-83`). `OutboundConnectErrorKind::Authentication /
  Policy / Timeout` map to `Authentication / Authorization / Timeout`,
  everything else to `Disconnected` (`client.rs:701-706`) — hence proxy auth
  failure is typed, never a silent direct fallback.

### 6.2 Credential handling (env var + redaction)

- Credentials "should be placed in the environment variable named by
  `outbound_proxy_env`" (`docs/SECURITY.md:76-78`). CLI `check` verifies the
  variable exists, is non-empty, and parses
  (`crates/eggtunnel-cli/src/main.rs:170-184`); runtime reads it once with
  `env::var(proxy_env)` (`main.rs:308`). The proxy URI (possibly containing
  userinfo) never comes from the TOML file itself.
- Redaction: "redacted from Eggtunnel diagnostics and the public `Snapshot`
  view" (`docs/SECURITY.md:77-78`, `docs/SUPPORT.md:17-19`). Structurally:
  `Snapshot` (`common.rs:134-157`) has no proxy/credential fields at all;
  `ClientConfig::Debug` (`client.rs:99-109`) prints no proxy material (proxy
  lives on `ClientDataTransport`, which has no `Debug` impl); outbound failure
  tests assert no secret in diagnostics, e.g.
  `outbound_proxy_refused_endpoint_terminates_without_secret_in_diagnostic`
  (`server.rs:1982-...`), `outbound_http_connect_auth_failure_rejects_without_secret_leak`
  (`server.rs:2276-...`), `outbound_socks5_auth_failure_rejects_without_secret_leak`
  (`server.rs:2485-...`). Failures surface only as typed termination
  categories (`docs/SUPPORT.md:18-19`).
- Multi-hop evidence: one two-hop SOCKS5→HTTP CONNECT end-to-end test
  (`server.rs:2566-...`,
  `outbound_two_hop_socks5_then_http_connect_routes_end_to_end`);
  "additional protocol combinations are unverified beyond the Eggress 1.0.8
  public API's typed compatibility layer" (`docs/SUPPORT.md:31-34`).

### 6.3 TLS+SNI end-to-end, no silent fallback, rejected combos

- "Eggtunnel TLS and server-name verification run over the established proxy
  path, protecting authentication from a proxy that only forwards CONNECT or
  SOCKS traffic" (`docs/SECURITY.md:74-76`). Concretely: proxy yields a raw
  `BoxStream`, then the **same** `tls_connect(stream, tls, &tls_server_name)`
  + optional WSS upgrade runs on top (`client.rs:587-619` control,
  `1099-1115` data). End-to-end TLS tests:
  `outbound_http_connect_keeps_eggtunnel_tls_end_to_end`
  (`server.rs:1730-...`),
  `outbound_socks5_keeps_eggtunnel_tls_end_to_end` (`server.rs:1816-...`).
- "The client does not silently fall back to direct networking when a proxy
  path fails" (`docs/SECURITY.md:79-81`): refusal, handshake timeout, and
  cancellation each produce typed termination categories, covered by
  `outbound_proxy_refused_endpoint_...` (`server.rs:1982-...`),
  `outbound_proxy_handshake_timeout_...` (`server.rs:2046-...`),
  `outbound_proxy_cancellation_...` (`server.rs:2116-...`).
- **Rejected combos (fail-closed, at both library and CLI layers):**

  | Combination | Library behavior | CLI `check` behavior |
  |---|---|---|
  | proxy + QUIC | No API exists (`start_with_outbound_proxy` builds `TcpTls`, never `Quic`; `start_quic_profile` takes no proxy arg) | `main.rs:160-169`: QUIC + `outbound_proxy_env` → error |
  | proxy + mTLS | `start_with_outbound_proxy*` takes no `ClientIdentity`; mTLS starters pass `None` as outbound (`client.rs:327-354`) | `main.rs:185-189`: proxy + `client_cert/key` → error |
  | QUIC + custom CA / mTLS | `start_quic_profile` rejects `ca_pem.is_some()` (`client.rs:270-274`); no client-cert plumbing on the QUIC path | `main.rs:160-169` (client), `main.rs:223-225` (server `client_ca` + QUIC) |
  | WSS + mTLS | No `start_websocket_with_mtls`; `start_websocket*` never builds an mTLS config | `main.rs:190-194` (client), `main.rs:226-228` (server) |
  | server + proxy | No server proxy API | `main.rs:229-231` |

  WSS **over** proxy (`start_websocket_with_outbound_proxy[+connector]`,
  `client.rs:223-245`) is the one allowed composition: proxy → TLS → WSS
  upgrade, selected by `transport = "websocket_tls"` + `outbound_proxy_env`
  (`main.rs:307-313`).

---

## 7. Eggress dependency direction: what Eggtunnel owns vs provides (pinned =1.0.8)

Per `ADR-0001:38-62` and `reverse-session-roadmap.md:16-52`:

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
| `eggress-relay` | `relay_with_options` + `RelayOptions::bounded(16 KiB, 15 s drain)` opaque byte copy | `client.rs:1129`, `server.rs:1190` |
| `eggress-transport-tls` | `TlsClient/ServerConfigBuilder`, `tls_connect`, `tls_accept` | `client.rs:5,521-528,591,1102`; `server.rs:10,126-132,149-155,760` |
| `eggress-transport-quic` | `QuicListener/Client/Connection`, `QuicClient/ServerConfig`, stream open/accept | `client.rs:35,726-767,1124`; `server.rs:173-191,519-684` |
| `eggress-protocol-websocket` | `WebSocketTunnelClient/Server`, `connect_over_stream_with_config`, `accept_upgrade_with_config_over_stream` | `client.rs:606-613,1107-1114`; `server.rs:787-792` |
| `eggress-outbound` (+ `pproxy-compat`) | `OutboundConnector::from_pproxy_uri`, `connect_tcp_timeout_detailed`, `OutboundConnectErrorKind` | `client.rs:419-425,698-707` |

Also: `tokio-tungstenite 0.26.2` supplies only the `WebSocketConfig`
(1 MiB caps) passed into the Eggress adapter (`client.rs:601-603`,
`1108-1110`; `server.rs:782-784`) — the tunnel framing itself is Eggress's.

Boundaries worth asserting in review:

- No `eggress-embed`, no pproxy reverse protocol as product, no Synvoid/i2pr
  production dependency (`ADR-0001:64-66,112-149,189-203`; roadmap §2
  embedding invariants). The pproxy URI syntax is reused for proxy chains
  only, via the `pproxy-compat` feature.
- Narrow crates + `default-features = false` where applicable
  (`Cargo.toml:25-27`) keep the minimal client slice (`client` + `tls`) free
  of QUIC/WebSocket/proxy code (roadmap §2, `ADR-0001:165-176`).
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
      (`client.rs:587-593`, `1099-1103`; `server.rs:755-762`). WS upgrade
      happens only over verified TLS (`client.rs:598-614`,
      `server.rs:780-795`). There is no `ws://` or bare-TCP session path.
      Reject any change that adds one without an ADR.
- [ ] **Version check is major-only, fail-closed.** `serve_control` rejects
      `hello.version.major != CURRENT.major` (`server.rs:894-901`);
      `decode_frame` rejects unknown major (`proto:483-485`) and unknown
      message IDs (`proto:486`). Client checks `ServerHello` major
      (`client.rs:896-903`). Minor is informational. Confirm tests still pin
      wire v1.0 + IDs 1–14 (`proto:606-639`).
- [ ] **Error-kind collapse on WS.** Client maps WS upgrade failure to
      `Tls`/`Timeout` (`client.rs:611-613`, `1113`); server maps it to
      `Timeout`/`Protocol(UnexpectedMessage)` (`server.rs:785-792`). Same
      wire event yields `Transport` on one side and `Protocol` on the other
      (`common.rs:301-315`). Acceptable today (typed, no silent retry
      difference — auth failures still break the reconnect loop,
      `client.rs:649-657`), but do not "fix" one side without updating
      dashboards/tests that key on termination categories.
- [ ] **Auth-failure loop break preserved.** Only
      `Authentication`/`Authorization` break `reconnect_loop`/`quic_reconnect_loop`
      (`client.rs:649-657`, `788-796`); `Tls`/`Timeout`/`Disconnected` retry
      with backoff. A downgrade attacker forcing TLS failures must not be
      reclassified into a loop-breaking category that bricks reconnect, nor
      into a retried category for auth failures.

### 8.2 SNI / verification gaps

- [ ] **SNI on every connection, not just control.** `tls_server_name` is
      cloned into `ClientDataTransport::TcpTls` and reused per data dial
      (`client.rs:623-632`, `1094-1103`). Verify any new data-path constructor
      threads `server_name` through; a missing SNI on data connections is a
      finding.
- [ ] **Custom CA only where supported.** Baseline + WSS + mTLS honor
      `ca_pem` (`client.rs:520-528`, `538-547`); QUIC rejects it
      (`client.rs:270-274`, CLI `main.rs:160-169`). Do not add a QUIC custom-CA
      knob by silently ignoring the PEM — the current fail-closed reject is
      the safe behavior until the adapter supports it
      (`docs/SECURITY.md:45-48`).
- [ ] **mTLS principal binding on data.** Server captures leaf SHA-256 at
      accept (`server.rs:771-777`, `391-395`), stores it on the session
      (`server.rs:939-947`), and `accept_data_hello` requires
      `session.principal == hello principal` (`server.rs:843-848`). Any new
      transport must carry the principal through `ConnectionContext`
      (`server.rs:692-703`) or be rejected alongside mTLS (§6 table).
- [ ] **Ring-provider global.** First Eggress TLS builder call installs the
      process-default provider (`docs/SECURITY.md:41-43`). Embedding tests
      that construct two different TLS stacks in one process should assert no
      provider conflict; changing `ring`/`tls12` workspace features
      (`Cargo.toml:29-30`) is a security-relevant change.

### 8.3 Proxy credential leaks

- [ ] **No credential in file, logs, or Snapshot.** Proxy URI comes from
      `outbound_proxy_env`, never TOML (`main.rs:177-184`, `308`);
      `Snapshot` has no proxy fields (`common.rs:134-157`); `ClientConfig`
      and `ServerConfig`/`ClientIdentity` `Debug` impls redact
      (`client.rs:99-109`, `443-450`; `server.rs:71-84`). Run the
      `*_without_secret_leak` / `*_without_secret_in_diagnostic` tests on any
      change to error formatting (`server.rs:1982-...`, `2276-...`,
      `2485-...`).
- [ ] **Typed proxy errors, no fallback.** `OutboundConnectErrorKind` →
      `TunnelError` mapping (`client.rs:701-706`) must stay total; adding a
      new Eggress error kind that hits the `_ => Disconnected` arm is fine,
      mapping it to silent direct-dial is a finding. Confirm refusal/timeout/
      cancellation tests still pass (`server.rs:1982-...`, `2046-...`,
      `2116-...`).
- [ ] **`__` chain parsing stays delegated.** `parse_outbound_proxy` is a thin
      wrapper over `from_pproxy_uri` (`client.rs:418-425`). Do not hand-roll
      URI splitting (userinfo `@`, IPv6 `[]`, multi-hop `__`) in Eggtunnel;
      divergence from `pproxy-compat` semantics is a finding.

### 8.4 Stream-vs-connection limit confusion

- [ ] **Name the layer.** `MAX_HANDSHAKES=64` (pre-auth, process-wide,
      `server.rs:43,408,509`) ≠ `MAX_ACTIVE_CONNECTIONS_PER_SESSION=128`
      (per-session, `server.rs:41`) ≠ Eggress `1024` connection-tasks /
      `4096` stream-tasks (adapter-internal, `docs/SECURITY.md:51-53`) ≠
      `max_concurrent_streams: 256` (QUIC transport knob, `server.rs:186`,
      `client.rs:744`) ≠ client `MAX_OPEN_TASKS=128` (`client.rs:39`) ≠
      `CONTROL_QUEUE=128` (`client.rs:40`, `server.rs:42`). Any log, metric,
      or doc that says "connection limit" must say which one.
- [ ] **Admission release paths.** Pre-auth `admission` permit + `handshake_guard`
      are dropped exactly once on auth success/failure/serve entry
      (`server.rs:919-920`, `932-933`; data-fast-path `807-808`). QUIC
      `stream_admission` permits are held by the spawned data task
      (`server.rs:647-650`). Leaking either permit under a new early-return is
      a resource-exhaustion finding; the saturation/recovery test
      (`server.rs:4304-4388`) is the regression net.
- [ ] **Pre-session UDP work is outside the 64-cap.** QUIC handshake/CPU cost
      inside `eggress-transport-quic` precedes `admission.try_acquire`
      (`docs/SECURITY.md:48-50`). Do not claim the 64-cap bounds unauthenticated
      UDP packet processing; it bounds post-accept handshake tasks.

### 8.5 Half-close / relay mismatches

- [ ] **No half-close through WSS.** WS close is whole-connection
      (`docs/SECURITY.md:67-68`); `relay_with_options` over a WS `BoxStream`
      cannot observe TCP write-half-close. The qualified property is narrower:
      peer close terminates the underlying TCP promptly with no dangling halves
      (`docs/SECURITY.md:69-70`, test `server.rs:1574-...`). Do not add
      application framing that depends on half-close over WSS or QUIC streams
      without a new correlation test.
- [ ] **Relay bounds identical on both ends.** Both relays use
      `RelayOptions::bounded(16 KiB, 15 s)` (`client.rs:1129`,
      `server.rs:1190`). Changing one side's buffer/drain without the other
      alters backpressure behavior under the 1 MiB WS caps (§5.2); the bounded
      backpressure C001 case is the gate.
- [ ] **`DataHello`-then-opaque invariant.** `DataHello` is the last
      Eggtunnel message on a data stream (`docs/SECURITY.md:23`); after
      `accept_data_hello` both sides hand the stream to `relay_with_options`
      and never `read_boxed` again. Any post-`DataHello` framing change breaks
      relay pairing — review data-path changes with `handle_quic_data_stream`
      (`server.rs:668-684`), `handle_connection` data branch
      (`server.rs:805-817`), and `handle_open` (`client.rs:1074-1150`) together.

### 8.6 Quick file:line index for reviewers

| Question | Answer at |
|---|---|
| Framing semantics | `wire_io.rs:7-47`, `proto:457-525` |
| Transport neutrality | `wire_io.rs:49-57`, `lib.rs:16-31` |
| Feature gates | `crates/eggtunnel/Cargo.toml:15-23`, `Cargo.toml:22-33` |
| TLS baseline build | `client.rs:520-528`, `server.rs:126-133`, `149-155` |
| Runtime invariant | `client.rs:266-268,365-367`, `server.rs:121-123,289-291`, `lib.rs:3-5` |
| QUIC dial/accept | `client.rs:716-806,1121-1126`, `server.rs:159-212,479-684` |
| QUIC limits | `server.rs:41,43,602-606,631-635`, `docs/SECURITY.md:48-62` |
| WSS upgrade | `client.rs:598-614,1104-1115`, `server.rs:780-800` |
| WSS close semantics | `docs/SECURITY.md:64-72`, `docs/SUPPORT.md:26-30`, `server.rs:1515-...`, `1574-...` |
| Proxy dial/auth | `client.rs:413-425,687-714`, `server.rs:1730-...`, `2174-...`, `2356-...` |
| Proxy redaction | `common.rs:134-157`, `client.rs:99-109`, `server.rs:1982-...`, `docs/SECURITY.md:74-78` |
| Rejected combos | `client.rs:270-274`, `main.rs:160-194,223-231` |
| Ownership boundary | `ADR-0001:38-66`, `roadmap:16-52`, `docs/ARCHITECTURE.md:1-21` |
