//! SDK surface for daemon requests and transport implementations.
//!
//! This crate exposes a small client API that higher-level CLIs or agents can
//! use to talk to a running `ployzd` instance.

mod client;
pub mod transport;

pub use client::DaemonClient;
pub use transport::{StdioTransport, Transport, UnixSocketTransport};
