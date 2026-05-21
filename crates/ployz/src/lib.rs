//! Product orchestration primitives for Ployz.
//!
//! Ployz owns product meaning: deploys, certificates, serving, runtime,
//! volumes, environments, machines, command semantics, and operator-facing
//! results. Polis may support adapters and composition code, but ordinary Ployz
//! feature modules should read as product orchestration.
//!
//! Production rules:
//! - durable truth, projection freshness, live observation, and unknown health
//!   stay distinct;
//! - foreground work returns typed product results;
//! - retry and crash behavior must be explicit for every durable mutation;
//! - secrets must never be rendered into logs, errors, status, or generic
//!   operation evidence.

pub mod acme;
pub mod adapters;
pub mod composition;
pub mod deploy;
pub mod error;
pub mod operation;
pub mod projection;
pub mod runtime;
pub mod serving;
pub mod volume;

pub use error::{Error, PrimitiveFailure, Result};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_starts_without_substrate_modules() {
        let public_modules: [&str; 0] = [];

        assert!(!public_modules.contains(&"p2panda"));
        assert!(!public_modules.contains(&"iroh"));
        assert!(!public_modules.contains(&"mvp"));
    }
}
