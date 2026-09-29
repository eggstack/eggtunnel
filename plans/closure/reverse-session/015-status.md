# Reverse Session M015 Closure — CLI Configuration Resolution and Operational Surface

Status: closed

Disposition: closed. All M015 acceptance criteria are met on the exact
implementation head recorded below. No unresolved high/medium finding
remains. M016 was already unblocked by M014 and is unaffected; no
further plan becomes unblocked by M015 (it is not a hard dependency of
any registered milestone).

## Baseline and commits

- Planning baseline: M014 strict closure (`b16ba2e`,
  `plans/closure/reverse-session/014-status.md`).
- Implementation + closure commits: recorded at push time (single
  M015 commit containing the CLI rewrite, tests, docs, and this
  record).
- Final reviewed head: the M015 commit on `origin/main`.

## Before/after config-resolution flow

Before: `read_config → check_config → client_builder/server_builder`
with environment variables and files read two to three times across
validation and startup (`token_env`, `outbound_proxy_env`, cert/key/CA
files), CLI-owned endpoint parsers duplicating library semantics, and
human-only output.

After — one pipeline, five explicit stages, executed once per command
(`run_check`, `run_server`, `run_client` in
`crates/eggtunnel-cli/src/main.rs`):

1. `read_config` (`:245-255`): TOML syntax only (`config_parse`).
2. `apply_client_overrides` / `apply_server_overrides` (`:257-321`):
   non-secret CLI flags over TOML fields (CLI > TOML > built-in).
3. `resolve_client_with` / `resolve_server_with` (`:496-640`): every
   environment variable and file read exactly once into the redacted
   `ResolvedClient` (`:374-406`) / `ResolvedServer` (`:407-440`)
   snapshot (hand-written redacted `Debug`, no `Serialize`).
4. `client_builder(resolved)` / `server_builder(resolved)`
   (`:641-681`): owned values lowered into the canonical library
   builders; no environment or file access.
5. `builder.validate()` (check) or `builder.start()` / `.bind()`
   (launch) — the same library validator owns transport/profile
   semantics in both paths.

The old `check_config`, `client_services`, `checked_addr`,
`checked_endpoint`, and `client_builder`/`server_builder` dual-read
forms are deleted. Client endpoint shape is the canonical library
`Endpoint::parse`; `listen_addr` remains `SocketAddr`.

## Override precedence matrix

| Flag(s) | Applies to | Precedence proof |
|---|---|---|
| `--server-addr`, `--tls-server-name`, `--transport`, `--ca-cert`, `--token-env`, `--outbound_proxy-env`→`--outbound-proxy-env`, `--client-cert`, `--client-key` | client | unit `overrides_win_over_toml_without_touching_unrelated_fields`; integration `cli_overrides_win_over_toml_fields` (invalid TOML endpoint rescued by a valid override; invalid override fails valid TOML) |
| `--bind-port` | client, single-service files only | unit `bind_port_override_requires_exactly_one_service` (0 and 2 services rejected; 1 service applied to `RequestedBind::Loopback`) |
| `--listen-addr`, `--transport`, `--tls-cert`, `--tls-key`, `--client-ca`, `--token-env` | server | same override application path (`apply_server_overrides`) |
| `--allow-public-service-binds` | server, one-way enable | documented; cannot disable a TOML `true` |

No `--token` or proxy-password flag exists by design (process-listing /
shell-history leakage). Rejected override/profile combinations fail
through the library validator before any socket bind/connect.

## JSON schema/examples with secrets demonstrably absent

- `eggtunnel.check/v1` (`CheckReport`, `:703-751`): `ok`, `mode`,
  `transport`, `services`, `custom_ca`/`mtls`/`outbound_proxy`
  booleans, null-or-`{category, message}` error. Example success:
  `{"schema":"eggtunnel.check/v1","ok":true,"mode":"client","transport":"tcp_tls","services":1,"custom_ca":false,"mtls":false,"outbound_proxy":false,"error":null}`.
