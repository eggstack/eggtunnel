# Reverse Session M016 — Capability-Negotiated Protocol Evolution

Status: closed

Planning baseline: ae35859c00089784254b6a078a5a519420de1994

Source roadmap:

- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md

Applicable ADRs:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md
- plans/adrs/ADR-0002-capability-negotiated-minor-protocol-evolution.md

Primary class: capability / invariant

Hard dependency: M014 strict closure.

M015 is independent after M014 and is not a hard dependency.

## 1. Objective

Implement the first backward-compatible Eggtunnel protocol-minor evolution using the capability semantics accepted in ADR-0002.

The milestone must:

- advertise protocol minor 1 while preserving major-1 baseline interoperability;
- negotiate capability intersection;
- add correlated Service registration rejection so multiple dynamic registrations may be safely in flight when negotiated;
- give the existing Drain deadline bounded interoperable meaning when negotiated;
- retain exact 1.0 fallback behavior when either peer does not negotiate an extension.

## 2. Why this milestone is dependency-ready after M014

The need is concrete:

- current generic `Error` has no ServiceId, forcing a one-registration-in-flight client state machine;
- `Drain.deadline_ms` is currently ignored by receivers;
- capability DTOs already exist and are bounded but unused;
- published 1.0 peers accept same-major peers and send/receive empty capabilities.

M014 is a hard dependency because client/server Session topology should be structurally stable before adding negotiated state and concurrent registration transactions.

## 3. Invariants that cannot regress

- Framing magic/header/length bounds and message IDs 1-14 remain unchanged.
- Protocol major remains 1.
- A 1.1 implementation interoperates with a 1.0 peer using baseline behavior.
- No extension-only message is sent without bilateral negotiation.
- Unknown capabilities are ignored; unknown/unnegotiated messages fail closed.
- Existing initial configured Service registration remains deterministic and bounded.
- Desired state changes only after an acknowledgement from the current Session generation.
- Stale/late registration responses cannot mutate current desired state.
- All registration transaction state is bounded and cleaned on disconnect/cancel/timeout.
- Peer Drain can never increase the receiver's local shutdown ceiling.
- Authentication, bind authorization, ConnectionId, relay, transport, and target-authority semantics are unchanged.
- Minimal client dependency surface does not grow due to protocol evolution.

## 4. In scope

- `PROTOCOL_MINOR = 1`;
- capability constants/typed helpers and intersection logic;
- message ID 15 `RegisterReject`;
- negotiated registration response behavior;
- bounded concurrent dynamic registration state;
- legacy serial fallback;
- negotiated Drain deadline behavior;
- protocol compatibility tests and mixed-version fixtures;
- protocol/security/architecture documentation.

## 5. Out of scope

- protocol major 2;
- changing payloads of message IDs 1-14;
- a generic request-id framework;
- authentication/provider changes;
- per-Principal authorization;
- UDP;
- persistent registration state;
- changing initial Service semantics beyond capability-aware error handling;
- release/version publication.

## 6. Required protocol changes

### A. Capability representation

Define stable capability IDs:

- 1: correlated registration rejection;
- 2: drain deadline semantics.

Provide bounded helpers that make supported/negotiated checks explicit and avoid ad-hoc raw integer matching throughout runtime code.

Keep the wire representation compatible with the existing bounded `Vec<u16>`. Emission must be unique/deterministic; receipt must safely handle unknown IDs.

### B. Negotiation handshake

Client:

- sends its supported capability set in `ClientHello`;
- accepts any same-major `ServerHello`;
- treats only the server-returned intersection as negotiated;
- validates that the server does not claim an extension the client did not advertise; either ignore such extras or reject according to the exact ADR interpretation chosen in implementation, but document/test the behavior consistently.

Server:

- validates major as today;
- computes intersection with server-supported capabilities;
- returns only the negotiated intersection in `ServerHello`;
- stores negotiated capabilities in Session context needed for control-loop behavior.

Do not gate baseline authentication/registration on minor equality.

### C. RegisterReject message

Add explicit message ID 15 with bounded DTO:

- ServiceId;
- numeric code retaining current registration categories where possible;
- BoundedDiagnostic.

Update encode/decode/kind/message-ID guard tests.

When capability 1 is negotiated:

- server sends RegisterReject for registration failure;
- client correlates by ServiceId and current Session generation;
- multiple registration requests may be active concurrently up to a finite policy-derived limit;
- each transaction has an acknowledgement timeout and caller reply ownership;
- cancellation/abandonment must not allow a late Ack to enter desired state; send cleanup UnregisterService when necessary, as in current abandoned-single-transaction behavior.

When capability 1 is absent:

- server uses existing generic Error;
- client permits at most one unacknowledged dynamic registration exactly as 1.0.

Initial configured registration during Session startup may remain sequential for simplicity unless changing it is necessary for a coherent shared implementation.

### D. Bounded registration transaction state

Replace the single optional pending transaction with a state owner that can operate in two modes:

- LegacySerial;
- CorrelatedBounded.

The correlated mode must bound in-flight transactions. Prefer deriving the ceiling from an existing finite runtime-policy field if semantically defensible; otherwise add one explicit validated ResourceLimits field with the current effective serial value or a conservative bounded default.

Do not use an unbounded HashMap merely because the command channel is bounded.

On disconnect/session replacement:

- fail all pending replies;
- clear generation-scoped transaction state;
- never commit an Ack/Reject from a stale generation.

### E. Drain deadline

When capability 2 is negotiated:

- preserve `Drain { deadline_ms }` wire shape;
- interpret value as relative duration from receipt;
- effective wait is `min(peer_deadline, local configured maximum)`;
- zero is immediate;
- use the effective deadline for joining/draining owned Open/relay/session tasks before forced cancellation.

