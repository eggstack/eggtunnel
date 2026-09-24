# Reverse Session M009 — Dynamic Service Lifecycle and Operational Observability

Status: blocked

Planning baseline: 2e2f3981a6e78596c32dfe5daf5b2585bfc37f1e

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#post-01-maintenance-and-evolution

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability

Hard dependency: M008 strict closure.

## 1. Objective

Make the embedding surface suitable for long-running applications whose local Services change at runtime, and close the current observability gap with bounded tracing and heartbeat health state.

This milestone adds no new transport and no new wire message. It uses the existing RegisterService/UnregisterService and Ping/Pong protocol semantics.

## 2. Why this milestone is blocked

M008 first establishes the canonical runtime policy and composition surface. Dynamic Service state and heartbeat policy should be added after those ownership/configuration boundaries are stable rather than creating another temporary configuration path.

No new ADR is required if this milestone uses the existing wire semantics exactly as defined.

## 3. Invariants

- Server remains authoritative for EffectiveBind.
- Client remains authoritative for its local Target.
- A runtime registration cannot redirect another configured Service.
- Duplicate ServiceId/name behavior is deterministic and documented.
- Successful dynamic registrations become desired client state and are restored after reconnect.
- Failed/unacknowledged registrations do not become silently persistent desired state.
- Unregistration remains bounded and idempotence semantics are explicit.
- Observability never includes bearer tokens, proxy credentials, private keys, or raw certificate material.
- Tracing never installs a subscriber or global runtime state.
- Heartbeat state is bounded; no event history grows without limit.
- No wire change.

## 4. Dynamic Service API

Add a programmatic ClientHandle registration operation complementary to the existing unregister operation.

Required semantics:

- caller supplies a validated `ClientService`;
- command delivery is bounded/cancellation-aware;
- a connected Session sends `RegisterService` and correlates the matching `RegisterAck`;
- success returns the server-authoritative `EffectiveBind` or an equivalently useful typed result;
- if the Session disconnects before acknowledgement, the operation returns a typed disconnected/cancelled result rather than claiming success;
- only acknowledged registration is inserted into desired/reconnect state;
- later reconnect re-registers all acknowledged dynamic Services;
- unregister removes the Service from desired state and current Session state;
- duplicate IDs/names and policy rejection have explicit outcomes.

If existing wire behavior cannot distinguish an important failure case, stop rather than inventing an implicit protocol change inside M009.

## 5. Desired-state ownership

Refactor initial configured Services and acknowledged dynamic Services into one bounded client-owned desired-Service registry.

Requirements:

- finite count governed by runtime policy;
- deterministic ordering where protocol/test behavior depends on ordering;
- no unbounded command backlog;
- reconnect uses a snapshot of desired state;
- concurrent register/unregister/reconnect races are tested;
- a stale acknowledgement from an older Session generation cannot mark a failed registration successful.

## 6. Tracing

Add library-level structured tracing without installing a subscriber.

Useful events/spans should cover:

- connect/reconnect attempt and outcome;
- TLS/transport establishment outcome without secrets;
- authentication result category;
- Session establishment/replacement;
- Service registration/unregistration and EffectiveBind;
- external Open/DataHello correlation outcome;
- admission rejection category;
- relay termination category;
- shutdown/drain.

Use typed fields and stable coarse categories; do not log secret-bearing URI/config objects through Debug.

If dependency footprint is material, record measurements and choose the smallest supported tracing surface.

## 7. Heartbeat health state

Use the existing Ping/Pong exchange to expose bounded operational health.

Track at least:

- last successful Pong age or equivalent monotonic health signal;
- latest and/or smoothed RTT;
- missed/unanswered heartbeat count;
- current Session generation/connected state already represented by Snapshot.

Prefer at most one or a very small bounded number of outstanding heartbeat probes. Do not create an unbounded nonce-to-timestamp map.

Expose health through `Snapshot` or a small typed nested snapshot, with values safe for embedding/JSON conversion.

Do not import Synvoid's health scoring or connection-quality state machine. RTT/missed-heartbeat primitives are enough for Eggtunnel.

## 8. Failure/cancellation/restart semantics

Test:

- register while connected;
- register racing with disconnect;
- register racing with shutdown;
- unregister racing with reconnect;
- reconnect restoration after successful dynamic registration;
- failed registration is not restored;
- heartbeat timeout/missed probe does not create an unbounded reconnect loop;
- shutdown clears command waiters and heartbeat state;
- capacity returns to baseline.

## 9. Required focused tests

- dynamic registration success with returned EffectiveBind;
- duplicate ServiceId and duplicate-name policy;
- server bind denial;
- resource-limit rejection;
- disconnect-before-ack;
- cancellation-before-ack;
- acknowledged dynamic Service survives reconnect/reregistration;
- unregistered Service does not return after reconnect;
- stale Session acknowledgement cannot mutate desired state;
- Ping/Pong RTT update;
- missed heartbeat accounting and recovery;
- trace formatting/redaction tests where practical;
- client-only embedding fixture dynamically registers a Service without restarting the Client.

## 10. Required broad verification

Run the full M008 matrix plus repeated race/reconnect tests. At least one repeated run should exercise dynamic registration across multiple reconnect generations.

Record the dependency delta caused by observability instrumentation.

## 11. Documentation

Update:

- docs/API.md;
- docs/EMBEDDING.md;
- docs/OPERATIONS.md;
- docs/SECURITY.md;
- docs/PROTOCOL.md only to clarify that existing RegisterService/UnregisterService and Ping/Pong are valid during the established Session; do not change message meaning;
- architecture/client.md;
- architecture/common-core.md;
- architecture/proto-wire-protocol.md if state-machine documentation changes.

## 12. Acceptance criteria

- an embedder can add and remove a Service at runtime without restarting the Client;
- successful dynamic Services are reconnect-stable;
- unsuccessful/stale registrations cannot enter desired state;
- all command paths are bounded and cancellation-aware;
- library tracing exists without global subscriber installation and without secret leakage;
- heartbeat RTT/missed-probe health is available through bounded typed state;
- no wire change;
- no unresolved high/medium finding remains.

## 13. Stop conditions

Stop and require a new ADR/protocol milestone if:

- safe dynamic registration requires a new wire message or changed RegisterAck meaning;
- stale-generation safety cannot be guaranteed using existing Session identity;
- heartbeat liveness requires changing compatibility/version semantics;
- tracing requires process-global initialization.

## 14. Closure evidence required

Create `plans/closure/reverse-session/009-status.md` with:

- baseline/final head;
- dynamic-Service state-machine/evidence matrix;
- reconnect/race repeated-run evidence;
- heartbeat state evidence;
- tracing/redaction evidence;
- downstream embedding evidence;
- feature/dependency delta;
- exact commands/hosted CI;
- residual findings/disposition.
