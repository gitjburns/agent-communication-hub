//! Shared library for the Agent Communication Hub.
//!
//! Everything both binaries must agree on at compile time lives here:
//! configuration and roster loading, service-log initialization, the
//! audit-log writer, the envelope schema, and the wire line types. All logic in this
//! library is synchronous and reviewable without a Tokio runtime or network
//! access; async exists only in the server binary's network layer.

pub mod audit;
pub mod config;
pub mod envelope;
pub mod roster;
pub mod service_log;
pub mod wire;
