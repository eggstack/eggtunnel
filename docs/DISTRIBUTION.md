# Distribution and release policy

## Current release state

Eggtunnel is at `0.1.0`. `eggtunnel-proto 0.1.0` and `eggtunnel 0.1.0` are
published on crates.io (`eggtunnel-proto` first, then `eggtunnel`, matching the
versioned dependency order). Git tag `v0.1.0` points at the qualified head and
GitHub release `v0.1.0` carries versioned archives, a SHA-256 manifest, build
provenance attestations, and `install.sh`. `eggtunnel-cli` remains a private
workspace package (`publish = false`) and ships only inside the binary
archive. The CLI crate depends on the library, never the reverse.

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

`install.sh <version> [destination]` supports the four candidate Linux/macOS
targets, verifies the archive against the release checksum manifest, extracts
to a private temporary directory, and installs to the selected destination.
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
a versioned dependency on the former). Both were published for `0.1.0` after
package inspection, downstream compile qualification, dependency/license
review, and acceptance of the release support matrix. The CLI remains
unpublished. No automatic crate publication is configured; each publish is an
explicit authorized action.

## Supply-chain review

- `cargo audit`: no vulnerabilities across the locked dependency graph. Two
  `unmaintained` warnings remain open: `atomic-polyfill 1.0.3` (transitive via
  `postcard`/`heapless`, only compiled on targets without native atomics) and
  `rustls-pemfile 2.2.0` (direct `mtls` dependency for PEM parsing). Neither
  has a known vulnerability; replacing the PEM parser is deferred follow-up
  work, not a release blocker.
- `cargo deny check licenses` with `deny.toml`: passes. Every third-party
  dependency resolves to a permissive license (MIT/Apache-2.0 family, ISC,
  BSD, Zlib, Unlicense, CDLA-Permissive-2.0, Unicode-3.0); no copyleft license
  is present. Four legacy `/`-separated license declarations are clarified to
  SPDX equivalents pinned by license-file hashes.
- Both checks run in CI on every push and pull request. The exact outcomes for
  the release head were verified before tagging.
