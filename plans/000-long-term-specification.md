# Eggtunnel Long-Term Architecture and Product Specification

Status: canonical long-term implementation directive

Companion documents:

- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/003-planning-process.md
- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

This document defines the intended end state for Eggtunnel. It establishes product scope, ownership boundaries, protocol expectations, security properties, embedding requirements, interoperability constraints, and acceptance criteria. The roadmap decomposes this specification into ordered implementation phases.

The keywords MUST, MUST NOT, REQUIRED, SHOULD, SHOULD NOT, and MAY are normative.

## 1. Product definition

Eggtunnel is a lightweight Rust-native reverse-tunnel library and CLI. It allows a process behind NAT or a restrictive inbound firewall to make one outbound authenticated connection to a reachable Eggtunnel server and expose one or more local services through server-owned listeners.

Eggtunnel is not another general proxy framework. Eggress already owns generic byte relay, outbound proxy chaining, and optional TLS, QUIC, and WebSocket transport machinery. Eggtunnel owns the missing reverse-session layer: session establishment, service registration, public-listener allocation, pending-connection correlation, lifecycle, policy, and a small embeddable API.

The same protocol and runtime MUST support:

1. a standalone client and standalone server;
2. an embedded client inside downstream Rust applications such as CodeGG;
3. an embedded server in applications that need programmatic listener/session ownership;
4. optional transport upgrades without changing the application-level service model.

## 2. Primary goals

Eggtunnel MUST provide:

1. A persistent authenticated client-to-server control session initiated from the private/NAT side.
2. Registration of multiple named TCP services over one control session.
3. Server-owned external listeners mapped to registered client services.
4. Bounded correlation of each accepted external connection to exactly one reverse data stream.
5. TLS-secured non-loopback operation by default.
6. A small async Rust embedding surface with no global runtime, logger, or process ownership.
7. Explicit cancellation, draining, resource ceilings, and deterministic cleanup.
8. A minimal dependency profile suitable for downstream consumers.
9. Optional QUIC, WebSocket/WSS, and outbound-proxy traversal without forcing those dependencies into the minimal client.
10. Stable machine-readable diagnostics and state snapshots suitable for CLI, tests, and downstream integrations.

## 3. Non-goals

Eggtunnel is not initially:

- a VPN or TUN/TAP stack;
- a WireGuard implementation;
- a general forward proxy;
- an HTTP reverse proxy or load balancer;
- an ingress controller;
- a mesh or service-discovery system;
- a distributed scheduler;
- a remote execution framework;
- a peer-to-peer NAT-hole-punching system;
- an anonymity or censorship-resistance network;
- a custom cryptographic protocol;
- a replacement for SSH;
- a custom TCP multiplexing framework;
- an ACME/certificate-management platform;
- a user-management web application.

UDP, datagram tunneling, dynamic DNS, ingress HTTP routing, multi-relay federation, and peer-to-peer traversal MAY be considered only after the TCP product is closed and a concrete downstream requirement exists.

## 4. Architectural principles

### 4.1 Thin ownership boundary

Eggtunnel MUST own only reverse-tunnel-specific behavior. Generic stream relay, proxy-chain traversal, and optional transport adapters SHOULD be reused from Eggress where the published API is sufficient.

Eggtunnel MUST NOT duplicate Eggress protocol implementations merely for repository independence.

### 4.2 Embeddability first

The library MUST NOT:

- install a Tokio runtime;
- install a global tracing subscriber;
- parse process-global CLI arguments;
- call process::exit;
- daemonize;
- mutate global networking configuration;
- require configuration files;
- require a separately installed sidecar.

All such process concerns belong to eggtunnel-cli or the downstream owner.

### 4.3 Bounded hostile-input handling

All wire lengths, names, counts, queues, pending correlations, concurrent connections, authentication attempts, and timeouts MUST be bounded before allocation or task creation.

Malformed, oversized, stale, replayed, or unauthorized input MUST fail closed with typed errors.

### 4.4 Explicit lifecycle ownership

Every listener, connection, spawned task, queue, and pending-correlation entry MUST have an explicit owner and cancellation path.

Disconnect is a lifecycle event, not an unconditional retry instruction.

### 4.5 Transport independence

The service/session model MUST remain independent of the selected transport.

