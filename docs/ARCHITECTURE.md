# Architecture

Eggtunnel owns reverse-session behavior. Eggress is intended to provide generic
relay and optional transport primitives in later milestones. `eggtunnel-proto`
is runtime-neutral and owns bounded wire DTOs and framing; it has no socket,
async runtime, timer, or task dependencies.

The current implementation includes the TCP/TLS baseline: one authenticated
control stream, server-owned service listeners, single-use pending connection
correlations, and one TLS data stream per external connection. Eggress relay
copies opaque application bytes. Optional mTLS and direct application target
connectors are feature/API-gated. QUIC is feature-gated and uses one
bidirectional stream per control or data path. WSS wraps each TCP/TLS control
and data connection in a binary WebSocket byte stream. The optional outbound
proxy adapter dials each client connection before Eggtunnel TLS; it does not
start a local proxy listener or change the Session protocol. The intended
dependency direction is documented in the [subsystem roadmap](../plans/subsystems/reverse-session-roadmap.md).
