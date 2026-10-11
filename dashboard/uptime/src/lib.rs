//! Ployz Cloud's uptime checker: probes users' public endpoints from outside their servers and
//! records what each region saw. The control plane owns state and incidents; regional agents only
//! probe.

pub mod agent;
pub mod config;
pub mod control;
pub mod domain;
pub mod link;
pub mod run;

#[cfg(test)]
#[path = "e2e_tests.rs"]
mod e2e_tests;
