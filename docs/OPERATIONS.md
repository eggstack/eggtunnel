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
