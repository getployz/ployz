//! Explicit machine bootstrap, join, and substrate lifecycle flows.

pub mod assigned_substrate;
pub mod cloud_bootstrap;
mod dispatch;
pub(crate) mod founder_bootstrap;
mod joiner_bootstrap;
pub mod machine_join;
pub(crate) mod substrate_update;

pub(crate) use dispatch::run_bootstrap_command;
