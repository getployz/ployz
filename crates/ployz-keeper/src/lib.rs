#![forbid(unsafe_code)]

//! Node-local substrate bootstrap installer.
//!
//! Keeper owns local artifact installation, supervisor unit planning, and join
//! material storage. It does not own product truth.

pub mod artifacts;
pub mod cli;
pub mod join;
pub mod steps;
pub mod systemd;
