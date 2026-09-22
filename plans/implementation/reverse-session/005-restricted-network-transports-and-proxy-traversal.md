# Reverse Session M005 — Restricted-Network Transports and Proxy Traversal

Status: closed

Planning baseline: 31458e83e543304d6b271898575bf2f6e98c7352

M003 is closed at the baseline above. Execution is sequenced after M004 to keep the optional transport change sets isolated.

Source roadmap:

- plans/subsystems/reverse-session-roadmap.md#10-milestone-m005--restricted-network-transports-and-outbound-proxy-traversal

Applicable ADR:

- plans/adrs/ADR-0001-session-transport-and-egress-boundary.md

Primary class: capability

## 1. Objective

Add opt-in connectivity profiles for networks where direct TCP/TLS or QUIC is unavailable:

- WebSocket/WSS transport using Eggress's WebSocket stream adapter;
- outbound HTTP CONNECT/SOCKS/proxy-chain traversal using eggress-outbound.

These are connectivity adapters. They MUST NOT create a second Session/Service protocol or change downstream application semantics.

## 2. Dependency readiness

Hard dependency: M003 strict closure.

M004 QUIC is not a hard dependency. M004 and M005 may execute in parallel after M003 if they preserve the shared transport abstraction.

Before execution:

- update baseline to M003 final reviewed head;
- inspect actual transport abstraction;
- verify current eggress-protocol-websocket and eggress-outbound published APIs;
- identify the minimal feature/dependency slice for each adapter.

## 3. Invariants

- websocket and outbound-proxy are opt-in features.
- client+TLS minimal build remains free of them.
- WSS never downgrades an otherwise secure remote profile to plaintext.
- proxy credentials are redacted.
- authentication/bind policy remains Eggtunnel-owned.
- outbound proxy routing cannot cause Server-directed arbitrary local Target selection.
- no unbounded WebSocket message queue/mux.
- no application payload inspection.
- transport/proxy errors remain typed enough for reconnect policy.

## 4. WebSocket/WSS scope

Prefer eggress-protocol-websocket's async stream adapter.

The adapter should present a stream-like boundary to the existing Eggtunnel transport/session layer.

Requirements:

- WSS for non-loopback production use;
- bounded WebSocket message size;
- non-browser tunnel semantics documented;
- Ping/Pong ownership conflict reviewed so Eggtunnel protocol liveness does not create pathological duplicate mechanisms;
- close/EOF propagates to Session cancellation;
- backpressure preserved;
- no Origin-based browser security claim unless explicitly implemented.

If a baseline TCP/TLS Data Connection model is used over WebSocket, document the exact control/data connection mapping. Do not silently add a custom multiplexed message channel.

## 5. Outbound proxy traversal scope

Use eggress-outbound listener-free connectors.

Support only upstream modes proven by the current published API, expected initially to include:

- direct;
- HTTP CONNECT;
- SOCKS5;
- supported explicit multi-hop chains where configuration is unambiguous.

Do not start a local proxy listener to implement outbound traversal.

Requirements:

- proxy dial happens before Eggtunnel TLS;
- end-to-end Eggtunnel TLS still authenticates the actual Eggtunnel Server;
- proxy cannot terminate/replace Eggtunnel authentication without explicit unsupported MITM behavior;
- proxy auth secrets redacted;
- connect/handshake timeouts bounded;
- failure stage classified (proxy hop vs Eggtunnel TLS vs protocol auth).

## 6. Configuration model

Add transport/upstream configuration without ambiguous combinations.

Examples of conceptual fields:

- transport = tcp-tls | websocket-tls;
- server endpoint;
- optional outbound chain;
- TLS server name;
- proxy auth secret reference.

Validation must reject:

- wss requested without TLS support;
- insecure ws to non-loopback unless explicitly test-only;
- invalid/unsupported proxy schemes;
- incompatible proxy+QUIC combinations if QUIC traversal is not implemented;
- duplicate/conflicting TLS ownership;
- missing server-name verification.

