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
| Reverse session | active | plans/subsystems/reverse-session-roadmap.md | M004 active | M003 is closed at `31458e83e543304d6b271898575bf2f6e98c7352`. |

## Active implementation plans

| Subsystem | Milestone | Status | Implementation plan | Dependencies / handoff note |
|---|---|---|---|---|
| Reverse session | M004 QUIC transport | active | plans/implementation/reverse-session/004-quic-transport.md | M003 is closed; transport and stream lifecycle boundaries are qualified. |

## Ready implementation plans

| Subsystem | Milestone | Status | Implementation plan | Dependencies / handoff note |
|---|---|---|---|---|
| Reverse session | M005 restricted-network transports/proxy traversal | ready | plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md | M003 is closed; execution is sequenced after M004. |

## Recently closed implementation plans

| Subsystem | Milestone | Status | Implementation plan | Closure record / pending evidence |
|---|---|---|---|---|
| Reverse session | M003 security/lifecycle/resource/embedding hardening | closed | plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md | plans/closure/reverse-session/003-status.md; final head `31458e83e543304d6b271898575bf2f6e98c7352`. |
| Reverse session | M002 TCP/TLS reverse-tunnel product | closed | plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md | plans/closure/reverse-session/002-status.md; final head `13402200e51b46031a1a82240be1eb48027a09f4`. |
| Reverse session | M001 repository and protocol foundation | closed | plans/implementation/reverse-session/001-repository-and-protocol-foundation.md | plans/closure/reverse-session/001-status.md; final head `357480b942e95ef7087d86a043f26b7a3d175687`. |

## Blocked implementation plans

| Subsystem | Milestone | Status | Implementation plan | Blocker |
|---|---|---|---|---|
| Reverse session | M006 distribution/downstream qualification | blocked | plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md | Requires strict M003 closure plus closure of every optional transport intended for the first supported release matrix. |

## Closure work

M001-M003 are closed with executable local evidence recorded in their closure files. M004 is active against accepted M003 head `31458e83e543304d6b271898575bf2f6e98c7352`; M005 is ready and sequenced after M004.

M001 closure was completed as follows:

1. committed and reviewed the implementation;
2. recorded required tests and dependency evidence;
3. closed M001 and unblocked M002;
4. refreshed the M002 baseline to the accepted reviewed head.

## Current architecture constraints

The following are not optional implementation preferences:

- Eggtunnel is a thin reverse-session layer, not another proxy/VPN stack.
- Generic byte relay should use eggress-relay.
- Optional Eggress QUIC/WebSocket/outbound transport components remain feature-gated.
- eggress-embed is not the default dependency boundary.
- Synvoid and i2pr are architecture references, not production dependencies.
- Baseline TCP/TLS uses one persistent control Session plus separate reverse Data Connections.
- No custom TCP stream multiplexer is authorized.
- QUIC later uses transport-native bidirectional streams.
- Non-loopback operation is encrypted/authenticated and server bind policy is explicit.
- ConnectionId is random, short-lived, single-use, and Session-bound.
- No production unbounded channels/tasks.
- Library APIs remain process-neutral for downstream embedding.

## Blocked external/deferred dependencies

These are not blockers for M001-M003:

- eggup readiness affects M006 installer/updater integration only;
- Eggchaos/Eggbench stable integration affects optional M006 qualification only;
- CodeGG production adoption is a downstream project task, not an Eggtunnel core blocker;
- QUIC/WebSocket support is not required to close the first TCP/TLS product.

## Recently closed work

None. The repository is in planning/bootstrap state.
