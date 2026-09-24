# Reverse Session M010 — Client Runtime Modularization and State-Machine Hardening

Status: closed

Planning baseline: 0c8830e76d92c209485eaf955cbdb51bdc953413

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#post-01-maintenance-and-evolution

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: polish / invariant

Hard dependency: M009 strict closure, satisfied by `plans/closure/reverse-session/009-status.md`.

## 1. Objective

Reduce the maintenance and review concentration that now sits in the client runtime after M007-M009, while preserving the published wire protocol, public Rust API, transport profiles, runtime defaults, and dynamic-Service behavior.

The implementation should turn the current client runtime into explicit private responsibility boundaries and make desired-Service / dynamic-registration state a small testable state machine instead of allowing further session-loop growth.

This milestone is behavior-preserving. It is not a protocol or feature milestone.

## 2. Current repository evidence

At the planning baseline:

- `crates/eggtunnel/src/client.rs` is about 2.1 kLOC;
- its production portion is about 1.66 kLOC before the inline test module;
- the file owns public configuration/builders, TLS/transport setup, reconnect orchestration, authenticated Session control, desired-Service state, dynamic registration/unregistration, heartbeat handling, Open/data-path spawning, shutdown, and client tests;
- M009 intentionally permits at most one dynamic registration acknowledgement in flight because the current generic Error message cannot identify the Service that caused the error;
- M007 already demonstrated the value of splitting server transport tests from runtime code;
- hosted CI on the exact M009 closure head is green across all feature slices and Rust 1.89.

There is no known correctness defect requiring semantic redesign.

## 3. Invariants that cannot regress

- Wire version 1.0, message IDs, DTO meanings, and capability behavior remain unchanged.
- Existing public exports and documented public method signatures remain source-compatible.
- `ClientHandle::register_service` / `unregister_service` semantics remain those closed in M009.
- Only server-acknowledged dynamic Services enter reconnect desired state.
- Stale Session generations cannot mutate current desired state.
- A cancelled/abandoned registration cannot silently become desired state.
- At most one dynamic registration is in flight per Session until a later protocol milestone provides correlation for registration errors.
- Reconnect restores acknowledged desired Services and excludes unregistered Services.
- Heartbeat state remains bounded to the current Session.
- No new production unbounded queue, task set, history, or retry loop.
- No transport-specific concrete type leaks into the public composition API.
- Minimal `client,tls` dependency isolation remains intact.
- No new production dependency is expected.

## 4. In scope

- private client runtime/module decomposition;
- extraction of desired-Service / registration transaction state;
- extraction of transport/session establishment helpers where responsibility is already stable;
- extraction of client tests into focused modules;
- explicit documentation/tests for the one-registration-in-flight protocol limitation;
- cancellation/reconnect/registration race hardening exposed by the refactor;
- architecture/developer documentation updates;
- full continuous qualification.

## 5. Out of scope

- concurrent dynamic registration over the wire;
- new message IDs or changes to Error/RegisterAck;
- capability negotiation;
- new transport profiles;
- performance optimization;
- release/version bump;
- Eggpack/Eggbench/Eggchaos dependencies;
- changing runtime policy defaults.

## 6. Required production changes

### A. Client responsibility decomposition

Split the current client implementation along ownership boundaries that already exist conceptually. Exact file names are implementation-owned, but the decomposition should make the following concerns independently reviewable:

- public configuration/builders and profile validation;
- transport/TLS establishment;
- reconnect controller and Session generation ownership;
- authenticated control-Session loop;
- desired-Service and dynamic-registration state;
- Open/data-path execution;
- heartbeat/liveness state.

Avoid over-fragmentation. A helper that is only a few lines and has no independent invariant need not become a module.

The public crate root should continue exporting the same API from the same crate-level names.

### B. Desired-Service state machine

Create a private type or tightly scoped module that owns the client-side Service lifecycle.

It must make these transitions explicit:

- initial configured Service -> desired;
- dynamic request -> pending;
- matching RegisterAck -> active + desired;
- rejection -> not desired;
- caller cancellation before acknowledgement -> tombstoned/cleanup path;
- unregister -> removed from desired state;
- disconnect -> pending waiter fails without inventing success;
- reconnect -> snapshot desired state -> re-registration;
- stale-generation command/ack -> cannot mutate current state.

