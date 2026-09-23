# C001 Closure Status — Optional Transport Qualification and Planning Reconciliation

Status: closed

Implementation plan: `plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md`.

This record is supplemental to the M004 and M005 historical closures. It does not amend them.

## Baseline and scope

- Planning baseline: `fc19fe57a32d0c45be339ff3dd80044c4a8bc069` (accepted M006 pre-C001 head).
- Reviewed head: implementation work completed on top of the planning baseline; no production behavior changed beyond sealing the test seam (gated on `cfg(all(test, feature = "quic"))` and `cfg(test)`).
- C001 owns transport-specific qualification for QUIC correlation/generation, QUIC stream saturation, QUIC half-close semantics, WSS close/backpressure, outbound-proxy failure/cancellation/authentication/multi-hop claim reconciliation, and planning-control-surface reconciliation.
- The M004 and M005 historical closure records are preserved unchanged. C001 supplies supplemental transport-specific evidence that closes the gaps recorded in M004/M005 section "Limitations and residual risk".

## Requirement evidence

### Work Package A — QUIC wrong/stale/replay DataHello handling

| Requirement | Evidence |
|---|---|
| DataHello with wrong SessionId is rejected without disturbing the live session entry | `quic_wrong_session_data_hello_is_rejected_and_pending_entry_survives` (crates/eggtunnel/src/server.rs) |
| DataHello replayed on a second QUIC stream is rejected as a replay | `quic_replay_data_hello_on_second_stream_is_rejected` (crates/eggtunnel/src/server.rs) |
| DataHello with a stale SessionId generation is rejected after a reconnect cycle | `quic_stale_old_generation_data_hello_is_rejected_after_reconnect` (crates/eggtunnel/src/server.rs) |

The QUIC server-side envelope (`eggress_transport_quic::QuicServerStream::Frame`) carries the same `DataHello` payload as the TCP path, so the test seam (`ClientHandle::quic_client_for_test()`) is required to drive multi-stream behavior at the wire level.

### Work Package B — QUIC stream saturation and capacity recovery

| Requirement | Evidence |
|---|---|
| Server-side `stream_admission` semaphore caps in-flight streams per session and reclaims capacity on stream completion | `quic_stream_saturation_recovers_capacity_and_keeps_unrelated_streams_alive` (crates/eggtunnel/src/server.rs); uses `Server::bind_quic_with_admission_for_test` to set a deterministic ceiling and `CapturingBlockingConnector` to drive many concurrent connections deterministically |
| `ConnectionContext::max_active_data_streams` is honored as the per-session stream ceiling; falls back to `MAX_ACTIVE_CONNECTIONS_PER_SESSION` when unset | `quic_server_loop` is now a thin wrapper calling `quic_server_loop_with_admission(..., MAX_ACTIVE_CONNECTIONS_PER_SESSION)`; `handle_quic_connection` reads `context.max_active_data_streams.unwrap_or(MAX_ACTIVE_CONNECTIONS_PER_SESSION)` for the admission semaphore |

### Work Package C — QUIC half-close classification

| Requirement | Evidence |
|---|---|
| QUIC preserves TCP-style half-close semantics through the relay | `quic_half_close_preserves_response_after_request_eof` (crates/eggtunnel/src/server.rs); uses `HalfCloseTarget` connector that reads the request, replies, and only then drops its socket. Client sees the response bytes before the request-side EOF arrives. |
| Documentation reflects this behavior | `docs/SUPPORT.md` distinguishes QUIC (preserves TCP half-close) from WebSocket (does not preserve TCP half-close) in the optional profile matrix |

### Work Package D — QUIC pre-session handshake admission classification

| Requirement | Evidence |
|---|---|
| Document the layered admission posture: Eggress bounded concurrency at the adapter layer (`MAX_CONCURRENT_CONNECTION_TASKS=1024`, `MAX_CONCURRENT_STREAM_TASKS=4096`), Eggtunnel `MAX_HANDSHAKES=64` at the session handshake, `MAX_ACTIVE_CONNECTIONS_PER_SESSION` (default 64) at the per-session stream-admission layer | Recorded in `docs/ARCHITECTURE.md` (Admission layers) and `docs/SUPPORT.md` (support matrix) |
| No replacement/vendoring of Eggress needed | Confirmed by review; Eggress's adapter-layer bounding suffices and is the canonical contract |

### Work Package E — WSS close-during-relay and bounded backpressure

