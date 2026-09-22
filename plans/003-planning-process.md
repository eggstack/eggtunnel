# Eggtunnel Planning and Agent-Handoff Process

Status: normative planning governance

This document defines how Eggtunnel's canonical architecture is translated into bounded implementation work and closure evidence.

The keywords MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT, and MAY are normative.

## 1. Purpose

Eggtunnel uses two planning horizons:

1. Long-term planning defines product identity, ownership boundaries, invariants, protocol/security direction, non-goals, and end-state acceptance.
2. Interim planning defines executable work against a repository baseline for handoff to an implementation agent.

These horizons MUST remain separate. A difficult implementation does not justify silently weakening the long-term contract.

## 2. Canonical long-term documents

The canonical long-term documents are:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- this planning-governance document

They MAY be amended only when:

- product direction intentionally changes;
- a contradiction or material omission is discovered;
- an accepted ADR changes the intended architecture;
- the user explicitly directs a long-term architecture revision.

Corrective implementation work alone is not justification for changing a canonical requirement.

## 3. Architecture decision records

ADRs live under plans/adrs/.

An ADR is REQUIRED when a decision changes one or more of:

- protocol compatibility meaning;
- public Rust API ownership;
- dependency direction;
- transport/session architecture;
- authentication trust model;
- persistent storage authority;
- security invariants;
- release/support contract;
- long-term non-goals.

An ADR MUST contain:

- status;
- context;
- forces/constraints;
- considered alternatives;
- selected decision;
- consequences/tradeoffs;
- affected plans;
- migration/compatibility implications.

Accepted ADR history MUST remain visible. A later ADR supersedes rather than rewrites history.

## 4. Subsystem roadmaps

Subsystem roadmaps live under plans/subsystems/.

A subsystem roadmap translates canonical requirements into one coherent workstream.

It MUST define:

- purpose and ownership boundary;
- canonical references;
- invariants and non-goals;
- current-state evidence;
- dependency graph;
- ordered milestones;
- security/protocol/runtime considerations;
- user/developer-visible exit criteria;
- known risks;
- deferred work;
- status table.

Subsystem roadmaps SHOULD avoid line-number-specific implementation instructions and SHOULD remain useful across several implementation commits.

## 5. Milestone implementation plans

Implementation plans live under plans/implementation/<subsystem>/.

A milestone plan is the primary coding-agent handoff artifact.

It MUST include:

- status;
- repository baseline;
- source roadmap/milestone;
- relevant canonical requirements;
- relevant ADRs;
- primary work class;
- objective;
- why the milestone is dependency-ready;
- current repository evidence;
- invariants that cannot regress;
- explicit in-scope and out-of-scope items;
- required production changes;
- ordered work packages;
- failure/cancellation/restart semantics;
- compatibility/migration effects;
- required focused tests;
- required broad verification;
- documentation updates;
- acceptance criteria;
- stop conditions;
- closure evidence required;
- handoff notes.

The plan MUST be independently executable. It MAY change when repository reality differs from assumptions, but material deviations MUST be recorded.

## 6. Work classification

Each milestone SHOULD identify a primary class.

Invariant:
A property that must stay true across implementations and releases. Examples include bounded hostile-input decoding, single-use ConnectionId semantics, and no library-owned global runtime.

Capability:
A user/developer-visible behavior such as multi-service reverse forwarding or QUIC transport.

Infrastructure:
Internal machinery such as a protocol codec or resource-budget layer that supports later capabilities.

Polish:
Diagnostics, performance tuning, documentation, cleanup, or ergonomics that does not establish a principal capability boundary.

A milestone may span two adjacent classes when a vertical slice requires it, but SHOULD NOT become a broad catch-all.

## 7. Dependency vocabulary

Milestone dependencies are classified as:

- hard: implementation cannot correctly begin before dependency closure;
- interface: implementation may proceed against an accepted contract/test double;
- soft: work may proceed in parallel but final integration depends on another milestone;
- operational: code may land but deployment/release requires external evidence.

A milestone is ready only when all hard dependencies are closed and interface dependencies have a stable written contract.

## 8. Milestone sizing

A milestone SHOULD fit one coherent implementation pass.

It is too large when it combines independently releasable capability boundaries, introduces several unrelated architectural decisions, or requires unrelated refactors.

It is too small when it produces no meaningful contract/evidence unless it is a targeted corrective action.

Prefer vertical slices that exercise the real ownership boundary.

For Eggtunnel, examples:

Good:
- bounded protocol foundation;
- complete TCP/TLS reverse-tunnel vertical;
- resource/lifecycle hardening;
- QUIC transport adapter.

Too broad:
- TLS + QUIC + WSS + UDP + release packaging in one milestone.

Too narrow:
- rename ServiceId;
- add one trace line.

## 9. Agent handoff contract

An implementation agent receives one primary implementation plan.

Authority order:

1. canonical specification and terminology;
2. accepted ADRs;
3. subsystem roadmap;
4. milestone implementation plan;
5. current repository evidence.

The implementation agent MUST:

- inspect current repository state before editing;
- preserve unrelated user changes;
- preserve long-term invariants;
- use the smallest coherent dependency surface;
- prefer published Eggress primitives over duplicated generic networking code;
- keep optional transport dependencies feature-gated;
- add/update tests with behavior;
- update architecture/protocol/security docs when contracts change;
- run the required verification or clearly record unavailable evidence;
- report residual findings.

