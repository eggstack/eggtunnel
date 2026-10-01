# Native protocol, version 1.1

The wire version is distinct from the Rust crate version. The current
published crate line is Eggtunnel `0.2.0` and uses wire version `1.0`;
this repository implements wire version `1.1` (see below) and remains
fully interoperable with `1.0` peers at baseline semantics. The
crates are pre-1.0: Rust API compatibility is not promised across minor
releases until a 1.0 library release. Wire major-version mismatches are
rejected. A peer with major 1 is eligible for baseline 1.0 behavior
regardless of minor number; optional behavior is enabled only by a
negotiated capability, never by minor version alone.

The published `0.2.0` artifact used wire 1.0; this repository's source at the
same workspace version implements wire 1.1. Crate version alone therefore
does not identify the wire behavior of a locally built binary.

Each control frame has a 14-byte header followed by one postcard payload:

| Offset | Size | Meaning |
|---:|---:|---|
| 0 | 4 | ASCII magic `ETUN` |
| 4 | 2 | major version, unsigned big-endian (`1`) |
| 6 | 2 | minor version, unsigned big-endian (`1`) |
| 8 | 2 | message ID, unsigned big-endian |
| 10 | 4 | payload length in bytes, unsigned big-endian |
| 14 | variable | postcard encoding of the message DTO |

The payload is capped at 1 MiB. The decoder checks the major version and
declared size before deserializing or copying the payload. It consumes exactly
one frame and reports the number of bytes consumed, so concatenated frames are
handled by repeated calls. Unknown IDs and malformed payloads are typed errors.
The minor version is currently informational; major versions other than 1 are
rejected. Auth tokens have a 4096-byte limit, service names 128 bytes,
diagnostics 256 bytes, capability lists 32 entries, and target hosts 253 bytes.

Stable message IDs are: 1 ClientHello, 2 ServerHello, 3 Auth, 4 AuthOk,
5 RegisterService, 6 RegisterAck, 7 UnregisterService, 8 Open, 9 OpenReject,
10 Ping, 11 Pong, 12 Drain, 13 Error, 14 DataHello, and 15 RegisterReject.
IDs are explicit Rust discriminants and are not derived from enum order.
Message 15 is extension-only: it is never sent without bilaterally
negotiated capability 1 (see below).

Credentials are represented only by the Auth payload and must only be sent
after a secure transport is established. After DataHello, data streams carry
opaque application bytes; application payload frames are not part of the
control protocol.

RegisterService and UnregisterService remain valid control messages throughout
an established authenticated Session, not only during initial setup. Each
RegisterService is answered with a RegisterAck, a bounded Error, or — when
capability 1 is negotiated — a correlated RegisterReject; clients
correlate acknowledgements by ServiceId within that Session generation.
Ping and Pong may
also repeat during the Session and correlate by nonce.

## Capability negotiation (1.1, ADR-0002)

`ClientHello.capabilities` advertises client-supported capability IDs;
`ServerHello.capabilities` returns the intersection with server-supported
IDs. Only the returned intersection counts as negotiated. Unknown IDs
are ignored; emission is unique and deterministic. A 1.0 peer advertises
an empty list and always receives baseline behavior.

Capability registry (IDs never reassigned):

| ID | Name | Meaning |
|---|---|---|
| 1 | correlated registration rejection | Registration failures arrive as `RegisterReject { service_id, code, diagnostic }` (message 15) instead of generic `Error`, so multiple dynamic registrations may be in flight, bounded and generation-scoped. Without it, the one-registration-in-flight serial fallback applies. Codes reuse the registration-category vocabulary (1 duplicate, 2 bind, 3 listener, 5 admission). |
| 2 | drain deadline | `Drain.deadline_ms` is a relative grace duration from receipt; the receiver waits at most `min(peer, local shutdown ceiling)` before forced cancellation. Zero means no peer-requested grace. Without it, receivers use local-only shutdown timing and tolerate the field. The governing local ceiling is `shutdown_grace` for the client open-task join and the server control-loop teardown join; in-flight relay bytes additionally drain under `relay_drain` inside `eggress-relay`. |

A peer can never extend the other's shutdown: the local ceiling always
caps the effective wait. Unknown or unnegotiated extension messages
(including an unexpected `RegisterReject`) fail closed as protocol
violations.

Mixed-version behavior: 1.1↔1.0 pairs use exact 1.0 baseline semantics
(serial registration, generic `Error`, local-only drain timing); 1.1↔1.1
pairs negotiate the intersection (partial intersections enable only the
negotiated subset). These existing message meanings are otherwise
unchanged; heartbeat reporting requires no new wire messages.
