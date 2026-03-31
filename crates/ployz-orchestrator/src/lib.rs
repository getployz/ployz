//! Core orchestration logic for mesh lifecycle, deploy planning, and runtime coordination.
//!
//! This crate is the orchestration kernel. Binaries and adapters should depend
//! on its public seams rather than re-implementing policy.

pub(crate) use ployz_types::error;
pub(crate) use ployz_types::model;
pub(crate) use ployz_types::time;

pub mod deploy;
pub mod ipam;
pub mod machine_liveness;
pub mod mesh;

pub use mesh::orchestrator::{Mesh, MeshError};
pub use mesh::phase::Phase;
