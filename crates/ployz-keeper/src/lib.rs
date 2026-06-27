#![forbid(unsafe_code)]

//! Machine-local substrate bootstrap installer.
//!
//! Keeper owns local artifact installation, supervisor unit planning, and join
//! material storage. It does not own product truth.

pub mod artifacts;
pub mod cli;
pub mod cloud_bootstrap;
pub mod command;
pub mod executor;
pub mod fsx;
pub mod join;
pub mod join_executor;
pub mod local;
pub mod nats_identity;
pub mod report;
pub mod steps;
pub mod systemd;
