//! Explicit machine bootstrap, join, and substrate lifecycle flows.

pub(crate) mod assigned_substrate;
pub(crate) mod cloud_bootstrap;
mod dispatch;
pub(crate) mod founder_bootstrap;
pub(crate) mod installed_substrate;
mod joiner_bootstrap;
pub(crate) mod machine_join;
pub(crate) mod substrate_update;

#[cfg(test)]
pub use assigned_substrate::*;
#[cfg(test)]
pub use cloud_bootstrap::*;
pub(crate) use dispatch::run_bootstrap_command;
#[cfg(test)]
pub use machine_join::*;
