//! Ployz Cloud's uptime checker: probes users' public endpoints from outside their servers and
//! records what each region saw. The control plane owns state and incidents; regional agents only
//! probe.

pub mod control;
pub mod domain;
