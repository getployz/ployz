#![forbid(unsafe_code)]

//! Domain models and policy for the Ployz control plane.
//!
//! This crate owns product-shaped concepts: ids, operation state, deploy
//! planning, node models, subject names, and security role models. It must not
//! own process wiring, NATS clients, iroh endpoints, Docker clients, or CLI
//! presentation.

pub mod deploy;
pub mod ids;
pub mod node;
pub mod ops;
pub mod security;
pub mod state;
pub mod subjects;
pub mod time;
