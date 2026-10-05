---
name: release
description: Qualify and cut an Eggtunnel release (tag, archives, publish order)
---

## What I do

Guide the release flow defined by `.github/workflows/release.yml`,
`scripts/test-install.sh`, and `docs/DISTRIBUTION.md`.

## When to use me

Use me when asked to cut, qualify, or publish a release. **Never push a `v*`
tag without explicit human confirmation** — the tag triggers the release
workflow and (via `publish-release`) crate/GitHub publication.

## Read this first: 0.2.0 is already published

`v0.1.0` and `v0.2.0` both exist as git tags, and both `eggtunnel-proto 0.2.0`
and `eggtunnel 0.2.0` are live on crates.io. The workspace manifest in the root
`Cargo.toml` still says `version = "0.2.0"`.

So a new release **must bump the root `Cargo.toml` version first**. Re-tagging
`v0.2.0` is not a release, it is a conflict. Bump the manifest, then tag.

This source tree also implements **wire protocol 1.1**, while the published
`0.2.0` artifact shipped **wire 1.0**. The crate version does not identify wire
behavior of a locally built binary — say this explicitly in the release notes.

## Rules

- Release tag `vX.Y.Z` must equal workspace `version` in root `Cargo.toml`
  (`release.yml` compares them with `sed`; mismatch fails the job).
- When bumping, also edit `scripts/test-install.sh:5`, which **hardcodes
  `version=0.2.0`**. It has no version env override — only
  `EGGTUNNEL_TEST_TARGET`. A version bump that misses this line makes the local
  install smoke test build and assert the *old* version string.
- Release builds only `-p eggtunnel-cli`, then regenerates notices:
  `python3 scripts/generate-third-party-notices.py`
  (output `THIRD_PARTY_NOTICES.md` is a staged artifact, not committed).
- Local qualification before tagging:
  `EGGTUNNEL_TEST_TARGET=<host-triple> ./scripts/test-install.sh`
  (defaults to `aarch64-apple-darwin`; override the triple as needed).
- Crate publication order is `eggtunnel-proto` first, then `eggtunnel`
  (the library has a versioned dependency on proto). `eggtunnel-cli`
  stays `publish = false` and ships only in the binary archives.
- Only these four triples are supported: `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`.
  `install.sh` refuses anything else.
- After tagging, `plans/closure/` records are historical evidence: do not
  rewrite closed milestone records for the new release; add a new one
  (see the `plan` skill for the required closure contents).

## After publishing, update these together

The published line is stated in several places; leaving them disagreeing is the
defect this repo has hit before. Flip all of them:

- `CHANGELOG.md` — new released section (move `Unreleased` content in).
- `docs/DISTRIBUTION.md` — current release state and targets.
- `docs/SUPPORT.md` — crates published at the new line.
- `docs/PROTOCOL.md` and `docs/API.md` — crate version vs wire version.
- `README.md` — `./install.sh vX.Y.Z` and the `version = "0.2"` snippet.
- `docs/EMBEDDING.md` — the `version = "0.2"` snippet.
- `AGENTS.md` — the version line, and the Crate publication order section.
- `architecture/overview.md` §7 and `ops-tooling-distribution.md` §5/§8-E.
- `plans/closure/reverse-session/<n>-status.md` — new record, plus the
  registry and roadmap status tables.

Then re-run the `verify` gate and let hosted CI confirm. Do not claim hosted
evidence from a local run.
