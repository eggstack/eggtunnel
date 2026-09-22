# Embedding

Add `eggtunnel` with `default-features = false` and enable `client` plus `tls`
for a private-side client. The library uses the Tokio runtime provided by the
calling application and does not create a runtime or install a tracing
subscriber.

```toml
eggtunnel = { version = "0.1", default-features = false, features = ["client", "tls"] }
```

Create `ClientConfig` programmatically with the server socket address, TLS
server name, optional CA PEM, a `SecretToken`, and a list of `ClientService`
values. `Client::start` returns an owner and a cloneable `ClientHandle` for
snapshots and cancellation. Call `Client::shutdown().await` during normal
application shutdown so owned session and data tasks are joined.

`ClientHandle::unregister_service(id).await` removes a configured mapping from
the active Session and from subsequent reconnect registration. Snapshots
include current effective binds, pending counts, reconnect/reject counts, and
bounded byte totals. They also include resource ceilings and current/high-water
counts for handshakes, services, pending and active connections, and client
Open tasks.

The default `Client::start` uses the configured TCP Target. An application can
provide an async byte stream without a loopback socket by implementing
`TargetConnector` and calling `Client::start_with_connector`. The connector
receives the trusted client-owned `ClientService` plus a `TargetContext` with
the Session ID, ConnectionId, and cancellation token. It returns
`TargetStream`, a transport-neutral async read/write stream. The server cannot
select or rewrite the client target.

Enable `mtls` in addition to `client` and `tls` to use
`Client::start_with_mtls`; the server counterpart is `Server::bind_mtls`.
That profile keeps bearer-token auth and adds a required client certificate.
The compile-only downstream fixture in `fixtures/embedder` demonstrates a
caller-owned Tokio runtime, programmatic service configuration, caller-owned
tracing, and a direct in-process connector without the CLI dependency.

Enable the optional `quic` feature and call `Client::start_quic` to use one
QUIC connection per Session. The current Eggress adapter uses platform trust
roots and does not support custom CA bundles or mTLS; QUIC can still use a
programmatic `TargetConnector` with `start_quic_with_connector`.

Enable `websocket` to use `Client::start_websocket` or
`Client::start_websocket_with_connector`. WSS verifies the server through the
configured CA or system roots. Enable `outbound-proxy` to use
`Client::start_with_outbound_proxy` (or its connector variant) with an Eggress
proxy URI/chain. `Client::start_websocket_with_outbound_proxy` composes both
adapters. Proxy credentials should be supplied by the embedding application's
secret store. QUIC cannot currently be combined with outbound proxy traversal.
