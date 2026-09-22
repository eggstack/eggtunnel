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
