# Transport support

| Profile | Library selection | Network path | Server trust | mTLS | Status |
|---|---|---|---|---|---|
| TCP/TLS | Default `client` + `tls`, and optional `server` | TCP control connection plus one TCP/TLS data connection per external TCP connection | System roots or configured custom CA; SNI verified | Supported with the `mtls` feature and explicitly provisioned client CA | Implemented and locally qualified |
| QUIC | Optional `quic` feature; CLI `transport = "quic"` | One UDP QUIC connection per Session, one control stream and one bidirectional stream per external TCP connection | Platform roots; SNI verified | Not supported by the current Eggress adapter | Implemented and locally qualified; no custom CA option |
| WebSocket/WSS | Optional `websocket` feature; CLI `transport = "websocket_tls"` | Separate TLS+WebSocket control and data connections | Configured or system roots; SNI verified | Not supported by current Eggtunnel WebSocket profile | Implemented; local end-to-end session/data test |
| Outbound proxy traversal | Optional `outbound-proxy` feature; CLI `outbound_proxy_env` | Listener-free Eggress connector dials before Eggtunnel TLS | Eggtunnel TLS remains end-to-end with configured roots and SNI | Proxy+mTLS unsupported | HTTP CONNECT locally qualified; SOCKS5 and chains use Eggress 1.0.8 but lack Eggtunnel integration tests |

QUIC and WebSocket do not change Service, authorization, TargetConnector, or ConnectionId
semantics. The CLI rejects custom CA and mTLS settings with QUIC rather than
silently dropping them. Service listeners remain TCP even when the control
Session uses QUIC over UDP. WebSocket uses WSS and rejects mTLS. Outbound proxy
credentials come from an environment variable; proxy+QUIC and proxy+mTLS are
rejected. WebSocket half-close behavior is limited by WebSocket full-connection
close semantics and is not qualified as TCP half-close equivalent.

## Release target evidence

| Target | Current evidence | Claim |
|---|---|---|
| `aarch64-apple-darwin` | Local host runs the workspace suite; local release archive/install smoke passed | Local smoke evidence only; hosted release qualification pending |
| `x86_64-unknown-linux-gnu` | Release workflow runner configured; hosted workflow has not run in this implementation pass | Candidate only |
| `aarch64-unknown-linux-gnu` | Release workflow runner configured; hosted workflow has not run in this implementation pass | Candidate only |
| `x86_64-apple-darwin` | Release workflow runner configured; hosted workflow has not run in this implementation pass | Candidate only |
| Windows, musl, armv7, Raspberry Pi/Le Potato variants | No release runner/runtime evidence recorded | Unsupported/unevaluated |

The release workflow produces archives, SHA-256 manifests, and build
attestations for the four candidate targets. It does not publish Rust crates.
