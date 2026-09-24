# Operations

Start the server with `eggtunnel server server.toml`; start a private-side
client with `eggtunnel client client.toml`. Both processes stop on Ctrl-C. The
server sends a bounded Drain notification before closing active sessions.

The server's `listen_addr` accepts both control sessions and reverse data
connections. Every accepted connection begins with TLS. A service's actual
address is server-assigned and is available through the Rust `ServerHandle`
snapshot. The CLI prints newly assigned service addresses while it is running.
For QUIC, `listen_addr` is the UDP control endpoint; service listeners still
bind TCP on the requested interface and port.

The client retries transient connection, TLS, and protocol failures with
bounded exponential backoff and jitter. Invalid authentication or service
authorization stops retries. Client service registrations are restored after
a new authenticated Session.

Use a trusted certificate whose subject alternative names include
`tls_server_name`. Keep token and private-key files readable only by the
service account. Service listeners are loopback-only unless public binding was
explicitly enabled.

The server bounds accepted unauthenticated handshakes to 64 at once and active
Sessions to 128. Per Session, it allows up to 64 services, 128 pending
connections, and 128 active connections. Client Open tasks and control queues
are bounded at 128; the ClientHandle command queue is bounded at 32.
Authentication failures are tracked per source IP
for 60 seconds; the tenth failure blocks that source until its window expires.

Runtime snapshots expose current counts and high-water values for Sessions,
Services, Pending Connections, active Connections, client Open tasks, and
unauthenticated handshakes. These counters cover one process lifetime and do
not persist across restart.
The snapshot includes the most recent bounded termination category and a
count of panicked owned tasks; it never retains event history or error text.
Client snapshots also expose bounded heartbeat health for the current
Session: generation, last matching Pong age, latest RTT, and consecutive
missed intervals. A missed probe does not create a reconnect attempt by itself;
the client keeps at most one outstanding Ping and relies on transport/session
failure handling for reconnect.

Embedders can set finite resource ceilings and timeout values through
`RuntimePolicy` on `ClientBuilder` and `ServerBuilder`. Defaults preserve the
values above. Policy validation limits configured values to finite ranges and
requires reconnect bounds and heartbeat timing to remain consistent. The CLI
uses these defaults and validates transport compatibility through the same
library builders used at startup.

Runtime registrations are acknowledged before they enter desired state.
`unregister_service` removes desired state before reconnect can register the
Service again; it is idempotent. Configure an application tracing subscriber
to collect the library's structured lifecycle events. Eggtunnel does not
initialize global tracing state.

For restricted egress networks, set `outbound_proxy_env` on the client to a
variable containing an HTTP CONNECT or SOCKS5 proxy URI/chain. Eggtunnel TLS
remains end to end and uses `tls_server_name` for verification. WebSocket mode
uses WSS on the configured TCP endpoint. QUIC uses UDP and does not support
outbound proxy traversal.

Outbound proxy support covers single-hop HTTP CONNECT, single-hop SOCKS5,
optional Basic HTTP CONNECT authentication, optional SOCKS5 username/password
authentication, and multi-hop chains using the canonical `__`-separated pproxy
URI syntax. Proxy credentials are placed in the environment variable named by
`outbound_proxy_env`; they are never formatted into diagnostics or the
public `Snapshot` view, and proxy failures are reported only as typed
termination categories.

## Developer sustained qualification

Long-running qualification is opt-in and stays out of routine CI latency.
From the repository root, run the decoder fuzzer with a nightly toolchain and
the checked-in seed corpus:

```sh
cargo +nightly fuzz run decode_frame fuzz/corpus/decode_frame -- -max_total_time=60 -max_len=1048590
```

Run deterministic lifecycle state sequencing and the bounded TCP/TLS
reconnect/churn soak explicitly:

```sh
cargo test --locked -p eggtunnel --all-features deterministic_service_state_sequence_preserves_invariants_for_10000_steps -- --nocapture
cargo test --locked --release -p eggtunnel --all-features qualification_tcp_tls_reconnect_and_connection_churn_soak -- --ignored --nocapture --test-threads=1
```

The soak performs 20 client/server Session cycles and 200 loopback relays with
64-byte and 64-KiB payloads at a maximum concurrency of four. Its timing and
throughput output is host-specific informational evidence. It asserts that
Session, pending-connection, active-connection, and client Open-task counts
converge after teardown.

When optional transports are enabled, repeat their bounded sustained paths:

```sh
for repeat_index in 1 2 3; do cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,quic quic_connection_replacement_creates_new_session_and_reregisters_services -- --test-threads=1 || exit; done
for repeat_index in 1 2 3; do cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,websocket websocket_tls_session_registers_and_relays_data_paths -- --test-threads=1 || exit; done
for repeat_index in 1 2 3; do cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,websocket wss_payload_larger_than_message_cap_roundtrips_multiple_frames -- --test-threads=1 || exit; done
cargo test --locked --release -p eggtunnel --all-features qualification_quic_stream_churn_soak -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p eggtunnel --all-features qualification_wss_connection_churn_soak -- --ignored --nocapture --test-threads=1
```

Record release footprint and the minimal normal dependency graph on the same
host:

```sh
cargo build --locked --release -p eggtunnel-cli --all-features
wc -c target/release/eggtunnel
cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal
```
