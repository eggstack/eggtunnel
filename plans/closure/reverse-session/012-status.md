# Reverse Session M012 Closure — 0.2.0 Release Qualification and Publication Gate

Status: conditionally closed

Disposition: release candidate qualified; awaiting explicit owner
authorization for the tag-triggered release workflow and publication sequence.
The 0.2.0 release has not been tagged or published. Candidate implementation,
local qualification, package inspection, and exact-head hosted CI are complete.

Implementation plan:

- `plans/implementation/reverse-session/012-0.2.0-release-qualification-and-publication-gate.md`

## Baseline and candidate

- Planning baseline: `0c8830e76d92c209485eaf955cbdb51bdc953413`.
- M011 strict closure: `plans/closure/reverse-session/011-status.md`.
- 0.2.0 candidate commit: `7aa3064e2f4a9ed95b55d7ecf33a63154c11cac1`.
- Candidate version: workspace packages `eggtunnel-proto`, `eggtunnel`, and
  private `eggtunnel-cli` are `0.2.0`; protocol constants remain wire 1.0.
- Exact candidate hosted CI: [run 36012640890](https://github.com/eggstack/eggtunnel/actions/runs/36012640890), completed `success` on `7aa3064e2f4a9ed95b55d7ecf33a63154c11cac1`.
- No tag, GitHub release, or crates.io publication was created.

## Requirement-to-evidence matrix

| Requirement | Evidence / disposition |
|---|---|
| Version and package references | Root workspace version, proto dependency requirement, root lockfile, independent embedder lockfile, fuzz lockfile, and installer smoke version are updated to 0.2.0. `docs/DISTRIBUTION.md` and `CHANGELOG.md` distinguish the candidate from the still-current published 0.1.0. Published dependency snippets in API/embedding guides remain `0.1` until publication. The private CLI remains `publish = false`. |
| Wire compatibility | Protocol major/minor and message IDs are unchanged at wire 1.0. Crate release version is documented separately from the wire version. |
| Public API/semver review | Reviewed `Client`, `ClientHandle`, builders/configuration, transport profiles, runtime policy, connector and Service types, Snapshot/heartbeat, termination/errors, and proto DTOs/constants against `v0.1.0`. `cargo semver-checks --baseline-rev v0.1.0` exited 0 for both crates; it reports the 0.1-to-0.2 transition as a major change, “no semver update required,” with 254 checks skipped and no incompatibility report. Manual review and the release narrative record the additive builders/runtime policy, dynamic Service/one-in-flight semantics, bounded heartbeat Snapshot, and structured tracing surface. |
| Release narrative | Added `CHANGELOG.md` with candidate status, post-0.1 capability summary, transport/profile rejections, four supported targets, wire 1.0 boundary, no updater, and current audit warning. It makes no universal performance claim. |
| Eggpack execution-time review | At Eggpack `main` SHA `4d673b901b51a1ab4d280748c014816ce156dbc4`, there were no tags/releases. Contract, ReleasePlan, manifest, and direct bootstrap components exist; CI orchestration M001 is only ready for plan authoring and ecosystem adoption remains blocked. No Eggpack end-to-end release/CI interface has been adopted or approved for Eggtunnel. Eggtunnel's existing workflow remains authoritative; adoption stays separate future work. |
| Proto package | `cargo package --locked --allow-dirty -p eggtunnel-proto` passed verification: 6 files, 33.2 KiB uncompressed / 8.6 KiB compressed. Package file list contains `Cargo.toml`, `Cargo.toml.orig`, `Cargo.lock`, `README.md`, crate source, and Cargo VCS metadata. |
| Library package | Plain `cargo package -p eggtunnel` correctly cannot validate against crates.io before proto 0.2.0 is published. Candidate packaging and verification passed with the command-scoped local proto patch: `cargo package --locked --allow-dirty --config 'patch.crates-io.eggtunnel-proto.path="crates/eggtunnel-proto"' -p eggtunnel`; 24 files, 391.7 KiB uncompressed / 64.9 KiB compressed. Contents include package manifests/lockfile, README, and all client/server/runtime sources. The patch is qualification-only and is absent from package metadata. |
| Downstream candidate consumption | Existing path-based embedder passed `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml`. Additionally, an isolated consumer compiled with `default-features = false`, `client,tls`, against both Cargo-packaged source directories, with only the unpublished proto provided by a local crates.io patch; its locked follow-up check passed. This does not substitute for the required post-publication crates.io registry consumer. |
| Normal local verification | All passed: `cargo fmt --all -- --check`; `cargo check --locked --workspace --all-targets` and all-target/all-feature check; `cargo test --locked --workspace --all-targets --all-features` (91 passed, 3 ignored); clippy with `-D warnings`; rustdoc with warnings denied; locked embedder check; `cargo audit`; `cargo deny check licenses`. |
| Feature matrix | All seven CI feature slices passed checks/tests: `client,tls` 21 passed; `client,server,tls` 44 passed / 1 ignored; `client,server,tls,mtls` 50 / 1 ignored; `client,server,tls,quic` 55 / 2 ignored; `client,server,tls,websocket` 49 / 2 ignored; `client,tls,outbound-proxy` 21; `client,server,tls,websocket,outbound-proxy` 60 / 2 ignored. |
| MSRV and dependency guard | Rust 1.89.0 passed locked proto, `client,tls`, and `client,server,tls` checks. `client,tls` normal dependency tree contains 61 unique packages and no QUIC, WebSocket, or outbound-proxy packages. |
| Supply chain and notices | `cargo audit` scanned 225 locked dependencies: zero known vulnerabilities and one allowed unmaintained warning for target-gated transitive `atomic-polyfill 1.0.3` (RUSTSEC-2023-0089). `cargo deny check licenses` passed. `python3 scripts/generate-third-party-notices.py` generated the 230-line staged notice inventory; the generated file is intentionally not committed. |
| M011 sustained qualification on the candidate source | Deterministic 10,000-transition Service-state test passed. TCP/TLS reconnect/churn soak passed (43.44 s). QUIC 200-stream soak passed. WSS 200-connection soak passed. The 61-second decoder fuzz run used cargo-fuzz 0.13.2 / Rust 1.100.0-nightly: 53,672,924 executions in 62 s, 111 new coverage units, 489 MB peak RSS, no crash/artifact. Fuzzer-discovered inputs were added to the regression corpus (559 corpus files); `-runs=1000` then completed with zero new units and no crash/artifact. Host-specific detailed throughput and resource convergence remain in the M011 closure record. |
| Binary footprint | Host: macOS 15 / Darwin 25.6, `aarch64-apple-darwin`, Apple M4 Pro; Rust 1.98.1; release profile. `cargo build --locked --release -p eggtunnel-cli` passed; CLI binary is 7,683,904 bytes. |
| Local installer and archive smoke | `EGGTUNNEL_TEST_TARGET=aarch64-apple-darwin ./scripts/test-install.sh` built/staged a local archive, checksum-verified and installed the candidate, and printed `eggtunnel 0.2.0`; corrupted checksum and unsupported target were rejected. This qualifies the native archive/installer path only. |
| Exact-head hosted CI | Rust run 36012640890 passed all standard jobs, all seven feature slices, MSRV, and minimal-dependency guard on the exact candidate SHA. |

## Supported targets and remaining publication evidence

The four M006-supported target triples remain unchanged:

| Target | 0.2.0 candidate evidence | Current disposition |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Existing v0.1.0 release support evidence; no 0.2.0 tag workflow run | Awaiting authorized release workflow build/install smoke |
| `aarch64-unknown-linux-gnu` | Existing v0.1.0 release support evidence; no 0.2.0 tag workflow run | Awaiting authorized release workflow build/install smoke |
| `x86_64-apple-darwin` | Existing v0.1.0 release support evidence; no 0.2.0 tag workflow run | Awaiting authorized release workflow build/install smoke |
| `aarch64-apple-darwin` | Native candidate local archive/install/version and checksum rejection passed; no 0.2.0 tag workflow run | Awaiting authorized release workflow build/install smoke |

The tag-triggered workflow must still produce all four archives, manifest,
attestations, and runner-side install/version smokes. `eggtunnel-proto 0.2.0`
and `eggtunnel 0.2.0` remain unpublished. The clean registry consumer cannot
resolve `eggtunnel = "0.2"` until the authorized proto-then-library
publication sequence succeeds. Current-release documentation must then be
updated to the published state and ordinary CI rerun on that documentation
closure commit.

## Security and residual findings

Authentication, authorization, bounded runtime policy, single-use
Session-bound correlation, cancellation, secret-safe tracing, and the
four-target support boundary are unchanged. No high/medium correctness or
security finding remains. The single audit warning is recorded above and in
`docs/DISTRIBUTION.md`; license policy passes.

The remaining conditions are operational and explicitly gated by the plan:
owner authorization for exact release commit/tag/publication, successful
four-target tag workflow, crates.io publication in dependency order, and a
clean registry consumer. No later reverse-session implementation plan is
registered behind M012. Protocol capability negotiation still requires a
concrete extension and accepted ADR; Eggpack adoption still lacks an adopted
CI/generator contract. Neither future item is dependency-ready as a result of
this candidate.

## Closure disposition

M012 is conditionally closed as a release-candidate qualification milestone.
It is not a published release. The plan can be completed after the owner
authorizes the publication sequence and the remaining target, registry, and
consumer evidence passes. Until then, v0.1.0 remains the current published
release and no `v0.2.0` tag exists.
