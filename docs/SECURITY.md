# Security model

The native TCP transport always performs TLS before sending the bearer token.
Clients verify the configured server name against either system roots or an
explicit custom CA bundle. The server rejects a bad token before creating a
Session or registering services. Token comparison uses constant-time equality,
and secret-bearing configuration Debug output is redacted.

The server chooses the effective listener address. Service binds are loopback
only unless `allow_public_service_binds` is explicitly enabled. The server
ignores the client Target as authority; only the client uses its configured
local target after a valid Open.

Embedders can use `Server::bind_with_policy` for address allowlists, port
ranges, ephemeral-port policy, and a per-session service ceiling. `BindPolicy`
is evaluated before a listener is bound. Authentication success never grants
bind permission by itself.

Each accepted external connection receives a random 128-bit ConnectionId,
bound to the current Session and Service, with a 30-second lifetime and
single-use consumption. Pending and active connection counts, services,
handshakes, and control queues have fixed ceilings. DataHello is the final
Eggtunnel message on a data stream; following bytes are opaque.

Unauthenticated accepted connections are capped at 64 concurrent handshakes.
The server retains a sliding 60-second authentication failure window for at
most 1,024 source IPs, blocks a source after 10 failures, and applies a 100 ms
delay after each failed token check. Unknown sources are rejected when the
bounded limiter table is full. This is a process-local, per-source throttle;
deployments behind a shared NAT should account for the shared source address.

The optional `mtls` feature adds `Server::bind_mtls` and
`Client::start_with_mtls`. This profile requires both a trusted client
certificate and the existing bearer token. The server maps a leaf certificate
to a Principal using SHA-256 of its DER bytes and requires that identity to
match on data connections. Trust roots are supplied explicitly to the server;
the client still validates the server name and either system or configured
roots. No enrollment or revocation service is included. Client private-key
buffers are redacted and zeroized on drop on a best-effort basis.

The ordinary TLS profile uses Eggress 1.0.8. Its public configuration builders
install the Rustls ring provider as the process default if no provider has
been set; Eggtunnel does not install a runtime or tracing subscriber.

The optional QUIC profile uses Eggress 1.0.8 with platform certificate roots
and verified SNI. The adapter currently has no custom-root or client-certificate
configuration, so QUIC rejects custom CA and mTLS settings instead of ignoring
them. QUIC still requires the bearer token inside its encrypted control stream.
Pre-session UDP/TLS handshake work runs inside the eggress-transport-quic
adapter before Eggtunnel's authenticated-session semaphore is acquired.
Eggress bounds its per-connection task fan-out at
`MAX_CONCURRENT_CONNECTION_TASKS=1024` and per-stream tasks at
`MAX_CONCURRENT_STREAM_TASKS=4096`. Eggtunnel further bounds accepted
unauthenticated handshake tasks at `MAX_HANDSHAKES=64`. The combined behavior is documented; no replacement or
vendoring of the adapter is required to keep the residual pre-session
admission risk observable. The per-session `stream_admission` semaphore caps
active data streams at `MAX_ACTIVE_CONNECTIONS_PER_SESSION=128`, with the
admission saturating, recovering, and rejecting additional streams as
qualified by stream-saturation tests. QUIC transport-specific
wrong-session, stale-session, replay, and half-close correlation cases are
qualified end-to-end.

The optional WebSocket profile establishes verified TLS before the WebSocket
upgrade and uses binary messages with a 1 MiB message limit. It is intended for
non-browser tunnel clients; the adapter does not validate Origin and makes no
browser cross-site security claim. Its close operation closes the WebSocket
connection as a whole, so TCP half-close equivalence is not promised. Peer
close during an active relay terminates the
underlying TCP connection promptly without leaving relay halves dangling, and
that multi-frame bounded backpressure round-trips within the configured 1 MiB
caps.

Outbound proxy chains are client-side only. Eggtunnel TLS and server-name
verification run over the established proxy path, protecting authentication
from a proxy that only forwards CONNECT or SOCKS traffic. Proxy credentials
should be placed in the environment variable named by `outbound_proxy_env`
and are redacted from Eggtunnel diagnostics and the public `Snapshot` view.
Proxy traversal with QUIC or mTLS is rejected. The client does not silently
fall back to direct networking when a proxy path fails; refusal, handshake
timeout, and cancellation paths produce typed termination categories. HTTP
CONNECT Basic authentication and SOCKS5 username/password authentication are
qualified as supported when the corresponding URI userinfo is supplied.
Two-hop chains using the canonical `__`-separated pproxy URI syntax are
qualified by one end-to-end SOCKS5+HTTP CONNECT integration test.
