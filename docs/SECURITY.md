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

Each accepted external connection receives a random 128-bit ConnectionId,
bound to the current Session and Service, with a 30-second lifetime and
single-use consumption. Pending and active connection counts, services,
handshakes, and control queues have fixed ceilings. DataHello is the final
Eggtunnel message on a data stream; following bytes are opaque.

TLS configuration uses Eggress 1.0.8. Its public configuration builders install
the Rustls ring provider as the process default if no provider has been set;
Eggtunnel does not install a runtime or tracing subscriber. Mutual TLS is not
implemented in this milestone.

