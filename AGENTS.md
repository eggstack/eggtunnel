# AGENTS.md

Rust workspace (resolver 2, edition 2024, rust-version 1.89, `--locked` builds): `crates/eggtunnel-proto`, `crates/eggtunnel`, `crates/eggtunnel-cli`. `fixtures/embedder` and `fuzz/` are separate workspaces with their own lockfiles.

## Layout / entrypoints

- `crates/eggtunnel-proto/src/lib.rs` — runtime-neutral wire DTOs + framing only. No socket, runtime, timer, or task deps.
- `crates/eggtunnel/src/lib.rs` — embeddable library; uses caller's Tokio runtime, installs no runtime/tracing state. Modules: `client.rs` + `client/`, `server.rs` + `server/`, `common.rs`, `endpoint.rs`, `pem.rs` (`mtls`), `wire_io.rs`.
- `crates/eggtunnel-cli/src/main.rs` — binary `eggtunnel` (`publish = false` crate): `eggtunnel check|client|server <config>` plus `version`.
- `examples/*.toml`, `docs/CONFIGURATION.md` — canonical TOML shapes. `docs/PROTOCOL.md` (wire), `docs/SECURITY.md`, `docs/SUPPORT.md` (transport matrix), `docs/OPERATIONS.md`, `docs/API.md` + `docs/EMBEDDING.md` (library surface), `docs/DISTRIBUTION.md` (release) are authoritative for behavior. `CHANGELOG.md` records the released lines.
- Dependency direction: proto <- eggtunnel library <- CLI / embedders; generic relay/transport comes from `eggress-* =1.0.8` adapters.

## Verify (mirror CI order)

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

- Use the `verify` skill for the full gate, expected result counts, and how to read them. `--all-features` is required to cover `quic`/`websocket`/`mtls`/`outbound-proxy`.
- Single test: `cargo test --locked -p eggtunnel --all-features <filter>` (tests live in `crates/eggtunnel/src/server_tests.rs` + `server_tests/{tcp,mtls,quic,websocket,proxy}.rs` and `client/` modules, not inline in `server.rs`).
- CI beyond the gate above: `feature-slices` matrix via `--no-default-features` (14 combos in `ci.yml`, including role-specific `quic-client`/`quic-server`/`websocket-client`/`websocket-server` slices), MSRV `1.89` check, and `minimal-dependencies` check that minimal slices pull no unrequested quic/websocket/outbound deps. Keep feature gates additive; never let a default-path import leak an optional dep.
- Sustained qualification is opt-in: see `docs/OPERATIONS.md` for the `cargo +nightly fuzz` decoder target, deterministic Service-state sequence, and ignored TCP/TLS lifecycle/churn soak commands. These long runs are not part of every-push CI.
- Never run workspace commands inside `fixtures/embedder` or `fuzz/`; always use `--manifest-path`.
- `cargo audit` is expected to show one informational `unmaintained` warning for `atomic-polyfill 1.0.3` (transitive via `postcard`/`heapless`, no-atomics targets only). That is the accepted baseline, not a regression.

## Docs / plans / skills

- `architecture/overview.md` is the index; dive by question: wire/framing → `proto-wire-protocol`; secrets/policy/counters/errors → `common-core`; session/reconnect/data-path → `client`; listeners/admission/mTLS → `server`; TLS/QUIC/WSS/proxy + Eggress → `transports-wire-io`; TOML/`check`/embedder facade → `cli-config-ops`; CI/release/licenses/`docs/`+`plans/` ownership → `ops-tooling-distribution` (§5 lists what each `docs/*.md` owns plus the cross-file sync points; §8-E is the pre-release stale-sweep checklist).
- Agent skills live in `.agents/skills/` and load on demand via the skill tool: `verify` (CI gate, expected counts), `plan` (registry/roadmap/milestone/closure workflow and status rules), `docs-sync` (which doc owns which fact, and the sync points that repeat across files), `release` (tag/archive/publish order). `.opencode/skills/<name>` are relative symlinks to the same directories (opencode + codex discovery). Keep the symlink structure; edit the real files under `.agents/`.
- `plans/registry.md` is the planning control surface. Current state: **M017 ready**; M018 (Eggress 1.0.11), M019 (WebSocket dep closure), M020 (standalone RuntimePolicy/BindPolicy TOML) are **blocked** — M018/M020 on M017 strict closure, M019 on M018 plus a published upstream Eggress WebSocket seam. Re-derive this before acting; do not assume a milestone shipped because its plan exists.
- `plans/closure/` records are immutable historical evidence: never rewrite a closed milestone, add a new record. The registry and the subsystem roadmap duplicate milestone status — flip both in the same change. Status vocabulary is fixed (`proposed`/`ready`/`active`/`blocked`/`closing`/`closed`/`conditionally closed`/`superseded`/`archived`); landing code is `closing`, not `closed`.
- `docs/`, `README.md`, and `AGENTS.md` describe **shipped** behavior only. Blocked or planned work belongs in `plans/`. See the `docs-sync` skill before editing any of them.
- Sync points that repeat across many files: numeric ceilings, the transport rejection matrix, the Eggress version, the release-target table, wire-vs-crate versioning, and published-line language.

