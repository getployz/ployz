#![forbid(unsafe_code)]

//! Domain models and policy for the Ployz control plane.
//!
//! This crate owns product-shaped concepts: ids, operation state, deploy
//! planning, node models, subject names, and security role models. It must not
//! own process wiring, NATS clients, iroh endpoints, Docker clients, or CLI
//! presentation.

pub mod backup;
pub mod cert;
pub mod dataplane;
pub mod deploy;
pub mod ha;
pub mod ids;
pub mod install;
pub mod machine;
pub mod node;
pub mod ops;
pub mod roles;
pub mod security;
pub mod state;
pub mod subjects;
pub mod time;
pub(crate) mod wire;
