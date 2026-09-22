# Reverse Session M006 — Distribution and Downstream Qualification

Status: active — M003–M005 are closed; distribution and downstream qualification is underway

Planning baseline: 448c615 (accepted M005 head)

The accepted M003–M005 closure records and the candidate transport matrix were reviewed before execution. The baseline above is the latest accepted reviewed head at M006 start.

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#11-milestone-m006--distribution-and-downstream-qualification

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability / polish

## 1. Objective

Turn the closed Eggtunnel library/runtime into a consumable Eggstack component and standalone product with truthful public API documentation, supported release targets, crates/release packaging, install/update integration, and downstream-shaped qualification.

This milestone does not add new tunnel semantics.

## 2. Dependency readiness

Hard dependencies:

- M003 strictly closed;
- every transport profile advertised as supported in the first release strictly closed.

M004/M005 need not both be closed if the first release intentionally supports only TCP/TLS, but documentation and feature metadata must state that accurately.

Before execution:

- update baseline to latest accepted reviewed head;
- inspect all closure records;
- define the release support matrix from evidence rather than roadmap intent;
- verify current Eggstack shared installer/updater status before adding local updater code.

## 3. Invariants

- Distribution does not change Session/Service protocol semantics.
- Published crates expose only APIs intended to be semver-relevant.
- Unsupported platforms/transports are not claimed.
- Release installers verify artifact integrity according to current Eggstack conventions.
- Eggtunnel does not fork a new updater/service-management implementation if eggup provides the needed supported interface.
- Client-only downstream usage remains possible without the CLI.
- CodeGG qualification remains application-neutral and does not add a CodeGG dependency.

## 4. In scope

- public crate classification;
- crates.io package metadata/publication ordering;
- semver and protocol compatibility policy;
- README;
- embed guide;
- protocol guide;
- security guide;
- configuration/operations guide;
- target/support matrix;
- GitHub release assets;
- checksums/provenance;
- installer integration;
- update/version command integration if delegated shared machinery is available;
- Linux/macOS/Windows/SBC target qualification as evidence permits;
- CodeGG-shaped downstream fixture;
- optional Eggchaos/Eggbench integration when stable;
- dependency/security audit evidence.

## 5. Public crate classification

Review each package and classify it explicitly.

Expected direction:

### eggtunnel-proto

Published supporting library if external protocol consumers/tests benefit from it.

Its public types become semver-sensitive.

### eggtunnel

Primary supported Rust library.

Document preferred feature slices and stable entry points.

### eggtunnel-cli

Published/installable CLI package and binary.

Do not make downstream libraries depend on it.

If any crate should remain unpublished, set publish = false and document why.

## 6. Public API qualification

Create a maintained API guidance document that identifies preferred entry points.

At minimum review:

- Client/ClientConfig/ClientHandle;
- Server/ServerConfig/ServerHandle;
- ServiceSpec/ServiceName/IDs;
- Target/TargetConnector;
- snapshots;
- shutdown;
- error/termination types;
- transport feature types.

Add downstream-shaped compile tests that import the supported surface without reaching private/internal modules.

Avoid introducing an expensive public-API tooling requirement unless it materially improves maintenance; ordinary compile contracts plus manual semver review may be sufficient for the initial release.

## 7. Protocol compatibility policy

Document:

- current protocol version;
- compatibility promise for 0.x;
- how major/minor protocol versions interact;
- capability negotiation;
- unknown-version behavior;
- whether older peers are supported and for how long.

Do not imply stable 1.x protocol guarantees before the project actually adopts them.

Published protocol docs must match the tested wire layout.

## 8. Release targets

Qualify targets individually.

Primary intended targets:

- x86_64-unknown-linux-gnu;
- aarch64-unknown-linux-gnu;
- x86_64-apple-darwin;
- aarch64-apple-darwin.

Secondary as toolchain/dependency evidence permits:

- armv7-unknown-linux-gnueabihf;
- musl Linux variants;
- x86_64-pc-windows-msvc;
- aarch64-pc-windows-msvc.

SBC qualification should explicitly consider Raspberry Pi and Le Potato-class Linux devices.

A target is not supported merely because cargo has a target triple. Record build and smoke/runtime evidence where required.

## 9. Release artifacts

