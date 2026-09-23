# Architecture

Eggtunnel owns reverse-session behavior. Eggress provides generic
relay and transport primitives. `eggtunnel-proto`
is runtime-neutral and owns bounded wire DTOs and framing; it has no socket,
async runtime, timer, or task dependencies.

The current implementation includes the TCP/TLS baseline: one authenticated
control stream, server-owned service listeners, single-use pending connection
correlations, and one TLS data stream per external connection. Eggress relay
copies opaque application bytes. Optional mTLS and direct application target
connectors are feature/API-gated. QUIC is feature-gated and uses one
bidirectional stream per control or data path. WSS wraps each TCP/TLS control
and data connection in a binary WebSocket byte stream. The optional outbound
proxy adapter dials each client connection before Eggtunnel TLS; it does not
start a local proxy listener or change the Session protocol. Outbound proxy
support covers direct, HTTP CONNECT, and SOCKS5 single-hop profiles, optional
Basic HTTP CONNECT authentication, optional SOCKS5 username/password
authentication, and multi-hop chains through the canonical `__`-separated
pproxy URI syntax. Dependency direction: Eggtunnel owns reverse-session
behavior and selects transport adapters; generic relay and transport
implementations come from Eggress. See [transport support](SUPPORT.md) for
the profile matrix.
