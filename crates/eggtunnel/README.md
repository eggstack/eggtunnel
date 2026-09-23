# eggtunnel

Embeddable Eggtunnel reverse-tunnel library: authenticated control session,
multi-service registration, and per-connection data relay over TCP/TLS, with
optional QUIC, WebSocket, outbound-proxy, and mTLS profiles.

The library uses the caller's Tokio runtime and installs no runtime or
tracing state. See [the embedding guide](../../docs/EMBEDDING.md) and
[the API guide](../../docs/API.md).
