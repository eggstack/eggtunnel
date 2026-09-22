# Eggtunnel Terminology and Domain Model

Status: canonical terminology

This document defines the terms used by Eggtunnel planning, protocol, code, documentation, and closure evidence. When older notes or external projects use overlapping terms differently, this document is authoritative for Eggtunnel.

## 1. Deployment

A Deployment is one logical Eggtunnel operating environment containing at least one reachable Server and one or more Clients.

A deployment is not a cluster-consensus concept. High-availability relay federation is outside the initial architecture.

## 2. Server

The Server is the reachable Eggtunnel role that:

- accepts client-initiated control sessions;
- authenticates a principal;
- owns public/external listener bindings;
- authorizes service registrations;
- accepts external connections;
- correlates them to reverse data connections or streams;
- owns server-side admission and resource ceilings.

Server refers to the Eggtunnel role, not necessarily a standalone process. An embedded application may host the server role in-process.

## 3. Client

The Client is the private-side Eggtunnel role that initiates outbound connectivity to the Server.

A client owns:

- local service declarations;
- the outbound control-session lifecycle;
- reconnect and service re-registration;
- local target connection;
- reverse data-path creation.

Client does not imply an interactive human client.

## 4. Principal

A Principal is the authentication and authorization identity attached to one Session.

The initial implementation may derive Principal identity from a configured service/bearer token. Future mTLS identity may map a certificate identity to the same conceptual Principal.

A Principal is distinct from SessionId and from a Service.

## 5. Session

A Session is one authenticated, version-negotiated client/server control relationship.

A reconnect creates a new Session even if the same Client and Principal reconnect immediately.

A Session owns:

- negotiated protocol/capability state;
- registered Services;
- session-scoped listeners where applicable;
- pending Connection correlations;
- session-scoped counters and cancellation;
- liveness/keepalive state.

Session identity MUST NOT be reused as an authentication secret.

## 6. SessionId

SessionId is the opaque identifier of one Session generation.

Properties:

- generated with sufficient entropy or collision resistance;
- not derived from socket address;
- distinct across reconnect generations;
- safe to expose in bounded diagnostics;
- not sufficient by itself to authenticate a connection.

## 7. Service

A Service is one registered reverse-forwarding endpoint.

A Service connects:

External listener -> Eggtunnel transport -> Client Target

A Service has:

- a ServiceId;
- a bounded human-readable ServiceName;
- an approved external Bind;
- a client-owned Target;
- effective policy/limits.

One Session may register multiple Services.

## 8. ServiceId

ServiceId identifies a Service within its owning Session or within a server-defined stable namespace if a later milestone explicitly promotes service identity across reconnect.

The first implementation SHOULD treat ServiceId as session-scoped and SHOULD use ServiceName/config identity for safe re-registration after reconnect.

## 9. ServiceName

ServiceName is a bounded operator-facing identifier such as codegg, ssh-dev, or metrics.

It is not a DNS name, authorization capability, or stable database key unless an explicit later contract makes it one.

Names MUST be validated for length and allowed characters before persistence or wire use.

## 10. Bind

Bind is the server-side listener request and resulting externally reachable socket address.

Distinguish:

- RequestedBind: what the client/config asks for;
- EffectiveBind: what server policy approves and actually binds.

The Server is authoritative for EffectiveBind.

A requested port of zero means server-assigned ephemeral port when policy permits.

## 11. External listener

An External Listener is the Server-owned TCP listener that accepts connections from external peers for a Service.

It is distinct from the control listener used by Eggtunnel Clients and distinct from the Client's local Target.

## 12. External connection

An External Connection is one accepted TCP connection from an external peer to a Service's EffectiveBind.

Each External Connection MUST correlate to at most one reverse Data Path.

## 13. ConnectionId

ConnectionId is a short-lived, unpredictable, server-generated, single-use correlation capability for one pending External Connection.

A ConnectionId MUST be:

- at least 128 bits of unpredictable entropy;
- bound to one Session;
- bound to one Service;
- stored only while pending/active as required;
- expired after a short bounded lifetime;
- atomically consumed;
- rejected on replay;
- rejected from the wrong Session.

ConnectionId is not a long-lived credential.

## 14. Pending connection

A Pending Connection is server-owned state created after accepting an External Connection and before the corresponding reverse Data Path is successfully paired.

Pending state MUST have:

- SessionId;
- ServiceId;
- ConnectionId;
- creation/expiry time;
- owned external stream;
- capacity/admission lease;
- cancellation relationship to session/server shutdown.

Pending state cannot be unbounded.

## 15. Control connection

A Control Connection is the persistent transport stream carrying Eggtunnel protocol control messages for one Session.

In the baseline TCP/TLS transport there is one control TCP/TLS connection per Session.

In QUIC mode, the conceptual Control Connection is one reserved bidirectional QUIC stream within the Session's QUIC connection.

## 16. Data connection

A Data Connection is a baseline TCP/TLS transport connection opened from Client to Server in response to an authorized Open message.

It starts with a DataHello carrying session/correlation identity and becomes opaque application bytes after pairing.

Data Connection is transport-specific. The transport-independent term is Data Path.

## 17. Data stream

A Data Stream is a QUIC bidirectional stream used for one tunneled external TCP connection.

It is the QUIC counterpart of a baseline Data Connection.

## 18. Data Path

Data Path is the transport-neutral logical stream carrying one external TCP connection between Server and Client.

It may be implemented by:

- a separate TCP/TLS Data Connection;
- a QUIC bidirectional Data Stream;
- a future approved stream-like adapter.

