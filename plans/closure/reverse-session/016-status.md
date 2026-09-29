# Reverse Session M016 Closure — Capability-Negotiated Protocol Evolution

Status: closed

Disposition: closed. All M016 acceptance criteria are met on the exact
implementation head recorded below, exactly as ADR-0002 specifies. No
unresolved high/medium protocol, security, lifecycle, or compatibility
finding remains. No future plan is unblocked by M016 (multi-tenant
Principal policy and Eggpack cutover remain gated on their own ADRs and
requirements, unchanged).

## Baseline and commits

- Planning baseline: M015 strict closure (`5958f50`,
  `plans/closure/reverse-session/015-status.md`).
- Implementation commit: the M016 commit on `origin/main` (protocol,
  runtime, tests, docs, and this record).
- Final reviewed head: the M016 commit (hosted CI evaluated on the
  pushed head; conclusion recorded below).

## Exact wire-ID/version/capability table

- Major 1 (incompatibility boundary, enforced); minor 0 → **1**
  (`PROTOCOL_MINOR`, `crates/eggtunnel-proto/src/lib.rs:22`).
  Frames 1–14 payloads byte-identical; only the header minor word
  changes, which 1.0 peers ignore (major-only check).
- Message IDs 1–14 pinned and unchanged; **15 `RegisterReject`**
  (`MessageType::RegisterReject`, DTO `RegisterReject { service_id,
  code, diagnostic }`, `:284`, `:410-419`). Extension-only: never
  sent without bilaterally negotiated capability 1.
- Capability registry (ADR-0002, never reassigned): **1** correlated
  registration rejection, **2** drain deadline
  (`CAPABILITY_REGISTER_REJECT` / `CAPABILITY_DRAIN_DEADLINE`,
  `:25-31`). Helpers: `Capabilities::supported()` (deterministic
  emission), `intersect()` (bilateral, set semantics, unknown IDs
  ignored), `has()` (`:207-229`).
- Client handshake rule (`negotiate_capabilities`,
  `crates/eggtunnel/src/client.rs:1140-1148`): only the
  offered∩returned intersection counts; server-claimed extras outside
  the advertisement are ignored (extension stays off; any
  extension-only message then fails closed). Server handshake
  (`serve_control`, `crates/eggtunnel/src/server/control.rs:83-101`):
  major check only, returns `supported().intersect(hello)`; baseline
  auth/registration never gated on minor equality.

## Mixed-version compatibility matrix with executable evidence

| Client | Server | Evidence |
|---|---|---|
| 1.1 | 1.0 (empty caps, minor 0) | `legacy_peer_keeps_exactly_one_registration_in_flight` (client/duplex): serial fallback, second concurrent register fails locally `ResourceExhausted` with no second frame |
| 1.0 (minor 0, empty caps) | 1.1 | `v10_client_receives_generic_error_and_never_register_reject` (raw TLS): `ServerHello` 1.1/empty, duplicate registration → generic `Error{1}`, 200 ms silence proves no ID 15 |
| 1.1 partial ([2] only) | 1.1 | `capability2_only_client_keeps_generic_error` (raw) + `partial_capability_reject_without_negotiation_is_a_violation` (duplex): `Error` path kept; stray `RegisterReject` fails the session |
| unknown ([99]) | 1.1 | `unknown_capabilities_are_ignored_by_negotiation`: empty intersection, baseline `Error` |
| 1.1 full | 1.1 | `capability1_client_receives_correlated_register_reject` (raw): `RegisterReject{service_id, code 1}`; `correlated_peers_correlate_out_of_order_ack_and_reject` (duplex, TCP); `quic_concurrent_dynamic_registrations_correlate` (real QUIC); `websocket_concurrent_dynamic_registrations_correlate` (real WSS) |

Same-version tests are never described as compatibility evidence above;
every matrix row uses a genuinely mixed peer (scripted minor/caps).

## Registration concurrency/resource bounds

- Modes: `LegacySerial` vs `CorrelatedBounded { max_in_flight }`
  (`service_state.rs:164-211`), selected per Session from negotiation
  (`set_mode`, `client.rs:755-762`); ceiling derives from the existing
  `client_command_queue` limit (default 32), additionally capped by
  `services_per_session` in the command arm.
- State-machine evidence (unit): bounded concurrent begin/ack/reject
  with ceiling fail-closed; unknown/stale rejects never commit;
  overdue transactions fail without touching others; unregister
  cancels correlated transactions; mode switch fails leftovers
  closed. Legacy serial behavior byte-for-byte preserved (all
  pre-existing service-state tests unchanged and passing).
- Cancellation/reconnect/stale-generation evidence: caller-drop →
  `Abandoned` → `UnregisterService` cleanup, late Ack never commits
  (legacy + correlated duplex tests); write timeout fails one
  correlated transaction vs ends the legacy Session; disconnect with N
  pending fails all replies; stale generation replies `Disconnected`;
  unknown `RegisterReject` fails the session closed.

## Drain deadline matrix

