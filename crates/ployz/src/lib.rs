mod cancellation;
pub mod changes;
pub mod cli;
mod cloud_enroll;
mod cluster;
mod cluster_teardown;
pub mod compose;
pub mod connect;
pub mod context;
pub mod deploy;
pub mod dns;
pub mod failure;
mod global_catch_up;
pub mod handlers;
pub mod image;
pub mod ingress;
pub mod operator;
pub mod project;
mod provisioning;
pub mod sdk;
pub mod service;
mod setup_retry;
pub mod volume;
// Shared Machine transport fixtures also exercise the in-process CLI handlers.
#[cfg(test)]
extern crate self as ployz;
