# Ops, Tooling, Distribution — Review Deep Dive

Parent index: [architecture/overview.md](overview.md) §7. This file is the
review-oriented map of everything that turns the workspace into a shippable,
auditable artifact: repo layout, CI gates, release pipeline, supply chain,
docs system, plans/evidence system, and fixtures. Code behavior itself lives in
the sibling dives (proto / client / server / transports / CLI).

## 0. Scope and claim boundary

- Current release line: `0.1.0` — `docs/DISTRIBUTION.md:5`, `Cargo.toml:6`,
  `README.md:25-26`.
- "Supported" in this repo means **hosted build + checksum + per-runner
  install/version smoke for that archive** — `docs/DISTRIBUTION.md:24-28`,
  `docs/SUPPORT.md:41-47`. It does **not** mean interactive tunnel runtime
  evidence on the runner; that lives only in the local loopback suite
  (`plans/closure/reverse-session/006-status.md:74-75`).
- Four release targets only. Everything else is unsupported/unevaluated even if
  `rustc` knows the triple (`docs/DISTRIBUTION.md:27-28`,
  `docs/SUPPORT.md:47`).

## 1. Repo layout map

Top level (`/`, see directory read):

| Path | What it owns | Key review anchor |
|---|---|---|
| `Cargo.toml` | Workspace root: resolver 2, 3 members, shared `[workspace.package]` + `[workspace.dependencies]` | `Cargo.toml:1-35` |
| `Cargo.lock` | Locked graph (226 packages at review time); exact Eggress checksums | `Cargo.lock` (eg. `eggress-transport-tls 1.0.8` checksum block) |
| `crates/eggtunnel-proto/` | Runtime-neutral wire DTOs + codec; published to crates.io | `crates/eggtunnel-proto/Cargo.toml:1-19` |
| `crates/eggtunnel/` | Embeddable library; feature-gated client/server/transports; published to crates.io | `crates/eggtunnel/Cargo.toml:1-48` |
| `crates/eggtunnel-cli/` | Private binary (`publish = false`); all-features consumer of the library | `crates/eggtunnel-cli/Cargo.toml:1-22` |
| `docs/` | 9 user-facing guides (see §5) | `docs/` directory listing |
| `plans/` | Canonical spec + roadmaps + ADRs + implementation/closure evidence (see §6) | `plans/registry.md:1-95` |
| `architecture/` | This review layer; currently only the index | `architecture/overview.md:1-183` |
| `examples/` | `client.toml`, `server.toml` starter configs | `examples/` |
| `fixtures/embedder/` | Downstream-shaped embedder proof (see §7); own `Cargo.toml` + `Cargo.lock`, excluded from workspace via `[workspace]` empty table | `fixtures/embedder/Cargo.toml:1-12`, `fixtures/embedder/src/main.rs:1-54` |
| `scripts/` | `generate-third-party-notices.py`, `test-install.sh` only | `scripts/` |
| `install.sh` | Install-only bootstrap script shipped as a release asset | `install.sh:1-60` |
| `.github/workflows/` | `ci.yml` (per-push/PR gates), `release.yml` (tag-triggered 4-target build) | `.github/workflows/ci.yml:1-25`, `.github/workflows/release.yml:1-91` |
| `deny.toml` | `cargo-deny` license policy + 4 `clarify` exceptions | `deny.toml:1-58` |
| `target/` | **Build cache, never committed.** `.gitignore:1-4` ignores `/target`, `target/`, `**/target/`. At review time it contained cross-check outputs (`aarch64-unknown-linux-gnu`, `armv7-unknown-linux-gnueabihf`, `x86_64-pc-windows-gnu`, `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `debug`, `release`, `doc`) — these are local `cargo check` artifacts, not release evidence (cf. `plans/closure/reverse-session/006-status.md:37-41`). Do not review `target/` contents as source. |
| `LICENSE-MIT`, `THIRD_PARTY_NOTICES.md` (generated) | License + per-release dependency inventory staged into every archive | `docs/DISTRIBUTION.md:32-33`, `scripts/generate-third-party-notices.py:40-52` |

Workspace membership and shared metadata:

- Members declared at `Cargo.toml:3`: `crates/eggtunnel-proto`,
  `crates/eggtunnel`, `crates/eggtunnel-cli`.
- Shared version/edition/MSRV/license at `Cargo.toml:5-10`: version `0.1.0`,
  edition `2024`, `rust-version = "1.89"`, `license = "MIT"`.
- `fixtures/embedder` is deliberately **not** a workspace member
  (`fixtures/embedder/Cargo.toml:7` empty `[workspace]` severs membership), so
  CI checks it separately as a downstream stand-in
  (`.github/workflows/ci.yml:20`).

## 2. CI — `.github/workflows/ci.yml`

Single job, no OS/feature matrix. File is 25 lines; every line is load-bearing.

| Step (`ci.yml` line) | Command | What it gates |
|---|---|---|
| `ci.yml:8-9` | `check` on `ubuntu-latest`, triggers `push` + `pull_request` (`ci.yml:3-5`) | Everything below must pass before merge |
| `ci.yml:15` | `cargo fmt --all -- --check` | Formatting; M006 re-verified at closure head (`plans/closure/reverse-session/006-status.md:54`) |
| `ci.yml:16` | `cargo check --locked --workspace --all-targets` | Locked, all-targets compile (default features) |
| `ci.yml:17` | `cargo test --locked --workspace --all-targets --all-features` | **Full feature-union test run.** This is the "test matrix": not a Build matrix but `--all-features` (client+server+tls+mtls+quic+websocket+outbound-proxy via `crates/eggtunnel/Cargo.toml:15-23` + CLI's all-features dep at `crates/eggtunnel-cli/Cargo.toml:13`). M006 closure additionally records per-slice runs (24 quic / 19 websocket / 29 websocket+proxy tests) at `plans/closure/reverse-session/006-status.md:62` — CI itself does not split those slices |
| `ci.yml:18` | `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Zero-warning lint |
| `ci.yml:19` | `cargo doc --locked --workspace --all-features --no-deps` | Doc build (note: CI does not pass `RUSTDOCFLAGS="-D warnings"`; the M006 closure verification did — `plans/closure/reverse-session/006-status.md:58`) |
| `ci.yml:20` | `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml` | Downstream-shaped compile proof (see §7) |
| `ci.yml:21-24` | `taiki-e/install-action` → `cargo audit` + `cargo deny check licenses` | Supply-chain gates (see §4). Note CI runs bare `cargo audit` (exit 0 with unmaintained warnings) and `deny check licenses` only — not `deny check advisories/bans/sources` |

What must pass before merge: **all seven runnable steps on `ubuntu-latest`**.
There is no Windows/macOS CI leg, no `--no-default-features` / minimal-slice
CI leg (minimal-slice evidence is M003/M006 closure-time only,
`plans/closure/reverse-session/006-status.md:61`), and no install-smoke leg —
`scripts/test-install.sh` is a manual/local script, not a CI step.

## 3. Release / distribution

### 3.1 Trigger and version gate (`release.yml`)

- Trigger: pushes of tags `v*` only (`release.yml:3-6`).
- Top-level permission is `contents: read` (`release.yml:8-9`); only
  `publish-release` escalates to `contents: write + id-token: write +
  attestations: write + artifact-metadata: write` (`release.yml:69-74`).
- Version gate (`release.yml:29-34`): strips `v` from `GITHUB_REF_NAME` and
  compares against the first `version = "..."` line of root `Cargo.toml` via
  `sed`. Mismatch fails the job. Review note: the `sed | head -n 1` picks the
  first version line in the file — today that is the `[workspace.package]`
  version, but it is position-sensitive rather than TOML-aware.

### 3.2 Build matrix — candidate targets and qualification state

`release.yml:13-24`:

| Target | Runner | Qualification state (per `docs/DISTRIBUTION.md:17-22`, `docs/SUPPORT.md:41-47`, `plans/closure/reverse-session/006-status.md:31-41`) |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | Supported (build + install/version smoke). Release run `35804871876` linux-x64 ~1m40s |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | Supported (build + install/version smoke) |
| `x86_64-apple-darwin` | `macos-15-intel` | Supported (build + install/version smoke) |
| `aarch64-apple-darwin` | `macos-15` | Supported (build + install/version smoke) **plus** independent consumer-side download/checksum/install/version verification (closure `006-status.md:22,36`). Only target with off-runner consumer evidence |

Unqualified / unsupported (explicitly called out so reviewers do not infer
support from toolchain availability):

| Target | Evidence | Claim |
|---|---|---|
| `x86_64-pc-windows-gnu` | Local `cargo check` via mingw only | Unsupported (informational) |
| `armv7-unknown-linux-gnueabihf` | Local `cargo check` via zig-cc only | Unsupported (informational) |
| `*-pc-windows-msvc`, musl, Raspberry Pi / Le Potato variants | No toolchain or runner evidence | Unsupported/unevaluated |
| Any other triple `install.sh` rejects | `install.sh:32-35` allowlist | Installer refuses |

`fail-fast: false` (`release.yml:14`) — one target failing does not cancel the
others; each uploads its own `release-<target>` artifact with 7-day retention
(`release.yml:60-65`).

### 3.3 Per-target packaging (`release.yml:35-59`)

1. `cargo build --locked --release -p eggtunnel-cli` (`release.yml:36`).
2. `python3 scripts/generate-third-party-notices.py` (`release.yml:38`) — see
   §3.5. Writes `THIRD_PARTY_NOTICES.md` into the workspace root.
3. Stage (`release.yml:41-50`): copy `target/release/eggtunnel`,
   `LICENSE-MIT`, `THIRD_PARTY_NOTICES.md` into `$RUNNER_TEMP/eggtunnel-release`;
   write bare version (no `v`) to `VERSION`; tar to
   `eggtunnel-<tag>-<target>.tar.gz`.
4. Per-runner smoke (`release.yml:50-57`): per-target `SHA256SUMS` written in a
   local `file://` staging dir, then `./install.sh <tag> <tempdir>` with
   `EGGTUNNEL_RELEASE_BASE_URL=file://...` + `EGGTUNNEL_TARGET=<matrix.target>`,
   then `<tempdir>/eggtunnel version`. Archive path exported via `GITHUB_OUTPUT`
   (`release.yml:58`).

### 3.4 Publish (`release.yml:67-91`)

- Downloads all `release-*` artifacts into `dist/` (`release.yml:77-81`).
- Recomputes a **global** `SHA256SUMS` with `LC_ALL=C sha256sum *.tar.gz`
  (`release.yml:83`) — note this differs from the per-runner
  `shasum -a 256 <single-archive>` manifest used for the smoke test; the
  published manifest covers all four archives.
- `actions/attest@v4` build-provenance attestations over `dist/*.tar.gz`
  (`release.yml:84-87`).
- `gh release create <tag> dist/*.tar.gz dist/SHA256SUMS install.sh
  --verify-tag --generate-notes` (`release.yml:88-91`): publishes archives +
  manifest + the installer itself, with **auto-generated** notes. The M006
  closure explicitly records "no manual release narrative was written.
  Acceptable for 0.1.0" (`plans/closure/reverse-session/006-status.md:73`).

Published crate order (not GitHub assets): `eggtunnel-proto 0.1.0` first, then
`eggtunnel 0.1.0` (`docs/DISTRIBUTION.md:5-7,55-60`), because the library has a
versioned dependency on the proto crate (`crates/eggtunnel/Cargo.toml:26` pins
`version = "0.1.0"`). The closure records the ordering proof: dry-run publish
of the library before the proto crate failed with `no matching package named
eggtunnel-proto found` (`plans/closure/reverse-session/006-status.md:50`).
`eggtunnel-cli` stays `publish = false` and ships only inside the binary
archive (`crates/eggtunnel-cli/Cargo.toml:10`, `docs/DISTRIBUTION.md:9-11`).

### 3.5 Third-party notices generation

`scripts/generate-third-party-notices.py:1-57`:

- Runs `cargo metadata --locked --format-version 1`
  (`generate-third-party-notices.py:12-14`) — locked graph, reproducible.
- Finds the single `eggtunnel-cli` package, walks `resolve.nodes[].deps`
  transitively (`:22-29`), sorts by `(name, version)` (`:32`), skips the root
  itself (`:34-35`).
- Each row: `` `name` | `version` | <SPDX-or-fallback> | <repository-or-homepage> ``
  (`:38`). Missing license metadata renders literally as
  `License metadata unavailable` rather than failing.
- Header disclaimer (`:41-46`): inventory does not replace license texts; exact
  graph is in `Cargo.lock`. Output defaults to `./THIRD_PARTY_NOTICES.md`,
  overridable by `argv[1]` (`:40`).

Review properties: deterministic given a lockfile; CLI-reachable closure only
(library-only deps not reachable from the CLI binary are out of scope by
design); no license-text embedding — texts come from the crates themselves via
the archive's `LICENSE-MIT` plus upstream package metadata.

### 3.6 `install.sh` verification and `scripts/test-install.sh`

`install.sh:1-60`:

| Line | Behavior |
|---|---|
| `install.sh:10-17` | Arity 1–3 (`<version> [destination] [target]`); version must match `vMAJOR.MINOR.PATCH` or exit 2 |
| `install.sh:19-31` | Target resolution: explicit `$3`/`EGGTUNNEL_TARGET` wins; else `uname -s`/`uname -m` mapped for Linux/Darwin x86_64/aarch64 (incl. `arm64` aliases). Anything else → `unsupported target` exit 1 |
| `install.sh:32-35` | Allowlist of exactly the 4 release triples; anything else → `unsupported target triple` exit 1 |
| `install.sh:37-44` | `EGGTUNNEL_RELEASE_BASE_URL` (default GitHub releases download) + `curl --fail --location --silent --show-error` fetch of `<archive>` and `SHA256SUMS` into a private `mktemp -d` |
| `install.sh:45-51` | `awk`-extracted expected hash for the archive name; `shasum -a 256` on Darwin else `sha256sum`; mismatch → `checksum verification failed` exit 1. Missing entry → `checksum entry missing` exit 1 |
| `install.sh:53-57` | Unpack to `$tmp/unpacked`; require executable `eggtunnel`, `VERSION` file, and `VERSION == ${version#v}` |
| `install.sh:58-60` | `mkdir -p destination` (default `$EGGTUNNEL_INSTALL_DIR` or `$HOME/.local/bin`), `install -m 755`. No root required, no service-manager update, no self-update (`docs/DISTRIBUTION.md:40-42`) |

`scripts/test-install.sh:1-37` is the local installer qualification harness
(not run in CI):

- Pins `version=0.1.0` (`test-install.sh:5`) and defaults
  `EGGTUNNEL_TEST_TARGET=aarch64-apple-darwin` (`:6`) — both drift candidates
  when the version bumps or when run on Linux (caller must override the env).
- Builds the release binary `--locked`, regenerates notices, stages
  `eggtunnel + LICENSE-MIT + THIRD_PARTY_NOTICES.md + VERSION` into a tarball,
  writes a `SHA256SUMS` with `shasum` (`:11-20`), then installs via
  `file://` base URL and asserts `eggtunnel version` prints `eggtunnel 0.1.0`
  (`:22-25`).
- Negative cases: appends a byte to the archive and asserts the installer
  **rejects** it (`:27-32`); asserts `unsupported-target` is rejected
  (`:34-37`).
- Cleanup trap removes `$tmp` **and** `$root/THIRD_PARTY_NOTICES.md`
  (`test-install.sh:8`) — the generated file is a build artifact, not to be
  left behind (or committed) after the test.

## 4. Supply chain

### 4.1 `Cargo.lock`

- 226 locked dependencies at review time; every Eggress crate pinned to
  `1.0.8` with registry checksums (eg. `eggress-transport-tls 1.0.8`).
- CI and release both build with `--locked` (`ci.yml:16-20`,
  `release.yml:36`), so a lockfile drift fails loudly rather than resolving
  silently.
- `cargo audit` runs on every push/PR (`ci.yml:24`). Recorded M006 outcome:
  **0 vulnerabilities, 2 `unmaintained` warnings** —
  `atomic-polyfill 1.0.3` (RUSTSEC-2023-0089, transitive via
  `postcard`/`heapless`, only compiled on targets without native atomics) and
  `rustls-pemfile 2.2.0` (RUSTSEC-2025-0134, direct `mtls` PEM-parsing dep)
  (`docs/DISTRIBUTION.md:64-69`,
  `plans/closure/reverse-session/006-status.md:66`). Bare `cargo audit` exits
  0; `--deny warnings` would exit 1. PEM-parser replacement is deferred
  follow-up, explicitly not a release blocker.

### 4.2 `deny.toml` policy

Header comment states the policy plainly (`deny.toml:1-7`): every third-party
dependency must be permissive; anything not in `allow` is denied, **including
GPL/AGPL/LGPL as a sole license**.

- `allow` list (`deny.toml:10-21`): `Apache-2.0`,
  `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`,
  `CDLA-Permissive-2.0`, `ISC`, `MIT`, `Unicode-3.0`, `Unlicense`, `Zlib`.
- Four `[[licenses.clarify]]` exceptions (`deny.toml:28-58`) for crates whose
  legacy `/`-separated metadata is not a valid SPDX expression. Each pins the
  SPDX equivalent with license-file hashes (first 32 bits of file SHA-256):

| Crate | Clarified expression | Files/hashes |
|---|---|---|
| `fnv` | `Apache-2.0 OR MIT` | `LICENSE-APACHE 0xa60eea81`, `LICENSE-MIT 0x65fdb6c7` |
| `same-file` | `Unlicense OR MIT` | `UNLICENSE 0x7e12e5df`, `LICENSE-MIT 0xcb3c929a` |
| `version_check` | `Apache-2.0 OR MIT` | `LICENSE-APACHE 0xa60eea81`, `LICENSE-MIT 0xb7e650f3` |
| `walkdir` | `Unlicense OR MIT` | `UNLICENSE 0x7e12e5df`, `LICENSE-MIT 0x0f96a838` |

- CI enforces `cargo deny check licenses` (`ci.yml:25`); M006 records `licenses
  ok` (`plans/closure/reverse-session/006-status.md:67`). Note the gap: only
  the `licenses` check runs — `advisories`, `bans`, and `sources` are not
  invoked (advisory coverage comes from `cargo audit` instead).

### 4.3 Eggress `=1.0.8` pinning rationale

Root pins (`Cargo.toml:22-27`):

```toml
eggress-core = "=1.0.8"
eggress-relay = "=1.0.8"
eggress-transport-tls = "=1.0.8"
eggress-transport-quic = { version = "=1.0.8", default-features = false }
eggress-protocol-websocket = { version = "=1.0.8", default-features = false }
eggress-outbound = { version = "=1.0.8", default-features = false, features = ["pproxy-compat"] }
```

Why exact (`=`) rather than caret:

- The subsystem roadmap records `1.0.8` as the inspected integration baseline:
  "The TLS, relay, and core stream APIs were inspected before adding
  dependencies" (`plans/subsystems/reverse-session-roadmap.md:103`), and the
  registry restates the direction — Eggtunnel is a thin reverse-session layer,
  generic byte relay uses the Eggress boundary, optional components stay
  feature-gated, `eggress-embed` is not the boundary, Synvoid/i2pr are
  references only (`plans/registry.md:71-77`).
- `SECURITY.md:41-48` documents the consequence: the TLS profile installs the
  Rustls ring provider as process default if unset, and the QUIC adapter's
  limits (platform roots only, no custom CA/mTLS) are inherited constraints,
  not Eggtunnel choices. Exact pinning makes such inherited behavior changes
  explicit upgrades rather than silent semver drift.
- Narrow crates + `default-features = false` where it matters
  (`Cargo.toml:25-27`, `crates/eggtunnel/Cargo.toml:29-34`) plus the M006
  feature-slice check (`cargo tree ... | quic|websocket|outbound` → no matches
  for the minimal slice, `plans/closure/reverse-session/006-status.md:61`)
  enforce the "no dependency leakage" mitigation from roadmap §17
  (`plans/subsystems/reverse-session-roadmap.md:434-439`).
- Cost: every Eggress upgrade (even patch) requires an intentional lockfile +
  manifest edit and re-qualification; Dependabot-style range updates cannot
  sneak in.

## 5. Docs system — what each `docs/*.md` owns

| Doc | Owns | Review anchor |
|---|---|---|
| `ARCHITECTURE.md` | One-paragraph ownership thesis: Eggtunnel = reverse-session behavior; Eggress = generic relay/transport; proto = runtime-neutral bounded DTOs/framing | `docs/ARCHITECTURE.md:1-21` |
| `PROTOCOL.md` | Wire v1.0: 14-byte header table, 1 MiB cap, exact-consumption decoding, 14 stable IDs, field bounds, crate-vs-wire version split (crate `0.1.x`, wire `1.0`, no 1.x guarantee) | `docs/PROTOCOL.md:1-39` |
| `CONFIGURATION.md` | CLI TOML reference for client+server, `token_env` indirection, proxy URI/`__`-chain syntax, per-profile rejection rules, `eggtunnel check` scope | `docs/CONFIGURATION.md:1-84` |
| `SECURITY.md` | Threat-relevant claims: TLS-before-auth, constant-time token compare, bind policy, ConnectionId lifecycle, handshake/auth throttle numbers, mTLS principal binding, per-transport caveats (QUIC pre-session admission, WSS close semantics, proxy redaction/no-fallback) | `docs/SECURITY.md:1-85` |
| `SUPPORT.md` | Transport matrix (TCP/TLS, QUIC, WSS, outbound-proxy) + **release-target evidence table** (the qualification half of the support claim) + inherited M004/M005 gaps | `docs/SUPPORT.md:1-52` |
| `OPERATIONS.md` | Runtime operator view: start/stop, Drain, snapshot counters, retry/backoff, file-permission hygiene, resource ceilings (64 handshakes / 128 sessions / 64 services / 128 pending+active / 128 open tasks+queues), proxy env wiring | `docs/OPERATIONS.md:1-49` |
| `DISTRIBUTION.md` | Release state, 4-target table, archive contents/integrity semantics, installer scope, Eggup deferral rationale, publication order, audit/license outcomes | `docs/DISTRIBUTION.md:1-76` |
| `API.md` | Crate roles + publication intent, recommended `default-features = false` dependency lines, client/server surface pointers, semver warning (breaking changes allowed pre-1.0) | `docs/API.md:1-53` |
| `EMBEDDING.md` | Caller-owned runtime/tracing/config/connector recipe, `start_with_connector` variants, per-transport entry points, secret-store guidance | `docs/EMBEDDING.md:1-51` |

Cross-links: `README.md:11-13,24-26` points to CONFIGURATION/SECURITY/SUPPORT,
the subsystem roadmap, and DISTRIBUTION; `overview.md:142-150` summarizes this
whole ops layer.

Doc-vs-code consistency risks (check these first in any behavior change):

1. **Numeric ceilings** are stated in three places: code constants,
   `OPERATIONS.md`/`SECURITY.md`, and plan invariants. Any limit change must
   update all three plus tests.
2. **Transport rejection matrix** (QUIC+custom-CA/mTLS/proxy, WSS+mTLS,
   proxy+mTLS, server+proxy) is enforced by `eggtunnel check` and repeated in
   CONFIGURATION + SECURITY + SUPPORT + API. A new allowed combination needs a
   code change, a `check` change, and four doc edits.
3. **Eggress version** appears in `Cargo.toml`, `Cargo.lock`, CONFIGURATION
   (`1.0.8` URI families), SECURITY (adapter limits), and roadmap §3. Pin bump
   = update all five.
4. **Release target table** is duplicated in DISTRIBUTION and SUPPORT with
   different emphasis (policy vs evidence). M006 kept them in sync via the
   closure record; future target changes must edit both plus `install.sh` and
   `release.yml`.
5. **Wire-vs-crate versioning** (PROTOCOL + API + roadmap §13): the "no 1.x
   guarantee" disclaimer must survive any copy-edit; deleting it would imply a
   stability promise the code does not keep.

## 6. Plans system — how planning and evidence flow

Governance: `plans/003-planning-process.md:1-60` separates long-term planning
(product identity, invariants) from interim planning (executable work vs a
baseline). Implementation difficulty never silently weakens the long-term
contract (`003:16`).

| Layer | Files | Role |
|---|---|---|
| Canonical direction | `plans/000-long-term-specification.md`, `plans/001-terminology-and-domain-model.md`, `plans/002-long-term-roadmap.md`, `plans/003-planning-process.md` | Normative MUST/SHOULD end-state, vocabulary, phase order, handoff rules. Amendable only on direction change, contradiction, ADR, or explicit user direction (`003:27-34`) |
| ADRs | `plans/adrs/ADR-0001-session-transport-and-egress-boundary.md` | Required for protocol/API/dependency/transport/trust/storage/security/release/non-goal changes (`003:38-50`); must carry status, context, alternatives, decision, consequences (`003:52-60`) |
| Control surface | `plans/registry.md` | Compact status table: subsystem roadmaps, implementation plans, closure records, architecture constraints (`registry.md:69-86`), deferred work (`:87-95`). Reviewers start here |
| Subsystem roadmaps | `plans/subsystems/reverse-session-roadmap.md` (M001–M006, 492 lines, §§1–19), `plans/subsystems/reverse-session-post-closure-corrective-addendum.md` (C001) | Own invariants (§2: protocol/correlation/listener/runtime/embedding), dependency graph (§5: M001→M002→M003→{M004,M005}→M006), per-milestone objectives/exit criteria (§§6–11), risks (§17), deferred work (§18), status table (§19) |
| Implementation plans | `plans/implementation/reverse-session/001..006-*.md` + `plans/implementation/reverse-session-post-closure-corrective/001-*.md` | Bounded executable work vs a named baseline (eg. M006 baseline `448c615`, `006-*.md:9-11`) |
| Closure records | `plans/closure/reverse-session/001..006-status.md` + `plans/closure/reverse-session-post-closure-corrective/001-status.md` | Evidence that exit criteria held: requirement tables, platform tables, verification command logs, dependency/license evidence, findings disposition. M006 (`006-status.md:1-82`) is the release qualification record; C001 supplements M004/M005 without rewriting their historical closures (`registry.md:53-58`) |

Flow: canonical spec → roadmap milestone → implementation plan (baseline-pinned)
→ code+tests → closure record (commands + hashes + tables) → `registry.md`
status flip. Statuses use the fixed vocabulary at `registry.md:16-26`
(proposed/ready/active/blocked/closing/closed/conditionally closed/superseded/
archived). Today: M001–M006 closed, C001 closed (supplemental), corrective
addendum retained active for traceability (`registry.md:30-33`).

## 7. Fixtures — `fixtures/embedder`

Purpose: downstream-shaped **compile/run proof that the public embedding API
suffices without the CLI, default features, or a library-owned runtime**
(`docs/API.md:51-53`, `docs/EMBEDDING.md:36-38`).

- `fixtures/embedder/Cargo.toml:1-12`: package `eggtunnel-embedder-fixture`
  (`publish = false`), own `[workspace]` (decoupled), deps pinned to
  `eggtunnel = { path = "../../crates/eggtunnel", default-features = false,
  features = ["client", "tls"] }` plus caller-owned `tokio` and `tracing`.
  Mirrors the recommended downstream line in `docs/API.md:19` and
  `docs/EMBEDDING.md:9`.
- `fixtures/embedder/src/main.rs:1-54`: public-imports-only client
  (`eggtunnel::{Client, ClientConfig, ...}` + `proto::{...}`), in-process echo
  `TargetConnector` over `tokio::io::duplex(16 KiB)` (`:11-23`), programmatic
  `ClientConfig` with `SecretToken` (`:25-40`), caller-built multi-thread
  runtime (`:50-52`), `client.shutdown().await` join (`:44`).
- Qualification: `cargo check --locked --manifest-path
  fixtures/embedder/Cargo.toml` in CI (`ci.yml:20`) and again at the M006
  closure head (`006-status.md:59`); registry-consumption variant (published
  `eggtunnel = "0.1"` from crates.io, lockfile `07edbb9a…`) compiled and ran
  (`006-status.md:23`). Own `fixtures/embedder/Cargo.lock` proves the
  downstream resolution independently of the workspace lockfile.

## 8. Review checklist — gaps, drift, and stale-state risks

### A. Unqualified targets (do not accept support claims beyond the table)

- [ ] Windows (`-gnu` check-only, `-msvc` no evidence), musl, armv7 (check-only),
  Raspberry Pi / Le Potato — all unsupported/unevaluated per
  `docs/SUPPORT.md:47` and `006-status.md:37-41`. A `cargo check` log is not
  install/smoke evidence.
- [ ] Interactive relay on runners beyond install/version smoke: recorded scope
  boundary, not a defect (`006-status.md:74`). Full behavior coverage rests on
  the local loopback suite — verify the suite actually ran for the head under
  review (`006-status.md:56`: 39 lib + 8 proto tests).
- [ ] Checksums detect corruption against a trusted manifest; they are not
  signatures or authenticity proofs (`docs/DISTRIBUTION.md:34-36`).
  Attestations are build provenance, not independent code review.

### B. Release-process gaps

- [ ] Release notes are `--generate-notes` auto-text; no curated narrative
  (`release.yml:91`, `006-status.md:73`). Acceptable for 0.1.0, but confirm a
  future process owner for user-facing changelogs.
- [ ] `install.sh` requires `curl` + `shasum`/`sha256sum` + `install(1)` and
  supports exactly 4 triples (`install.sh:32-35`, `006-status.md:75`). No
  self-update, no service-manager integration by design
  (`docs/DISTRIBUTION.md:40-42,44-51`). Any "update" request reopens the Eggup
  deferral decision — needs an ADR, not a script patch.
- [ ] Post-tag tree discipline: M006 allowed docs/planning-only changes after
  the tag with re-verified CI (`006-status.md:12`). Confirm no production,
  dependency, workflow, or packaging input changed after the tag for any future
  release the same way (diff tag commit vs closure tree).

### C. Script drift

- [ ] `scripts/test-install.sh:5` hardcodes `version=0.1.0` — must be bumped
  with every release (or parameterized). Default test target
  `aarch64-apple-darwin` (`:6`) mismatches Linux CI hosts unless
  `EGGTUNNEL_TEST_TARGET` is overridden.
- [ ] `test-install.sh` is not invoked by CI or `release.yml`; the release
  smoke is inline in `release.yml:50-57`. Changes to one harness do not
  propagate to the other — keep archive layout (`eggtunnel/VERSION/LICENSE/
  NOTICES`), negative cases (corruption, bad target), and `VERSION`-exactness
  checks in sync across both.
- [ ] `release.yml:33` version extraction (`sed | head -n 1`) is
  position-sensitive; adding a `version` line above `[workspace.package]` (or a
  comment matching the pattern) silently changes what is compared.
- [ ] Per-runner (`shasum`, single-file) vs published (`sha256sum`, all-files,
  `LC_ALL=C`) manifest paths differ (`release.yml:53` vs `:83`). Both are
  correct for their context, but a checksum-tooling change must touch both.
- [ ] `generate-third-party-notices.py` falls back to the literal string
  `License metadata unavailable` (`:36`) and emits repository-or-empty
  (`:37`) — a new dependency without license metadata passes generation while
  failing `cargo deny`. Treat a fallback string in a release diff as a
  stop-and-investigate signal.

### D. `deny.toml` exceptions (re-verify on every bump)

- [ ] Four `clarify` blocks (`deny.toml:28-58`) pin license-file hashes. Any
  upstream re-licensing, re-formatting, or file rename in
  `fnv / same-file / version_check / walkdir` breaks `cargo deny` until hashes
  are re-attested — verify hash updates against actual license texts, not just
  CI output.
- [ ] Only `check licenses` is enforced (`ci.yml:25`). New `allow` entries
  widen the policy permanently; each addition should cite the crate(s) that
  require it and confirm no copyleft-only license slipped in via dual-license
  clarification.
- [ ] `cargo audit` vs `cargo deny advisories`: audit covers RustSec DB on
  every push (`ci.yml:24`), but bare `cargo audit` tolerates the two recorded
  `unmaintained` findings. If policy ever hardens to `--deny warnings`, both
  `atomic-polyfill` and `rustls-pemfile` become blockers — track the deferred
  PEM-parser replacement (`docs/DISTRIBUTION.md:68-69`, `006-status.md:71`).

### E. Stale docs / plans sweep (run before any release)

- [ ] `README.md:25-26` says "candidate release targets and qualification
  state" — after M006 the targets are qualified, not candidate. Confirm wording
  still matches DISTRIBUTION/SUPPORT intent.
- [ ] `plans/registry.md:32-33`: corrective addendum stays `active` for
  traceability while C001 is closed — intentional, but any new corrective work
  must decide whether to reopen C002 or amend the addendum rather than leaving
  two "active" corrective surfaces.
- [ ] Roadmap §3 (`reverse-session-roadmap.md:99-103`) and `registry.md:32`
  both restate M001–M006/C001 closure; milestone status tables (§19) duplicate
  `registry.md:41-49`. Status flips must land in all three or the control
  surface lies.
- [ ] `target/` cross-check triples observed on disk
  (`aarch64-unknown-linux-gnu`, `armv7-...`, `x86_64-pc-windows-gnu`, ...) are
  consistent with `006-status.md:37-41` build-only claims today — if new triple
  directories appear, confirm they are check-only experiments and not nascent
  support claims.
- [ ] Eggup deferral (`docs/DISTRIBUTION.md:44-51`, `registry.md:93`) cites an
  inspected Eggup interface snapshot. Re-validate before citing it for the next
  release; an Eggup downloader/bootstrap release since would reopen the
  installer-vs-shared-updater decision.
