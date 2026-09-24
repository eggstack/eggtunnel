# Native protocol, version 1.0

The wire version is distinct from the Rust crate version. The current
published crate line is Eggtunnel `0.2.0` and uses wire version `1.0`. The
crates are pre-1.0: Rust API compatibility is not promised across minor
releases until a 1.0 library release. Wire major-version mismatches are
rejected. Minor versions are currently informational; there is no
backward-peer support window or capability negotiation beyond exchanging the
current empty capability set. Do not infer a long-term 1.x protocol guarantee
from the current wire number.

Each control frame has a 14-byte header followed by one postcard payload:

| Offset | Size | Meaning |
|---:|---:|---|
| 0 | 4 | ASCII magic `ETUN` |
| 4 | 2 | major version, unsigned big-endian (`1`) |
| 6 | 2 | minor version, unsigned big-endian (`0`) |
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
10 Ping, 11 Pong, 12 Drain, 13 Error, and 14 DataHello. IDs are explicit Rust
discriminants and are not derived from enum order.

Credentials are represented only by the Auth payload and must only be sent
after a secure transport is established. After DataHello, data streams carry
opaque application bytes; application payload frames are not part of the
control protocol.

RegisterService and UnregisterService remain valid control messages throughout
an established authenticated Session, not only during initial setup. Each
RegisterService is answered with a RegisterAck or a bounded Error; clients
correlate acknowledgements by ServiceId within that Session. Ping and Pong may
also repeat during the Session and correlate by nonce. These existing message
meanings are unchanged; dynamic registration and heartbeat reporting require
no new wire messages.
