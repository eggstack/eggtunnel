# Reverse Session M011 Closure — Sustained Robustness and Performance Qualification

Status: closed

Disposition: closed — implementation, bounded sustained evidence, broad local
verification, and exact-head hosted CI all passed. No unresolved high/medium
correctness or security finding remains.

Implementation plan:

- `plans/implementation/reverse-session/011-sustained-robustness-and-performance-qualification.md`

## Baseline and reviewed heads

- Planning baseline: `0c8830e76d92c209485eaf955cbdb51bdc953413`.
- M011 implementation head: `950f027e96e0490050af269651b780f473f835ef`.
- Planning/closure status follow-up: `69f7376` (this commit's parent state recorded M011 as closing while hosted CI was queued).
- Exact implementation-head hosted CI: [run 36003630154](https://github.com/eggstack/eggtunnel/actions/runs/36003630154), completed `success` on `950f027e96e0490050af269651b780f473f835ef`.

## Requirement-to-evidence matrix

| Requirement | Evidence / disposition |
|---|---|
| Protocol fuzz target, malformed inputs and retained fast guard | Separate cargo-fuzz workspace targets public `eggtunnel_proto::decode_frame`; seeded from valid and malformed frames. Fuzzer-discovered coverage corpus is retained. Existing deterministic hostile decoder test remains in the normal suite. |
| Sustained fuzz run | `cargo fuzz run decode_frame fuzz/corpus/decode_frame -- -max_total_time=61 -print_final_stats=1`; cargo-fuzz 0.13.2, Rust nightly 1.100.0-nightly, libFuzzer. Exact M011 head: 56,922,258 executions, 100 new coverage units, no crash/artifact, peak RSS 621 MB. Start corpus 472 seeds; resulting temporary corpus 543 entries. Regression: `cargo fuzz run decode_frame fuzz/corpus/decode_frame -- -runs=1000`; 1,000 runs, no crash/artifact and no new units. These bounded runs are evidence, not exhaustive proof. |
| Deterministic lifecycle sequence | Private Service state harness: fixed seed `0x4e4f_574d_414e_3031`, 10,000 transitions, invariant assertions throughout; passed. |
| TCP/TLS lifecycle and churn | Ignored qualification soak executed twice on the exact implementation head. Each run: 20 client/server lifecycle cycles, 200 relays, concurrency 4, payloads 64 B and 64 KiB, 6.56 MB each direction, Service register/unregister, disconnect/reconnect and shutdown. Durations 44.31 s and 44.02 s; 0.28 MiB/s aggregate including lifecycle overhead; mean registration 0.41/0.20 ms; mean reconnect 1155.76/1139.40 ms. High-water Sessions 1, Services 1, pending 4, active relays 4, handshakes 4, client Open tasks 4. All current counters converged to zero after teardown; task panics 0. |
| QUIC sustained churn | 200 streams/connections, concurrency 4, 6.56 MB each direction; 113 ms, 1762.91 streams/s, 110.29 MiB/s aggregate. High-water Open 4 / server Session 1; current counters converged after teardown. Reconnect regression additionally verifies QUIC registered-Service accounting clears on cancellation. |
| WSS sustained churn | 200 connections, concurrency 4, 6.56 MB each direction; 76 ms, 2600 connections/s, 162.66 MiB/s aggregate. High-water Open 5 / server Session 1; current counters converged after teardown. The scenario uses fixed-size request/response and makes no TCP half-close equivalence claim. |
| Host/config/footprint | Host: macOS 15, `aarch64-apple-darwin`, Apple M4 Pro; Rust 1.98.1; release profile. Host-specific measurements above are informational. Release CLI binary: 7,683,904 bytes. Minimal `client,tls` graph: 61 unique packages, with no QUIC/WebSocket/outbound-proxy optional packages. |
| Broad verification | Exact M011 implementation tree passed `cargo fmt --all -- --check`; locked workspace check; locked all-target/all-feature tests (91 passed, 3 ignored); clippy with `-D warnings`; rustdoc with warnings denied; embedder locked check; `cargo audit`; `cargo deny check licenses`. Seven supported feature slices passed; Rust 1.89 MSRV and minimal-dependency checks passed. QUIC cancellation regression passed. |
| Hosted CI | Exact source SHA above passed hosted CI run 36003630154. |
| Documentation and dependency boundary | Qualification commands and evidence boundaries are documented in `docs/OPERATIONS.md`, `docs/SECURITY.md`, architecture operations documentation, and `AGENTS.md`. Fuzz tooling is separate from production dependency resolution; no production dependency, feature, public API, or wire change was introduced. |

## Security, resource, and residual review

Authentication and authorization remain on the existing Session/Service
boundaries. Fuzz input is passed to the public bounded decoder and does not
raise protocol limits. Soaks use bounded concurrency and finite durations;
current task/session/connection counters converge after cancellation and
shutdown. The existing secret-safe telemetry contract is unchanged. No new
high/medium finding was identified.

`cargo audit` reports zero vulnerabilities and one informational
`atomic-polyfill 1.0.3` unmaintained warning (RUSTSEC-2023-0089), inherited
through the existing target-gated dependency path. `cargo deny check licenses`
passes. Performance values are host-specific and not correctness thresholds.

## Dependency handoff

M011's hard dependency, strict closure of M010, is satisfied. M012 is therefore
unblocked and ready to execute. Its explicit tag, GitHub release, and crates.io
publication authorization gate remains in force; readiness does not grant that
authorization.
