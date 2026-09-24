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
