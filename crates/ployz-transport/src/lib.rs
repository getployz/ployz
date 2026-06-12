#![forbid(unsafe_code)]

//! Future transport adapters if private connectivity returns.
//!
//! v1 machine connectivity is a direct TLS-authenticated NATS connection
//! (ADR-0013). This crate is intentionally empty: it reserves the seam for
//! private overlay transports without exposing product commands. Product
//! commands go through NATS services.
