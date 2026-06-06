#![forbid(unsafe_code)]

//! Node-local substrate installer and updater.
//!
//! Keeper owns local artifact installation, supervisor unit planning, role
//! restarts, and health gates. It does not own product truth.

pub mod artifacts;
pub mod cli;
pub mod health;
pub mod join;
pub mod steps;
pub mod systemd;
