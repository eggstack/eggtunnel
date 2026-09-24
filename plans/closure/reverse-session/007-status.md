# M007 Closure Status — Maintainability and Continuous Qualification

Status: closed

Implementation plan: `plans/implementation/reverse-session/007-maintainability-and-continuous-qualification.md`.

## Baseline and implementation

- Planning baseline: `2e2f3981a6e78596c32dfe5daf5b2585bfc37f1e`.
- Implementation commit: `954b4ee` (`feat: complete M007 maintainability qualification`).
- Hosted-qualified head: `954b4ee04112d26c35bef1ddb9bfd22456e940d1`.
- Final reviewed implementation head: `954b4ee04112d26c35bef1ddb9bfd22456e940d1`. Closure-record and dependency-status changes are planning-only; no production code changed after hosted qualification.

## Requirement evidence

| Requirement | Evidence |
|---|---|
| Focused integration-test topology | Removed the inline `server.rs` test module. `server_tests.rs` retains shared fixtures; `server_tests/{tcp,mtls,quic,websocket,proxy}.rs` hold feature-focused tests. `server.rs` is now runtime code (~1.3 kLOC) instead of runtime plus tests (~4.5 kLOC). |
| Client-only behavior qualification | `client.rs` unit tests cover endpoint and duplicate Service identity validation with only `client,tls` enabled. |
| Maintained PEM parsing | Removed direct `rustls-pemfile`. mTLS uses Rustls `pki-types::pem::PemObject`; tests reject empty/malformed material and multiple private keys. Existing mTLS integration tests exercise valid generated cert/key material and certificate-chain use. |
| Rust version contract | Rust 1.89 hosted job checks `eggtunnel-proto`, minimal `client,tls`, and `client,server,tls`; all passed. |
| Supported feature slices | Hosted checks and serial tests passed for `client,tls` (2 tests), `client,server,tls` (18), mTLS (22), QUIC (26), WebSocket (21), `client,tls,outbound-proxy` (2), and WebSocket+outbound-proxy (31). |
| Rustdoc warning gate | CI invokes `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps`; passed hosted and locally. |
| Minimal dependency boundary | Hosted `minimal-dependencies` guard passed. Local `cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal` had no QUIC, WebSocket, outbound-proxy, `eggress-protocol-reverse`, or `tokio-tungstenite` matches. |
| Eggtunnel/Eggress ownership | Architecture docs state Eggtunnel owns authenticated persistent multi-Service Sessions, listener policy, and ConnectionId correlation, while Eggress supplies generic transport/relay primitives. No reverse-protocol dependency was added. |
| Behavior/resource invariants | Full all-feature suite passed, retaining existing auth, bind, reconnect, teardown, saturation, cancellation, half-close, and transport cases. No wire DTO/message changes were made. |

## Verification

Local host: `aarch64-apple-darwin`, stable Rust 1.98.1.

- `cargo fmt --all -- --check` — passed.
- `cargo check --locked --workspace --all-targets` — passed.
- `cargo test --locked --workspace --all-targets --all-features` — passed, 51 tests.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps` — passed.
- `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml` — passed.
- `cargo audit` — zero vulnerabilities; one allowed `unmaintained` warning for transitive `atomic-polyfill 1.0.3`.
- `cargo deny check licenses` — passed.
- Each of the seven supported feature slices was tested locally; all passed. Profile tests ran serially after default-parallel runs exposed host-scheduling-sensitive timeouts in relay integration tests.
- Rust 1.89 local checks matched the hosted MSRV commands and passed.
- Hosted GitHub Actions Rust run `35954785375` — success at the hosted-qualified head. Main check, all seven feature-slice jobs, MSRV, and minimal dependency guard passed.

## Documentation and security/resource review

Updated CI/process architecture descriptions, client test inventory, server and transport documentation, and current distribution audit status. The M006 closure remains unchanged as historical evidence. The M007 changes add no global runtime/tracing state, no new production tasks/queues, no protocol changes, and no transport dependencies. `ClientIdentity` continues to zeroize its application-owned PEM key buffer on drop; parsed DER ownership is handed to Rustls as in the previous parser path.

Security review covered TLS/authentication, bind policy, attacker-controlled allocations, finite queues/tasks, timeout behavior, secret diagnostics, stale-session handling, cancellation/teardown, and transport profile downgrade/rejection behavior through unchanged implementation tests and review.

## Findings and disposition

- No unresolved high or medium correctness/security finding remains.
- The existing transitive `atomic-polyfill 1.0.3` unmaintained advisory remains as one informational audit warning; it is outside Eggtunnel's direct dependency control and is not a known vulnerability.
- No stop condition was triggered. Production semantics and wire behavior are unchanged.

Disposition: **closed**.

## Handoff

M007's hard dependency for M008 is satisfied. M008 is dependency-ready under ADR-0001 and is moved to `ready`. M009 remains blocked until M008 strictly closes.