The initial native transport is TCP with TLS. QUIC and WebSocket are optional adapters. Application code SHOULD NOT depend on Quinn, Tungstenite, or Rustls concrete types.

### 4.6 No custom multiplexing before evidence

The baseline TCP/TLS product SHOULD use one persistent control connection and independent short-lived reverse data connections. Eggtunnel MUST NOT introduce a custom stream-multiplexing protocol merely to reduce socket count.

When QUIC is enabled, native QUIC bidirectional streams SHOULD provide multiplexing.

### 4.7 Security defaults over convenience

Non-loopback listeners and control endpoints MUST require an explicit secure configuration. Secrets MUST be redacted from Debug, Display, tracing, diagnostics, and snapshots.

## 5. Canonical deployment model

A deployment contains one reachable Eggtunnel server and one or more outbound-connecting Eggtunnel clients.

Server responsibilities:

- accept and authenticate control sessions;
- enforce per-principal/service policy;
- approve and own external listener bindings;
- track registered services;
- accept external TCP connections;
- allocate bounded single-use connection identifiers;
- request reverse data connections;
- pair pending external connections with authenticated reverse data connections;
- relay bytes using the generic relay substrate;
- expose bounded state/diagnostics;
- drain and clean up session-owned resources.

Client responsibilities:

- establish and maintain one authenticated control session;
- negotiate protocol version and capabilities;
- register configured local services;
- respond to Open requests;
- connect to the local target only after an authorized Open request;
- create the reverse data path;
- reconnect with bounded jittered backoff;
- restore service registrations after a new session;
- expose bounded state/diagnostics;
- shut down without orphaned tasks.

## 6. Canonical connection model

The initial TCP/TLS data path is:

1. The client establishes an outbound TLS control connection.
2. Client and server negotiate protocol version/capabilities and authenticate.
3. The client registers one or more named services.
4. The server binds or reuses the approved external listener for each service.
5. An external peer connects to one service listener.
6. The server creates a cryptographically random single-use ConnectionId and records a bounded pending entry.
7. The server sends Open(service_id, connection_id, bounded metadata) over the control session.
8. The client validates the request, connects to the local service, then opens a new outbound TLS data connection to the server.
9. The data connection begins with DataHello(session_id, connection_id).
10. The server atomically consumes the matching pending entry, pairs the two streams, and relays them.
11. Expired, duplicate, wrong-session, or already-consumed identifiers are rejected.
12. Closing either service/session cleans up all owned pending entries and listeners according to configured persistence policy.

The data stream carries opaque bytes after DataHello. Eggtunnel MUST NOT inspect application payloads in the generic TCP mode.

## 7. QUIC model

QUIC is optional.

When enabled:

- one authenticated QUIC connection represents the tunnel session;
- one bidirectional QUIC stream is reserved for control;
- each accepted external TCP connection maps to one independent bidirectional QUIC stream;
- each data stream begins with a bounded data preface identifying the session/service/correlation;
- generic Eggress relay handles the opaque byte stream after the preface;
- stream limits and connection admission MUST remain bounded;
- QUIC-specific types MUST remain below the transport adapter boundary.

QUIC is an optimization and transport option, not a second product protocol.

## 8. WebSocket and outbound-proxy traversal

WebSocket/WSS MAY be offered as an optional control/data carrier where restrictive networks permit HTTP upgrade traffic but block direct TCP/QUIC patterns.

Outbound proxy traversal MAY use Eggress outbound connectors for HTTP CONNECT, SOCKS, or configured chains.

These features MUST be opt-in and MUST NOT enter the minimal dependency graph for a direct TLS client.

## 9. Protocol requirements

Eggtunnel MUST define an explicitly versioned native protocol.

The protocol MUST provide:

- a fixed magic/preface;
- explicit numeric protocol version;
- explicit stable message identifiers;
- bounded length-delimited payloads;
- exact-consumption decode semantics;
- typed unknown-version and unknown-message errors;
- capability negotiation;
- deterministic maximum frame size;
- no serialization of secrets into diagnostics.

The initial control vocabulary SHOULD include:

- ClientHello;
- ServerHello;
- Authenticate or authenticated-session confirmation;
- RegisterService;
- RegisterAck;
- UnregisterService;
- Open;
- OpenReject;
- Ping;
- Pong;
- Drain;
- Error.