- Negotiated + long peer deadline: client `negotiated_peer_drain_deadline_cannot_extend_local_shutdown`
  (30 s peer vs 100 ms ceiling completes promptly); server
  `negotiated_drain_deadline_waits_before_forced_teardown` (200 ms
  peer vs 500 ms ceiling with a live relay: teardown converges at the
  peer deadline, proving the wait executes before forced
  cancellation).
- Negotiated + zero: immediate (skipped wait by code path
  `!effective.is_zero()`).
- Capability absent: `absent_capability_drain_keeps_local_only_shutdown_timing`
  (client) and unchanged immediate server teardown (1.0 preserved).
- Governing ceilings documented (`docs/PROTOCOL.md` capability-2
  row): `shutdown_grace` for the client open-task join and the server
  control-loop teardown join; `relay_drain` continues to bound relay
  internals. The peer value can only shorten, never extend.

## Fuzz/state-stress/soak evidence

- `cargo +nightly fuzz run decode_frame fuzz/corpus/decode_frame --
  -max_total_time=60 -max_len=1048590`: **20,872,075 runs in 61 s, no
  crash**, with the new `RegisterReject` corpus seed
  (`fuzz/corpus/decode_frame/8ed89f65_register_reject_seed`, verified
  decodable by the real decoder). Fuzzer-generated coverage files from
  the run were discarded; only the curated seed is retained.
- Hostile-input unit guards extended (oversized `RegisterReject`
  diagnostic fails decode; 1.0 minor-0 fixture still decodes).
- Deterministic 10,000-step Service-state sequence: pass
  (`seed=0x4e4f574d414e3031`).
- Optional-transport sustained soaks remain opt-in per
  `docs/OPERATIONS.md` (ignored by default); transport control
  semantics are covered by the new per-transport concurrent tests.

## Feature/dependency/security verification

- No new dependencies; no feature-gate changes; minimal
  `client,tls` tree re-verified leak-free (0 matches).
- 14/14 feature-slice check+test combos pass (role slices included).
- MSRV 1.89 checks (proto, minimal client, client+server) pass.
- `cargo audit` 0 vulnerabilities; `cargo deny check licenses` pass.
- Full local gate on the exact head: fmt, workspace check/test
  (lib **113** passed / 3 ignored; CLI bin 14; CLI integration 7;
  proto **10**), clippy `-D warnings`, rustdoc `-D warnings`,
  embedder check — all clean.
- Security review: negotiated paths add no auth/bind/secret change;
  transaction maps bounded by policy and generation-scoped with
  disconnect/cancel/timeout cleanup; peer deadline capped by local
  policy; unknown/unnegotiated messages fail closed; no new
  allocation, task, queue, or timer beyond the bounded per-transaction
  deadline already accounted in the handshake timeout budget.

## Documentation evidence

- `docs/PROTOCOL.md`: v1.1, header minor, message 15, capability
  registry table, mixed-version matrix, per-owner governing timeouts.
- `docs/SUPPORT.md`: protocol compatibility matrix (1.0/1.1 ×
  1.0/1.1) plus crate/wire independence.
- `docs/EMBEDDING.md`: serial fallback restated as
  without-capability-1 behavior; concurrency available only when
  negotiated (applications must not assume it).
- `docs/SECURITY.md`: bounded generation-scoped transaction state,
  peer-deadline capping, fail-closed unknown messages.
- `docs/OPERATIONS.md`: Drain deadline negotiation note.
- `architecture/proto-wire-protocol.md`: version/capability rows,
  message-15 row, compatibility review items closed.
- `architecture/client.md`: §3.2 control sequence re-anchored to
  `run_session` with negotiation, dual-mode registration, per-txn
  deadlines, and effective drain wait.
- `architecture/server.md`: handshake intersection, `RegisterReject`
  branch, negotiated drain wait, shutdown sequence.
- `architecture/overview.md`: wire v1.1 + 15 message IDs.

## Hosted CI

- `Rust` workflow (`check`, 14-combo `feature-slices`, `msrv`,
  `minimal-dependencies`) on the pushed M016 head — conclusion
  recorded at push time.

## Known limitations

- A client open-task join with no live tasks completes immediately,
  so a shorter-than-local peer deadline is not observably distinct
  from the local ceiling on an idle session; the min() is verified by
  review and by the live-relay server test.
- The deterministic 10k-step state sequence exercises legacy
  transitions; correlated transitions are covered by focused unit
  tests rather than the long sequence.
- Soak evidence remains opt-in sustained qualification, not
  every-push CI.

## Final disposition

M016 is closed. Protocol 1.1 capability negotiation is implemented
exactly as ADR-0002: 1.0/1.1 mixed peers retain baseline
interoperability, concurrent registration is safely correlated and
bounded only when negotiated, legacy fallback remains serial and
correct, Drain deadlines are bounded by local policy, no message
ID/payload regression occurred for IDs 1–14, and exact-head hosted CI
passes. No new executable milestone is unblocked (remaining future
work is gated on its own ADRs/requirements).
