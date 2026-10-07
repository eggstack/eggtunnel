# Changelog

## Unreleased

Post-`0.2.0` source work. **Not yet published** — no release tag exists for
this state, and `Cargo.toml` still reads `0.2.0`, which is already on crates.io.
Cutting a release requires a version bump plus a wire-version note (see
`docs/PROTOCOL.md:13-15`).

### Changed

- **Wire protocol is now v1.1** (was `1.0` in the published `0.2.0` artifact),
  with `1.0` fallback. Extension behavior is gated on negotiated capabilities
  only, never on the minor number. The crate version is independent of the wire
  version, so the crate number does not identify the wire behavior of a locally
  built binary.
  - Capability 1, correlated registration rejection: registration failures
    arrive as `RegisterReject { service_id, code, diagnostic }`, so multiple
    dynamic registrations may be in flight, bounded and generation-scoped.
    Against a `1.0` peer the one-registration-at-a-time serial fallback still
    applies exactly as before.
  - Capability 2, drain deadline: `Drain.deadline_ms` is honored as
    `min(peer, local shutdown ceiling)`, and can only shorten local shutdown.
- CLI configuration is resolved once through a single
  parse → override → resolve → validate path shared by `check` and startup,
  with redacted `--json` output (`eggtunnel.events/v1`) and
  `eggtunnel.check/v1`.
- Runtime decomposition and role-sliced optional transports
  (`quic-client`/`quic-server`, `websocket-client`/`websocket-server`), so an
  embedder can compile one transport role without the other role's code.

### Fixed

- `ClientHandle::unregister_service` no longer wedges the legacy-serial
  registration slot. Unregistering a Service while its registration was still
  in flight against a `1.0` peer used to leave the slot occupied with no reply
  channel: every later dynamic registration on that Session was refused with
  `ResourceExhausted`, one slot of the per-Session service ceiling was consumed
  for the rest of the Session, and the retained acknowledgement deadline
  eventually ended the whole Session and reconnected all Services. The
  transaction is now released and its generation tombstoned, so a late
  acknowledgement is benign cleanup.
- An `OpenReject` the client could not queue (full control queue) is now
  counted in `Snapshot::rejected_connections` and logged as
  `open_reject_dropped`. It was previously discarded silently while the server
  kept the pending entry — and its admission permit — until
  `pending_connection` expired.
- `eggtunnel server` and `eggtunnel client` register the Ctrl-C handler once
  instead of rebuilding the signal future on every 250 ms tick, which left
  brief windows with no live listener.

### Performance

- The effective-bind table is copy-on-write behind its mutex, so `Snapshot`
  copies it outside the critical section instead of blocking service
  registration and unregistration for the length of a full table clone.
- Added `ServerHandle::effective_binds` for callers that poll only for bind
  changes, and the default (non-`--json`) client CLI path no longer builds a
  full `Snapshot` four times a second. Additive: no existing API changed.
- Server shutdown drain wakes on Session registration instead of re-enumerating
  the Session registry every 25 ms for the whole grace period.

### Unchanged

- No configuration or Rust API break for existing users: existing TOML files
  remain valid, wire-`1.0` peers keep baseline semantics, and the transport
  rejection matrix is unchanged.
- Runtime limits and timeouts are unchanged and still configurable only through
  the library `RuntimePolicy`. A standalone TOML surface for them is planned
  (M020) and **not implemented**.

## 0.2.0 — 2026-09-24

The post-0.1 line adds material additive capability to the public library API
and hardens the existing reverse-session implementation. Wire protocol
compatibility remains version 1.0. The crate version is independent of the wire
version.

Published release artifacts:

- GitHub release [`v0.2.0`](https://github.com/eggstack/eggtunnel/releases/tag/v0.2.0)
  carries the four target archives (`x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`,
  `aarch64-apple-darwin`), the global `SHA256SUMS` manifest,
  `install.sh`, and build provenance attestations.
- `eggtunnel-proto 0.2.0` and `eggtunnel 0.2.0` are published on
  crates.io in dependency order.
- Qualification evidence: `plans/closure/reverse-session/012-status.md`
  (release-candidate qualification) + `plans/closure/reverse-session/013-status.md`
  (publication event).

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
- `cargo audit` finds no known vulnerabilities and one target-gated,
  transitive unmaintained advisory for `atomic-polyfill 1.0.3`; see
  `docs/DISTRIBUTION.md` and the M012 closure record for its disposition.

### Pre-1.0 compatibility note

Public Rust API changes may be breaking across minor releases until `1.0`.
Compile against the exact version selected by the downstream lockfile.
`eggtunnel-proto` types and message IDs are wire-facing and require extra care;
see [the protocol guide](docs/PROTOCOL.md).

## 0.1.0 — initial published release

First public release. Authenticated TCP/TLS reverse-session library and CLI,
optional QUIC, WebSocket, outbound-proxy, and mTLS profiles, four supported
binary targets, and supply-chain review (`cargo audit` 0 vulns,
`cargo deny check licenses` pass). See
[`v0.1.0`](https://github.com/eggstack/eggtunnel/releases/tag/v0.1.0) for
release assets and the M006 closure record for qualification evidence.
