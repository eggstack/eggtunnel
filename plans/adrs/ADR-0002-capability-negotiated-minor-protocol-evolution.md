# ADR-0002 — Capability-Negotiated Minor Protocol Evolution

Status: accepted

Date: 2026-09-28

## Context

Eggtunnel wire protocol 1.0 was intentionally minimal. `ClientHello` and `ServerHello` already carry a bounded capability list and a major/minor protocol version, but 0.2.0 always exchanges an empty capability set and treats the minor number as informational.

Two concrete limitations now justify defining compatibility semantics rather than continuing to defer negotiation:

1. Dynamic Service registration can have only one unacknowledged request per Session. `RegisterAck` identifies a Service, but the generic `Error` message does not. A client therefore cannot safely correlate multiple registration failures.
2. `Drain` already carries `deadline_ms`, but 0.2.0 receivers ignore the peer-provided value and use only local shutdown policy. The field has no interoperable behavior.

The published 0.1.0/0.2.0 line uses protocol major 1 and must not be gratuitously disconnected from a compatible extension.

## Forces and constraints

- Existing message IDs 1-14 and their serialized payloads cannot be silently repurposed.
- An old 1.0 peer rejects unknown message IDs.
- Old clients and servers already tolerate a peer with the same major and a different minor number because only the major is enforced.
- Old peers exchange an empty capability list.
- Capability negotiation must be bounded and must not become a plugin protocol.
- New semantics must be used only when both peers explicitly support them.
- The one-registration-in-flight fallback must remain correct with a 1.0 peer.
- Shutdown must remain locally bounded even if a peer advertises an excessive deadline.
- The protocol remains native Eggtunnel and does not adopt third-party reverse-tunnel compatibility.

## Decision

Eggtunnel protocol 1.x will use capability negotiation as the authoritative gate for backward-compatible optional semantics.

### Version meaning

- Protocol major remains the incompatibility boundary. Major mismatch is rejected.
- The next extension bumps the implementation's advertised minor from 0 to 1.
- Minor version alone does not authorize an extension.
- A peer with major 1 is eligible for the baseline 1.0 behavior regardless of minor number.
- Optional behavior is enabled only by a negotiated capability.
- Unknown capability IDs are ignored.
- Implementations emit capability IDs without duplicates; receivers interpret the bounded list as a set.

### Negotiation

- `ClientHello.capabilities` advertises capabilities supported by the client.
- `ServerHello.capabilities` returns the intersection of client-advertised capabilities and server-supported capabilities.
- The client treats only the returned intersection as negotiated.
- A 1.0 server naturally returns an empty list; a 1.1 client then uses baseline semantics.
- A 1.0 client advertises an empty list; a 1.1 server returns an empty list and uses baseline semantics.
- No extension-only message may be sent without the corresponding negotiated capability.

### Capability 1 — correlated registration rejection

Reserve capability ID 1 for correlated dynamic registration rejection.

Add a new explicit message ID 15:

`RegisterReject { service_id, code, diagnostic }`

Semantics:

- when capability 1 is negotiated, a server responds to post-authentication `RegisterService` failure with `RegisterReject`, identifying the rejected Service;
- the client may then maintain multiple bounded registration transactions in flight, keyed by ServiceId and Session generation;
- `RegisterAck` retains its current message ID and meaning;
- when capability 1 is absent, the server continues to use legacy generic `Error` for registration failure and the client retains the one-registration-in-flight rule;
- authentication and other existing generic errors continue using `Error`;
- a capability-1 peer receiving an unnegotiated/unknown `RegisterReject` treats it as a protocol violation.

The implementation may choose a smaller configured concurrency than the command queue ceiling, but all in-flight registration state must remain bounded by existing or explicitly added runtime policy.

### Capability 2 — negotiated drain deadline

Reserve capability ID 2 for drain-deadline semantics.

The existing `Drain { deadline_ms }` payload and message ID 12 remain unchanged.

When capability 2 is negotiated:

- `deadline_ms` is a relative grace duration beginning when the Drain is received;
- zero means no peer-requested grace;
- the receiver's effective drain wait is the minimum of the peer-advertised duration and its own configured local shutdown/drain ceiling;
- the peer value can shorten but never lengthen the receiver's local maximum;
- after the effective deadline, owned remaining relay/session tasks are cancelled according to existing teardown invariants.

When capability 2 is absent, 1.0 behavior remains valid: the field is tolerated but the receiver may use local-only shutdown policy.

### Capability registry

Capability numeric assignments are documented in `docs/PROTOCOL.md` and guarded by tests. IDs are never silently reassigned.

Future capabilities require a concrete semantic extension. They do not require a new ADR unless they change compatibility meaning, trust/authority, transport architecture, or another ADR-governed boundary.

## Alternatives considered

### Bump protocol major to 2

Rejected for these extensions. Both changes can be safely negotiated while retaining baseline 1.0 behavior, and existing peers already share the same framing/Session model.

### Change the payload of generic Error

Rejected. Postcard payload shape is part of the published wire contract; changing message ID 13 in place would make old decoders fail or misinterpret the payload.

### Infer extensions from minor version only

Rejected. Old peers do not negotiate a support window and minor has historically been informational. Capability intersection gives explicit bilateral evidence and allows independent future extensions.

### Use one generic request ID on every control message

Deferred. It would be a much broader wire redesign than the concrete registration-correlation need and would add identifiers to stable message payloads unnecessarily.

### Remove Drain.deadline_ms

Rejected. The field is already published and can be given safe bounded semantics without changing its encoding.

## Consequences and tradeoffs

Positive:

- 1.0 and 1.1 peers retain baseline interoperability;
- concurrent dynamic registration becomes possible only when safely correlated;
- the existing Drain field gains useful bounded semantics;
- future minor extensions have a defined compatibility mechanism.

Costs:

- client Service state must support both legacy serial registration and negotiated bounded concurrent registration;
- server registration response logic has a capability-dependent branch;
- protocol test matrices must cover 1.0/1.1 and capability-present/absent cases;
- capabilities become a compatibility surface that must be documented and kept stable.

## Security and resource consequences

- Negotiated capabilities do not weaken TLS/authentication or bind authorization.
- Registration transaction maps/queues must remain bounded and generation-scoped.
- A malicious peer cannot extend shutdown beyond local policy because peer drain duration is capped by the local maximum.
- Unknown/unnegotiated extension messages fail closed.
- Capability lists retain the existing bounded maximum.

## Migration and compatibility

The intended compatibility matrix is:

| Client | Server | Negotiated extensions | Behavior |
|---|---|---|---|
| 1.0 | 1.0 | none | current baseline |
| 1.1 | 1.0 | none | current baseline; serial registration |
| 1.0 | 1.1 | none | current baseline |
| 1.1 | 1.1 | intersection | correlated registration and/or drain semantics as supported |

The crate version is independent from the wire version. An implementation release carrying protocol 1.1 must document which crate version first advertises it.

## Affected planning documents

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md if capability terminology needs clarification
- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md
- plans/implementation/reverse-session/016-capability-negotiated-protocol-evolution.md
- docs/PROTOCOL.md
- architecture/proto-wire-protocol.md
- architecture/client.md
- architecture/server.md

## Review trigger

Supersede or amend this ADR if:

- a future extension cannot be represented as baseline major-1 behavior plus a capability;
- old/new peer interoperation proves ambiguous in implementation;
- protocol authentication or authorization authority changes;
- a generic transaction/request-correlation layer becomes necessary across multiple independent message families;
- a protocol major-version support window is introduced.