- `eggtunnel.events/v1` (runtime): `startup` (version/mode/transport/
  services), `server_listening` (addr), `service_bind`
  (service/session/address/port), `session_ready` (generation/services),
  `session_lost` (termination/reconnects), periodic `snapshot`
  (bounded library `Snapshot` fields + binds), `shutdown` (reason).
  `--snapshot-interval-secs` (minimum 5, requires `--json`).
- Absence evidence: unit `debug_and_json_never_carry_secret_values`
  (Debug + check JSON + snapshot JSON scanned for token and proxy
  password), integration redaction scans over live `client`/`server`
  stdout/stderr (`tests/cli.rs`), and the snapshot-bind correspondence
  test (`snapshot_event_renders_effective_binds_from_the_snapshot`).

## Strictness note (correctness fix per plan §11)

`Endpoint::parse` is stricter than the retired CLI parser:
unbracketed IPv6 and URL-authority-ambiguous hosts now fail `check`
with `bind_validation` instead of passing structurally. No in-tree
example, test, or fixture uses a rejected shape. Documented in
`docs/CONFIGURATION.md` and `architecture/cli-config-ops.md` §4/§7.5.

## CLI integration test outcomes

- Unit (`--bin eggtunnel`): 14 passed — precedence, single-service
  selector, snapshot-ignores-later-inputs (incl. exact-once read
  count), redaction, JSON schema stability, transport/mode categories,
  QUIC+CA library-validator rejection, endpoint parity, interval
  minimum, missing-token category, server-mode rejection,
  category vocabulary, snapshot bind rendering.
- Integration (`tests/cli.rs`): 7 passed — `version`, human `check`,
  JSON `check` (stable + redacted), JSON `check` failure category,
  override precedence (behavioral, via `client` spawn), client startup
  JSON events + redaction, server startup/listening JSON + redaction
  (openssl-generated cert).
- Installer smoke (`scripts/test-install.sh`): pass (`eggtunnel
  0.2.0`, checksum-rejection and unsupported-target rejection
  negative-pass) — shipped-binary command compatibility preserved.

## Full workspace/hosted verification

- `cargo fmt --all -- --check`, `cargo check --locked --workspace
  --all-targets`, `cargo test --locked --workspace --all-targets
  --all-features` (lib 91 / CLI bin 14 / CLI integration 7 / proto 8),
  `cargo clippy --locked --workspace --all-targets --all-features --
  -D warnings`, `RUSTDOCFLAGS="-D warnings" cargo doc --locked
  --workspace --all-features --no-deps`, `cargo check --locked
  --manifest-path fixtures/embedder/Cargo.toml` — all clean.
- `cargo +1.89.0 check --locked -p eggtunnel-cli` — clean (MSRV).
- `cargo audit` — 0 vulnerabilities (1 pre-existing allowed
  informational warning). `cargo deny check licenses` — pass (new
  `serde_json` CLI-only dependency is MIT/Apache-2.0).
- Hosted CI (`Rust` workflow: `check`, 14-combo `feature-slices`,
  `msrv`, `minimal-dependencies`) run `36576415963` on the pushed M015
  head: **success** (all jobs passed).

## Compatibility notes and unresolved findings

- Existing TOML files, `version`/`check`/`client`/`server` command
  forms, and human output are unchanged (human remains the default).
  JSON/override/snapshot flags are purely additive.
- No library or wire change (`Endpoint`/`EndpointError` were added in
  M014; M015 consumes them).
- No remote-control, reload, account, persistence, installer,
  self-update, ACME, or subscriber-policy surface introduced.
- Findings: high — none; medium — none; low — §7.1 cross-mode silent
  ignores retained as open review items (unchanged behavior, now
  tracked against the new stage numbers).

## Disposition

M015 is closed. The CLI is a single-resolution adapter over canonical
library validation with redacted machine-readable surfaces and tested
override precedence. No new executable milestone is unblocked (M016
depends only on M014, already closed).
