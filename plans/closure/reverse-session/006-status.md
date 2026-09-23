# M006 Closure Status — Distribution and Downstream Qualification

Status: closed

Implementation plan: `plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md`.

## Baseline and implementation

- Planning baseline: `448c6159696c7f2792e563622bc525c35028d24b` (accepted M005 head at M006 start).
- Qualified head: `531ccbc126fe2aa1aa5ba1b7b0566db41d33a458`, tagged `v0.1.0`. All hosted release, hosted CI, publication, and downstream-registry evidence was produced against this head.
- C001 (`dbbd622`) closed before M006 completion and supplies the supplemental optional-transport evidence referenced by the support matrix. M004/M005 historical closure records are preserved unchanged.
- Post-tag changes are documentation and planning-closure only (`docs/DISTRIBUTION.md`, `docs/SUPPORT.md`, this record, plan/registry/roadmap status updates). No production code, dependency, workflow, or packaging input changed after the tag; workspace verification was re-run at the closure tree and CI re-ran on push.

## Requirement evidence

| Requirement | Evidence |
|---|---|
| Public crate classification | `docs/API.md`: `eggtunnel` primary library (intended for crates.io), `eggtunnel-proto` supporting wire library (semver-sensitive), `eggtunnel-cli` private (`publish = false`, ships only in the binary archive). CLI depends on the library, never the reverse. |
| Preferred public APIs documented and compile-tested | `docs/API.md` recommended client/server surface; `fixtures/embedder` downstream-shaped compile check (public imports only, default features off, caller runtime/tracing, programmatic config, direct connector). `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml` — passed. |
| Protocol compatibility policy explicit and matching tested wire layout | `docs/PROTOCOL.md` (wire 1.0 vs crate 0.1.x, major rejection, no 1.x guarantee, explicit ID table). Guard test `documented_wire_version_and_message_ids_are_pinned` pins `PROTOCOL_MAJOR/MINOR` and all 14 message IDs against the document. |
| Release support matrix evidence-backed | `docs/DISTRIBUTION.md` + `docs/SUPPORT.md` target tables. Four supported targets (see platform table below). Windows/musl/armv7/SBC remain unsupported/unevaluated. |
| Supported archives/checksums install and smoke successfully | GitHub release `v0.1.0`: 4 archives + `SHA256SUMS` + `install.sh` + build attestations. Consumer-side verification: downloaded `aarch64-apple-darwin` archive matches manifest checksum, contains `eggtunnel`/`VERSION`/`LICENSE-MIT`/`THIRD_PARTY_NOTICES.md`; release `install.sh` installed to a custom destination and `eggtunnel version` printed `eggtunnel 0.1.0`. |
| Client-only downstream fixture uses public APIs only | Registry consumer crate (`eggtunnel = "0.1"`, `default-features = false`, `client,tls`) resolved `eggtunnel 0.1.0` from `registry+crates.io` (lockfile checksum `07edbb9a…`), compiled, and ran successfully. |
| Installer/updater duplication avoided | Eggup inspection (recorded in the M006 plan) found no released downloader/selector/bootstrap/service-manager interface; Eggtunnel ships the small install-only `install.sh` and defers self-update. No update engine embedded. |
| Supply-chain/security checks recorded | `cargo audit`: 0 vulnerabilities; 2 unmaintained warnings (see findings). `cargo deny check licenses` with `deny.toml`: passes. Both run in CI on every push/PR. |
| No unsupported transport/platform advertised | Support matrix distinguishes implemented/locally-qualified profiles from unsupported combinations (proxy+QUIC, proxy+mTLS, WSS+mTLS, custom CA with QUIC rejected by the CLI). |
| Release documentation internally consistent | README → DISTRIBUTION/SUPPORT/API/PROTOCOL/EMBEDDING/SECURITY/CONFIGURATION/OPERATIONS reviewed; release state, publication order, target claims, and credential handling agree. |

## Platform/target evidence

| Target | Evidence | Claim |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Hosted release build + per-runner install/version smoke (Release run `35804871876`); local `cargo check -p eggtunnel-cli` via zig-cc | Supported (build + install/version smoke) |
| `aarch64-unknown-linux-gnu` | Hosted release build + per-runner install/version smoke; local cross-check | Supported (build + install/version smoke) |
| `x86_64-apple-darwin` | Hosted release build + per-runner install/version smoke; local cross-check | Supported (build + install/version smoke) |
| `aarch64-apple-darwin` | Hosted release build + per-runner install/version smoke; independent consumer-side download/checksum/install/version verification | Supported (build + install/version smoke) |
| `x86_64-pc-windows-gnu` | Local `cargo check` via mingw only | Unsupported (informational check only) |
| `armv7-unknown-linux-gnueabihf` | Local `cargo check` via zig-cc only | Unsupported (informational check only) |
| `*-pc-windows-msvc`, musl, Raspberry Pi/Le Potato | No toolchain or runner evidence | Unsupported/unevaluated |

