# Reverse Session M018 — Eggress 1.0.11 Adoption and TLS Ownership Reconciliation

Status: blocked — hard dependency M017 is not yet closed

Planning baseline: ece46fd223265b7b0609e3640b0caa9efadd1535

Source roadmap:

- plans/002-long-term-roadmap.md
- plans/subsystems/reverse-session-roadmap.md

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: polish / infrastructure

Hard dependency: M017 strict closure.

Operational dependency: Eggress 1.0.11 remains available from crates.io during implementation.

## 1. Objective

Move Eggtunnel from the current exact Eggress 1.0.8 integration baseline to the published 1.0.11 line and use the newer TLS builder surface to remove Eggtunnel-owned certificate/configuration construction that no longer belongs here.

This milestone is an ownership/dependency reconciliation, not a transport feature expansion. Existing TCP/TLS, mTLS, QUIC, WSS and outbound-proxy behavior must remain equivalent.

## 2. Why this milestone is planned now

The repository audit found that Eggtunnel still pins:

- `eggress-core = =1.0.8`;
- `eggress-relay = =1.0.8`;
- `eggress-transport-tls = =1.0.8`;
- `eggress-transport-quic = =1.0.8`;
- `eggress-protocol-websocket = =1.0.8`;
- `eggress-outbound = =1.0.8`.

Eggress 1.0.11 is published and its TLS crate now exposes:

- `TlsClientConfigBuilder::with_client_cert_pem`;
- `TlsServerConfigBuilder::with_client_ca_pem`;
- `TlsServerConfigBuilder::with_require_client_cert`;
- maintained PEM/root helpers.

Eggtunnel currently constructs mTLS client/server `rustls` configs locally and owns a local `pem.rs` parser. That is unnecessary duplication once 1.0.11 is adopted.

The server still needs access to the verified peer leaf certificate to derive Eggtunnel's opaque Principal. M018 therefore may continue to own the mTLS accept wrapper needed to observe peer certificates; it must not discard Principal binding merely to eliminate a dependency.

## 3. Invariants that cannot regress

- Exact native Eggtunnel Session/wire semantics remain unchanged.
- Eggtunnel continues to own Session/Service/Connection correlation; `eggress-protocol-reverse` must not enter the native dependency graph.
- mTLS still requires bearer authentication in addition to the verified client certificate.
- the same verified leaf identity remains bound to the Session and rechecked on DataHello.
- custom CA/system-root behavior remains equivalent.
- QUIC remains platform-roots + bearer only unless separately planned.
- WSS/proxy support matrix remains unchanged.
- optional dependency slices remain narrow.
- no git/path Eggress dependency is introduced into the publishable workspace.
- no Eggress type becomes part of Eggtunnel's public API merely to simplify internals.

## 4. In scope

- exact Eggress crate pin update from 1.0.8 to 1.0.11;
- compile/API reconciliation for all consumed Eggress crates;
- migration of mTLS client configuration to `TlsClientConfigBuilder`;
- migration of mTLS server configuration to `TlsServerConfigBuilder`;
- removal of local PEM parsing that is no longer necessary;
- reduction of direct `webpki-roots`/PEM-construction dependencies where no longer load-bearing;
- review of direct `rustls`/`tokio-rustls` dependencies and retention only where Eggtunnel needs concrete TLS state for Principal extraction;
- dependency-tree, binary-size and transport regression evidence;
- documentation/version-baseline updates.

## 5. Out of scope

- removing the direct `tokio-tungstenite` dependency while Eggress's bounded WebSocket configuration API still exposes Tungstenite configuration types; that is M019 after the upstream seam is published;
- adding new Eggress protocols;
- adopting `eggress-protocol-reverse`;
- changing mTLS Principal definition/hash;
- QUIC custom CA/mTLS/proxy support;
- transport performance tuning;
- release-version publication.

## 6. Required production changes

### A. Upgrade the Eggress family coherently

Move all direct Eggress exact pins together to `=1.0.11`. Do not mix Eggress internal versions.

Run Cargo resolution with `--locked` after intentionally updating the lockfile and verify the feature graph for every currently qualified slice.

Document any transitive dependency movement that materially affects MSRV, licensing, binary size or optional-feature isolation.

### B. Use Eggress TLS builders for mTLS policy construction

Client mTLS should be expressed through `TlsClientConfigBuilder`:

- system or custom roots selected exactly as today;
- client cert/key attached through the Eggress builder;
- malformed/missing material fails before network use;
- SNI verification remains performed by the existing `tls_connect` path.

Server mTLS should be expressed through `TlsServerConfigBuilder`:

- server cert/key;
- trusted client CA;
- required client certificate.

Remove local rustls configuration-building code and local PEM helpers made redundant by these builders.

### C. Preserve Eggtunnel Principal extraction

The mTLS server accept path may keep a direct `tokio-rustls::TlsAcceptor` (or another ownership-correct mechanism) if that is required to access `peer_certificates()`.

The accepted stream must still:

- be verified under the Eggress-built `rustls::ServerConfig`;
- expose the verified leaf certificate to `certificate_principal`;
- attach the same Principal semantics to control and DataHello validation.

Do not replace the Principal with source IP, bearer-token hash, certificate subject text or any lower-entropy/ambiguous identifier.