| Requirement | Evidence |
|---|---|
| Peer-initiated WebSocket close during an active relay terminates the connection cleanly | `wss_peer_close_during_active_relay_terminates_cleanly` (crates/eggtunnel/src/server.rs) |
| Multi-frame payloads larger than the message cap roundtrip correctly | `wss_payload_larger_than_message_cap_roundtrips_multiple_frames` (crates/eggtunnel/src/server.rs); 64 KiB payload across 64+ 1 KiB frames, client reads until response complete before dropping |

### Work Package F — Outbound-proxy refusal/timeout/cancellation

| Requirement | Evidence |
|---|---|
| Proxy refusal terminates the session with bounded error category; credentials do not appear in diagnostics | `outbound_proxy_refused_endpoint_terminates_without_secret_in_diagnostic` (crates/eggtunnel/src/server.rs); binds loopback and drops the listener to obtain a deterministic closed-port path |
| Proxy handshake timeout tears down the bounded connection within the configured timeout | `outbound_proxy_handshake_timeout_tears_down_bounded` (crates/eggtunnel/src/server.rs); proxy accepts but never responds, timed out via short `connect_timeout` |
| Cancellation during in-progress proxy handshake terminates the session cleanly | `outbound_proxy_cancellation_terminates_in_progress_handshake` (crates/eggtunnel/src/server.rs) |

### Work Package G — Proxy authentication success and failure

| Requirement | Evidence |
|---|---|
| HTTP CONNECT with valid Basic credentials routes end-to-end | `outbound_http_connect_auth_success_routes_through_proxy` (crates/eggtunnel/src/server.rs); URI `http://alice:s3cret@proxy/`; proxy validates `YWxpY2U6czNjcmV0` base64 |
| HTTP CONNECT with invalid credentials rejects and the failure diagnostic does not contain the password | `outbound_http_connect_auth_failure_rejects_without_secret_leak` (crates/eggtunnel/src/server.rs) |
| SOCKS5 with valid userinfo routes end-to-end | `outbound_socks5_auth_success_routes_through_proxy` (crates/eggtunnel/src/server.rs); proxy sends `[1, 0]` success after authenticator accepts `alice:s3cret` |
| SOCKS5 with invalid credentials rejects and the failure diagnostic does not contain the password | `outbound_socks5_auth_failure_rejects_without_secret_leak` (crates/eggtunnel/src/server.rs) |

### Work Package H — Multi-hop chain qualification

| Requirement | Evidence |
|---|---|
| SOCKS5 + HTTP CONNECT chain routes end-to-end with both hops authenticating | `outbound_two_hop_socks5_then_http_connect_routes_end_to_end` (crates/eggtunnel/src/server.rs); URI `socks5://alice:s3cret@hop1__http://bob:s3cret@hop2`; chain executor drives protocol logic on each hop |
| Documentation reflects multi-hop syntax | `docs/CONFIGURATION.md` documents the `__` chain separator; `docs/SUPPORT.md` lists multi-hop as a supported configuration |

### Work Package I — Planning and documentation reconciliation

| Requirement | Evidence |
|---|---|
| Subsystem roadmap reflects C001 status | `plans/subsystems/reverse-session-roadmap.md` updated: `Status: active — M001-M005 closed; C001 closed (supplemental transport-specific evidence); M006 distribution qualification in progress`. C001 row in the milestone table references the closure record. |
| Corrective addendum reflects C001 status | `plans/subsystems/reverse-session-post-closure-corrective-addendum.md` updated: C001 section marked `closed`, closure record path added, table updated |
| Plan registry reflects C001 status and M006 gate list | `plans/registry.md` updated: C001 entry marked `closed` in subsystem and implementation tables, M006 dependency gate shifted to hosted release-target / advisory / publication / downstream-registry only |
| Implementation plan marked closed | `plans/implementation/reverse-session-post-closure-corrective/001-optional-transport-qualification-and-planning-reconciliation.md` status field updated to `closed` and linked to this record |
| M006 strict-closure gate no longer depends on C001 | `plans/subsystems/reverse-session-roadmap.md` M006 section updated to: `Status: active — implementation/local qualification landed; strict closure blocked on hosted release/security/publication/downstream evidence` |
| `docs/SUPPORT.md` reflects optional transport profile support matrix, including QUIC, WSS, single-hop and multi-hop proxy with auth | Updated; C001 evidence cited |
| `docs/SECURITY.md` reflects credential redaction guarantees (proxy passwords, mTLS identities, Snapshot diagnostics) | Updated |
| `docs/OPERATIONS.md` reflects operational tuning for QUIC stream ceilings, proxy connect timeouts, multi-hop chain syntax | Updated |
| `docs/CONFIGURATION.md` reflects proxy URI/chain syntax, including userinfo credentials, `__` chain separator, `outbound_proxy_env` reference | Updated |
| `docs/ARCHITECTURE.md` reflects admission layering (Eggress adapter-layer, Eggtunnel session handshake, per-session stream admission) | Updated |

