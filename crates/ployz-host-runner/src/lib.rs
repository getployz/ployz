#![forbid(unsafe_code)]

//! Privileged machine-local mechanics.
//!
//! This crate owns bounded host effects and local artifact staging. It has no
//! control-plane transport and does not own cluster truth.

pub mod builtin_wireguard;
pub mod container_isolation;
mod execution;
pub mod lifecycle;
mod release_manifest;

pub use execution::*;
pub use release_manifest::*;

#[cfg(test)]
mod tests;
