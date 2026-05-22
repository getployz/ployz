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
pub(crate) mod adapters;
pub mod composition;
pub mod deploy;
pub mod domain;
pub mod error;
pub mod facts;
pub mod machine;
pub mod operation;
pub mod runtime;
pub mod serving;
pub mod volume;

pub use error::{Error, PrimitiveFailure, Result};
