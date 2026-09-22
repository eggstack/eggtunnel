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
bounded byte totals.

This first public surface supports TCP targets. Direct application duplex
connectors and mTLS are planned for later milestones.
