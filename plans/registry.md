# Eggtunnel Active Planning Registry

This file is the compact control surface for active Eggtunnel planning. Detailed requirements remain in canonical plans, ADRs, subsystem roadmaps, implementation plans, closure records, and Git history.

Canonical direction:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md

Accepted architectural decisions:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

## Status vocabulary

- proposed — document exists but is not approved/dependency-ready.
- ready — dependencies/interfaces are satisfied; implementation may begin.
- active — implementation is in progress.
- blocked — a named dependency/evidence condition prevents progress.
- closing — implementation landed and closure evidence is being assembled.
- closed — closure record accepted.
- conditionally closed — substantial work landed but a named correctness/evidence condition remains.
- superseded — replaced by another document.
- archived — retained for traceability but no longer active.

## Active subsystem roadmaps

| Subsystem | Status | Roadmap | Current milestone | Dependencies / blockers |
|---|---|---|---|---|
| Reverse session | active | plans/subsystems/reverse-session-roadmap.md | M009 ready | M001-M008 and C001 are closed; M009 is ready under ADR-0001. |
| Reverse session post-closure corrective | active | plans/subsystems/reverse-session-post-closure-corrective-addendum.md | C001 closed (historical) | No implementation dependency. C001 closure record at plans/closure/reverse-session-post-closure-corrective/001-status.md supplies supplemental evidence; the corrective addendum itself remains active for traceability. |

## Active and ready implementation plans

| Subsystem | Milestone | Status | Implementation plan | Dependencies / handoff note |
|---|---|---|---|---|
| Reverse session | M009 dynamic Service lifecycle/observability | ready | plans/implementation/reverse-session/009-dynamic-service-lifecycle-and-operational-observability.md | M008 strict closure recorded at plans/closure/reverse-session/008-status.md; no new ADR is needed while existing wire semantics are preserved. |
## Recently closed implementation plans

| Subsystem | Milestone | Status | Implementation plan | Closure record / note |
|---|---|---|---|---|
| Reverse session | M008 configurable runtime policy/API composition | closed | plans/implementation/reverse-session/008-configurable-runtime-policy-and-api-composition.md | plans/closure/reverse-session/008-status.md; canonical builders, finite configurable policy, supported profile validation, and hosted qualification. |
| Reverse session | M007 maintainability/continuous qualification | closed | plans/implementation/reverse-session/007-maintainability-and-continuous-qualification.md | plans/closure/reverse-session/007-status.md; Rust 1.89, seven feature slices, minimal graph, PEM parser replacement, and full hosted CI passed. |
| Reverse session | M006 distribution/downstream qualification | closed | plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md | plans/closure/reverse-session/006-status.md; tag v0.1.0, hosted 4-target release with install/version smoke, crates.io publication of eggtunnel-proto then eggtunnel, registry-consumer qualification, audit (0 vulns) + deny licenses pass. |
| Reverse session post-closure corrective | C001 optional-transport qualification and planning reconciliation | closed | plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md | plans/closure/reverse-session-post-closure-corrective/001-status.md; M004/M005 historical closure records remain unchanged; supplemental evidence adds QUIC wrong/stale/replay/saturation/half-close, WSS close-during-relay and multi-frame backpressure, proxy refusal/timeout/cancellation, HTTP CONNECT and SOCKS5 authentication success/failure, and two-hop SOCKS5+HTTP CONNECT chain evidence. |
| Reverse session | M005 restricted-network transports/proxy traversal | closed | plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md | plans/closure/reverse-session/005-status.md; historical closure retains explicit proxy/WSS qualification limitations; C001 supplemental evidence covers the listed cases. |
| Reverse session | M004 QUIC transport | closed | plans/implementation/reverse-session/004-quic-transport.md | plans/closure/reverse-session/004-status.md; historical closure retains explicit QUIC qualification limitations; C001 supplemental evidence covers the listed cases. |
| Reverse session | M003 security/lifecycle/resource/embedding hardening | closed | plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md | plans/closure/reverse-session/003-status.md; final reviewed head 31458e83e543304d6b271898575bf2f6e98c7352. |
| Reverse session | M002 TCP/TLS reverse-tunnel product | closed | plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md | plans/closure/reverse-session/002-status.md; implementation head 13402200e51b46031a1a82240be1eb48027a09f4. |
| Reverse session | M001 repository and protocol foundation | closed | plans/implementation/reverse-session/001-repository-and-protocol-foundation.md | plans/closure/reverse-session/001-status.md; implementation head 357480b942e95ef7087d86a043f26b7a3d175687. |

## Closure work

### C001 optional-transport corrective (closed)

C001 closure record at plans/closure/reverse-session-post-closure-corrective/001-status.md
records the supplemental transport-specific evidence. No new high/medium
correctness or security findings remain open. M004/M005 historical closure
records are preserved unchanged.

### M006 distribution/downstream qualification (closed)

M006 closure record at plans/closure/reverse-session/006-status.md records
the hosted 4-target release, crates.io publication (proto then library),
downstream registry-consumption, advisory/license review, and the supported
platform claims. No high/medium correctness or security findings remain open.
Post-0.1 follow-up is now sequenced through M007-M009. M007 owns PEM-parser
maintenance, structural cleanup, and continuous MSRV/feature qualification;
M008 owns runtime policy/API composition; M009 owns dynamic Service lifecycle
and bounded tracing/heartbeat observability.

## Current architecture constraints

The following are not optional implementation preferences:

- Eggtunnel is a thin reverse-session layer, not another proxy/VPN stack.
- Generic byte relay uses the Eggress relay boundary rather than a duplicate data plane.
- Optional Eggress QUIC/WebSocket/outbound transport components remain feature-gated.
- eggress-embed is not the default dependency boundary.
- Synvoid and i2pr are architecture references, not production dependencies.
- Baseline TCP/TLS uses one persistent control Session plus separate reverse Data Connections.
- No custom TCP stream multiplexer is authorized.
- QUIC uses transport-native bidirectional streams.
- WebSocket is not claimed to provide transparent TCP half-close unless direct evidence proves it.
- Non-loopback operation is encrypted/authenticated and server bind policy is explicit.
- ConnectionId is random, short-lived, single-use, Service-bound, and Session-bound.
- No production unbounded channels/tasks.
- Library APIs remain process-neutral for downstream embedding.

## Blocked / future work

These are not M007 implementation blockers:

- protocol capability/minor-version negotiation remains unplanned until a concrete extension exists and an ADR defines compatibility semantics;
- Eggpack release/bootstrap/CI cutover remains unplanned until Eggpack exposes stable build/qualification, bootstrap-installer, and generated-CI interfaces that can preserve M006 evidence;
- Eggup consumer-side self-update integration remains deferred; do not copy Eggup transaction machinery into Eggtunnel;
- Windows, musl, armv7, Raspberry Pi, and Le Potato release qualification remain future support work requiring explicit evidence;
- QUIC custom CA/mTLS and QUIC proxy traversal remain unsupported current profiles unless a later plan/ADR changes the transport boundary.
