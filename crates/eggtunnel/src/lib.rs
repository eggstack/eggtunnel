#![forbid(unsafe_code)]
//! Embeddable Eggtunnel reverse-session library.
//!
//! The library uses the caller's Tokio runtime and does not install global
//! runtime or tracing state. Optional transports remain Cargo-feature gated.

mod common;
#[cfg(feature = "mtls")]
mod pem;
#[cfg(any(feature = "client", feature = "server"))]
mod wire_io;

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "server")]
mod server;

#[cfg(all(feature = "client", feature = "mtls"))]
pub use client::ClientIdentity;
#[cfg(feature = "outbound-proxy")]
pub use client::validate_outbound_proxy;
#[cfg(feature = "client")]
pub use client::{
    ApplicationStream, Client, ClientConfig, ClientHandle, TargetConnector, TargetContext,
    TargetError, TargetFuture, TargetStream,
};
pub use common::{
    BindPolicy, ClientService, ResourceLimits, SecretToken, ServiceSpec, Snapshot,
    TerminationCategory, TunnelError,
};
pub use eggtunnel_proto as proto;
#[cfg(feature = "server")]
pub use server::{Server, ServerConfig, ServerHandle};