The Data Path becomes opaque after its Eggtunnel preface.

## 19. Target

Target is the Client-side destination for a Service.

The first implementation supports TcpTarget(host, port).

The embedding API MAY support an application connector that returns an async duplex stream. Such a connector is still a Target implementation; it does not alter protocol semantics.

## 20. Target connector

A Target Connector is the Client-side code responsible for turning an authorized Open request into a connected local async duplex stream.

It owns local connection policy and errors, not server listener policy.

## 21. Transport

Transport is the mechanism carrying Eggtunnel control and data traffic between Client and Server.

Initial transport profiles:

- TcpTls: baseline;
- Quic: optional;
- WebSocketTls: optional.

Transport selection MUST NOT change Service, Session, or authorization semantics.

## 22. Carrier

Carrier is a lower-level stream mechanism used by a Transport implementation, for example Eggress WebSocket adaptation or Eggress QUIC streams.

Use Transport for the user/product contract and Carrier only when discussing lower-level composition.

## 23. Relay

Relay is the protocol-neutral bidirectional copying of bytes between two already-connected streams.

Eggtunnel SHOULD delegate Relay behavior to eggress-relay.

Relay is not the same as the Eggtunnel Server, which additionally owns sessions, listeners, policy, and correlation.

## 24. Reverse session

Reverse Session is a descriptive synonym for Session emphasizing that the private-side Client initiates connectivity outward.

Use Session in protocol types and code.

## 25. Capability

Capability is one negotiated protocol feature bit or named protocol behavior supported by both peers.

Examples may include:

- multi-service registration;
- QUIC data streams;
- mTLS identity metadata;
- graceful drain;
- future datagram support.

Capabilities MUST be versioned/bounded and MUST NOT silently authorize an operation.

## 26. Authentication

Authentication answers: which Principal is this Session associated with?

Initial bearer-token authentication occurs only inside an encrypted transport.

## 27. Authorization

Authorization answers: may this Principal register this Service, request this Bind, or consume this amount of capacity?

Authentication success does not imply unrestricted authorization.

## 28. Admission

Admission is the bounded decision to allow a resource-consuming action such as:

- new Session;
- new Service;
- new Pending Connection;
- new active Relay;
- new QUIC stream task.

Admission SHOULD be represented by owned capacity where practical.

## 29. Lease or permit

Lease/Permit is an owned resource-admission token whose drop releases capacity.

This is distinct from an I2P lease and has no relationship to i2pr protocol semantics.

## 30. Drain

Drain is an orderly shutdown state in which new admissions stop and existing work is allowed to finish within a bounded grace period.

Drain is distinct from immediate cancellation.

## 31. Reconnect

Reconnect is Client creation of a new transport connection and Session after the prior Session terminates.

Reconnect does not resurrect stale ConnectionIds or prior pending state.

Configured Services may be re-registered after authentication.

## 32. Registration restoration

Registration Restoration is the Client action of registering configured Services into a newly established Session.

It creates new session-owned registration state. It is not continuation of old pending connections.

## 33. Liveness

Liveness is the observed state indicating whether a Session transport is usable.

Ping/Pong, transport close, read/write failures, and transport-native idle timeout MAY contribute to liveness.

Liveness is not application-service health.

## 34. Health

Health is a bounded diagnostic summary of a Client, Server, Session, or Service.

The initial project SHOULD keep health simple: connected/disconnected, counts, last error class, and optional RTT. Broad scoring systems are out of scope.

## 35. Snapshot

Snapshot is bounded read-only structured state exposed by a library handle or CLI.

Snapshots MUST be secret-free and MUST NOT contain unbounded connection history.

## 36. Protocol frame

Protocol Frame is one bounded length-delimited control message.

A protocol frame is distinct from raw application bytes after DataHello.

## 37. Preface

Preface is the fixed beginning identifying Eggtunnel protocol traffic and version/profile before normal frame decoding.

The exact byte shape is defined by M001 and recorded in protocol documentation/tests.

## 38. Generation

Generation is a monotonically changing or otherwise unique incarnation identifier used to prevent stale work from attaching to replacement state.

SessionId is the primary session generation boundary in the initial architecture.

## 39. Downstream consumer

A Downstream Consumer is another Rust application embedding the eggtunnel library.

CodeGG is the reference downstream consumer for client-only embedding qualification, but Eggtunnel MUST remain application-neutral.

## 40. Eggress boundary

Eggress is the generic networking substrate from which Eggtunnel reuses narrowly scoped published components.

Eggtunnel does not call Eggress a tunnel/session authority. Eggtunnel remains authoritative for reverse-session semantics.

## 41. Synvoid reference boundary

Synvoid is an architecture reference for QUIC/session/admission/reconnect patterns.

Synvoid internal crates are not Eggtunnel's supported dependency boundary.

## 42. i2pr reference boundary

i2pr is an architecture reference for runtime separation, bounded channels, explicit ownership, and resource accounting.

No i2pr code or dependency is part of Eggtunnel's initial implementation.

## 43. CLI process

CLI Process is the eggtunnel executable composition root.

It may:

- create a Tokio runtime through the normal async main entry point;
- initialize tracing subscriber;
- read TOML/env/CLI configuration;
- create Client or Server library objects;
- handle OS signals.

These are not library responsibilities.

## 44. Closure

Closure means a milestone's production behavior, tests, documentation, and required evidence satisfy the implementation plan and have an accepted closure record.

An implementation commit without a closure record is implemented or closing, not closed.
