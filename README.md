# Eggtunnel

Eggtunnel is a Rust reverse-tunnel library and CLI: a process behind NAT
connects outward to a reachable server, which exposes approved local services
through server-owned listeners.

**Implementation status:** this repository currently contains the workspace
and native protocol foundation only. It does not yet provide a client/server
network tunnel. See the [roadmap](plans/subsystems/reverse-session-roadmap.md).