The initial data vocabulary SHOULD contain a single bounded DataHello followed by opaque stream bytes.

Serde plus postcard is the preferred initial encoding unless M001 demonstrates a concrete compatibility, size, or maintenance reason to select another bounded binary codec.

Wire numeric values MUST be explicit rather than derived from Rust enum ordering.

## 10. Identity and correlation

The following identifiers MUST remain distinct:

- SessionId: one authenticated client/server session generation;
- ServiceId: one registered service within a session;
- ConnectionId: one single-use pending external connection correlation;
- PrincipalId or authentication identity: the authorization identity associated with a session.

Identifiers MUST NOT be filesystem paths, socket addresses, or sequential database row numbers exposed as security capabilities.

ConnectionId MUST have at least 128 bits of unpredictable entropy, MUST be server-generated, MUST be bound to one session and service, MUST expire quickly, and MUST be consumed exactly once.

## 11. Service model

A service represents one externally reachable TCP listener mapped to one client-owned target.

A service registration includes bounded data such as:

- stable service name within the session;
- requested/declared external bind specification;
- local target descriptor or connector key;
- optional service metadata needed for diagnostics;
- requested limits within server ceilings.

The server is authoritative for the actual bound address. A requested port is not a grant.

The first product MUST support:

- exact configured TCP target;
- loopback TCP target;
- port zero / server-assigned external port when policy allows.

The library SHOULD also expose an application connector boundary capable of yielding an async duplex stream without requiring a loopback TCP hop. This allows downstream consumers such as CodeGG to integrate directly later while preserving the same tunnel/session protocol.

## 12. Authentication and authorization

TLS is REQUIRED for ordinary non-loopback TCP transport.

The initial authentication mechanism MAY be a bearer/service token carried only after TLS establishment. Token comparison MUST be constant-time and rate-limited.

Optional mutual TLS SHOULD be added during the hardening/security phase without replacing the simpler token profile.

Authentication establishes a principal. It does not automatically authorize arbitrary listeners.

Server authorization policy MUST be able to bound:

- allowed bind addresses;
- allowed port ranges;
- maximum registered services;
- maximum active connections;
- maximum pending correlations;
- session duration/idle policy;
- optional per-service connection ceilings.

Unauthenticated public relay operation is not a supported default.

## 13. Resource model

The runtime MUST define explicit ceilings for at least:

- active sessions;
- services per session;
- listeners per session;
- pending external connections;
- active relays;
- queued control messages;
- frame size;
- service-name size;
- error/diagnostic text size;
- handshake duration;
- pending-correlation lifetime;
- local target connection timeout;
- idle timeout;
- shutdown drain duration.

Where practical, admission SHOULD use owned permits/RAII so teardown automatically returns capacity.

Unbounded mpsc channels are prohibited in production paths.

## 14. Reconnect and recovery

Client reconnect MUST use bounded exponential backoff with jitter and cancellation awareness.

A reconnect creates a new SessionId. Registrations from a previous session MUST NOT silently remain authoritative.

The client MAY restore configured services after successful reauthentication. The server MUST distinguish session generation so stale Open or DataHello messages cannot attach to a replacement session.

Server restart, client restart, control disconnect, local-target refusal, external-peer disconnect, and data-connect timeout MUST all have deterministic cleanup behavior.

## 15. Cancellation and shutdown

Client and server handles MUST provide explicit asynchronous shutdown.

Shutdown MUST:

1. stop new admissions;
2. signal Drain where possible;
3. stop/close listeners owned by the shutting-down session or server;
4. reject or expire unpaired pending connections;
5. cancel owned connection tasks;
6. allow configured bounded relay drain;
7. join owned tasks;
8. return a typed shutdown result or bounded report.

Dropping a handle MAY trigger cancellation as a fallback, but normal correctness MUST NOT depend on Drop running asynchronous cleanup.

## 16. Eggress integration boundary

Eggtunnel SHOULD depend directly on the smallest published Eggress crates needed for each feature.

Expected usage:

