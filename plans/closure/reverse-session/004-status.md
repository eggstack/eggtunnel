# M004 Closure Status — QUIC Transport

Status: closed

Disposition: closed with adapter limitations recorded. QUIC is an opt-in carrier for the existing Session and Service protocol, using one QUIC connection per Session and native bidirectional streams for control and data.

Implementation plan: `plans/implementation/reverse-session/004-quic-transport.md`.

## Baseline and implementation

- Planning baseline: `31458e83e543304d6b271898575bf2f6e98c7352` (accepted M003 head).
- Final reviewed head: recorded by the M004 implementation commit.
- Adapter: `eggress-transport-quic = 1.0.8`, using `QuicClient`, `QuicConnection`, `QuicListener`, and native `open_stream`/`accept_stream` APIs. No custom mux and no `egress-embed` dependency.
- Control role is fixed: the client opens the first bidirectional stream for Eggtunnel control. Each server Open causes the client to open a distinct bidirectional stream and send the existing DataHello.

## Requirement evidence

| Requirement | Evidence |
|---|---|
| Optional dependency boundary | `quic` gates the Eggress QUIC dependency. `cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal` contains no QUIC packages. |
| Session and service compatibility | End-to-end test registers two services on one QUIC Session, relays concurrent external TCP connections, resets one data path, and verifies another path still works. |
| Session generation replacement | Integration test stops and rebinds the QUIC server, observes a new SessionId, and confirms configured services register on the replacement Session. |
| Production certificate validation | Production-profile negative test rejects the self-signed server certificate and does not register services. |
| Bounded data stream tasks | Server uses the M003 per-session active-connection limit and an owned JoinSet for accepted stream handlers; the Eggress connection is configured with a finite bidi stream limit. |
| Config/docs | CLI selects `transport = "quic"`; it rejects mTLS and custom CA configuration because the selected adapter cannot express those settings. Support, security, architecture, configuration, operations, embedding, and README documentation state this limitation. |

## Verification

Toolchain: rustc 1.98.1, cargo 1.98.1, aarch64-apple-darwin host.

- `rtk cargo fmt --all` — passed.
- `rtk cargo test --locked --workspace --all-targets --all-features` — passed, 28 tests across 3 suites.
- `rtk cargo check --locked --workspace --all-targets --all-features` — passed.
- `rtk cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `rtk cargo doc --locked --workspace --all-features --no-deps` — passed.
- `rtk cargo test --locked -p eggtunnel --no-default-features --features client,tls` — passed.
- `rtk cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal` filtered for `quic|eggress-transport-quic` — no matches.
- Focused QUIC stream isolation/reset, reconnect, and untrusted-certificate tests — passed as part of the workspace suite.

## Limitations and residual risk

- Eggress 1.0.8 exposes platform root verification but no client custom-root or mTLS configuration. QUIC therefore supports platform-trusted server certificates only; mTLS and custom CA settings fail explicitly in CLI configuration.
- QUIC TLS handshake occurs inside the adapter before Eggtunnel's authenticated-session semaphore is acquired. Eggtunnel bounds accepted application sessions and stream work, but does not independently gate the adapter's underlying UDP/TLS handshake processing.
- Tests cover multi-service multiplexing, concurrent paths, one-path reset isolation, server-certificate rejection, and connection replacement. Wrong/stale DataHello, half-close, and stream-saturation cases do not have QUIC-specific integration tests yet; shared protocol validation and M003 admission tests remain in place.
- No hosted CI, cross-platform run, or independent security review is claimed.

## Handoff

M004 is closed. M005 may proceed on the shared stream boundary. M006 remains blocked until the intended first-release transport set is selected and closed.
