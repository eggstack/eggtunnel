# Reverse Session M014 Closure — Post-0.2 Runtime Topology and Maintenance Consolidation

Status: closed

Disposition: closed. All M014 acceptance criteria are met on the exact
implementation head recorded below. No unresolved high/medium
correctness, security, lifecycle, or dependency finding remains. M015
and M016 are unblocked by this closure.

## Baseline and commits

- Planning baseline: `e51ebf5d61f9f4b2317b1e11d6deee983be17c83`
  ("plans: codify capability negotiation invariant").
- Implementation commit: `410ff2ae4e24f8dce30aa2da1e21c25f18a764c6`
  ("feat: complete M014 runtime topology and maintenance consolidation").
- Final reviewed head: `410ff2a` (this closure record and the
  registry/roadmap status updates land in a follow-up closure commit;
  hosted CI is evaluated on the pushed head).
- Pre-change baseline verification (on `e51ebf5`): `cargo fmt --check`
  clean, `cargo check --locked --workspace --all-targets` clean,
  `cargo test --locked --workspace --all-targets --all-features`
  91 passed / 3 ignored.

## Before/after module topology

Server (`crates/eggtunnel/src/server.rs`, 1489 lines) is now a 324-line
coordinator (`Server`, `ServerHandle`, bind orchestration,
`require_caller_runtime`) plus private modules with stable ownership:

| Module | Lines | Owns |
|---|---|---|
| `server/config.rs` | 173 | `ServerConfig`, `ServerTransportProfile`, `ServerBuilder`, `validate_config`, `validate_server_profile` |
| `server/tls.rs` | 67 | `ServerTls`, server/mTLS material construction, `certificate_principal` |
| `server/accept.rs` | 477 | TCP/WSS + QUIC accept loops, shared `AcceptContext` admission, `drain`, TLS accept/upgrade, QUIC control/data handling |
| `server/auth.rs` | 113 | `AuthFailureLimiter`, auth constants, `reject_authentication` |
| `server/session.rs` | 163 | `Principal`, `SessionRegistry`, `SessionContext`, `SessionGuard`, `HandshakeGuard`, `record_saturation` |
| `server/control.rs` | 351 | `ControlAdmission`, `serve_control`, `register_service`, `write_registration_error`, registration codes |
| `server/pending.rs` | 122 | `PendingEntry`, `accept_data_hello`, pending removal |
| `server/service.rs` | 154 | `ServiceEntry`, `run_service`, `ActiveConnectionGuard`, `socket_to_effective` |

Client (`crates/eggtunnel/src/client.rs`, 1410 lines) keeps `Client`,
`ClientHandle`, `run_session`, and validation (1091 lines); the two
duplicated reconnect drivers (`reconnect_loop`, `quic_reconnect_loop`,
~340 lines combined) are replaced by `client/reconnect.rs` (549 lines):
`ReconnectSupervisor` (desired state, disconnected commands, backoff
progression/reset, terminal classification, counter/bind cleanup),
`drive`, private `Transport` trait, `StreamTransport`,
`QuicTransport`, shared `connect_tcp` and `random_jitter_ms`.

New `crates/eggtunnel/src/endpoint.rs` (171 lines): canonical
`Endpoint`/`EndpointError` replacing `valid_endpoint` + `split_endpoint`
+ the CLI `checked_endpoint` shape (CLI adoption is M015).

## Behavior-preservation evidence

- Full workspace suite passes unchanged in outcome:
  `cargo test --locked --workspace --all-targets --all-features`
  → lib 91 passed / 3 ignored, CLI 0, proto 8 passed
  (baseline lib count was 83; +8 new focused tests).
- Focused tests added:
  - `endpoint::tests`: DNS/IPv4/bracketed-IPv6 accept; malformed,
    zero-port, whitespace/control, unbracketed-IPv6 reject (2 tests).
  - `client/reconnect.rs::tests`: shared backoff progression to the
    ceiling for every transport; shutdown interrupts the retry sleep
    without counting a reconnect; auth/authz terminal + session-state
    clearing; jitter bounded by a quarter of the delay (5 tests).
  - `client/tests.rs::a_ready_session_resets_the_shared_reconnect_backoff_independent_of_transport`:
    a ready Session resets the supervisor delay for `tcp_tls`,
    `websocket_tls`, and `quic` labels.
