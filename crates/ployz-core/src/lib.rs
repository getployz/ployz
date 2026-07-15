#![forbid(unsafe_code)]

//! Domain models and policy for the Ployz control plane.
//!
//! This crate owns product-shaped concepts: ids, operation state, deploy
//! planning, machine models, and security role models. It must not
//! own process wiring, TLS NATS connections, Docker clients, or CLI
//! presentation.

pub mod cert;
pub mod certificate;
pub mod dataplane;
pub mod deploy;
pub mod ids;
pub mod image;
pub mod ingress;
pub mod install;
pub mod intent;
pub mod internal_dns;
pub mod machine;
pub mod machine_rpc;
pub mod machine_runtime;
pub mod nats_config;
pub mod network;
pub mod operation;
pub mod ops;
pub mod reachability;
pub mod roles;
pub mod security;
pub mod state;
pub(crate) mod state_key;
pub(crate) mod wire;
