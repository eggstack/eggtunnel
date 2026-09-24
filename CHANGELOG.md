# Changelog

## 0.2.0 candidate — not yet published

This candidate expands the pre-1.0 public library API and hardens the existing
reverse-session implementation. Wire protocol compatibility remains version
1.0. The crate version is independent of the wire version.

### Added and clarified

- Typed `ClientBuilder` and `ServerBuilder` composition for validated
  transport profiles, caller-provided connectors, bind policy, and finite
  `RuntimePolicy`, `ResourceLimits`, and `TimeoutPolicy` configuration.
- Dynamic client Service registration and unregistration with server-assigned
  binds, reconnect persistence only after a matching acknowledgement, and a
  one-registration-at-a-time limit because wire `Error` has no ServiceId.
- Bounded heartbeat health in `Snapshot`: session generation, last matching
  Pong age, latest RTT, and consecutive missed intervals. Only one Ping is
  outstanding at a time.
- Structured `tracing` events for session and Service lifecycle, admission and
  rejection, DataHello correlation, relay completion, and shutdown. The
  library uses the caller's runtime and never installs a tracing subscriber.
- Sustained qualification tooling and evidence: bounded protocol fuzzing,
  deterministic Service-state stress, TCP/TLS lifecycle and connection churn,
  and optional QUIC/WSS churn. Host-specific measurements are qualification
  evidence, not throughput guarantees.

### Compatibility and operational boundaries

- Supported binary targets remain `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, and
  `aarch64-apple-darwin`. Windows, musl, armv7, Raspberry Pi, and Le Potato
  remain unsupported.
- QUIC rejects custom CA and mTLS; WSS rejects mTLS; outbound proxy rejects
  QUIC and mTLS. WebSocket relay does not promise TCP half-close semantics.
- There is no self-update engine. The CLI remains a private archive binary.
- Candidate `cargo audit` found no known vulnerabilities and one
  target-gated, transitive unmaintained advisory for `atomic-polyfill 1.0.3`;
  see `docs/DISTRIBUTION.md` and the M012 closure record for its disposition.

The candidate is not a published release. Do not create tag `v0.2.0`, publish
crates, or publish a GitHub release until explicit owner authorization.