- QUIC teardown ordering note (intentional consistency fix, recorded
  here): `QuicTransport::close` now releases the QUIC connection and
  clears the handle slot immediately when the attempt ends (previously
  the slot cleared after the backoff sleep), and terminal
  authentication failures now clear session counters/binds like every
  other Session end (previously the TCP path broke out before
  clearing). Both are strictly more consistent with the documented
  cleanup invariant; no test observes the old ordering
  (`quic_client_for_test` is only read while a Session is established).

## Feature/dependency graph evidence

- `crates/eggtunnel/Cargo.toml`: `quic = ["quic-client",
  "quic-server"]`, `websocket = ["websocket-client",
  "websocket-server"]` umbrellas retained with identical role-enabling
  meaning; new `quic-client = ["client", "quic-transport"]`,
  `quic-server = ["server", "quic-transport"]`,
  `websocket-client`, `websocket-server`, plus shared
  `quic-transport` / `websocket-transport` dependency-only features.
  `cfg(feature = "quic")` / `websocket` gates split by role
  (`-client` for client code, `-server` for server code); transport
  integration suites require both halves.
- All 14 feature-slice `cargo check` + `cargo test
  -- --test-threads=1` combos pass, including the 7 new role slices:
  `client,tls,quic-client`, `client,tls,websocket-client`,
  `client,tls,quic-client,websocket-client`, `server,tls,quic-server`,
  `server,tls,websocket-server`,
  `client,server,tls,quic-client,quic-server`,
  `client,server,tls,websocket-client,websocket-server`.
  Client-only slices run 31 lib unit tests; server-only slices compile
  clean.
- Dependency-tree guards (local `cargo tree` runs recorded; same
  commands run in CI `minimal-dependencies`):
  - minimal `client,tls` unchanged: no
    `eggress-(transport-quic|protocol-websocket|outbound)`,
    `eggress-protocol-reverse`, or `tokio-tungstenite`.
  - `client,tls,quic-client`: QUIC present; no WSS/outbound leakage;
    `cargo tree -e features -i eggtunnel` shows no `server`,
    `quic-server`, `websocket*`, `outbound-proxy`, or `mtls` feature
    enabled from the command line.
  - `client,tls,websocket-client`: WSS present; no QUIC/outbound
    leakage; no `server`/QUIC/proxy/mTLS command-line features.
  - `server,tls,quic-server` and `server,tls,websocket-server`:
    no `client`/opposite-role/proxy/mTLS command-line features.
- Umbrella compatibility: existing `quic`/`websocket` selections
  compile the prior public profile surface (all-features workspace
  build plus `client,server,tls,quic` and `client,server,tls,websocket`
  slices pass unchanged).

## Endpoint/config validation evidence

- `ClientConfig.server_addr: String` is source-compatible; runtime
  validation is `Endpoint::parse` (single semantic owner).
- Strictness deltas vs the old helpers (correctness fixes): bare
  unbracketed IPv6 (`::1:443`) is rejected (ambiguous split); hosts
  containing `/`, `?`, `#`, `@`, brackets, quotes, or `<>` are rejected
  (URL-authority ambiguity, relevant to the `wss://` construction);
  host length capped at `MAX_TARGET_HOST_BYTES`. No in-tree test or
  example uses a rejected shape.
- Public API diff (HEAD vs head): strictly additive —
  `pub use endpoint::{Endpoint, EndpointError}` in `lib.rs`; all other
  public structs/enums/functions/methods identical (server items moved
  to `server/config.rs` but re-exported unchanged; `snapshot()` return
  type spelled `crate::common::Snapshot`, same type).

## Public-surface disposition