- eggress-relay: generic bidirectional byte relay;
- eggress-core: common boxed stream boundary where needed;
- eggress-transport-tls: reusable TLS stream composition where its public API fits;
- eggress-transport-quic: optional QUIC connection/stream transport;
- eggress-protocol-websocket: optional WebSocket stream adapter;
- eggress-outbound: optional outbound proxy traversal.

Eggtunnel SHOULD NOT depend on eggress-embed merely to obtain lower-level functionality.

The Eggress pproxy-compatible reverse protocol is reference material and a compatibility component, not Eggtunnel's native wire protocol.

At planning time Eggress 1.0.8 is the current integration baseline. Implementation plans MUST verify the actual published API/version before pinning dependencies.

## 17. Synvoid and i2pr boundary

Synvoid and i2pr are architecture references, not production dependencies.

From Synvoid, Eggtunnel SHOULD reuse concepts such as:

- version/capability handshake;
- bounded length-prefixed binary control messages;
- per-session registries;
- semaphore admission;
- authentication throttling;
- keepalive/RTT observation;
- jittered reconnect;
- one QUIC stream per logical TCP stream.

Eggtunnel MUST NOT absorb Synvoid's TUN, WireGuard, mesh, distributed routing, VPN, or broad health subsystem.

From i2pr, Eggtunnel SHOULD reuse architectural principles such as:

- runtime-neutral protocol/policy where practical;
- explicit task ownership;
- bounded channels;
- resource budgets and owned leases;
- typed lifecycle and termination;
- exact-consumption hostile-input handling.

No i2pr code is to be copied or depended upon while its repository has no selected license. I2P-specific protocol/transport machinery is out of scope regardless.

## 18. Workspace and crate layout

The intended initial workspace is:

- crates/eggtunnel-proto: bounded wire DTOs/codecs and runtime-neutral protocol types;
- crates/eggtunnel: public async library, client/server/session runtime, policy, and transport adapters;
- crates/eggtunnel-cli: process-level configuration and the eggtunnel binary.

A future eggtunnel-testkit crate MAY be added only when test reuse justifies a separate package.

The library's public API SHOULD avoid exposing internal dependency types unless deliberately re-exported as a stable boundary.

## 19. Cargo feature policy

Feature flags SHOULD keep expensive or deployment-specific transports optional.

The intended direction is:

- client: client/session role;
- server: relay/listener role;
- tls: TCP TLS transport;
- quic: QUIC adapter;
- websocket: WebSocket/WSS adapter;
- outbound-proxy: Eggress outbound-chain integration;
- mtls: mutual-TLS authentication profile if separable from tls.

The CLI MAY enable client, server, and tls together.

Downstream applications MUST be able to build a client-only TLS profile without QUIC, WebSocket, CLI parsing, server listeners, or full Eggress service/runtime machinery.

## 20. MSRV and unsafe policy

The workspace SHOULD use Rust 1.89 or newer to align with current Eggstack release targets.

Production crates MUST forbid unsafe code unless a later accepted ADR identifies a narrowly justified requirement.

Dependencies that contain unsafe internally are not prohibited, but Eggtunnel-owned unsafe requires explicit architectural review.

## 21. Observability

Libraries MUST emit structured tracing events but MUST NOT install subscribers.

Snapshots and diagnostics MUST be bounded and secret-free.

Useful counters include:

- active sessions;
- registered services;
- active/pending connections;
- accepted/rejected connections;
- authentication failures;
- stale/replayed correlation rejection;
- reconnect count;
- bytes relayed;
- shutdown/drain outcome.

Prometheus or other exporter dependencies are not required for the core product. Downstream owners may adapt snapshots/events.

## 22. CLI and configuration

The CLI SHOULD support TOML configuration plus explicit command-line overrides.

Initial command shape SHOULD converge around:

- eggtunnel server;
- eggtunnel client;
- eggtunnel version;
- eggtunnel check or config-test.

The CLI MUST print machine-readable JSON for state/check output where useful.

Secrets SHOULD be accepted through environment/file/secret-reference mechanisms without requiring plaintext persistence in the main configuration.

## 23. Downstream integration contract

Eggtunnel exists partly to be consumed by other Eggstack and related projects.

For CodeGG specifically:

