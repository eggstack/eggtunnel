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
cargo doc --locked --workspace --all-features --no-deps
cargo check --locked --manifest-path fixtures/embedder/Cargo.toml
cargo audit
cargo deny check licenses
```

## Rules

- Never run workspace commands inside `fixtures/embedder` — it is a
  separate workspace with its own lockfile. Always use
  `--manifest-path fixtures/embedder/Cargo.toml` from the repo root.
- Focused test run: `cargo test --locked -p eggtunnel --all-features <filter>`
  (most tests are inline `#[tokio::test]` in `crates/eggtunnel/src/server.rs`).
- Do not skip `--all-features`: default features hide the
  `quic`/`websocket`/`mtls`/`outbound-proxy` code paths.
- Markdown/docs-only changes need no gate run; say so instead of running them.
