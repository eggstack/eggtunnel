# Reverse Session M019 — WebSocket Adapter Dependency Closure

Status: blocked — hard dependency M018 is not closed; upstream bounded WebSocket seam is not yet published

Planning baseline: ece46fd223265b7b0609e3640b0caa9efadd1535

Source roadmap:

- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: polish / invariant

Hard dependency: M018 strict closure.

Interface dependency: Eggress Transports M003 provides a bounded WebSocket client/server configuration API whose public signature does not expose `tokio_tungstenite::WebSocketConfig`.

Operational dependency: that Eggress API is available in a published crates.io version consumable by Eggtunnel without git/path dependencies.

## 1. Objective

Remove Eggtunnel's direct `tokio-tungstenite` dependency and make Eggress the sole implementation owner of WebSocket framing/configuration while preserving Eggtunnel's current 1 MiB hostile-input bounds and WSS Session semantics.

This is intentionally a narrow dependency-ownership closure milestone.

## 2. Why this milestone is blocked

Eggtunnel currently constructs a Tungstenite `WebSocketConfig` directly on both client and server paths so it can enforce:

- `max_message_size = 1 MiB`;
- `max_frame_size = 1 MiB`.

Eggress owns `WebSocketTunnelClient` / `WebSocketTunnelServer`, but the current bounded configuration methods accept the concrete Tungstenite config type. Calling the simpler Eggress methods would drop the explicit underlying frame/message ceiling and would therefore not be an equivalent security-preserving cleanup.

The dependency can be removed only after Eggress exposes the same bounded configuration through an Eggress-owned public type or method and publishes it.

## 3. Invariants that cannot regress

- WSS remains a non-browser tunnel profile over verified TLS.
- WebSocket message and frame allocations remain bounded at or below the current 1 MiB Eggtunnel ceiling before proportional hostile-input growth.
- control/data Session framing remains Eggtunnel-owned above the byte-stream adapter.
- WebSocket close/backpressure behavior remains at least as qualified by C001.
- no direct `tokio-tungstenite` type appears in Eggtunnel public or private production code after closure.
- optional WebSocket dependencies remain feature-gated and absent from minimal client builds.
- unsupported mTLS/WSS combinations remain fail-closed.
- no wire change.

## 4. In scope

- adopt the published Eggress bounded WebSocket configuration seam;
- replace client `connect_over_stream_with_config` call-site construction with the Eggress-owned bounded API;
- replace server `accept_upgrade_with_config_over_stream` call-site construction likewise;
- remove direct `tokio-tungstenite` dependency and feature wiring from Eggtunnel;
- preserve `MAX_WEBSOCKET_FRAME_SIZE` or replace it with one equivalent Eggtunnel protocol-bound constant passed into Eggress;
- update minimal dependency guards and architecture docs;
- re-run WSS/C001 qualification.

## 5. Out of scope

- changing the 1 MiB Eggtunnel frame ceiling;
- adding browser Origin policy;
- changing WebSocket protocol behavior or URI semantics;
- adding permessage-deflate or extension negotiation;
- changing WSS half-close claims;
- changing Eggress itself during M019;
- transport performance tuning.

## 6. Required production changes

### A. Adopt the Eggress-owned bounded API

Use only the published Eggress abstraction to express the current message/frame ceiling. No production import from `tokio_tungstenite` may remain.

The call sites must remain cancellation- and timeout-bounded exactly as today.

### B. Remove the direct implementation dependency

Delete `tokio-tungstenite` from root/workspace and `eggtunnel` dependency declarations when no other load-bearing call site remains.

Update feature definitions so `websocket-client` / `websocket-server` pull only the Eggress WebSocket adapter plus their existing role/runtime dependencies.

### C. Preserve dependency isolation

Extend the minimal dependency CI guard to ensure:

- minimal `client,tls` has no WebSocket/Tungstenite graph;
- `websocket-client` does not imply server role;
- `websocket-server` does not imply client role;
- no direct Tungstenite package is introduced by Eggtunnel itself.

A transitive Tungstenite dependency under `eggress-protocol-websocket` is expected and is not ownership duplication.

## 7. Ordered work packages

1. Verify the published Eggress version and exact bounded WebSocket API.
2. Capture current WSS dependency tree and focused C001 behavior.
3. Replace client/server direct config construction with the upstream API.
4. Remove direct Tungstenite dependency/feature wiring.
5. Update dependency guards and docs.
6. Run focused WSS backpressure/close/oversize tests plus all feature slices.
7. Run full exact-head CI and create closure evidence.

## 8. Failure and cancellation semantics

- an oversized incoming WebSocket message/frame fails within the configured bound;
- handshake timeout/cancellation behavior remains unchanged;
- WSS protocol/upgrade failures remain typed and never fall back to raw TLS;
- Session cancellation still tears down the boxed WebSocket stream;
- outbound-proxy + WSS composition remains supported as before.

## 9. Required focused tests

- payloads at/below the current ceiling continue to relay;
- oversized message/frame input is rejected without unbounded allocation;
- multi-frame payload behavior remains correct;
- close-during-relay/backpressure regressions from C001 pass;
- client-only/server-only WebSocket feature slices compile/test;
- `cargo tree` proves Eggtunnel no longer directly depends on `tokio-tungstenite`.

## 10. Broad verification

Run the full Eggtunnel AGENTS.md gate, all 14 feature slices, minimal-dependency guards, MSRV, audit/license checks and hosted exact-head CI.

Re-run the WSS sustained churn/qualification commands documented in `docs/OPERATIONS.md` when practical and record the evidence class accurately.

## 11. Compatibility and migration effects

No public API, config or wire migration.

The dependency graph changes only in ownership: Tungstenite remains an implementation detail of Eggress's WebSocket adapter rather than an Eggtunnel direct dependency.

## 12. Documentation updates

Update:

- `architecture/transports-wire-io.md`;
- `architecture/ops-tooling-distribution.md`;
- `docs/SUPPORT.md` if implementation ownership is described;
- `AGENTS.md` dependency/feature guidance;
- minimal-dependency CI comments/guards.

## 13. Acceptance criteria

M019 may close only when:

- no direct Eggtunnel `tokio-tungstenite` dependency/import remains;
- the current 1 MiB WebSocket message/frame bound remains explicit and tested;
- WSS client/server/proxy behavior remains qualified;
- role-specific feature isolation remains intact;
- no wire/public/config behavior changes;
- full exact-head hosted CI passes with no unresolved high/medium finding.

## 14. Stop conditions

Stop if the published Eggress API:

- cannot express both message and frame bounds;
- weakens cancellation/timeout behavior;
- requires leaking a new concrete implementation type into Eggtunnel;
- is not available from crates.io.

Do not substitute a git/path dependency as a shortcut.

## 15. Closure evidence required

Create `plans/closure/reverse-session/019-status.md` with:

- Eggress version/API consumed;
- before/after dependency tree;
- oversize/backpressure/close evidence;
- feature-isolation evidence;
- full local/hosted gate results;
- unresolved findings.

## 16. Handoff notes

The goal is ownership cleanup, not dependency erasure. A transitive Tungstenite implementation inside Eggress is correct; Eggtunnel should only own the reverse-session semantics and the bound it asks the adapter to enforce.
