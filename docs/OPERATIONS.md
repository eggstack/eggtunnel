# Operations

Start the server with `eggtunnel server server.toml`; start a private-side
client with `eggtunnel client client.toml`. Both processes stop on Ctrl-C. The
server sends a bounded Drain notification before closing active sessions.

The server's `listen_addr` accepts both control sessions and reverse data
connections. Every accepted connection begins with TLS. A service's actual
address is server-assigned and is available through the Rust `ServerHandle`
snapshot. The CLI prints newly assigned service addresses while it is running.

The client retries transient connection, TLS, and protocol failures with
bounded exponential backoff and jitter. Invalid authentication or service
authorization stops retries. Client service registrations are restored after
a new authenticated Session.

Use a trusted certificate whose subject alternative names include
`tls_server_name`. Keep token and private-key files readable only by the
service account. Service listeners are loopback-only unless public binding was
explicitly enabled.

The server bounds accepted unauthenticated handshakes to 64 at once and active
Sessions to 128. Per Session, it allows up to 64 services, 128 pending
connections, and 128 active connections. Client Open tasks and control queues
are also bounded at 128. Authentication failures are tracked per source IP
for 60 seconds; the tenth failure blocks that source until its window expires.

Runtime snapshots expose current counts and high-water values for Sessions,
Services, Pending Connections, active Connections, client Open tasks, and
unauthenticated handshakes. These counters cover one process lifetime and do
not persist across restart.
The snapshot includes the most recent bounded termination category and a
count of panicked owned tasks; it never retains event history or error text.
