# Eggtunnel

Authenticated TCP reverse tunnel (Rust library + CLI). A client behind NAT dials
**out** to a reachable server, which then exposes approved local services
through server-owned loopback listeners. One TLS data connection is opened per
accepted external connection.

## Install

```sh
./install.sh v0.2.0                                  # release archive
cargo build --locked --release -p eggtunnel-cli     # or build from source
```

## Quickstart

This brings up a real tunnel on a single machine: a local HTTP service, an
Eggtunnel server, and an Eggtunnel client in front of it.

**1 — Server certificate.** Eggtunnel needs a real certificate whose name
matches `tls_server_name`. For a local trial a self-signed one is fine, with two
constraints: it must be an **end-entity** certificate (`CA:FALSE`), and the key
must be PKCS#8 — `openssl ecparam -genkey` writes the SEC1 form, which Eggtunnel
does not accept.

```sh
mkdir eggtunnel-demo && cd eggtunnel-demo
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out server-key.pem
openssl req -x509 -new -key server-key.pem -out server-cert.pem -days 2 \
  -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost" \
  -addext "basicConstraints=critical,CA:FALSE"
```

**2 — Configs.** The token lives in the environment, never in the file.

```sh
export EGGTUNNEL_TOKEN="$(openssl rand -hex 32)"

cat > server.toml <<'EOF'
mode = "server"
listen_addr = "127.0.0.1:9443"
tls_cert = "server-cert.pem"
tls_key = "server-key.pem"
token_env = "EGGTUNNEL_TOKEN"
EOF

cat > client.toml <<'EOF'
mode = "client"
server_addr = "127.0.0.1:9443"
tls_server_name = "localhost"
token_env = "EGGTUNNEL_TOKEN"
ca_cert = "server-cert.pem"

[[services]]
id = 1
name = "web"
target_host = "127.0.0.1"
target_port = 8080
bind_port = 0        # 0 = ask the server for an ephemeral loopback port
EOF
```

**3 — Validate.** `check` is structural only: it never opens a socket and does
not parse the certificate, so a pass does not mean the tunnel will come up. Run
it anyway — it rejects bad combinations up front instead of at connect time.

```sh
eggtunnel check server.toml     # configuration is structurally valid
eggtunnel check client.toml
```

**4 — Run.** Expose a service, start the server, then the client in a second
terminal:

```sh
python3 -m http.server 8080 --bind 127.0.0.1   # the service to expose
eggtunnel server server.toml
eggtunnel client client.toml                   # second terminal
```

The server prints the loopback address it actually bound, with an ephemeral port
because `bind_port = 0`:

```text
server listening on 127.0.0.1:9443
service 1 session SessionId([REDACTED]) listening on [::1]:40979
```

**5 — Use it.** Traffic to that address is relayed to `127.0.0.1:8080` on the
client's side. Both processes stop on Ctrl-C.

```sh
curl http://[::1]:40979/
```

> **The client retries silently.** If it stops at
> `client started; waiting for authenticated session`, it is reconnecting with
> no further output. Almost always this is certificate verification: check that
> the certificate is `CA:FALSE` and that `ca_cert` matches `tls_server_name`.

## Library

```toml
eggtunnel = { version = "0.2", default-features = false, features = ["client", "tls"] }
```

The library uses your Tokio runtime and never installs a tracing subscriber.
Start from [API](docs/API.md) and [Embedding](docs/EMBEDDING.md).

## Docs

| Guide | Covers |
|---|---|
| [Configuration](docs/CONFIGURATION.md) | TOML reference, transports, `check`, CLI overrides, server certificate requirements |
| [Operations](docs/OPERATIONS.md) | Running server/client, events, limits, proxies, troubleshooting |
| [Security](docs/SECURITY.md) | Threat model, auth, bind policy, mTLS |
| [Transport support](docs/SUPPORT.md) | TCP/TLS, QUIC, WSS, proxy matrix + limitations |
| [Protocol](docs/PROTOCOL.md) | Wire v1.1 (1.0 fallback), capabilities |
| [Architecture](docs/ARCHITECTURE.md) | Ownership split: Eggtunnel vs Eggress vs proto |
| [Distribution](docs/DISTRIBUTION.md) | Release targets and qualification state |
| [API](docs/API.md) / [Embedding](docs/EMBEDDING.md) | Library surface, connectors, runtime policy |