The agent MUST NOT:

- weaken a security invariant to make a test pass;
- add a custom stream mux without an accepted ADR;
- add Synvoid/i2pr runtime dependencies without explicit architecture review;
- claim closure from prose alone;
- mark hosted CI evidence when only local commands were run.

## 10. Corrective passes

A failed or incomplete milestone is followed by a new corrective plan.

Corrective plans MUST:

- reference the original implementation plan;
- reference the closure/status evidence that found the gap;
- list each unclosed requirement or defect;
- explain why prior verification missed it;
- add regression evidence preventing recurrence;
- avoid reopening unrelated closed scope.

Repeated corrective passes SHOULD trigger review of milestone sizing or architecture assumptions.

## 11. Closure records

Closure records live under plans/closure/<subsystem>/.

A closure record MUST include:

- milestone and plan;
- baseline;
- implementation commit(s)/PR(s);
- final reviewed head;
- requirement-to-evidence matrix;
- exact tests/commands run and outcomes;
- feature/dependency evidence where relevant;
- security/resource/lifecycle evidence where relevant;
- documentation evidence;
- known limitations;
- unresolved findings by severity;
- disposition.

Allowed dispositions:

- closed;
- conditionally closed;
- corrective pass required;
- blocked.

A milestone is not closed merely because implementation landed.

## 12. Evidence rules

Evidence MUST distinguish:

- source inspection;
- unit/integration tests;
- repeated local tests;
- benchmarks;
- fuzzing;
- hosted CI;
- external interop;
- downstream integration.

Do not describe one evidence class as another.

A flaky closure-bearing test blocks strict closure until its mechanism is understood or the requirement is explicitly revised through the proper planning layer.

Performance evidence SHOULD state host/configuration and SHOULD NOT turn noisy CI wall-clock timing into a hard gate.

## 13. Security review expectations

Every networking milestone MUST explicitly review:

- authentication boundary;
- authorization boundary;
- externally controlled allocation;
- queue/task bounds;
- timeout behavior;
- secret logging;
- replay/stale-state behavior;
- cancellation/teardown;
- bind exposure;
- downgrade behavior.

Security review belongs inside milestone acceptance, not as a late release-only activity.

## 14. Dependency and footprint review

Because lightweight embedding is a product requirement, every milestone that changes dependencies/features MUST record:

- cargo tree or equivalent evidence for the affected feature slice;
- whether optional features leaked into default/minimal builds;
- whether a new process/runtime/global dependency was introduced;
- justification for any heavy dependency.

Binary size may be tracked as informational evidence but should not be a brittle hard threshold without an accepted plan.

## 15. Protocol change process

Changes to wire meaning after M001 MUST:

- update protocol documentation;
- update compatibility tests;
- preserve stable numeric message IDs;
- define version/capability behavior;
- state whether old peers can interoperate;
- use an ADR if compatibility meaning materially changes.

Never derive wire compatibility from Rust enum order or serde implementation detail alone.

## 16. Registry requirements

plans/registry.md is the active planning control surface.

It SHOULD contain only:

- canonical document references;
- status vocabulary;
- active subsystem roadmaps;
- dependency-ready implementation plans;
- blocked milestones and blockers;
- closing/recent closure records.

It SHOULD NOT duplicate all historical plan details.

## 17. Status vocabulary

The canonical planning statuses are:

- proposed: document exists but is not approved/dependency-ready;
- ready: dependencies/interfaces are satisfied; implementation may begin;
- active: implementation is in progress;
- blocked: a named dependency/evidence condition prevents progress;
- closing: implementation landed; closure evidence is being assembled;
- closed: closure record accepted;
- conditionally closed: substantial work landed but named evidence/correctness condition remains;
- superseded: replaced by another document;
- archived: retained for traceability but no longer active.

## 18. Registry update rules

When an implementation plan is added:

- register it;
- set accurate dependency status;
- identify the current milestone in the subsystem roadmap.

When implementation lands:

- change status to closing, not closed;
- create/assemble closure evidence.

When closure is accepted:

- set plan/milestone/subsystem statuses consistently;
- register the closure record;
- unblock only milestones whose dependencies are actually satisfied.

## 19. Archive policy

Old implementation plans and closure records SHOULD generally remain in place while the project is young because they are valuable traceability.

An archive move MAY be introduced later when volume justifies it. Canonical documents and accepted ADRs are never archived merely because implementation completed.

## 20. Initial planning structure

The initial planning tree is expected to be:

plans/
  000-long-term-specification.md
  001-terminology-and-domain-model.md
  002-long-term-roadmap.md
  003-planning-process.md
  registry.md
  adrs/
    ADR-0001-session-transport-and-egress-boundary.md
  subsystems/
    reverse-session-roadmap.md
  implementation/
    reverse-session/
      001-repository-and-protocol-foundation.md
      002-tcp-tls-reverse-tunnel-product.md
      003-security-lifecycle-resource-embedding-hardening.md
      004-quic-transport.md
      005-restricted-network-transports-and-proxy-traversal.md
      006-distribution-and-downstream-qualification.md
  closure/
    reverse-session/
      <created only when evidence exists>

The roadmap uses phases 0-5 while implementation plans use M001-M006 for stable handoff numbering.
