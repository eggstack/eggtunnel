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
| Reverse session | active | plans/subsystems/reverse-session-roadmap.md | M001 ready | Repository is planning-only; M001 has no implementation dependency. |

## Dependency-ready implementation plans

| Subsystem | Milestone | Status | Implementation plan | Dependencies / handoff note |
|---|---|---|---|---|
| Reverse session | M001 repository and protocol foundation | ready | plans/implementation/reverse-session/001-repository-and-protocol-foundation.md | Execute against baseline 82fe121c208a6dd4c06acaaf1ab3b5ba03d7a847 plus later planning-only commits; inspect current head first and preserve user changes. |

## Blocked implementation plans

| Subsystem | Milestone | Status | Implementation plan | Blocker |
|---|---|---|---|---|
| Reverse session | M002 TCP/TLS reverse-tunnel product | blocked | plans/implementation/reverse-session/002-tcp-tls-reverse-tunnel-product.md | Requires strict M001 closure and execution-baseline refresh. |
| Reverse session | M003 security/lifecycle/resource/embedding hardening | blocked | plans/implementation/reverse-session/003-security-lifecycle-resource-embedding-hardening.md | Requires strict M002 closure and execution-baseline refresh. |
| Reverse session | M004 QUIC transport | blocked | plans/implementation/reverse-session/004-quic-transport.md | Requires strict M003 closure and execution-baseline refresh. |
| Reverse session | M005 restricted-network transports/proxy traversal | blocked | plans/implementation/reverse-session/005-restricted-network-transports-and-proxy-traversal.md | Requires strict M003 closure and execution-baseline refresh. |
| Reverse session | M006 distribution/downstream qualification | blocked | plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md | Requires strict M003 closure plus closure of every optional transport intended for the first supported release matrix. |

## Closure work

No implementation milestone has landed yet. No closure record should exist until executable evidence is available.

When M001 implementation lands:

1. change M001 from ready/active to closing as appropriate;
2. create plans/closure/reverse-session/001-status.md;
3. record implementation commits, final reviewed head, tests, dependency evidence, docs, and residual findings;
4. only after accepted strict closure change M001 to closed and M002 to ready;
5. refresh M002 repository baseline to the accepted M001 reviewed head before handoff.

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