## Gotchas an agent would miss

- Secrets never go in TOML: `token_env` / `outbound_proxy_env` name env vars holding the bearer token and proxy URI/chain. Always run `eggtunnel check <file>` before `client`/`server`; `check` rejects invalid combos instead of ignoring them.
- `transport` values are exactly `tcp_tls` | `quic` | `websocket_tls` (never `wss`/`tcp`/`udp`). `check` is structural only — no PEM parsing, DNS, or dialing — so a passing `check` does not mean certs/addresses are live. CLI requires ≥1 `[[services]]`; the library allows an empty set with programmatic `register_service` only. There is no dynamic-service or standalone-policy TOML key (that is M020, not shipped).
- Rejected combos (enforced by `check`, do not work around): QUIC rejects custom CA, mTLS, and proxy; WSS rejects mTLS; outbound-proxy rejects QUIC and mTLS. QUIC uses platform roots, UDP control endpoint on `listen_addr`, but service listeners stay TCP. Proxy URI/chain uses canonical `__`-separated pproxy syntax (e.g. `socks5://a:1080__http://b:8080`); no silent fallback to direct on proxy failure.
- Server owns listeners: binds are loopback-only unless `allow_public_service_binds = true`. Client `target_host/port` is client-authoritative; server never rewrites it.
- `ClientBuilder` / `ServerBuilder` are the canonical transport/profile validators used by library startup and CLI `check`; keep the typed rejection matrix in sync with docs. `RuntimePolicy` owns finite runtime ceilings and lifecycle timeouts; wire/name/token bounds stay fixed. The CLI always installs `RuntimePolicy::default()` and derives `BindPolicy` from `allow_public_service_binds` alone — richer policy is library-only until M020.
- Dynamic client Services enter reconnect desired state only after a matching RegisterAck from the **current** Session generation. In-flight registration count is **mode-dependent**: `RegistrationMode::LegacySerial` (1.0 peer, no capability 1) permits exactly one, because wire `Error` carries no ServiceId; `RegistrationMode::CorrelatedBounded` (capability 1 negotiated) permits up to the `client_command_queue`-derived ceiling, correlating by ServiceId through `RegisterReject`. Do not "restore" the one-in-flight rule as a global invariant — that would break the correlated path. Heartbeat stores one outstanding Ping and bounded Snapshot health only.
- Features are additive on `eggtunnel` (`default = ["client", "tls"]`): `server`, `quic` (+ role slices `quic-client`/`quic-server`), `websocket` (+ `websocket-client`/`websocket-server`), `outbound-proxy`, `mtls`. CLI enables all of them. Embedders use `default-features = false` + only what they need (see `docs/EMBEDDING.md`).
- Hard constraints: `#![forbid(unsafe_code)]` in all crates (proto, library, CLI); `cargo-deny` allows permissive licenses only (GPL/AGPL/LGPL denied — check `deny.toml`); release tag `vX.Y.Z` must equal workspace `version` in root `Cargo.toml` (see `.github/workflows/release.yml`); release builds only `-p eggtunnel-cli` and regenerate notices via `python3 scripts/generate-third-party-notices.py`.
- **Releasing requires a version bump.** `v0.1.0` and `v0.2.0` are already tagged and published, and the root manifest still says `0.2.0`. Bump the manifest first, and also edit `scripts/test-install.sh:5`, which hardcodes `version=0.2.0` and has no version env override (only `EGGTUNNEL_TEST_TARGET`) — otherwise the local install smoke test asserts the old version string.
- Version lines: workspace manifest is `0.2.0`; published crates.io line / tag / GitHub release are `0.2.0` (see `docs/DISTRIBUTION.md` + `plans/closure/reverse-session/013-status.md`). Dependency snippets in `docs/API.md` + `docs/EMBEDDING.md` must match the published line (`version = "0.2"`). Wire protocol is v1.1 with 1.0 fallback, independent of crate version — and the **published `0.2.0` artifact shipped wire 1.0** while this source implements 1.1, so the crate version does not identify wire behavior of a local build.
