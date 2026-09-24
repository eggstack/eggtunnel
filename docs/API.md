# Public Rust API

## Package roles

| Package | Role | Publication |
|---|---|---|
| `eggtunnel` | Primary embeddable client/server library | Intended for crates.io |
| `eggtunnel-proto` | Runtime-neutral wire DTOs and codec for protocol tooling | Intended for crates.io; wire types are semver-sensitive |
| `eggtunnel-cli` | Standalone product binary and configuration frontend | `publish = false`; install from release archives |

The CLI is a consumer of the library. A downstream library should depend on
`eggtunnel` with `default-features = false` and should never depend on the CLI.

## Recommended client surface

For a private-side client using TCP/TLS:

```toml
eggtunnel = { version = "0.2", default-features = false, features = ["client", "tls"] }
```

Use `ClientConfig`, `ClientService`, `SecretToken`, and `Client::start` for
loopback TCP targets. Use `TargetConnector` and
`Client::start_with_connector` when the application owns destination dialing.
Use `ClientHandle::snapshot` for status, `unregister_service` for an active
session mapping, and `Client::shutdown().await` for joined shutdown.
Programmatic clients may start with an empty `services` list when all mappings
will be added dynamically after connection; the CLI still requires at least one
configured service.

`ClientHandle::register_service(service).await` registers a Service in the
current authenticated Session and returns the server-assigned `EffectiveBind`.
Only an acknowledged registration is restored after reconnect. Registration
while disconnected returns `TunnelError::Disconnected`; duplicate IDs or
names return `ServiceAlreadyExists`; server bind denial returns
`Authorization`; runtime capacity rejection returns `ResourceExhausted`.
Unregistration is idempotent, removes desired state, and remains removed after
reconnect. Dropping a registration future before its acknowledgement causes
the client to unregister it when the acknowledgement arrives.

For composed deployments, `ClientBuilder` accepts a typed
`ClientTransportProfile`, optional `TargetConnector`, and a validated
`RuntimePolicy`. Call `validate()` before `start()` when configuration is
assembled in stages. `ServerBuilder` provides the equivalent server surface
with `BindPolicy`, transport profile, optional client CA, and runtime policy.
Both builders use the same profile validation as the legacy convenience
constructors.

Optional client transport features are `quic`, `websocket`, and
`outbound-proxy`. Optional `mtls` adds certificate identity to TCP/TLS. Their
configuration limits are in [the support matrix](SUPPORT.md).

## Recommended server surface

Use `ServerConfig`, `Server::bind`, and `ServerHandle`. The server owns
requested service listeners and authorizes binds through `BindPolicy`.
`ServerHandle::snapshot` reports current and high-water resource counts.
`Server::shutdown().await` sends a bounded drain before joining the server
task. `Server::bind_mtls` is available with the `mtls` feature.

The runtime is caller-owned. Eggtunnel does not create a Tokio runtime or
install a tracing subscriber. The library emits structured `tracing` events
when a subscriber is present; applications control filters and output.
Heartbeat age, latest RTT, missed probes, and session generation are available
in `Snapshot.heartbeat`. Secret token/key values should come from the embedding
application's secret store.

## Compatibility

The current published crates.io line is `0.2.0` (see
[distribution](DISTRIBUTION.md)); the wire protocol remains version `1.0`.
Public Rust API changes may be breaking across minor releases before `1.0`;
compile against the exact version selected by the downstream lockfile.
`eggtunnel-proto` types and message IDs are wire-facing and require extra
care: see [protocol compatibility](PROTOCOL.md).

The `fixtures/embedder` crate is a downstream-shaped compile check that uses
only public imports, disabled default features, the caller's Tokio runtime,
caller-owned tracing, programmatic configuration, and a direct connector.
