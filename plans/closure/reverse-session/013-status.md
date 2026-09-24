# Reverse Session M013 Closure — 0.2.0 Publication Event

Status: closed

Disposition: the M012 candidate was authorized for publication. Tag
`v0.2.0` was pushed, the tag-triggered release workflow built all four
target archives, the GitHub release `v0.2.0` was published, and the
`eggtunnel-proto 0.2.0` then `eggtunnel 0.2.0` crates were published to
crates.io in the dependency order recorded in M012. The clean registry
consumer resolves `eggtunnel = "0.2"` from crates.io.

M012 (`plans/closure/reverse-session/012-status.md`) remains the
qualification record and is left untouched (historical evidence).
This record captures the publication-only events that completed the
M012 gate.

## Publication evidence

- Authorizing commit: `31e7145f5e26738a2b3bdbde1be3d8ce37b59c80`
  ("docs: re-anchor 0.2.0 release docs to the published line").
- Tag `v0.2.0` (annotated) pushed to `origin/main` with explicit
  authorization.
- Tag-triggered GitHub Actions release run: `36058175606` —
  [`build` matrix](https://github.com/eggstack/eggtunnel/actions/runs/36058175606)
  passed all four target build jobs (linux x64 1m37s, linux arm64
  1m18s, macOS Intel 3m31s, macOS arm64 1m26s); `publish-release`
  passed; GitHub release `v0.2.0` published with the four archives,
  global `SHA256SUMS`, `install.sh`, and build provenance attestations.
- GitHub release URL:
  [`https://github.com/eggstack/eggtunnel/releases/tag/v0.2.0`](https://github.com/eggstack/eggtunnel/releases/tag/v0.2.0)
  (release ID `396075232`, published `2026-09-24T21:00:09Z`).
- `cargo publish --locked -p eggtunnel-proto`: uploaded
  `eggtunnel-proto 0.2.0` to `registry+crates.io` (6 files,
  33.1 KiB uncompressed / 8.5 KiB compressed).
- `cargo publish --locked -p eggtunnel`: uploaded `eggtunnel 0.2.0` to
  `registry+crates.io` (24 files, 391.8 KiB uncompressed / 65.0 KiB
  compressed). The verification step resolved `eggtunnel-proto v0.2.0`
  from the registry, confirming the required proto-then-library
  publication order.

## Clean registry consumer evidence

A temporary consumer (`eggtunnel = "0.2"`, `default-features = false`,
`features = ["client", "tls"]`) resolved both crates from
`registry+https://github.com/rust-lang/crates.io-index`, compiled with
the public-API surface (`ClientBuilder` / `ClientConfig` /
`ClientService` / `ClientTransportProfile::TcpTls` /
`RuntimePolicy::default` / `SecretToken` / `proto::ServiceId` /
`proto::ServiceName` / `proto::RequestedBind` / `proto::TcpTarget`),
and ran `cargo run` to print the wire major constant.

- `eggtunnel-proto 0.2.0` registry checksum:
  `4e3dc4f73be4ccbe32558b0019c2d16abb83953a22aed7bca51204394016b4af`.
- `eggtunnel 0.2.0` registry checksum:
  `5cb7ea8c744876973f661f866844b14752bce52daa20aff214c5bfc2647c07d4`.
- Runtime output: `eggtunnel-registry-consumer; wire major 1`.

## Documentation gate

Pre-publication documentation polish was committed at
`31e7145f5e26738a2b3bdbde1be3d8ce37b59c80` and covered:

- `CHANGELOG.md`: 0.2.0 promoted to released (with date); historical
  0.1.0 entry retained; "candidate only" / "do not tag" guard dropped.
- `docs/DISTRIBUTION.md`, `docs/SUPPORT.md`, `docs/PROTOCOL.md`: 0.2.0
  described as the current published line; 0.1.0 retained as the
  previous immutable release.
- `docs/API.md`, `docs/EMBEDDING.md`: dependency snippets bumped from
  `version = "0.1"` to `version = "0.2"` to match the published
  crates.io line.
- `AGENTS.md`: version-line note updated to point at the published
  0.2.0 state and the resolved dep snippets.
- Architecture deep dives (`overview.md`, `ops-tooling-distribution.md`,
  `proto-wire-protocol.md`, `transports-wire-io.md`): candidate-only
  language dropped and historical 0.1.0 quotes marked as such.

Pre-publication verification on `31e7145`:

- `cargo fmt --all -- --check`
- `cargo check --locked --workspace --all-targets`
- `cargo test --locked --workspace --all-targets --all-features`
  (workspace tests 83 passed / 3 ignored; eggtunnel-proto 8 passed)
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps`
- `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml`
- `cargo audit` (0 vulns, 1 informational unmaintained warning for
  target-gated transitive `atomic-polyfill 1.0.3`)
- `cargo deny check licenses` (passes)
- `./scripts/test-install.sh` (built/staged/installed 0.2.0 archive,
  version smoke `eggtunnel 0.2.0`, checksum-rejection and
  unsupported-target rejection both negative-passed)

Post-publication documentation update captured by this record and
committed in the same documentation-only closure commit; ordinary CI
rerun on this commit is the final M012 gate (`012-status.md:61-62`).

## Closure disposition

M013 is closed. The 0.2.0 line is published: tag `v0.2.0`, GitHub
release `v0.2.0`, `eggtunnel-proto 0.2.0` and `eggtunnel 0.2.0` on
crates.io, and a clean registry consumer that resolves
`eggtunnel = "0.2"` from crates.io. The M012 qualification record
remains the historical evidence for the candidate itself; this record
is the publication event that completed it.
