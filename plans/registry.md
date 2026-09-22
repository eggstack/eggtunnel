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
| Reverse session | active | plans/subsystems/reverse-session-roadmap.md | M006 active | M001-M005 historically closed; M006 strict closure additionally depends on post-closure C001 and M006 release-evidence gates. |
| Reverse session post-closure corrective | active | plans/subsystems/reverse-session-post-closure-corrective-addendum.md | C001 ready | No implementation dependency; operates against current head and historical M004/M005 closure evidence. |

## Active implementation plans

| Subsystem | Milestone | Status | Implementation plan | Dependencies / handoff note |
|---|---|---|---|---|
| Reverse session | M006 distribution/downstream qualification | active | plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md | Release-qualification foundation landed at fc19fe57a32d0c45be339ff3dd80044c4a8bc069. Strict closure waits on C001 plus hosted release-target, advisory/license, publication, and downstream-registry evidence. |

## Dependency-ready implementation plans

| Subsystem | Milestone | Status | Implementation plan | Dependencies / handoff note |
|---|---|---|---|---|
| Reverse session post-closure corrective | C001 optional-transport qualification and planning reconciliation | ready | plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md | Execute against baseline fc19fe57a32d0c45be339ff3dd80044c4a8bc069 or current descendant head after inspection; preserve non-conflicting M006 work. |

## Recently closed implementation plans

| Subsystem | Milestone | Status | Implementation plan | Closure record / note |
|---|---|---|---|---|
| Reverse session | M005 restricted-network transports/proxy traversal | closed | plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md | plans/closure/reverse-session/005-status.md; historical closure retains explicit proxy/WSS qualification limitations now owned by C001. |
| Reverse session | M004 QUIC transport | closed | plans/implementation/reverse-session/004-quic-transport.md | plans/closure/reverse-session/004-status.md; historical closure retains explicit QUIC qualification limitations now owned by C001. |
| Reverse session | M003 security/lifecycle/resource/embedding hardening | closed | plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md | plans/closure/reverse-session/003-status.md; final reviewed head 31458e83e543304d6b271898575bf2f6e98c7352. |
| Reverse session | M002 TCP/TLS reverse-tunnel product | closed | plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md | plans/closure/reverse-session/002-status.md; implementation head 13402200e51b46031a1a82240be1eb48027a09f4. |
| Reverse session | M001 repository and protocol foundation | closed | plans/implementation/reverse-session/001-repository-and-protocol-foundation.md | plans/closure/reverse-session/001-status.md; implementation head 357480b942e95ef7087d86a043f26b7a3d175687. |

## Closure work

### C001 optional-transport corrective

C001 must produce:

- plans/closure/reverse-session-post-closure-corrective/001-status.md

Strict C001 closure requires direct evidence for the QUIC and WSS/proxy cases named in its implementation plan, feature/dependency isolation, full workspace verification, accurate adapter limitation classification, and reconciled planning/support documentation.

M004/M005 historical closure records must not be rewritten to hide the limitations that motivated C001. C001 closure is supplemental evidence.

### M006 distribution/downstream qualification

M006 implementation/local qualification is active. Its own execution record currently identifies these open closure gates:

- hosted candidate release-target workflow/archive/install evidence;
- advisory/license review;
- registry publication ordering and downstream registry-consumption evidence if publication is authorized;
- final supported-platform claims based on actual evidence.

M006 MUST NOT be declared strictly closed before C001 closes.

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

## External/deferred work

These are not C001 implementation blockers:

- hosted release-target evidence remains M006;
- crates.io publication remains M006 and requires explicit release authorization;
- eggup downloader/self-update integration remains deferred because the inspected interface does not provide the required released bootstrap/downloader surface;
- Windows, musl, armv7, Raspberry Pi, and Le Potato release qualification remain M006/deferred support work;
- QUIC custom CA/mTLS and QUIC proxy traversal remain unsupported current profiles unless a later plan/ADR changes the transport boundary.
