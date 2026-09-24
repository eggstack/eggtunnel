# Reverse Session Post-Closure Corrective Addendum

Status: archived — C001 closed; retained as historical corrective evidence

Parent subsystem:

- plans/subsystems/reverse-session-roadmap.md

Original milestones under corrective review:

- M004 QUIC transport
- M005 restricted-network transports and outbound-proxy traversal

Historical closure records:

- plans/closure/reverse-session/004-status.md
- plans/closure/reverse-session/005-status.md
- plans/closure/reverse-session-post-closure-corrective/001-status.md (supplemental evidence)

Implementation plan:

- plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md

Planning baseline:

- fc19fe57a32d0c45be339ff3dd80044c4a8bc069

## 1. Purpose

M004 and M005 landed functional optional transports and recorded honest limitations, but post-implementation review found two classes of residual work:

1. several test cases named as required by the original handoff plans were not directly exercised in the transport-specific integration suites;
2. the planning control surface drifted behind implementation, with registry and individual roadmap sections still describing M004 as active, M005 as ready, and M006 as blocked after M004/M005 had closed and M006 implementation had begun.

This addendum creates a narrow post-closure corrective pass. It does not reopen the Session/Service architecture, wire protocol, TCP/TLS baseline, M003 resource model, or downstream embedding boundary.

## 2. Corrective ownership boundary

C001 owns:

- QUIC-specific correlation/lifecycle qualification gaps;
- QUIC stream-admission/saturation qualification;
- QUIC half-close behavior qualification or precise documented limitation;
- WSS close/backpressure qualification;
- outbound-proxy failure/cancellation qualification;
- proxy-authentication and multi-hop qualification only where the current Eggress 1.0.8 public connector actually supports those profiles and Eggtunnel claims them;
- reconciliation of closure evidence, roadmap status text, registry status, and M006 release-gating language.

C001 does not own:

- new transports;
- UDP/datagram tunnels;
- custom multiplexing;
- changes to protocol message IDs;
- a new authentication model;
- a new proxy implementation;
- release publication;
- hosted release-target qualification already owned by M006.

## 3. Why a corrective pass is required

The M004 closure record explicitly states that QUIC-specific wrong/stale DataHello, half-close, and stream-saturation cases do not have direct integration coverage even though the M004 handoff listed those cases as required tests.

The M005 closure record explicitly states that proxy authentication, multi-hop chains, timeout/refusal, and cancellation during individual proxy hops do not have Eggtunnel integration coverage even though the M005 handoff required those cases when supported/selected.

The M005 closure also records WebSocket half-close limitations. The corrective must not pretend WebSocket is TCP-half-close equivalent; it must qualify the actual close/backpressure semantics and preserve that limitation in the support matrix.

The active registry is materially stale relative to the repository. This undermines the planning governance requirement that registry, roadmap, implementation plan, and closure status agree.

## 4. Invariants

C001 MUST preserve:

- M001-M003 strict closures;
- native Eggtunnel protocol and stable wire IDs;
- SessionId/ServiceId/ConnectionId separation;
- single-use Session-bound ConnectionId semantics;
- no custom TCP or WebSocket mux;
- QUIC native-stream mapping;
- WebSocket and proxy features remain opt-in;
- minimal client+TLS dependency slice remains free of QUIC/WebSocket/outbound-proxy dependencies;
- no eggress-embed adoption;
- no Synvoid/i2pr runtime dependency;
- caller-owned runtime/tracing;
- no unbounded production channel/task admission.

## 5. Corrective milestone C001

Status: closed

Plan:

- plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md

Closure record:

- plans/closure/reverse-session-post-closure-corrective/001-status.md

Primary class: corrective / invariant / evidence

### Objective

Bring optional-transport evidence and planning state into strict alignment with the original M004/M005 requirements without expanding product scope.

### Required outcomes

QUIC:

- direct transport-specific wrong-session DataHello rejection;
- direct stale-session or old-generation DataHello rejection after reconnect/replacement;
- replay/duplicate correlation rejection through the QUIC data-stream path;
- bounded stream-saturation behavior with deterministic rejection/backpressure and no unrelated-stream corruption;
- half-close behavior exercised and documented accurately;
- review of the recorded adapter-level UDP/TLS handshake-admission limitation, with either the smallest supported bound added or an explicit residual-risk classification tied to Eggress's public API.

WSS/proxy:

- deterministic WebSocket close/backpressure/flush behavior test;
- deterministic proxy refusal/unreachable case;
- deterministic proxy timeout case using a local fixture;
- cancellation during at least one in-progress proxy establishment path with proof of prompt teardown/no reconnect leak;
- HTTP CONNECT and SOCKS5 proxy-auth success/failure tests if supported by the selected Eggress public URI/config surface;
- multi-hop chain test if Eggtunnel documentation/configuration claims multi-hop support; otherwise narrow the support claim explicitly instead of fabricating coverage.

Planning/evidence:

- registry reflects M001-M005 closed, C001 closed, M006 active;
- reverse-session roadmap individual milestone sections and status table agree;
- M006 is closed (closure record at plans/closure/reverse-session/006-status.md);
- M004/M005 historical closure records remain intact; a C001 closure record supplies supplemental evidence rather than rewriting history;
- support/security/operations docs accurately distinguish tested support from adapter limitations.

## 6. Dependency graph

M004 historical closure ----+
                            |
M005 historical closure ----+--> C001 optional-transport corrective
                            |          |
M006 implementation active -+          v
                              M006 strict closure gate

C001 executed while M006's non-conflicting release-workflow and documentation work continued. Both are now closed; the M006 closure record is at plans/closure/reverse-session/006-status.md.

## 7. Closure policy

C001 closure requires:

- all mandatory transport-specific tests in its implementation plan passing;
- unsupported Eggress adapter features narrowed/documented instead of implied;
- no new high/medium transport lifecycle/security finding;
- full workspace verification;
- client+TLS dependency isolation evidence;
- registry/roadmap/status reconciliation;
- a new closure record at plans/closure/reverse-session-post-closure-corrective/001-status.md.

C001 does not require crates.io publication or hosted multi-platform release evidence; those remain M006 responsibilities.

## 8. Deferred work

The following remain outside this corrective:

- QUIC custom CA/mTLS until Eggress exposes a suitable supported API or a later ADR selects another boundary;
- QUIC pre-Eggtunnel UDP/TLS handshake admission below the public Eggress listener boundary if it cannot be controlled without replacing the adapter;
- transparent TCP half-close equivalence over WebSocket;
- QUIC through HTTP/SOCKS proxy;
- new proxy protocols;
- Windows/musl/armv7/SBC release qualification;
- crates.io publication.

## 9. Status table

| Corrective | Status | Plan | Closure | Blocker |
|---|---|---|---|---|
| C001 optional-transport qualification and planning reconciliation | closed | plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md | plans/closure/reverse-session-post-closure-corrective/001-status.md | none |