Provide versioned archives containing the eggtunnel binary and required notices/licenses.

Release process SHOULD produce:

- target-specific archive;
- SHA-256 checksum manifest;
- version metadata;
- third-party notices if required by dependency policy;
- install scripts or shared installer metadata.

Do not include private keys/example secrets.

## 10. Installer/updater integration

Investigate the current supported eggup interface at execution time.

Preferred outcome:

- Eggtunnel consumes shared verified download/install/update/service-management primitives;
- Eggtunnel owns only its product-specific release metadata/config;
- update/version commands delegate to shared machinery.

If eggup is not ready or cannot meet Eggtunnel's requirements, the initial release MAY provide install-only scripts and defer self-update. Do not copy a large updater implementation into this repo merely to satisfy parity with another Eggstack project.

## 11. Downstream CodeGG-shaped qualification

Create a fixture/example that approximates downstream use:

- dependency on published/path eggtunnel library only;
- default features disabled;
- client + selected secure transport;
- caller-owned Tokio runtime;
- caller-owned tracing;
- programmatic config;
- one loopback service;
- one direct application connector service if public;
- startup/status/shutdown;
- no CLI subprocess or sidecar.

Where practical, also compile a small patch/branch against CodeGG externally, but closure of Eggtunnel should not require modifying CodeGG unless explicitly requested in a separate downstream plan.

Document the exact API that CodeGG would consume.

## 12. Fault/performance integration

If Eggchaos and Eggbench expose stable consumable interfaces by execution time:

- add non-release-blocking fault scenarios for latency/drop/reset/connection churn;
- add informational throughput/connection-churn benchmarks;
- keep results host/config specific;
- do not make noisy wall-clock thresholds routine CI gates.

If those projects are not ready, record the deferral rather than adding ad hoc replacements.

## 13. Supply-chain/security checks

Use current Eggstack conventions for:

- cargo audit or equivalent advisory review;
- cargo deny/license/source policy if adopted;
- locked builds;
- release dependency provenance;
- checksum verification;
- unsafe/dependency review.

Do not introduce redundant scanners solely for badge count.

## 14. Ordered work packages

A. Public API/protocol support contract
- classify crates;
- compile contracts;
- semver/protocol docs.

B. Documentation
- README;
- embedding;
- security;
- protocol;
- operations/config;
- support matrix.

C. Release pipeline
- target builds;
- archives/checksums;
- notices;
- manual/automated publish policy.

D. Installer/update integration
- consume eggup if ready;
- install-only fallback if not.

E. Downstream/SBC qualification
- client-only fixture;
- supported targets;
- Raspberry Pi/Le Potato assumptions;
- optional fault/perf tools.

## 15. Required tests/evidence

- public import/compile contracts;
- clean checkout locked build;
- release-mode client/server smoke;
- install archive extraction and version command;
- checksum failure test;
- installer custom destination;
- unsupported target diagnostic;
- downstream-shaped client-only build;
- feature/dependency tree;
- target matrix build evidence;
- security/advisory/license checks;
- protocol docs vs codec constants test/guard if practical.

## 16. Verification

Run all project verification plus release-specific commands.

Record:

- exact Rust/toolchain version;
- exact target triple;
- host/runner type;
- whether commands were local or CI;
- archive/checksum names;
- crates dry-run/publish checks;
- downstream fixture command;
- dependency/audit commands.

## 17. Documentation acceptance

Public docs must clearly answer:

- what Eggtunnel is/is not;
- minimal server/client setup;
- TLS/auth expectations;
- how to embed;
- feature flags;
- current protocol compatibility;
- supported transports;
- supported platforms;
- resource/security defaults;
- failure/reconnect behavior;
- install/update procedure;
- known limitations.

Roadmap-only features must be labeled as unimplemented.

## 18. Acceptance criteria

- preferred public Rust APIs are documented and compile-tested;
- protocol compatibility policy is explicit;
- release support matrix is evidence-backed;
- supported archives/checksums install and smoke successfully;
- client-only downstream fixture uses public APIs only;
- installer/updater duplication is avoided or explicitly deferred;
- supply-chain/security checks have recorded outcomes;
- no unsupported transport/platform is advertised;
- release documentation is internally consistent.

## 19. Stop conditions

Stop and report if:

- public APIs are still unstable due to unresolved M003 findings;
- a transport intended for release is only conditionally closed on correctness/security;
- crates publication requires exposing internal APIs not intended for support;
- eggup integration would require depending on an unstable/private interface;
- a target cannot be meaningfully runtime-qualified but documentation would need to claim support.

## 20. Closure evidence required

- refreshed baseline/final reviewed head;
- public crate/API classification;
- protocol support matrix;
- platform/target evidence table;
- release asset/checksum evidence;
- install/update evidence;
- downstream fixture evidence;
- dependency/security/license evidence;
- exact commands/results;
- deferred targets/features;
- final release-readiness disposition.

## 21. Execution record (2026-09-22)

### Implemented

- Classified `eggtunnel-proto` and `eggtunnel` as publishable libraries and
  kept `eggtunnel-cli` private. Added crate metadata and documented the required
  `eggtunnel-proto` then `eggtunnel` publication order.
- Added public API, distribution, and protocol compatibility guidance. The
  protocol document distinguishes crate versioning from wire version 1.0 and
  states that the current capability set is empty.
- Added a deterministic dependency-notice generator, an install-only
  Linux/macOS script, a local archive/install smoke script, and a tag-driven
  release workflow for four candidate Linux/macOS targets. The release job
  assembles versioned archives and SHA-256 checksums and generates GitHub build
  provenance attestations.
- Added CI coverage for all workspace features, generated docs, and the
  downstream embedder fixture.
- Eggup inspection found verified local staging and transaction primitives but
  no released downloader, release selector, bootstrap installer, or service
  manager interface. Self-update is explicitly deferred; Eggtunnel uses the
  small install-only script.

### Local verification

Environment: Rust/Cargo 1.98.1; local Apple Silicon macOS host
(`aarch64-apple-darwin`). Commands run against the working tree based on
`448c6159696c7f2792e563622bc525c35028d24b`.

- `cargo fmt --all -- --check` and `git diff --check`: passed.
- `cargo test --locked --workspace --all-targets --all-features`: 31 tests
  passed across 3 suites.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D
  warnings`: passed with no warnings.
- `cargo doc --locked --workspace --all-features --no-deps`: passed.
- `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml`: passed.
- Release-mode CLI `cargo check` passed for `x86_64-apple-darwin` and
  `aarch64-apple-darwin`.
- Local Linux cross-target checking could not run: the host lacks
  `x86_64-linux-gnu-gcc`, required by `ring`. Linux release builds remain
  assigned to their matching hosted runners.
- `scripts/test-install.sh`: passed. It installed a local archive to a custom
  destination, ran `eggtunnel version`, and confirmed checksum corruption and
  an unsupported target are rejected.
- `cargo package --locked --list -p eggtunnel --allow-dirty` and the equivalent
  `eggtunnel-proto` command listed their package contents successfully.
- `cargo publish --locked --dry-run -p eggtunnel-proto --allow-dirty` passed
  earlier in M006. No package was published. Packaging/publishing `eggtunnel`
  cannot complete until its versioned `eggtunnel-proto` dependency is available
  from crates.io; do not bypass this ordering by publishing automatically.
- The local aarch64 macOS archive path has install/runtime smoke evidence. No
  hosted release workflow, other archive runtime, Linux runtime, or public
  registry publication has been executed.

### Open closure gates

- Run the release workflow on hosted x86_64/arm64 Linux and Intel/arm64 macOS
  runners; inspect each produced archive, checksum, and attestation, and run
  install/version smoke on each target.
- Complete advisory and license policy review before any crate publication.
  The generated notice is a dependency inventory, not legal approval; this
  repository has no established `cargo audit`/`cargo deny` workflow to claim.
- Publish `eggtunnel-proto` and then `eggtunnel` only after review, then run the
  embedder fixture against registry dependencies. No publish or release action
  is authorized by this implementation work.
- Decide supported-platform claims from the hosted evidence. Windows, musl,
  armv7, Raspberry Pi-class runtime qualification, and Le Potato-class runtime
  qualification remain deferred.

Disposition: M006 implementation and local qualification are in place, but the
acceptance criteria requiring hosted release artifacts, downstream registry
consumption, and completed security/license review are still open. Keep M006
active; do not declare the distribution qualified or publish a release yet.