Avoid accepting a free-form URL that silently changes multiple security policies unless parsing is strict and diagnostics are explicit.

## 7. Error and reconnect semantics

Proxy/WebSocket failures feed the existing reconnect controller but retain stage/category:

- DNS;
- proxy connect;
- proxy auth;
- proxy handshake;
- WebSocket handshake;
- TLS;
- Eggtunnel protocol auth;
- policy;
- cancellation.

Non-retryable configuration/auth errors must not loop indefinitely.

Transient transport failures may reconnect with the existing bounded policy.

## 8. Ordered work packages

A. Outbound connector integration
- feature gate;
- config;
- direct/HTTP/SOCKS fixtures;
- failure taxonomy.

B. WebSocket/WSS adapter
- feature gate;
- control/data stream composition;
- close/backpressure/message bounds.

C. Security/config convergence
- reject insecure/ambiguous combos;
- secret redaction;
- end-to-end TLS verification through proxy.

D. Lifecycle/equivalence
- reconnect;
- cancellation;
- existing Session/Service suite through new adapters.

E. Dependency/docs
- feature-off cargo tree;
- configuration/security/operations docs.

## 9. Required tests

Outbound proxy:
- direct path unchanged;
- HTTP CONNECT success;
- HTTP CONNECT auth success/failure if supported;
- SOCKS5 success;
- SOCKS5 auth success/failure if supported;
- multi-hop chain only if supported/selected;
- proxy refused/unreachable/timeout;
- proxy closes during Eggtunnel TLS;
- wrong Eggtunnel certificate through successful proxy;
- cancellation during each hop.

WebSocket:
- WSS session/auth/register;
- one and concurrent data paths;
- message-size limit;
- close/EOF;
- handshake failure;
- backpressure/flush;
- reconnect;
- non-TLS non-loopback rejection.

Feature matrix:
- client+tls excludes WebSocket/outbound dependencies;
- websocket feature excludes QUIC unless explicitly selected;
- outbound-proxy does not pull full Eggress service runtime.

## 10. Verification

Run full workspace verification plus focused feature slices for:

- websocket;
- outbound-proxy;
- websocket+outbound-proxy if supported;
- minimal client+tls;
- server+tls.

Use deterministic local proxy fixtures; do not make closure depend on public internet proxy services.

## 11. Documentation

Update:

- docs/CONFIGURATION.md;
- docs/SECURITY.md;
- docs/ARCHITECTURE.md;
- docs/OPERATIONS.md;
- docs/EMBEDDING.md;
- README examples;
- support matrix;
- subsystem roadmap/registry.

Clearly label which combinations are implemented and tested.

## 12. Acceptance criteria

- WSS provides the same logical Eggtunnel Session/Service behavior.
- supported outbound proxies establish end-to-end authenticated Eggtunnel TLS.
- unsupported/insecure combinations fail during validation rather than silently downgrading.
- proxy/WebSocket secrets are redacted.
- reconnect classifies retryable vs non-retryable failures correctly.
- minimal client dependency slice remains clean.
- no custom mux or local proxy sidecar is introduced.
- all required tests pass.

## 13. Stop conditions

Stop and report if:

- Eggress WebSocket adapter requires browser-oriented semantics incompatible with the tunnel product;
- eggress-outbound cannot provide required listener-free chaining without broad runtime adoption;
- secure TLS layering becomes ambiguous;
- QUIC proxy traversal is needed to claim success;
- a custom WebSocket multiplexing protocol appears necessary.

## 14. Closure evidence required

- refreshed baseline/final head;
- exact Eggress crate versions/APIs;
- supported transport/proxy matrix;
- end-to-end TLS-through-proxy evidence;
- WSS close/backpressure evidence;
- feature/dependency tree evidence;
- exact tests/commands/results;
- known unsupported combinations;
- closure disposition.
