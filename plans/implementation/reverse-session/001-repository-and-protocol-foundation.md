# Reverse Session M001 — Repository and Protocol Foundation

Status: closed

Repository baseline: 121955694d2151095e409b3940922fc021a4b91a

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#6-milestone-m001--repository-and-protocol-foundation

Long-term requirements:

- plans/000-long-term-specification.md#9-protocol-requirements
- plans/000-long-term-specification.md#10-identity-and-correlation
- plans/000-long-term-specification.md#18-workspace-and-crate-layout
- plans/000-long-term-specification.md#19-cargo-feature-policy
- plans/000-long-term-specification.md#20-msrv-and-unsafe-policy
- plans/000-long-term-specification.md#24-testing-and-evidence
- plans/000-long-term-specification.md#28-system-invariants

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: infrastructure / invariant

## 1. Objective

Create the initial Rust workspace and a bounded, runtime-neutral native Eggtunnel protocol foundation. This milestone must leave M002 able to implement TCP/TLS client/server behavior without redesigning protocol identity, framing, feature ownership, or dependency direction.

No real external listener, client/server network session, or TLS tunnel is implemented in this milestone.

## 2. Why this milestone is ready

The repository has no production code and no predecessor implementation dependency.

Canonical product, terminology, roadmap, planning governance, and ADR-0001 are present at the baseline.

The protocol choices required to start are sufficiently constrained:

- native Eggtunnel protocol, not pproxy compatibility;
- explicit versioning and numeric message IDs;
- bounded binary framing;
- SessionId/ServiceId/ConnectionId distinction;
- TCP/TLS baseline later;
- narrow Eggress integration later;
- no custom multiplexing.

## 3. Current repository evidence

At the baseline:

- the repository contains planning only;
- no Cargo workspace exists;
- no Rust source exists;
- no CI exists;
- no protocol implementation or public crate surface exists;
- no license file or package metadata exists beyond planning statements.

Implementation MUST inspect the actual repository head before editing and preserve any user changes added after the baseline.

## 4. Invariants that must not regress

- Protocol decoding is bounded before payload allocation.
- Numeric message IDs are explicit, not enum-order-derived.
- SessionId, ServiceId, ConnectionId, and Principal identity are distinct concepts.
- ConnectionId representation supports at least 128 bits of unpredictable entropy.
- Protocol crate owns no sockets, Tokio tasks, timers, or runtime.
- Optional transport dependencies do not enter the protocol crate.
- Eggtunnel-owned production crates forbid unsafe code.
- Library crates do not install a global runtime or tracing subscriber.
- Synvoid/i2pr are not added as dependencies.
- eggress-embed is not added merely for future convenience.
- M001 does not prematurely implement a custom stream multiplexer.

## 5. Scope

### In scope

- workspace Cargo manifests;
- initial crate directories;
- rust-toolchain/MSRV decision consistent with canonical plan;
- license selection consistent across workspace;
- lint policy;
- core feature declarations;
- protocol preface and frame envelope;
- explicit protocol version;
- initial control/data message vocabulary;
- typed bounded identifiers/names/specs;
- encode/decode;
- error taxonomy;
- redacted secret wrapper as needed for later auth types;
- unit/property-style boundary tests;
- protocol documentation;
- dependency/feature guards;
- basic CI formatting/check/test workflow if repository conventions permit.

### Explicitly out of scope

- TcpListener/TcpStream product runtime;
- TLS;
- authentication verification;
- actual service registration state;
- pending connection table;
- relay;
- CLI behavior beyond an empty/compiling crate skeleton;
- QUIC;
- WebSocket;
- outbound proxies;
- mTLS;
- persistence;
- release packaging.

## 6. Required workspace shape

Create:

- crates/eggtunnel-proto
- crates/eggtunnel
- crates/eggtunnel-cli

The root workspace should use resolver 2 and a declared workspace rust-version of at least 1.89.

Prefer edition 2024 if all selected dependencies and tooling support the declared MSRV; otherwise edition 2021 is acceptable. Record the chosen edition once rather than mixing editions without reason.

The root package should not exist merely to hold code. A virtual workspace is preferred unless a benchmark or integration-test root package is justified.

## 7. Dependency policy

### eggtunnel-proto

Keep dependencies small and runtime-neutral.

Expected dependencies:

- serde with derive;
- postcard or selected bounded binary codec;
- thiserror;
- rand/rand_core only if identifier generation is placed here and can remain runtime-neutral;
- subtle/zeroize only if required for protocol-level secret wrapper types.

