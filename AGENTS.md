# AGENTS.md

Rust workspace (resolver 2, edition 2024, rust-version 1.89, `--locked` builds): `crates/eggtunnel-proto`, `crates/eggtunnel`, `crates/eggtunnel-cli`. `fixtures/embedder` is a separate workspace with its own lockfile.

## Layout / entrypoints

- `crates/eggtunnel-proto/src/lib.rs` — runtime-neutral wire DTOs + framing only. No socket, runtime, timer, or task deps.
- `crates/eggtunnel/src/lib.rs` — embeddable library; uses caller's Tokio runtime, installs no runtime/tracing state. Modules: `client.rs`, `server.rs`, `common.rs`, `wire_io.rs`.
- `crates/eggtunnel-cli/src/main.rs` — binary `eggtunnel` (`publish = false` crate): `eggtunnel check|client|server <config>` plus `version`.
- `examples/*.toml`, `docs/CONFIGURATION.md` — canonical TOML shapes. `docs/ARCHITECTURE.md`, `docs/SECURITY.md`, `docs/SUPPORT.md`, `docs/OPERATIONS.md`, `docs/EMBEDDING.md` are authoritative for behavior.
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

- Single test: `cargo test --locked -p eggtunnel --all-features <filter>` (tests live in `crates/eggtunnel/src/server_tests.rs` + `server_tests/{tcp,mtls,quic,websocket,proxy}.rs` and `client/` modules, not inline in `server.rs`). `--all-features` is required to cover `quic`/`websocket`/`mtls`/`outbound-proxy` paths.
- CI beyond the gate above: 7 `feature-slices` combos via `--no-default-features` (see `ci.yml` matrix), MSRV `1.89` check, and `minimal-dependencies` check that `client,tls` pulls no quic/websocket/outbound deps. Keep feature gates additive; never let a default-path import leak an optional dep.
- Sustained qualification is opt-in: see `docs/OPERATIONS.md` for the `cargo +nightly fuzz` decoder target, deterministic Service-state sequence, and ignored TCP/TLS lifecycle/churn soak commands. These long runs are not part of every-push CI.
- Never run workspace commands inside `fixtures/embedder`; always use `--manifest-path fixtures/embedder/Cargo.toml`.

## Docs / process

- `architecture/overview.md` is the index; dive by question: wire/framing → `proto-wire-protocol`; secrets/policy/counters/errors → `common-core`; session/reconnect/data-path → `client`; listeners/admission/mTLS → `server`; TLS/QUIC/WSS/proxy + Eggress → `transports-wire-io`; TOML/`check`/embedder facade → `cli-config-ops`; CI/release/licenses/`docs/` ownership → `ops-tooling-distribution` (§5 lists what each `docs/*.md` owns; §8-E is the pre-release stale-sweep checklist).
- Agent skills live in `.agents/skills/` and load on demand via the skill tool: `verify` mirrors the gate above (CI order, `--locked`, `--all-features`); `release` gates tagging (never push a `v*` tag without explicit human confirmation — it triggers publication). `.opencode/skills/<name>` are relative symlinks to the same directories (opencode + codex discovery). Keep the symlink structure; edit the real files under `.agents/`.
- `plans/closure/` records are immutable historical evidence; never rewrite a closed milestone, add a new record. `docs/` guides are living: keep numeric ceilings, the transport rejection matrix, and the Eggress version in sync across code + docs (sync points listed in `architecture/ops-tooling-distribution.md` §5).

## Gotchas an agent would miss

- Secrets never go in TOML: `token_env` / `outbound_proxy_env` name env vars holding the bearer token and proxy URI/chain. Always run `eggtunnel check <file>` before `client`/`server`; `check` rejects invalid combos instead of ignoring them.
- `transport` values are exactly `tcp_tls` | `quic` | `websocket_tls` (never `wss`/`tcp`/`udp`). `check` is structural only — no PEM parsing, DNS, or dialing — so a passing `check` does not mean certs/addresses are live. CLI requires ≥1 `[[services]]`; the library allows an empty set with programmatic `register_service` only.
- Rejected combos (enforced by `check`, do not work around): QUIC rejects custom CA and mTLS; WSS rejects mTLS; outbound-proxy rejects QUIC and mTLS. QUIC uses platform roots, UDP control endpoint on `listen_addr`, but service listeners stay TCP. Proxy URI/chain uses canonical `__`-separated pproxy syntax (e.g. `socks5://a:1080__http://b:8080`); no silent fallback to direct on proxy failure.
- Server owns listeners: binds are loopback-only unless `allow_public_service_binds = true`. Client `target_host/port` is client-authoritative; server never rewrites it.
- `ClientBuilder` / `ServerBuilder` are the canonical transport/profile validators used by library startup and CLI `check`; keep the typed rejection matrix in sync with docs. `RuntimePolicy` owns finite runtime ceilings and lifecycle timeouts; wire/name/token bounds stay fixed.
- Dynamic client Services enter reconnect desired state only after a matching RegisterAck from the current Session generation. Error has no ServiceId, so only one dynamic registration may be in flight; keep that invariant unless the wire protocol changes. Heartbeat stores one outstanding Ping and bounded Snapshot health only.
- Features are additive on `eggtunnel` (`default = ["client", "tls"]`): `server`, `quic`, `websocket`, `outbound-proxy`, `mtls`. CLI enables all of them. Embedders use `default-features = false` + only what they need (see `docs/EMBEDDING.md`).
- Hard constraints: `#![forbid(unsafe_code)]` in all crates (proto, library, CLI); `cargo-deny` allows permissive licenses only (GPL/AGPL/LGPL denied — check `deny.toml`); release tag `vX.Y.Z` must equal workspace `version` in root `Cargo.toml` (see `.github/workflows/release.yml`); release builds only `-p eggtunnel-cli` and regenerate notices via `python3 scripts/generate-third-party-notices.py`.
- Version lines: workspace manifest is `0.2.0`; published crates.io line / tag / GitHub release are `0.2.0` (see `docs/DISTRIBUTION.md` + `plans/closure/reverse-session/012-status.md`). Dependency snippets in `docs/API.md` + `docs/EMBEDDING.md` must match the published line (`version = "0.2"`). Wire protocol stays v1.0 independent of crate version.
