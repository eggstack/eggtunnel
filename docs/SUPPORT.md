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
