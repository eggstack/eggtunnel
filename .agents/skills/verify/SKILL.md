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

## Rules

- Never run workspace commands inside `fixtures/embedder` — it is a
  separate workspace with its own lockfile. Always use
  `--manifest-path fixtures/embedder/Cargo.toml` from the repo root.
- Focused test run: `cargo test --locked -p eggtunnel --all-features <filter>`
  (tests live in `crates/eggtunnel/src/server_tests.rs` +
  `server_tests/{tcp,mtls,quic,websocket,proxy}.rs` and `client/` modules,
  not inline in `server.rs`).
- Do not skip `--all-features`: default features hide the
  `quic`/`websocket`/`mtls`/`outbound-proxy` code paths.
- Beyond the gate: CI also runs 7 `--no-default-features` feature-slice
  combos, an MSRV `1.89` check, and a minimal-dependencies check
  (`client,tls` must not pull quic/websocket/outbound deps) — see
  `.github/workflows/ci.yml`. Keep feature gates additive.
- Markdown/docs-only changes need no gate run; say so instead of running them.