### D. Minimize direct TLS dependencies honestly

After migration, audit whether these direct dependencies are still needed:

- `webpki-roots`;
- `rustls`;
- `tokio-rustls`;
- `sha2`.

Remove only dependencies with no remaining semantic owner.

Expected likely outcome:

- `webpki-roots` and local PEM helpers can disappear;
- `rustls` may remain because builders/config types cross the private transport boundary;
- `tokio-rustls` may remain for peer-certificate extraction;
- `sha2` remains while Principal is the SHA-256 digest of the verified leaf certificate.

Record the actual outcome instead of optimizing dependency count for its own sake.

### E. Re-run every transport composition

Qualify TCP/TLS, mTLS, QUIC, WSS and outbound-proxy paths after the family upgrade. A green minimal client build alone is insufficient because the exact-pin move crosses every optional adapter consumed by Eggtunnel.

## 7. Ordered work packages

1. Capture current dependency trees and release-binary size as informational baseline.
2. Update the Eggress family to 1.0.11 and reconcile compilation/API changes without behavioral refactors.
3. Migrate client mTLS construction to the Eggress TLS builder.
4. Migrate server mTLS construction to the Eggress TLS builder while preserving peer-certificate Principal extraction.
5. Delete redundant local PEM/root construction and remove truly unused direct dependencies.
6. Run focused transport/mTLS/proxy/correlation tests.
7. Run all feature/MSRV/minimal-dependency/security/license gates.
8. Refresh support/architecture/dependency docs.
9. Record closure evidence.

## 8. Failure and cancellation semantics

- malformed TLS/mTLS material fails before listener/dial startup as today;
- failed client certificate verification remains an authentication/TLS failure, never silent bearer-only fallback;
- cancellation during TLS handshake remains prompt and bounded;
- Principal absence where mTLS requires one fails closed through existing Session/DataHello semantics;
- outbound proxy failures never fall back direct;
- QUIC/WSS unsupported combinations remain explicit configuration failures.

## 9. Required focused tests

- ordinary TCP/TLS custom CA and system-root profiles;
- mTLS valid client certificate;
- missing/untrusted/invalid client certificate;
- client cert/key parse failure before dial;
- wrong/stale Principal on DataHello fails;
- server certificate/key parse failure before bind;
- QUIC/WSS/proxy existing support-matrix tests;
- Eggress 1.0.11 dependency versions are coherent in `Cargo.lock`;
- minimal `client,tls` graph still excludes optional transports and `eggress-protocol-reverse`.

## 10. Broad verification

Run the full Eggtunnel gate from AGENTS.md plus:

- all optional-transport integration tests;
- C001 transport-specific regression cases relevant to upgraded Eggress crates;
- mTLS tests under the client+server+tls+mtls slice;
- dependency-tree before/after report;
- release binary size before/after as informational evidence;
- hosted exact-head CI.

If a new Eggress advisory/license finding appears, stop and resolve it rather than suppressing it solely to complete the upgrade.

## 11. Compatibility and migration effects

No public API, wire or config migration is intended.

The lockfile/transitive graph will change. Existing downstream consumers remain on the Eggtunnel public facade and should not need to name Eggress types.

The Eggress integration baseline in architecture/docs moves from 1.0.8 to 1.0.11 only after exact-head qualification.

## 12. Documentation updates

Update at minimum:

- `architecture/transports-wire-io.md`;
- `architecture/ops-tooling-distribution.md`;
- `architecture/common-core.md` if `pem.rs` disappears;
- `docs/SUPPORT.md`;
- `docs/SECURITY.md`;
- `docs/API.md` / `docs/EMBEDDING.md` only where dependency ownership is described;
- `AGENTS.md`;
- planning references naming Eggress 1.0.8 as the current baseline.

## 13. Acceptance criteria

M018 may close only when:

- every direct Eggress crate consumed by Eggtunnel is coherently pinned to published 1.0.11;
- existing transport behavior and feature slices remain qualified;
- mTLS config construction uses Eggress TLS builders instead of a parallel local implementation;
- verified leaf Principal semantics remain exactly enforced;
- redundant PEM/root dependencies are removed where no longer load-bearing;
- no native dependency on `eggress-protocol-reverse` appears;
- full exact-head hosted CI passes;
- no unresolved high/medium transport, security, dependency or compatibility finding remains.

## 14. Stop conditions

Stop and write a corrective/upstream plan if:

- Eggress 1.0.11 behavior differs incompatibly from the existing support contract;
- peer-certificate access cannot be retained without weakening Principal binding;
- an optional feature leaks into the minimal build;
- the upgrade requires a new public Eggtunnel API;
- removing a direct implementation dependency requires a new Eggress public seam.

## 15. Closure evidence required

Create `plans/closure/reverse-session/018-status.md` with:

- baseline/final head;
- exact Eggress version table;
- dependency-tree delta;
- TLS/mTLS ownership before/after;
- Principal regression evidence;
- optional-transport matrix results;
- binary-size informational delta;
- audit/license/MSRV/hosted CI evidence;
- known residual direct transport dependencies.

## 16. Handoff notes

Adopt the published upstream API; do not fork it locally. Dependency-count reduction is subordinate to preserving Eggtunnel's trust semantics, especially the verified leaf-certificate Principal.
