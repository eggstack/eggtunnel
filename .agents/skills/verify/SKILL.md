---
name: verify
description: Run the Eggtunnel CI verification gate sequence in order
---

## What I do

Run the repo's verification gates in CI order (`.github/workflows/ci.yml`).
Every command uses `--locked`; test/lint/doc coverage requires
`--all-features` (covers `quic`/`websocket`/`mtls`/`outbound-proxy` paths).

## When to use me

Use me before committing, after any code change, or when asked to
"verify", "check CI", or "run the tests".

## Steps

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

Run them in this order and stop at the first failure — a later gate's output is
noise until the earlier one is green.

## Rules

- Never run workspace commands inside `fixtures/embedder` — it is a
  separate workspace with its own lockfile. Always use
  `--manifest-path fixtures/embedder/Cargo.toml` from the repo root.
- `fuzz/` is likewise a separate workspace. Run the decoder fuzzer with
  `cargo +nightly fuzz run decode_frame fuzz/corpus/decode_frame` only when
  explicitly asked; it is opt-in sustained qualification, not push CI.
- Focused test run: `cargo test --locked -p eggtunnel --all-features <filter>`
  (tests live in `crates/eggtunnel/src/server_tests.rs` +
  `server_tests/{tcp,mtls,quic,websocket,proxy}.rs` and `client/` modules,
  not inline in `server.rs`).
- Do not skip `--all-features`: default features hide the
  `quic`/`websocket`/`mtls`/`outbound-proxy` code paths.
- Beyond the gate: CI also runs a `feature-slices` matrix via
  `--no-default-features` (14 combos including role-specific
  `quic-client`/`quic-server`/`websocket-client`/`websocket-server` slices),
  an MSRV `1.89` check, and a minimal-dependencies check
  (minimal slices must not pull unrequested quic/websocket/outbound deps) —
  see `.github/workflows/ci.yml`. Keep feature gates additive.
- `cargo audit` is expected to report **zero vulnerabilities** plus one
  informational `unmaintained` warning for `atomic-polyfill 1.0.3` (transitive
  via `postcard`/`heapless`, only compiled on targets without native atomics).
  That one warning is the accepted baseline, not a regression. Any *new*
  advisory is a blocker. See `docs/DISTRIBUTION.md`.
- A green local run is not hosted CI evidence. Never report a milestone or
  release as qualified on local evidence alone (see the `plan` skill).
- Docs/AGENTS/CHANGELOG-only changes need no gate run: say that explicitly
  instead of running it. Do run the gate if any `.rs`, `Cargo.toml`,
  workflow, or `scripts/` file changed.

## Reading the results

Expected on the current tree: library **131** passed + 3 ignored, CLI bin
**16**, CLI integration **7**, proto **11**. The 3 ignored library tests are
the opt-in soak/fuzz targets documented in `docs/OPERATIONS.md`, not dead
tests. Re-derive these counts by running the suite rather than trusting this
file or `architecture/overview.md`.
