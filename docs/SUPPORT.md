# Transport support

| Profile | Library selection | Network path | Server trust | mTLS | Status |
|---|---|---|---|---|---|
| TCP/TLS | Default `client` + `tls`, and optional `server` | TCP control connection plus one TCP/TLS data connection per external TCP connection | System roots or configured custom CA; SNI verified | Supported with the `mtls` feature and explicitly provisioned client CA | Implemented and locally qualified |
| QUIC | Optional `quic` feature; CLI `transport = "quic"` | One UDP QUIC connection per Session, one control stream and one bidirectional stream per external TCP connection | Platform roots; SNI verified | Not supported by the current Eggress adapter | Implemented and locally qualified; no custom CA option |
| WebSocket/WSS | — | — | — | — | Not implemented |
| Outbound proxy traversal | — | — | — | — | Not implemented |

QUIC does not change Service, authorization, TargetConnector, or ConnectionId
semantics. The CLI rejects custom CA and mTLS settings with QUIC rather than
silently dropping them. Service listeners remain TCP even when the control
Session uses QUIC over UDP.
