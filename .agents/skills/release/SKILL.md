---
name: release
description: Qualify and cut an Eggtunnel release (tag, archives, publish order)
---

## What I do

Guide the release flow defined by `.github/workflows/release.yml`,
`scripts/test-install.sh`, and `docs/DISTRIBUTION.md`.

## When to use me

Use me when asked to cut, qualify, or publish a release. Never push a
`v*` tag without explicit human confirmation — the tag triggers the
release workflow and (via `publish-release`) crate/GitHub publication.

## Rules

- Release tag `vX.Y.Z` must equal workspace `version` in root `Cargo.toml`
  (`release.yml` compares them with `sed`; mismatch fails the job).
  Bump the manifest first, then tag.
- Release builds only `-p eggtunnel-cli`, then regenerates notices:
  `python3 scripts/generate-third-party-notices.py`
  (output `THIRD_PARTY_NOTICES.md` is a staged artifact, not committed).
- Local qualification before tagging:
  `EGGTUNNEL_TEST_TARGET=<host-triple> ./scripts/test-install.sh`
  (defaults pin `version=0.1.0` and `aarch64-apple-darwin` — override both).
- Crate publication order is `eggtunnel-proto` first, then `eggtunnel`
  (the library has a versioned dependency on proto). `eggtunnel-cli`
  stays `publish = false` and ships only in the binary archives.
- Only these four triples are supported: `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`.
  `install.sh` refuses anything else.
- After tagging, `plans/closure/` records are historical evidence: do not
  rewrite closed milestone records for the new release; add a new one.
