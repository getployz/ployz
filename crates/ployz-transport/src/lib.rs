#![forbid(unsafe_code)]

//! Private transport adapters.
//!
//! This crate owns iroh endpoint identity and byte tunnels used to carry NATS
//! client connections. It must not expose product commands; product commands
//! go through NATS services.

pub mod iroh_endpoint;
pub mod nats_tunnel;