- `ServiceSpec` (exported, runtime-unused): **documented as reserved
  vocabulary, not removed, not wired**. `common.rs` now states it is
  the "server view" reserved for a future authentication/authorization
  ADR (multi-tenant Principal policy), that the 0.2 runtime does not
  consume it, and that a future consumer must drop `target` explicitly
  and keep `requested_bind` behind `bind_to_socket`. No breaking
  removal; no semantic expansion.

## Action/dependency-update automation evidence

- All third-party Actions pinned to immutable SHAs with human-readable
  version comments: `actions/checkout@11d5960a…` (# v4),
  `dtolnay/rust-toolchain@6bed0761…` (# stable),
  `dtolnay/rust-toolchain@b4f984c1…` (# 1.89.0),
  `taiki-e/install-action@4cef1412…` (# v2),
  `actions/upload-artifact@ea165f8d…`,
  `actions/download-artifact@d3f86a10…`,
  `actions/attest@1e69f48a…` (all # v4) in `ci.yml`/`release.yml`.
  SHAs resolved from upstream tags/branches at implementation time.
- New `.github/dependabot.yml`: weekly Monday Cargo (workspace root +
  `fixtures/embedder`) and GitHub Actions updates, PR limits 5/2/5.
- Release semantics unchanged: exact Eggress `1.0.8` pins retained;
  `cargo audit`, `cargo deny check licenses`, MSRV, rustdoc, and
  feature-slice gates retained and extended.

## Full local verification outcomes (exact head `410ff2a`)

- `cargo fmt --all -- --check` — clean.
- `cargo check --locked --workspace --all-targets` — clean.
- `cargo test --locked --workspace --all-targets --all-features` —
  lib 91 passed / 3 ignored; CLI 0; proto 8 passed.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — clean.
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps` — clean.
- `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml` — clean.
- Rust 1.89.0 checks: proto, `client,tls`, `client,server,tls` — clean.
- 14/14 feature-slice check+test combos — pass (7 pre-existing + 7 new).
- Minimal + 4 role-slice dependency-tree guards — pass.
- `cargo audit` — 0 vulnerabilities (1 allowed informational warning,
  pre-existing `RUSTSEC-2023-0089` unmaintained notice).
- `cargo deny check licenses` — pass.

## Documentation/planning reconciliation evidence

- `plans/registry.md`, `plans/subsystems/reverse-session-roadmap.md`,
  M014 plan: `ready` → `active` at implementation start; set to
  `closing`/`closed` by this record (see below).
- `architecture/*.md`: target-metadata wording corrected in
  `client.md`, `common-core.md` (§3.2 retitled to "the server receives
  `TcpTarget` but never as authority"), `overview.md`,
  `cli-config-ops.md`, `server.md` — the server receives the bounded
  `TcpTarget` wire field as non-authoritative registration metadata
  and never selects/rewrites the client connector target.
- Architecture line anchors re-pointed to the new module topology
  (automated unique-line remap + proportional range map + manual
  verification of the §2/§3/§6/§8 tables, the protocol message table,
  and the feature matrix); stale whole-file headers updated;
  `docs/SECURITY.md` wording ("ignores the client Target as
  authority") already matches and is unchanged.

## Hosted verification

- Pushed head `410ff2a` (plus this closure commit); hosted CI (`Rust`
  workflow: `check`, 14-combo `feature-slices`, `msrv`,
  `minimal-dependencies`) evaluated on the pushed head — see run
  reference recorded at push time.

## Unresolved findings by severity

- High: none.
- Medium: none.
- Low: (1) architecture anchors inside heavily rewritten reconnect
  regions point at function granularity (±10 lines) rather than exact
  statements in a few prose paragraphs; the normative tables
  (observability, timeouts, validation matrix, protocol messages,
  feature matrix) carry exact verified numbers. (2) `cargo tree -e
  features` role guards depend on the `(command-line)` marker format
  of the installed Cargo; pinned by the locked toolchain in CI.

## Disposition

M014 is closed. M015 (CLI configuration resolution) and M016
(capability-negotiated protocol evolution) are unblocked: both list
M014 strict closure as their only hard dependency (M016 additionally
governed by accepted ADR-0002), and the client/server Session
topology they build on is now structurally stable with canonical
endpoint validation and role-sliced features available.
