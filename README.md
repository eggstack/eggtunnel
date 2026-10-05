# Eggtunnel

Authenticated TCP reverse tunnel (Rust library + CLI): a client behind NAT
dials out to a reachable server, which exposes approved local services
through server-owned loopback listeners. One TLS data connection is opened
per accepted external connection.

## Install

Release archives (Linux/macOS, x64/arm64):

```sh
./install.sh v0.2.0
```

Or build from source:

```sh
cargo build --locked --release -p eggtunnel-cli
./target/release/eggtunnel version
```

Library embedders: see [Embedding](docs/EMBEDDING.md)
(`eggtunnel = { version = "0.2", default-features = false, features = ["client", "tls"] }`).

## Quickstart

Copy `examples/client.toml` / `examples/server.toml`, point the server file
at a real certificate/key, then validate (the token variable must be set —
`check` verifies it exists, not its value):

```sh
export EGGTUNNEL_TOKEN='use-a-high-entropy-secret'
eggtunnel check server.toml
eggtunnel check client.toml
```

Run the server, then the client:

```sh
eggtunnel server server.toml
EGGTUNNEL_TOKEN='use-a-high-entropy-secret' eggtunnel client client.toml
```

When the session is up the server prints one line per bound service, with
the actual port (`bind_port = 0` asks for an ephemeral one; the address may
be IPv6 loopback):

```text
server listening on 127.0.0.1:9443
service 1 session SessionId(267e4a11…) listening on [::1]:40023
```

Connect through the printed address — traffic is relayed to the client's
configured `target_host:target_port`. Secrets always come from environment
variables (`token_env`); they never go in the TOML file. Both processes stop
on Ctrl-C.

## Docs

| Guide | Covers |
|---|---|
| [Architecture](docs/ARCHITECTURE.md) | Ownership split: Eggtunnel vs Eggress vs proto |
| [Configuration](docs/CONFIGURATION.md) | TOML reference, transports, `check`, CLI overrides |
| [Operations](docs/OPERATIONS.md) | Running server/client, events, limits, proxies |
| [Security](docs/SECURITY.md) | Threat model, auth, bind policy, mTLS |
| [Transport support](docs/SUPPORT.md) | TCP/TLS, QUIC, WSS, proxy matrix + limitations |
| [Protocol](docs/PROTOCOL.md) | Wire v1.1 (1.0 fallback), capabilities |
| [Distribution](docs/DISTRIBUTION.md) | Release targets and qualification state |
| [API](docs/API.md) / [Embedding](docs/EMBEDDING.md) | Library surface, connectors, runtime policy |
