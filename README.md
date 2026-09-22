# Eggtunnel

Eggtunnel is a Rust TCP/TLS reverse-tunnel library and CLI: a process behind
NAT connects outward to a reachable server, which exposes approved local
services through server-owned listeners.

The client establishes an authenticated TLS session, registers multiple TCP
services, and opens a separate TLS data connection per accepted external
connection. Embedders can provide a direct application stream connector, and
the optional `mtls` feature adds certificate authentication alongside the
bearer token. See the [configuration guide](docs/CONFIGURATION.md),
[security model](docs/SECURITY.md), [transport support](docs/SUPPORT.md), and
[roadmap](plans/subsystems/reverse-session-roadmap.md).

```sh
eggtunnel check client.toml
EGGTUNNEL_TOKEN='use-a-high-entropy-secret' eggtunnel client client.toml
```

QUIC is available with the optional `quic` feature and `transport = "quic"` CLI
setting. Optional `websocket` support provides WSS, and `outbound-proxy` adds
listener-free direct, HTTP CONNECT, and SOCKS5 traversal. Proxy credentials are
read from an environment variable; see the configuration guide and support
matrix for limitations and tested combinations. Release packaging remains a
roadmap item.
