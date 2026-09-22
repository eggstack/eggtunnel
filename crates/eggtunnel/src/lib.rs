#![forbid(unsafe_code)]
//! Embeddable Eggtunnel reverse-session library.
//!
//! The library uses the caller's Tokio runtime and does not install global
//! runtime or tracing state. Optional transports remain Cargo-feature gated.

mod common;
#[cfg(any(feature = "client", feature = "server"))]
mod wire_io;

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "server")]
mod server;

#[cfg(feature = "client")]
pub use client::{Client, ClientConfig, ClientHandle};
pub use common::{ClientService, SecretToken, ServiceSpec, Snapshot, TunnelError};
pub use eggtunnel_proto as proto;
#[cfg(feature = "server")]
pub use server::{Server, ServerConfig, ServerHandle};
