# Configuration

The CLI reads a TOML file. Credentials are read from an environment variable
named by `token_env`; the token is not stored in the configuration file.

## Client

```toml
mode = "client"
transport = "tcp_tls" # or "quic" (UDP) or "websocket_tls"
server_addr = "tunnel.example.net:9443"
tls_server_name = "tunnel.example.net"
token_env = "EGGTUNNEL_TOKEN"
# Optional private root bundle. Without it the system root set is used.
# ca_cert = "/etc/eggtunnel/server-ca.pem"
# Optional mTLS identity. Configure both fields to require/use a client cert.
# client_cert = "/etc/eggtunnel/client-chain.pem"
# client_key = "/etc/eggtunnel/client-key.pem"
# Optional outbound proxy chain, read from this environment variable. Supported
# URI examples include http://proxy:3128 and socks5://proxy:1080. Chains use
# Eggress's __ separator. Keep proxy credentials out of this file.
# outbound_proxy_env = "EGGTUNNEL_OUTBOUND_PROXY"

[[services]]
id = 1
name = "web"
target_host = "127.0.0.1"
target_port = 8080
bind_port = 0 # ask the server for an ephemeral loopback listener
```

`bind_port` requests the server-side service port. Port zero requests an
ephemeral port. `target_host` and `target_port` are always client-owned; the
server cannot change the client's local destination.

## Server

```toml
mode = "server"
transport = "tcp_tls" # or "quic" (UDP; listeners remain TCP) or "websocket_tls"
listen_addr = "0.0.0.0:9443"
tls_cert = "/etc/eggtunnel/server-chain.pem"
tls_key = "/etc/eggtunnel/server-key.pem"
token_env = "EGGTUNNEL_TOKEN"
allow_public_service_binds = false
# Optional mTLS client trust roots; when present the server requires a
# trusted client certificate as well as the bearer token.
# client_ca = "/etc/eggtunnel/client-ca.pem"
```

Service binds are loopback-only by default. `allow_public_service_binds = true`
enables explicit non-loopback binds requested by authenticated clients. The
control endpoint itself is TLS protected and requires the token after TLS.

The QUIC profile uses operating-system certificate roots and bearer-token
authentication. The current Eggress QUIC adapter does not accept custom CA
bundles or mTLS identity material; `eggtunnel check` rejects those combinations.
The QUIC control endpoint uses UDP on `listen_addr`; approved service listeners
continue to use TCP.

The `websocket_tls` profile performs verified TLS first, then upgrades each
control or data TCP connection to a binary WebSocket stream. It supports
configured CA roots and bearer-token authentication. The current profile does
not support mTLS. This is a non-browser tunnel endpoint; Origin is not a
browser security boundary.

For clients, `outbound_proxy_env` selects a listener-free Eggress outbound
chain. Proxying establishes the TCP path before Eggtunnel TLS, so the tunnel
still authenticates the configured server name end to end. Supported URI
families in Eggress 1.0.8 include direct, HTTP CONNECT, and SOCKS5; chains use
the `__` separator. Proxy traversal over QUIC and proxy+mTLS are rejected.
Put proxy credentials in the named environment variable, not TOML.

Run `eggtunnel check <file>` to validate the TOML structure, required paths,
service names, endpoint syntax, and token environment variable. Server startup
also parses and validates its certificate and key.
