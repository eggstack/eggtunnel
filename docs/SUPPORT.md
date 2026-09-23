# Transport support

| Profile | Library selection | Network path | Server trust | mTLS | Status |
|---|---|---|---|---|---|
| TCP/TLS | Default `client` + `tls`, and optional `server` | TCP control connection plus one TCP/TLS data connection per external TCP connection | System roots or configured custom CA; SNI verified | Supported with the `mtls` feature and explicitly provisioned client CA | Implemented and locally qualified |
| QUIC | Optional `quic` feature; CLI `transport = "quic"` | One UDP QUIC connection per Session, one control stream and one bidirectional stream per external TCP connection | Platform roots; SNI verified | Not supported by the current Eggress adapter | Implemented and locally qualified; no custom CA option; transport-specific wrong-session, stale-session, replay, saturation, and half-close cases qualified in C001 |
| WebSocket/WSS | Optional `websocket` feature; CLI `transport = "websocket_tls"` | Separate TLS+WebSocket control and data connections | Configured or system roots; SNI verified | Not supported by current Eggtunnel WebSocket profile | Implemented; local end-to-end session/data test; close-during-relay and bounded backpressure cases qualified in C001 |
| Outbound proxy traversal | Optional `outbound-proxy` feature; CLI `outbound_proxy_env` | Listener-free Eggress connector dials before Eggtunnel TLS | Eggtunnel TLS remains end-to-end with configured roots and SNI | Proxy+mTLS unsupported | Single-hop HTTP CONNECT and SOCKS5 locally qualified; Basic auth and SOCKS5 username/password auth success/failure qualified; multi-hop chains supported through `__`-separated pproxy URI syntax with one two-hop SOCKS5+HTTP CONNECT end-to-end test; refusal, handshake timeout, and cancellation cases qualified in C001 |

QUIC and WebSocket do not change Service, authorization, TargetConnector, or ConnectionId
semantics. The CLI rejects custom CA and mTLS settings with QUIC rather than
silently dropping them. Service listeners remain TCP even when the control
Session uses QUIC over UDP. WebSocket uses WSS and rejects mTLS. Outbound proxy
credentials come from an environment variable; proxy+QUIC and proxy+mTLS are
rejected. WebSocket half-close behavior is limited by WebSocket full-connection
close semantics and is not qualified as TCP half-close equivalent. Proxy
credentials placed in the environment variable are redacted from Eggtunnel
diagnostics and the public `Snapshot` view; failures are recorded only as typed
termination categories.

## Transport evidence gaps inherited from M004 / M005

The C001 corrective pass added transport-specific evidence for the previously
uncovered M004/M005 cases. Remaining narrow gaps:

- WebSocket is still not qualified as TCP half-close equivalent because
  WebSocket's protocol-level close closes the entire connection. C001
  confirmed Eggtunnel's relay and the Eggress WebSocket adapter both close the
  underlying TCP connection promptly when the WebSocket peer closes, but the
  application relay does not observe a TCP-style write-half-close.
- Multi-hop chains are qualified through one SOCKS5+HTTP CONNECT integration
  test that covers the canonical `__`-separated pproxy URI syntax. Additional
  protocol combinations are unverified beyond the Eggress 1.0.8 public API's
  typed compatibility layer.

No new high/medium correctness or security findings remain open against the
optional transport profiles.

## Release target evidence

| Target | Current evidence | Claim |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Hosted release build, SHA-256 manifest, per-runner install/version smoke; local cross-check | Supported (build + install/version smoke) |
| `aarch64-unknown-linux-gnu` | Hosted release build, SHA-256 manifest, per-runner install/version smoke; local cross-check | Supported (build + install/version smoke) |
| `x86_64-apple-darwin` | Hosted release build, SHA-256 manifest, per-runner install/version smoke; local cross-check | Supported (build + install/version smoke) |
| `aarch64-apple-darwin` | Hosted release build, SHA-256 manifest, per-runner install/version smoke; independent consumer-side download/checksum/install/version verification | Supported (build + install/version smoke) |
| Windows, musl, armv7, Raspberry Pi/Le Potato variants | No release runner evidence; local check-only cross-compiles exist for `x86_64-pc-windows-gnu` and `armv7-unknown-linux-gnueabihf` but neither is installed, smoked, or released | Unsupported/unevaluated |

The release workflow produces archives, SHA-256 manifests, and build
attestations for the four supported targets. Rust crates `eggtunnel-proto`
and `eggtunnel` are published at `0.1.0`; downstream registry consumption is
qualified with a registry-dependent consumer build.