Local cross-checks used host-available toolchains (Xcode SDK, zig 0.16 with a wrapper stripping ring's `--target=<rust-triple>` flag, mingw). They are build-only evidence and are not release claims.

## Release/publication evidence

- Tag `v0.1.0` (annotated) pushed with explicit user authorization; tag commit `531ccbc`.
- Release workflow run `35804871876`: all 4 build jobs passed (linux x64 1m40s, linux arm64 1m21s, macOS arm64 2m1s, macOS Intel 6m51s), `publish-release` passed; release `v0.1.0` published with 4 archives, `SHA256SUMS`, `install.sh`, attestations.
- CI (`Rust`) on tag run `35804871853`: success — fmt, check, test, clippy, doc, embedder, `cargo audit`, `cargo deny check licenses`. CI on `main` push `35804865598`: success.
- `cargo publish -p eggtunnel-proto`: published `eggtunnel-proto 0.1.0` to crates.io.
- `cargo publish -p eggtunnel`: published `eggtunnel 0.1.0` to crates.io (after the proto dependency resolved from the registry, confirming the required publication order).
- `cargo publish --dry-run -p eggtunnel` before publication failed as expected with `no matching package named eggtunnel-proto found`, confirming publication cannot bypass the ordering.

## Verification (qualified head `531ccbc`, `aarch64-apple-darwin` host, Rust/Cargo 1.98.1)

- `cargo fmt --all -- --check` — passed.
- `cargo check --locked --workspace --all-targets --all-features` — passed.
- `cargo test --locked --workspace --all-targets --all-features` — passed: 39 lib + 8 proto tests (incl. new wire-version guard) across suites.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps` — passed.
- `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml` — passed.
- `scripts/test-install.sh` — passed (custom-destination install, `eggtunnel 0.1.0` version smoke, checksum-corruption and unsupported-target rejection).
- `cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal` filtered for `quic|websocket|outbound` — no matches.
- Feature slices: `client,tls` 0 tests (by design); `client,server,tls,quic` 24 passed; `client,server,tls,websocket` 19 passed; `client,tls,outbound-proxy` 0 tests (by design); `client,server,tls,websocket,outbound-proxy` 29 passed.

## Dependency/security/license evidence

- `cargo audit` (RustSec DB, 226 locked dependencies): 0 vulnerabilities; 2 allowed `unmaintained` warnings — `atomic-polyfill 1.0.3` (RUSTSEC-2023-0089, transitive via `postcard`/`heapless`) and `rustls-pemfile 2.2.0` (RUSTSEC-2025-0134, direct `mtls` PEM-parsing dependency). Plain `cargo audit` exits 0; `--deny warnings` exits 1 on these two findings.
- `cargo deny check licenses` — passes (`licenses ok`).
- `forbid(unsafe_code)` remains in effect; no new process/runtime/global dependency introduced.

## Findings and disposition of open items

1. `rustls-pemfile` unmaintained (low; no known vulnerability). Deferred follow-up: evaluate a maintained PEM-parsing replacement for the `mtls` feature. Not a release blocker.
2. `atomic-polyfill` unmaintained (informational; transitive, target-gated). No action.
3. Hosted release notes are auto-generated (`--generate-notes`); no manual release narrative was written. Acceptable for 0.1.0.
4. Interactive tunnel runtime evidence on Linux/Intel-macOS runners is limited to install/version smoke; full relay behavior is covered by the local loopback suite. Recorded as a scope boundary, not a defect.
5. `install.sh` supports only the four release targets and requires `curl`; documented in DISTRIBUTION.md.

No high/medium correctness or security findings remain open. No stop condition in the M006 plan triggered.

## Handoff

M006 is closed. The 0.1.0 distribution (crates.io packages, GitHub release archives, installer, support matrix, supply-chain policy) is qualified as recorded above. Follow-up work is not gated on M006: PEM-parser replacement, broader runtime qualification (Windows/musl/SBC), and any future release automation stay as ordinary future milestones.