## Test seam

The following seams were added in production code, gated on test-only `cfg`:

- `ClientHandle::quic_client_for_test()` (crates/eggtunnel/src/client.rs) returns `Option<Arc<QuicClient>>`. Field is `Arc<Mutex<Option<Arc<QuicClient>>>>`, set by `quic_reconnect_loop` after the QUIC client has connected and cleared on the next reconnect cycle. Public production behavior is unchanged.
- `Client::start_quic_insecure_with_connector_for_test` (crates/eggtunnel/src/client.rs) — QUIC insecure mode with a custom `TargetConnector`. Public production behavior is unchanged.
- `Server::bind_quic_with_admission_for_test` (crates/eggtunnel/src/server.rs) — exposes the existing `quic_server_loop_with_admission` entry point with a deterministic per-session stream ceiling and sets `max_concurrent_streams: max_active_data_streams.max(1) * 2` on the Quinn transport config so the server-side accept loop honors the ceiling. Public production behavior is unchanged.

## Verification

Toolchain: rustc 1.98.1, cargo 1.98.1, aarch64-apple-darwin host.

- `cargo fmt --all -- --check` — passed.
- `cargo check --locked --workspace --all-targets --all-features` — passed.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `cargo test --locked --workspace --all-targets --all-features` — passed, 39 lib tests + 7 proto tests across 3 suites.
- `RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps` — passed.
- `cargo check --locked --manifest-path fixtures/embedder/Cargo.toml` — passed.
- `cargo tree --locked -p eggtunnel --no-default-features --features client,tls -e normal` filtered for `quic|websocket|outbound` — no matches.

Feature slices:

- `cargo test --locked -p eggtunnel --no-default-features --features client,tls` — 0 lib tests (slice has no client integration tests by design).
- `cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,quic` — 24 lib tests passed.
- `cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,websocket` — 19 lib tests passed.
- `cargo test --locked -p eggtunnel --no-default-features --features client,tls,outbound-proxy` — 0 lib tests (slice has no client integration tests by design).
- `cargo test --locked -p eggtunnel --no-default-features --features client,server,tls,websocket,outbound-proxy` — 29 lib tests passed.

## Limitations and residual risk

- The QUIC test seam is gated on `cfg(all(test, feature = "quic"))`. Future refactors must keep this gating intact to prevent the seam from being compiled into release builds.
- The `quic_server_loop_with_admission` helper is `#[cfg(feature = "quic")]` and remains an internal implementation detail. It is only exposed via the test-only `bind_quic_with_admission_for_test` server constructor.
- WebSocket half-close behavior remains as recorded in M005: a peer-initiated WebSocket close ends the connection. Do not claim transparent TCP half-close semantics on WSS.
- Proxy authentication uses Basic over HTTP CONNECT and the SOCKS5 RFC 1929 user/password subnegotiation. Eggress 1.0.8 does not negotiate SOCKS5 over TLS or other authentication mechanisms.
- Multi-hop chains use the `__` separator between adjacent proxy URIs. Each hop is responsible for forwarding raw bytes once the chain executor has driven its protocol logic; intermediate hops cannot re-authenticate the application credentials.
- The single-hop SOCKS5 fixture authenticator validates `alice:s3cret`. The multi-hop fixture uses `alice:s3cret` for the SOCKS5 hop and `bob:s3cret` for the HTTP CONNECT hop. Credentials are hard-coded test fixtures only; no production paths store passwords.
- No hosted CI, cross-platform run, or independent security review is claimed.

## Handoff

C001 is closed. M006 strict-closure now depends only on the hosted release-target, advisory/license, publication, and downstream-registry gates listed in `plans/implementation/reverse-session/006-distribution-and-downstream-qualification.md`.

The optional QUIC, WSS, single-hop authenticated proxy, and multi-hop chain profiles now have local integration evidence. The support matrix and configuration documentation distinguish these profiles from the fully qualified TCP/TLS profile.