The one-registration-in-flight constraint must be represented directly rather than emerging accidentally from a HashMap plus generic Error behavior.

Do not change the wire protocol to make this abstraction cleaner.

### C. Session-loop simplification

The authenticated Session loop should primarily coordinate typed state and I/O rather than directly own every transition.

Preserve:

- bounded command/control queues;
- Open-task semaphore behavior;
- Ping/Pong RTT accounting;
- registration timeout behavior;
- Drain/shutdown semantics;
- typed termination classification;
- trace redaction.

### D. Test topology

Move the large inline client test module into focused test modules or files.

At minimum distinguish:

- builder/profile/config validation;
- reconnect/session generation;
- dynamic Service lifecycle;
- heartbeat/liveness;
- transport/data-path behavior where client-owned.

Shared fake-session helpers should have one owner.

Tests must not expose release-build internals solely for convenience.

## 7. Failure, cancellation, and restart semantics

The refactor must explicitly preserve and test:

- cancellation while command send is blocked;
- cancellation after RegisterService write but before acknowledgement;
- timeout after RegisterService write;
- disconnect with pending registration;
- unregister racing with a pending registration;
- unregister while disconnected;
- reconnect with desired dynamic Services;
- stale generation commands;
- stale/late acknowledgement cleanup;
- heartbeat state reset on Session replacement;
- shutdown with active Open tasks.

Resource/task counters must return to their pre-operation baseline where the existing contract requires it.

## 8. Required focused tests

Retain all existing M009 cases and add/refine tests that directly target the extracted state machine:

- initial desired-Service snapshot is deterministic;
- pending -> acknowledged -> desired transition;
- pending -> rejected transition;
- pending -> cancelled/tombstoned transition;
- pending -> disconnected transition;
- unregister of active and pending Service;
- duplicate ID/name rejection across initial, active, and pending state;
- reconnect snapshot excludes rejected/tombstoned Services;
- stale generation cannot commit an acknowledgement;
- only one registration transaction may be pending;
- heartbeat state resets when Session generation changes.

If a deterministic model/sequence harness can exercise these transitions without adding a heavy property-testing dependency, prefer it.

## 9. Required broad verification

At minimum:

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
cargo check --locked --manifest-path fixtures/embedder/Cargo.toml
cargo audit
cargo deny check licenses
```

Also run:

- all seven continuously supported feature slices;
- the Rust 1.89 CI-equivalent checks;
- minimal `client,tls` dependency guard;
- repeated dynamic-registration/reconnect race tests.

Hosted CI on the exact implementation head is required for closure.

## 10. Documentation updates

Update as needed:

- `architecture/client.md`;
- `architecture/common-core.md` if desired-state ownership moves;
- `docs/ARCHITECTURE.md`;
- `docs/API.md` only if explanatory text changes;
- `docs/EMBEDDING.md`;
- `AGENTS.md`;
- subsystem roadmap/registry status.

Do not document private module names as a compatibility promise.

## 11. Acceptance criteria

- client runtime responsibilities are materially separated without public/wire behavior change;
- desired-Service and dynamic-registration transitions have one clear private owner;
- the one-registration-in-flight limitation is explicit and tested;
- client tests are no longer concentrated in the production runtime file;
- reconnect/cancel/timeout behavior remains deterministic;
- heartbeat and Open-task behavior are unchanged;
- no new production dependency or optional-feature leakage is introduced;
- all continuous qualification lanes pass;
- no unresolved high/medium correctness or security finding remains.

## 12. Stop conditions

Stop and report rather than silently broadening scope if:

- clean decomposition requires a public API change;
- desired-state correctness requires a new wire field/message;
- supporting multiple concurrent registrations requires correlating Error responses;
- an existing M009 behavior is found to be incorrect in a way that changes protocol semantics;
- the refactor requires a new async runtime/framework abstraction.

A discovered semantic defect should receive a narrow corrective successor instead of being hidden inside structural cleanup.

## 13. Closure evidence required

Create `plans/closure/reverse-session/010-status.md` recording:

- baseline, implementation commits, and final reviewed head;
- before/after client responsibility/test topology;
- desired-Service transition/invariant matrix;
- repeated race/reconnect evidence;
- public API and wire compatibility evidence;
- feature/MSRV/minimal-dependency results;
- dependency delta;
- exact local commands and hosted CI run;
- residual findings and disposition.
