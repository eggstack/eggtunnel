# Distribution and release policy

## Current release state

The current release line is `0.2.0` (published 2026-09-24). `eggtunnel-proto
0.2.0` and `eggtunnel 0.2.0` are published on crates.io (`eggtunnel-proto`
first, then `eggtunnel`, matching the versioned dependency order). Git tag
`v0.2.0` points at the qualified head and the GitHub release `v0.2.0`
carries versioned archives, a SHA-256 manifest, build provenance
attestations, and `install.sh`. `eggtunnel-cli` remains a private workspace
package (`publish = false`) and ships only inside the binary archive. The CLI
crate depends on the library, never the reverse.

The previous published line `0.1.0` remains immutable on crates.io and on
the GitHub release of the same tag for historical reference; future work
builds on `0.2.0`.

## Release targets

The release workflow builds archives for these supported targets:

| Target | Build runner | Evidence |
|---|---|---|
| `x86_64-unknown-linux-gnu` | GitHub Ubuntu x64 | Hosted build, checksum, per-runner install/version smoke |
| `aarch64-unknown-linux-gnu` | GitHub Ubuntu arm64 | Hosted build, checksum, per-runner install/version smoke |
| `x86_64-apple-darwin` | GitHub macOS Intel | Hosted build, checksum, per-runner install/version smoke |
| `aarch64-apple-darwin` | GitHub macOS arm64 | Hosted build, checksum, per-runner install/version smoke, plus independent consumer-side download/checksum/install/version verification |

Support means hosted build plus install/version smoke for that archive. No
target has interactive tunnel runtime evidence beyond the local loopback
suite; Windows, musl, armv7 and SBC targets are not in the release set and
are unsupported. A Rust target being available does not make that target
supported.

## Archives and integrity

Each versioned archive contains `eggtunnel`, `VERSION`, `LICENSE-MIT`, and a
generated `THIRD_PARTY_NOTICES.md`. Release assets include a SHA-256
`SHA256SUMS` file and GitHub build-provenance attestations. Checksums detect
asset corruption when the manifest is trusted; they are not a signature or
independent authenticity proof.

`install.sh <version> [destination]` supports the four supported
Linux/macOS targets, verifies the archive against the release checksum
manifest, extracts to a private temporary directory, and installs to the
selected destination.
It does not require root and does not update an existing service manager.
There is no self-update command.

## Shared updater decision

Eggup currently provides local verified staging, integrity, validation,
ownership checks, commit/rollback, and a distribution schema. The inspected
Eggup packages do not provide a released binary downloader, release selector,
bootstrap installer, or service manager integration. Eggtunnel therefore
uses a small install-only script and defers self-update rather than copying
Eggup transaction machinery into the CLI. No update engine is embedded.

## Publication ordering

The Rust publication order is `eggtunnel-proto`, then `eggtunnel` (which has
a versioned dependency on the former). Both are published after package
inspection, downstream compile qualification, dependency/license review, and
acceptance of the release support matrix. The CLI remains unpublished. No
automatic crate publication is configured; each publish is an explicit
authorized action.

## Supply-chain review

- `cargo audit`: no vulnerabilities across the locked dependency graph. The current repository review has one informational `unmaintained` warning:
  `atomic-polyfill 1.0.3` (transitive via `postcard`/`heapless`, only compiled
  on targets without native atomics). M007 removed direct `rustls-pemfile` use
  and now parses PEM through Rustls pki-types. The M006 closure record retains
  the two-warning result observed at that historical head.
- `cargo deny check licenses` with `deny.toml`: passes. Every third-party
  dependency resolves to a permissive license (MIT/Apache-2.0 family, ISC,
  BSD, Zlib, Unlicense, CDLA-Permissive-2.0, Unicode-3.0); no copyleft license
  is present. Four legacy `/`-separated license declarations are clarified to
  SPDX equivalents pinned by license-file hashes.
- Both checks run in CI on every push and pull request. The exact outcomes for
  the release head are verified before tagging.

## Release evidence

The current release evidence is in the M012 closure record
(`plans/closure/reverse-session/012-status.md`, release-candidate
qualification) and the M013 closure record
(`plans/closure/reverse-session/013-status.md`, publication event:

- GitHub release `v0.2.0` — release workflow run `36058175606`.
- `eggtunnel-proto 0.2.0` then `eggtunnel 0.2.0` published to crates.io
  in the dependency order recorded in M012.
- Clean registry consumer: `eggtunnel = "0.2"` resolved from
  `registry+https://github.com/rust-lang/crates.io-index`.
