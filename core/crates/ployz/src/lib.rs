// Human text goes through `ui` so `--json` keeps stdout parseable; tests may print.
#![cfg_attr(not(test), deny(clippy::print_stdout))]

mod approval;
pub mod build;
mod cancellation;
mod checkin;
pub mod cli;
mod cloud_account;
mod cloud_enroll;
mod cloud_login;
mod cloud_origin;
mod cluster;
mod cluster_teardown;
mod config_dir;
pub mod connect;
pub mod context;
pub mod deploy;
pub mod dns;
pub mod drain;
pub mod enrollment;
pub mod env;
pub mod failure;
mod global_catch_up;
#[cfg(test)]
mod global_fixtures;
mod global_slot;
pub mod handlers;
pub mod image;
pub mod ingress;
pub mod namespace;
pub mod operator;
pub mod private_access;
mod provisioning;
mod published;
pub mod sdk;
mod sealed_compat;
pub mod sealing_backfill;
mod sealing_pin;
pub mod service;
mod setup_report;
mod setup_retry;
pub mod ui;
mod volume_recovery;
// Shared Machine transport fixtures also exercise the in-process CLI handlers.
#[cfg(test)]
extern crate self as ployz;
