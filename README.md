# Eggtunnel

Eggtunnel is a Rust TCP/TLS reverse-tunnel library and CLI: a process behind
NAT connects outward to a reachable server, which exposes approved local
services through server-owned listeners.

The client establishes an authenticated TLS session, registers multiple TCP
services, and opens a separate TLS data connection per accepted external
connection. See the [configuration guide](docs/CONFIGURATION.md),
[security model](docs/SECURITY.md), and [roadmap](plans/subsystems/reverse-session-roadmap.md).

```sh
eggtunnel check client.toml
EGGTUNNEL_TOKEN='use-a-high-entropy-secret' eggtunnel client client.toml
```

QUIC, WebSocket, outbound-proxy traversal, mTLS, and release packaging remain
roadmap items.