- CodeGG SHOULD be able to embed the Eggtunnel client with default features disabled and only the needed transport enabled;
- CodeGG remote/TUI protocol semantics remain CodeGG-owned;
- CodeGG authorization, node enrollment, audit, project policy, and remote execution remain CodeGG-owned;
- Eggtunnel provides authenticated reachability only;
- a first integration MAY forward to CodeGG's loopback HTTP/WebSocket endpoint;
- a later integration MAY provide a direct connector returning an async duplex stream to avoid the loopback hop.

Eggtunnel MUST NOT become a dependency on CodeGG.

## 24. Testing and evidence

The project MUST prioritize deterministic local loopback tests.

Required classes include:

- codec boundary/property tests;
- malformed/oversized frame tests;
- handshake/version/capability tests;
- auth success/failure/rate-limit tests;
- multi-service registration;
- connection correlation single-use/replay/wrong-session tests;
- target refusal/timeout;
- simultaneous external connections;
- half-close semantics;
- cancellation during every handshake stage;
- client/server restart and reconnect;
- bounded queue/admission saturation;
- graceful and forced shutdown;
- transport feature slices;
- downstream-shaped compile tests.

Fuzzing SHOULD target the protocol decoder and state-machine boundaries once M001 is functional.

Closure claims MUST distinguish local evidence from hosted CI.

## 25. Performance and footprint

Correctness and boundedness precede micro-optimization.

The project SHOULD nevertheless protect its lightweight objective through:

- client-only dependency-tree inspection;
- release binary size tracking as informational evidence;
- no mandatory TLS+QUIC+WebSocket co-compilation;
- no general database;
- no admin web server;
- no persistent per-connection history;
- zero application-payload inspection on the raw TCP path;
- buffer reuse only after measurement demonstrates value.

No performance gate should rely on unstable wall-clock CI timings.

## 26. Distribution targets

The standalone CLI SHOULD eventually provide prebuilt binaries for:

- Linux x86_64 GNU;
- Linux aarch64 GNU;
- Linux armv7 GNU when dependencies/toolchains permit;
- Linux musl targets where practical;
- macOS x86_64;
- macOS aarch64;
- Windows x86_64;
- Windows ARM64 when dependencies/toolchains permit.

SBC-class Linux systems, including Raspberry Pi and Le Potato-class devices, are explicit target environments.

Distribution machinery SHOULD reuse Eggstack-wide installer/updater infrastructure such as eggup when that external interface is ready, rather than copying another updater implementation into Eggtunnel.

## 27. Documentation requirements

Before 0.1 product closure, the repository MUST contain:

- README with product definition and minimal client/server example;
- protocol overview;
- embedding guide;
- security model;
- configuration reference;
- operations/troubleshooting guide;
- release/support matrix;
- architecture notes for transport and lifecycle ownership.

Documentation MUST distinguish implemented behavior from roadmap intent.

## 28. System invariants

The following are long-term invariants:

1. Eggtunnel remains a reverse-session layer rather than another proxy/VPN stack.
2. All externally sourced lengths and counts are bounded before allocation.
3. A ConnectionId is single-use, short-lived, and session-bound.
4. Public listener allocation is server-authoritative and policy-checked.
5. Non-loopback native operation is encrypted and authenticated by default.
6. No library path installs a global runtime or logging subscriber.
7. Every spawned production task has explicit cancellation and ownership.
8. No production channel is unbounded.
9. Application bytes are opaque in generic TCP tunneling.
10. Optional transports do not leak into the minimal client dependency graph.
11. QUIC multiplexing uses the transport's native streams rather than an Eggtunnel custom mux.
12. Synvoid and i2pr remain architecture references unless a separately reviewed supported/licensed library boundary emerges.
13. Downstream application authorization remains downstream-owned.
14. Closure requires executable evidence, not documentation assertions.

## 29. Initial product acceptance

The first stable product boundary is satisfied when:

- a standalone server accepts an authenticated TLS session;
- one client registers multiple TCP services;
- external clients can concurrently connect through those services;
- each connection is safely correlated and relayed;
- client reconnect restores configured services without accepting stale correlations;
- resource limits and malformed input fail closed;
- shutdown leaves no owned listener/task/pending-entry leaks;
- a client-only library build remains materially smaller than the CLI/full transport profile;
- a downstream-shaped integration test demonstrates embedding without a sidecar;
- documentation and closure evidence accurately describe the supported transport/security matrix.
