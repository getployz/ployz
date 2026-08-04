//! Shared concrete adapter for the machine-local Corrosion sidecar.

mod claim;
mod client;
mod stored_rows;
mod stream;

pub use claim::*;
pub use client::*;
pub(crate) use stored_rows::*;
