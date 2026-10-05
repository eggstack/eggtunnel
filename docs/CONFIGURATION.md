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
# URI examples include http://user:pass@proxy:3128, socks5://user:pass@proxy:1080,
# and chains such as socks5://proxy:1080__http://proxy:8080. Credentials are
# passed via URI userinfo and redacted from diagnostics and the public Snapshot.
# Keep proxy credentials out of this file.
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

### Server certificate requirements

`tls_cert` must be a PEM certificate chain and `tls_key` a PEM private key.
Three constraints are easy to trip over, and all of them fail at startup or at
connect time rather than during `check`:

- The certificate must be an **end-entity** certificate, not a CA. Rustls
  rejects a presented certificate whose `basicConstraints` asserts
  `CA:TRUE`. `openssl req -x509` adds `CA:TRUE` by default, so a self-signed
  test certificate needs an explicit
  `-addext "basicConstraints=critical,CA:FALSE"`.
- The private key must be **PKCS#8** (`-----BEGIN PRIVATE KEY-----`).
  `openssl genpkey` and `openssl req -newkey rsa:2048` produce this form;
  `openssl ecparam -genkey` produces the SEC1 form
  (`-----BEGIN EC PRIVATE KEY-----`), which is rejected. Convert with
  `openssl pkcs8 -topk8 -nocrypt -in key.pem -out key-p8.pem`.
- The certificate must carry a subject alternative name matching
  `tls_server_name`, which is also the SNI name sent to the server.

A working self-signed pair for a local trial:

```sh
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out server-key.pem
openssl req -x509 -new -key server-key.pem -out server-cert.pem -days 2 \
  -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost" \
  -addext "basicConstraints=critical,CA:FALSE"
```

Clients verify against the system roots unless `ca_cert` names a bundle, so a
self-signed server also needs `ca_cert` pointed at the same certificate.

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
families in Eggress 1.0.8 include direct, HTTP CONNECT, and SOCKS5; chains
use the `__` separator. HTTP CONNECT Basic authentication and SOCKS5
username/password authentication are honored when supplied as URI userinfo;
credentials remain in the environment variable and are redacted from
Eggtunnel diagnostics and the public `Snapshot` view. Proxy traversal over
QUIC and proxy+mTLS are rejected. Multi-hop chains use the canonical
`__`-separated pproxy URI syntax (e.g.
`socks5://proxy1:1080__http://proxy2:8080`) and are qualified end-to-end
through one SOCKS5+HTTP CONNECT integration test.

Run `eggtunnel check <file>` to validate the TOML structure, required paths,
service names, endpoint syntax, and token environment variable. The named
environment variables must be set and referenced files must exist for `check`
to pass; their contents are not validated (no PEM parsing, DNS, or dialing). Server startup
also parses and validates its certificate and key.

`eggtunnel check --json <file>` emits a single versioned JSON object
(`eggtunnel.check/v1`) with `ok`, `mode`, `transport`, service count,
`custom_ca`/`mtls`/`outbound_proxy` booleans, and a stable
`error.category` on failure. Secrets, file contents, and paths never
appear in the JSON; only booleans and the coarse error category do.

## Command-line overrides

Every non-secret field can be overridden on the command line for service
managers and automation. Precedence is always
CLI override > TOML field/default > built-in default:

- client: `--server-addr`, `--tls-server-name`, `--transport`,
  `--ca-cert`, `--token-env`, `--outbound-proxy-env`, `--client-cert`,
  `--client-key`, `--bind-port` (single-service files only, so the
  selector stays deterministic).
- server: `--listen-addr`, `--transport`, `--tls-cert`, `--tls-key`,
  `--client-ca`, `--token-env`, `--allow-public-service-binds`
  (one-way enable).

There is deliberately no `--token` or proxy-password flag: secret
*values* never appear in arguments (they would leak to process listings
and shell history). The `*_env` flags only rename which environment
variable is read. Meaningless combinations for the selected
mode/profile (for example QUIC with a custom CA) are rejected by the
same library validator used at startup.

The CLI uses default runtime limits and timeouts. Rust embedders can select
finite non-default limits and timeout values with `RuntimePolicy` on
`ClientBuilder` and `ServerBuilder`; these settings are programmatic and are
not secret-bearing TOML fields.