When capability 2 is absent, preserve 1.0 local-only shutdown timing.

Document which local timeout (`shutdown_grace`, `relay_drain`, or a clearly defined combination) governs each owner; do not create contradictory nested deadlines.

## 7. Ordered work packages

1. Re-run exact M014 baseline and add protocol compatibility test scaffolding.
2. Implement capability constants/helpers and 1.1 handshake intersection with no extension behavior enabled.
3. Add message ID 15 codec/DTO plus hostile-input/round-trip/ID-registry tests.
4. Implement server capability-aware RegisterReject.
5. Refactor client Service transaction state into legacy serial vs correlated bounded modes.
6. Add concurrent registration success/reject/cancel/timeout/stale-generation tests.
7. Implement capability-2 Drain effective-deadline behavior on both roles.
8. Add mixed 1.0/1.1 and partial-capability integration matrices across supported transports.
9. Update protocol/architecture/security/embedding docs.
10. Run sustained state-sequence/soak regression relevant to concurrent registration and shutdown.
11. Run full exact-head CI and create closure evidence.

## 8. Failure, cancellation, and restart semantics

Explicitly test and preserve:

- cancellation before a RegisterService write;
- cancellation after write but before Ack/Reject;
- caller dropping a reply while the transaction is active;
- timeout of one correlated registration without corrupting unrelated transactions;
- duplicate ServiceId/name rejection among simultaneous requests;
- disconnect with N pending registrations;
- reconnect after pending failures;
- stale Ack/Reject from a previous Session generation;
- server error with capability absent;
- unnegotiated RegisterReject received unexpectedly;
- shutdown with peer deadline 0, shorter than local, equal to local, and longer than local;
- shutdown while active relays/Open tasks remain;
- QUIC/WSS/TCP behavior equivalence for the control semantics.

## 9. Required focused protocol tests

- protocol v1.0 frame fixtures continue decoding;
- 1.1 version round-trip;
- capability lists remain bounded and unknown IDs are tolerated;
- negotiation returns intersection only;
- RegisterReject is ID 15 and all existing IDs remain pinned;
- malformed/oversized RegisterReject diagnostics fail closed;
- 1.1 client + 1.0 server never sends/depends on ID 15;
- 1.0 client + 1.1 server receives baseline generic Error behavior;
- capability-1 peers correlate out-of-order Ack/Reject for multiple Services;
- legacy peers retain exactly one registration in flight;
- capability-2 peer cannot extend local shutdown;
- capability-absent Drain behavior remains the 1.0 baseline.

## 10. Broad verification

Run all M014 qualification plus:

- protocol fixture/compatibility suite;
- every supported transport integration suite with capability negotiation;
- deterministic dynamic-Service state stress with concurrent registrations;
- bounded reconnect/dynamic-Service soak;
- decoder hostile-input unit guard and bounded fuzz run including new message corpus;
- minimal feature dependency guard;
- hosted exact-head CI.

Record mixed-version evidence explicitly; do not describe same-version tests as backward compatibility evidence.

## 11. Documentation updates

Update at minimum:

- `docs/PROTOCOL.md` with v1.1 negotiation, capability registry, message 15, and 1.0 fallback;
- `docs/EMBEDDING.md` to remove the unconditional one-registration-in-flight statement and describe negotiated fallback;
- `docs/SECURITY.md` for bounded transaction state and peer deadline capping;
- `architecture/proto-wire-protocol.md`;
- `architecture/client.md`;
- `architecture/server.md`;
- support/compatibility statements naming the tested peer matrix.

Crate version and wire version remain explicitly independent.

## 12. Compatibility and migration effects

No configuration migration is required.

Existing 1.0 peers remain supported at baseline semantics under the matrix in ADR-0002.

New capability behavior is opportunistic and bilateral. Applications must not assume concurrent registration is available until the authenticated Session reports/uses negotiated capability 1 internally.

Public Rust API additions, if any, should be additive. The existing `ClientHandle::register_service` method can gain concurrency behavior without signature change.

## 13. Acceptance criteria

M016 may close only when:

- protocol 1.1 capability negotiation is implemented exactly as ADR-0002;
- 1.0/1.1 mixed peers are demonstrated to retain baseline interoperability;
- multiple dynamic registrations are safely correlated and bounded only when capability 1 is negotiated;
- legacy fallback remains serial and correct;
- Drain deadline semantics are bounded by local policy and tested;
- no message ID/payload regression occurs for IDs 1-14;
- hostile-input/fuzz and sustained lifecycle evidence cover the new state;
- all supported transports pass control-semantics regression;
- no unresolved high/medium protocol, security, lifecycle, or compatibility finding remains;
- exact-head hosted CI passes.

## 14. Stop conditions

Stop and require a superseding/new ADR if:

- implementation requires changing an existing DTO payload incompatibly;
- minor/capability negotiation cannot provide safe old-peer fallback;
- a generic cross-protocol transaction ID becomes necessary;
- authentication trust or authorization authority changes;
- protocol major 2 becomes necessary.

## 15. Closure evidence required

Create `plans/closure/reverse-session/016-status.md` with:

- baseline/final head;
- exact wire-ID/version/capability table;
- mixed-version compatibility matrix with executable evidence;
- registration concurrency/resource bounds;
- cancellation/reconnect/stale-generation evidence;
- Drain deadline matrix;
- fuzz/state-stress/soak evidence;
- feature/dependency/security verification;
- hosted CI;
- known limitations and final disposition.

## 16. Handoff notes

Backward compatibility is the primary invariant, not maximizing new concurrency. Implement the negotiation and fallback first, then add extension behavior behind the negotiated capability. Never infer support from minor version alone.
