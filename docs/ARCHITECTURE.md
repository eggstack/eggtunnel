# Architecture

Eggtunnel owns reverse-session behavior. Eggress is intended to provide generic
relay and optional transport primitives in later milestones. `eggtunnel-proto`
is runtime-neutral and owns bounded wire DTOs and framing; it has no socket,
async runtime, timer, or task dependencies.

The current repository implements M001 only. Session runtime, TLS, listener
ownership, and byte relay are not yet implemented. The intended dependency
direction is documented in the [subsystem roadmap](../plans/subsystems/reverse-session-roadmap.md).

