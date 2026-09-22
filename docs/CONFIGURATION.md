# Configuration

The CLI reads a TOML file. Credentials are read from an environment variable
named by `token_env`; the token is not stored in the configuration file.

## Client

```toml
mode = "client"
transport = "tcp_tls" # or "quic" (UDP)
server_addr = "tunnel.example.net:9443"
tls_server_name = "tunnel.example.net"
token_env = "EGGTUNNEL_TOKEN"
# Optional private root bundle. Without it the system root set is used.
# ca_cert = "/etc/eggtunnel/server-ca.pem"
# Optional mTLS identity. Configure both fields to require/use a client cert.
# client_cert = "/etc/eggtunnel/client-chain.pem"
# client_key = "/etc/eggtunnel/client-key.pem"

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
transport = "tcp_tls" # or "quic" (UDP; service listeners remain TCP)
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

Run `eggtunnel check <file>` to validate the TOML structure, required paths,
service names, endpoint syntax, and token environment variable. Server startup
also parses and validates its certificate and key.
