# M001 Closure Status — Repository and Protocol Foundation

Status: closed

Disposition: closed — implementation, local verification, and reviewed final head are recorded below. M002 is unblocked against this baseline.

Implementation plan:

- plans/implementation/reverse-session/001-repository-and-protocol-foundation.md

## Baseline and implementation

- Planning baseline: `121955694d2151095e409b3940922fc021a4b91a`.
- Implementation commit: `357480b942e95ef7087d86a043f26b7a3d175687`.
- Final reviewed head: `357480b942e95ef7087d86a043f26b7a3d175687` (local source review and required checks completed).
- Workspace: Rust 2024, declared MSRV 1.89, virtual resolver-2 workspace.
- Packages: `eggtunnel-proto`, `eggtunnel`, and `eggtunnel-cli`.

## Requirement evidence

| Requirement | Evidence |
|---|---|
| Runtime-neutral protocol crate | `eggtunnel-proto` depends on serde, postcard, thiserror, and getrandom; no Tokio, socket, or runtime dependency appears in its Cargo tree. |
| Bounded frame decode | 14-byte documented header; declared payload checked against 1 MiB before payload access; max/max+1 and truncation checks are covered in unit tests. |
| Exact frame/message consumption | Decoder returns bytes consumed for one frame and rejects trailing postcard payload bytes; concatenated frame test decodes each boundary exactly. |
| Stable message IDs | Explicit numeric IDs 1–14 and `TryFrom<u16>` mapping; all message variants round-trip. |
| Bounded hostile fields | ServiceName 128 bytes; target host 253 bytes and nonzero port; diagnostic 256 bytes; capabilities 32 entries; Auth token 4096 bytes. Validated serde types re-check incoming values. |
| Identity distinction and handling | Separate SessionId (128-bit random), ServiceId, and ConnectionId (128-bit random); ConnectionId has constant-time byte comparison and redacted Debug; Auth Debug redacts token. |
| Feature shape | `client`, `server`, `tls`, `quic`, `websocket`, `outbound-proxy`, and `mtls` features exist. Optional features are off in the client-only no-default-features check. |
| Dependency direction | No Synvoid, i2pr, eggress-embed, or runtime dependency in the protocol foundation. `eggtunnel` client-only slice depends on the protocol crate only. |
| Documentation and CI | Root README, architecture/protocol guides, package READMEs, and CI workflow created. Protocol guide documents exact header and IDs. |

## Verification performed locally

Toolchain: rustc 1.98.1, cargo 1.98.1, aarch64-apple-darwin host.

- `rtk cargo fmt --all -- --check` — passed.
- `rtk cargo check --locked --workspace --all-targets` — passed.
- `rtk cargo test --locked --workspace --all-targets` — passed, 7 protocol tests; workspace has 3 packages.
- `rtk cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` — passed.
- `RUSTDOCFLAGS='-D warnings' rtk cargo doc --locked --workspace --no-deps` — passed.
- `rtk cargo check --locked -p eggtunnel-proto --no-default-features` — passed.
- `rtk cargo check --locked -p eggtunnel --no-default-features --features client` — passed.
- `rtk cargo tree --locked -p eggtunnel-proto` — reviewed; runtime-neutral.
- `rtk cargo tree --locked -p eggtunnel --no-default-features --features client` — reviewed; optional transport dependencies absent.

Dependency versions resolved in Cargo.lock include serde 1.0.229, postcard 1.1.3, thiserror 2.0.20, and getrandom 0.3.4.

## Limitations and findings

- No high or medium correctness finding is known from the implementation and local checks. This is not an independent security review or hosted CI result.
- `proptest` was not added. Focused table-driven boundary/round-trip tests plus a deterministic arbitrary-input no-panic loop cover the M001 decoder evidence.
- Auth tokens are redacted but are not zeroized in memory; credential lifecycle hardening belongs to later security work.
- The CLI is a placeholder and no network behavior is implemented, as required for M001.