Avoid Tokio.

### eggtunnel

M001 may depend on eggtunnel-proto and establish feature placeholders.

Do not add broad Eggress dependencies until M002 unless a compile-only public boundary requires them.

### eggtunnel-cli

M001 should compile as a placeholder/process shell only.

Do not add a large CLI/config dependency surface until M002 needs it.

## 8. Protocol preface and envelope

Define an explicit protocol magic and version.

A reasonable shape is:

- fixed 4-byte magic;
- u16 protocol major/version;
- explicit message type;
- bounded u32 payload length;
- payload encoded by the selected codec.

The exact byte layout may vary if implementation evidence identifies a cleaner bounded representation, but it MUST:

- be documented byte-for-byte;
- reject wrong magic;
- reject unsupported version before message dispatch;
- reject frame length above MAX_FRAME_BYTES before allocating that payload;
- reject truncated frame;
- reject unknown message type with a typed error;
- preserve exact message boundaries;
- provide deterministic encoding.

If a separate DataHello preface uses a specialized fixed layout, document and test it independently.

## 9. Initial message vocabulary

Assign explicit stable numeric IDs to at least:

- ClientHello;
- ServerHello;
- Auth;
- AuthOk or authenticated ServerHello profile;
- RegisterService;
- RegisterAck;
- UnregisterService;
- Open;
- OpenReject;
- Ping;
- Pong;
- Drain;
- Error;
- DataHello.

The implementation may combine Auth into hello negotiation if that produces a clearer TLS-first state machine, but the wire/security consequence must be documented before M002 and must not put credentials in a plaintext-capable preface.

Do not include application payload frames.

## 10. Bounded domain types

Define validated types for at least:

- ProtocolVersion;
- SessionId;
- ServiceId;
- ConnectionId;
- ServiceName;
- RequestedBind / bind DTO;
- EffectiveBind / bind DTO;
- Target descriptor for the initial TCP target;
- capability set;
- bounded error code + bounded diagnostic text where wire diagnostics exist.

Avoid unconstrained public String fields in hostile wire DTOs when a validated wrapper can encode the limit once.

Every maximum should be a named constant with tests.

## 11. Identifier rules

SessionId:
- opaque;
- enough entropy/collision resistance for runtime use;
- safe diagnostic formatting.

ServiceId:
- typed separately from SessionId;
- no accidental From<u64> conversions that blur identity unless strongly justified.

ConnectionId:
- at least 128 bits;
- representation supports constant-time equality if used as a capability-like correlation token;
- Debug should be redacted or shortened if revealing the full token adds no operational value;
- generation helper must use an OS-backed CSPRNG or appropriate rand abstraction.

Do not use UUID v4 automatically unless its exact representation/security/display behavior is reviewed against these constraints; either UUID or fixed random bytes is acceptable with tests.

## 12. Error taxonomy

Create typed errors distinguishing at least:

- invalid magic;
- unsupported version;
- unknown message;
- truncated frame;
- frame too large;
- malformed payload;
- invalid identifier/name;
- invalid bind/target data;
- unexpected message for state/role where state validation is implemented.

Do not use anyhow in eggtunnel-proto public errors.

Error Display MUST not echo arbitrary oversized attacker-controlled payloads.

## 13. Feature skeleton

Declare the intended library feature names early:

- client;
- server;
- tls;
- quic;
- websocket;
- outbound-proxy;
- mtls if appropriate.

M001 does not have to implement them all.

The feature graph should be structured so:

- protocol types are always minimal;
- quic does not activate unless requested;
- websocket does not activate unless requested;
- outbound-proxy does not activate unless requested;
- server can be absent from a downstream client build.

Decide and document default features. Prefer defaults that support the common library case without compromising client-only downstream builds. The CLI may select a broader feature set explicitly.

## 14. Ordered work packages

### Work package A — Workspace and governance scaffolding

- create Cargo workspace;
- declare rust-version/edition/license;
- add rustfmt/clippy config only if necessary;
- forbid unsafe in production crates;
- create crate README/doc stubs;
- create CI check/test workflow if appropriate;
- make empty workspace build.

Acceptance:
- cargo check --workspace;
- cargo test --workspace;
- cargo fmt --all -- --check.

### Work package B — Protocol identity and bounded primitives

- implement typed IDs and ServiceName;
- implement maximum constants;
- implement capabilities;
- implement bind/target DTO validation;
- implement redacted display where needed.

