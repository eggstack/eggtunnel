---
name: docs-sync
description: Keep AGENTS.md, docs/, README, CHANGELOG and architecture/ accurate after a code, config, CI or release change
---

## What I do

Keep the repo's prose layer truthful. Eggtunnel repeats a small number of facts
in many places; this skill is the list of places, so a change lands everywhere
it must and nowhere it must not.

## When to use me

Use me after any change to code, config keys, features, dependency versions,
numeric limits, CI jobs, release targets, or version numbers — and whenever
asked to review, prune, or refresh `AGENTS.md`, `docs/`, `README.md`,
`CHANGELOG.md`, or `architecture/`.

## The index

`architecture/overview.md` is the entry point and the module index. Dive by
question:

| Question | Deep dive |
|---|---|
| wire/framing | `proto-wire-protocol.md` |
| secrets/policy/counters/errors | `common-core.md` |
| session/reconnect/data path | `client.md` |
| listeners/admission/mTLS | `server.md` |
| TLS/QUIC/WSS/proxy + Eggress | `transports-wire-io.md` |
| TOML/`check`/embedder facade | `cli-config-ops.md` |
| CI/release/licenses/`docs/`+`plans/` ownership | `ops-tooling-distribution.md` |

`ops-tooling-distribution.md` §5 is the authoritative table of what each
`docs/*.md` owns; §8-E is the pre-release stale-sweep checklist.

## Sync points (from `ops-tooling-distribution.md` §5)

Each of these appears in more than one place. Edit all of them together:

1. **Numeric ceilings** — code constants + `docs/OPERATIONS.md` +
   `docs/SECURITY.md` + plan invariants + tests. Current defaults: 128
   sessions, 64 services/session, 128 pending, 128 active connections, 64
   handshakes, 128 client open tasks, 128 control queue, 32 client command
   queue. Auth throttle: 100 ms delay, 10 failures / 60 s / 1024 source IPs.
2. **Transport rejection matrix** — QUIC rejects custom CA, mTLS, and proxy;
   WSS rejects mTLS; outbound-proxy rejects QUIC and mTLS. Repeated in
   `CONFIGURATION`, `SECURITY`, `SUPPORT`, `API`, and enforced by `check`.
   A new allowed combination needs a code change, a `check` change, and four
   doc edits.
3. **Eggress version** — `Cargo.toml`, `Cargo.lock`, `CONFIGURATION` (URI
   families), `SECURITY` (adapter limits), roadmap §3, `AGENTS.md`,
   `architecture/overview.md`, `transports-wire-io.md`. A pin bump touches all.
4. **Release target table** — `DISTRIBUTION` (policy) and `SUPPORT`
   (evidence), plus `install.sh` and `release.yml`. Four supported triples.
5. **Wire-vs-crate versioning** — `PROTOCOL` + `API` + `README` + `CHANGELOG`.
   Wire is `1.1` with `1.0` fallback; the crate line is `0.2`. These are
   independent numbers and must never be conflated. The published `0.2.0`
   artifact shipped wire `1.0`; this source implements wire `1.1`.
6. **Published-vs-previous language** — `DISTRIBUTION`, `SUPPORT`,
   `CHANGELOG`, `README`, and the release closure record must agree on which
   line is actually published. No "candidate only" / "do not tag" hedging once
   a release exists.

## What may live where

- `docs/`, `README.md`, `AGENTS.md` describe **shipped behavior only**. Planned
  or blocked work belongs in `plans/`. If you cannot point at the code that
  implements it, it does not belong in user docs.
- `AGENTS.md` is a **thin index and gotcha list**, not a documentation copy. It
  should stay short enough to read in one pass and should point into
  `architecture/` rather than restate it.
- `architecture/*.md` are review deep dives that quote line counts and
  `file:line` anchors, so they decay as code refactors. Update them when the
  code moves, not only when a doc is rewritten.
- `CHANGELOG.md` gets an `## Unreleased` section for post-release work so the
  next published line is not silently missing shipped changes.

## Verifying instead of trusting

- Re-derive line counts with `wc -l`; never copy a count from a sibling table.
  A prior audit here found every `docs/*.md` range stale while the summary
  tables looked green.
- Open every `file:line` anchor you keep. A stale anchor in a review doc is
  worse than no anchor.
- Re-derive test counts by running the suite; do not trust the number in
  `architecture/overview.md`. Current: library 131 passed + 3 ignored (the
  opt-in soak/fuzz set, not dead tests), CLI bin 16, CLI integration 7,
  proto 11.
- Re-derive feature/CI/release facts from the manifests and workflows:
  `feature-slices` has 14 matrix entries, CI has 4 jobs, `Cargo.lock` has 230
  packages.
- When a doc-only change touches no Rust/TOML/workflow file, say so instead of
  running the full build gate — but do re-run it if any code, manifest, CI, or
  script changed.

## Pruning

Delete guidance that the code no longer supports rather than softening it. Keep
limitations that are still true: WebSocket is not qualified as TCP half-close
equivalent; `check` is structural only (no PEM parsing, DNS, or dialing); CLI
requires at least one `[[services]]` while the library allows an empty set;
there is no self-update engine and `eggtunnel-cli` is never published.
