# Distribution and release policy

## Current release state

Eggtunnel is at `0.1.0`. No crate has been published and no GitHub release has
been made. `eggtunnel` and `eggtunnel-proto` are intended as public Rust
packages; `eggtunnel-cli` remains a private workspace package and ships only
inside the binary archive. The CLI crate depends on the library, never the
reverse.

## Candidate release targets

The release workflow builds archives for these candidate targets:

| Target | Build runner | Local runtime evidence |
|---|---|---|
| `x86_64-unknown-linux-gnu` | GitHub Ubuntu x64 | Pending hosted release workflow |
| `aarch64-unknown-linux-gnu` | GitHub Ubuntu arm64 | Pending hosted release workflow |
| `x86_64-apple-darwin` | GitHub macOS Intel | Pending hosted release workflow |
| `aarch64-apple-darwin` | GitHub macOS arm64 | Local workspace and release binary checks |

These are candidate build outputs until the release workflow succeeds and the
archives pass version and checksum smoke checks. Windows, musl, armv7 and SBC
targets are not in the candidate release set. A Rust target being available
does not make that target supported.

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

The initial Rust publication order is `eggtunnel-proto`, then `eggtunnel`
(which has a versioned dependency on the former). Publish only after package
inspection, downstream compile qualification, dependency/license review, and
the release support matrix have been accepted. The CLI remains unpublished.
No automatic crate publication is configured.
