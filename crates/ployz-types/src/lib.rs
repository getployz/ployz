//! Shared model, spec, time, and error types used across the workspace.

pub mod error;
pub mod model;
pub mod spec;
pub mod time;

pub use error::{Error, Result};
