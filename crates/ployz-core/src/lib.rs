#![forbid(unsafe_code)]

//! Domain models and policy for the Ployz control plane.
//!
//! This crate owns product-shaped concepts: ids, operation state, deploy
//! planning, machine models, and security role models. It must not
//! own process wiring, transport clients, Docker clients, or CLI
//! presentation.

mod api;
pub mod certificate;
pub mod corrosion;
pub mod deploy;
pub mod founding;
pub mod ids;
pub mod image;
pub mod ingress;
pub mod install;
pub mod intent;
pub mod join;
pub mod machine;
pub mod namespace;
pub mod network;
pub mod operation;
pub mod placement;
pub mod roles;
pub(crate) mod state_key;
pub mod storage;
pub(crate) mod wire;

pub use api::*;
