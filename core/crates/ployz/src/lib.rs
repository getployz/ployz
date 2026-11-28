// Output goes through `ui`, so `--json` keeps stdout parseable and URL user info is
// masked; tests may print.
#![cfg_attr(
    not(test),
    deny(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)
)]

mod agent_call;
mod approval;
mod atomic_file;
pub mod build;
mod cancellation;
pub mod cli;
mod cloud;
mod cluster;
mod cluster_teardown;
mod coded;
mod config_dir;
pub mod connect;
pub mod context;
pub mod deploy;
mod device;
pub(crate) mod dns;
pub mod drain;
pub mod enrollment;
pub mod env;
pub(crate) mod failure;
mod first_key_line;
mod global_catch_up;
#[cfg(test)]
mod global_fixtures;
mod global_slot;
pub mod handlers;
pub(crate) mod image;
pub use image::{PinError, pin_public};
pub(crate) mod ingress;
pub mod namespace;
pub mod operator;
pub(crate) mod private_access;
mod provisioning;
mod published;
mod scope;
pub mod sdk;
mod sealed_compat;
pub mod sealing_backfill;
mod sealing_pin;
mod setup_report;
mod setup_retry;
pub mod ui;
mod volume_recovery;
// Shared Machine transport fixtures also exercise the in-process CLI handlers.
#[cfg(test)]
extern crate self as ployz;
