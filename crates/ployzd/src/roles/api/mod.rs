//! Transport-free machine-local execution mechanics owned by the API fold.
//!
//! This module owns effects only. It does not own Corrosion row reads or
//! command admission.

pub mod execution;
pub mod http;
pub(crate) mod keeper_control;
pub mod lenses;
pub mod runner;
