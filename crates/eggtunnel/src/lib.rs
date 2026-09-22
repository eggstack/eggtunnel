#![forbid(unsafe_code)]
//! Embeddable Eggtunnel reverse-session library.
//!
//! Runtime and transport behavior is implemented in later milestones. This
//! crate currently exposes the feature boundary and protocol dependency only.

pub use eggtunnel_proto as proto;