Acceptance:
- max boundary tests;
- invalid-name/bind/target tests;
- ID distinction compile/runtime tests.

### Work package C — Framing and message codec

- fixed preface;
- explicit message IDs;
- frame encode/decode;
- message payload encode/decode;
- exact consumption;
- typed errors.

Acceptance:
- round-trip every message;
- max-1/max/max+1 frame tests;
- truncation/unknown-version/unknown-message tests;
- concatenated-frame decode behaves intentionally.

### Work package D — Feature/dependency guards and documentation

- initial feature matrix;
- protocol overview;
- architecture boundary doc;
- static/cargo checks preventing accidental heavy feature leakage where practical.

Acceptance:
- minimal package checks;
- docs match actual implemented wire layout.

## 15. Failure and hostile-input semantics

- Never allocate the declared payload length before verifying it is <= MAX_FRAME_BYTES.
- Never loop indefinitely waiting for impossible frame completion in a pure decoder API; return a clear need-more-data/truncated result according to the chosen API.
- Unknown message IDs do not deserialize as another variant.
- Unknown protocol major versions fail before auth/service state.
- Diagnostic strings decoded from peers are bounded.
- Invalid input does not panic.

## 16. Compatibility and migration

There is no previous Eggtunnel wire protocol, so no migration is required.

However, once M001 closes, M002 SHOULD treat the explicit wire IDs/layout as the first internal compatibility baseline. Breaking changes before the first published compatibility commitment are allowed only with updated docs/tests and accurate plan/closure notes.

## 17. Required tests

Focused unit tests:

- every identifier validation/formatting rule;
- ServiceName empty/max/oversize/invalid characters;
- every message encode/decode;
- every frame boundary;
- unsupported version;
- unknown ID;
- malformed payload;
- exact consumption;
- Debug/Display redaction.

Property/proptest tests SHOULD cover:

- arbitrary byte decoder never panics;
- valid generated messages round-trip;
- declared lengths cannot bypass maximum checks.

If proptest is not added, equivalent table-driven and randomized tests are acceptable for M001; record the choice.

## 18. Required verification commands

At minimum:

cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

Also run feature-slice checks appropriate to the final manifests, including the smallest eggtunnel-proto build and a client-only eggtunnel build if the feature is already meaningful.

If exact commands differ because the initial lockfile/workspace is created during this plan, record the final equivalents in closure evidence.

## 19. Documentation updates

Create/update:

- README.md at repository root with planning-state warning and product one-liner;
- docs/ARCHITECTURE.md or equivalent;
- docs/PROTOCOL.md;
- crates/* README/package docs as appropriate;
- plans/subsystems/reverse-session-roadmap.md status after implementation.

Do not document M002 network behavior as implemented.

## 20. Acceptance criteria

- Repository is a clean compiling Rust workspace.
- Protocol crate is runtime-neutral.
- Initial wire layout and message IDs are explicit and documented.
- Every external length/name is bounded.
- Decoder cannot allocate an oversized declared frame.
- Every initial message round-trips.
- Unknown/malformed input fails with typed bounded errors and no panic.
- Cargo feature skeleton can represent a client-only minimal build.
- No Synvoid/i2pr dependency exists.
- No eggress-embed dependency exists.
- No custom mux exists.
- Required local verification passes.

## 21. Stop conditions

Stop and report rather than improvise if:

- selecting a codec requires a wire/compatibility tradeoff not covered by the current specification;
- Eggress forces a dependency/API shape that leaks broad runtime machinery into M001;
- a proposed identifier type cannot provide the required entropy/redaction semantics;
- implementing the protocol requires Tokio/socket ownership in eggtunnel-proto;
- a change to ADR-0001 appears necessary.

## 22. Closure evidence required

- implementation commit(s);
- final reviewed head;
- workspace/crate tree;
- exact dependency tree for eggtunnel-proto and client-only eggtunnel slice;
- message-ID/wire-layout evidence;
- boundary/hostile-input test results;
- required verification command outcomes;
- docs created;
- unresolved findings by severity;
- closure disposition.

## 23. Handoff notes

Keep M001 small. Do not try to demonstrate a tunnel by adding ad hoc sockets here. A strong protocol/workspace foundation makes M002 faster and prevents the first network implementation from freezing accidental wire semantics.

Prefer simple owned byte buffers and explicit limits over premature zero-copy abstractions.
