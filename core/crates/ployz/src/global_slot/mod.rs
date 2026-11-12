//! A Global's slot on one Machine: which copies it holds, how a whole-Cluster observation
//! of it is read, and how a copy is retired from it. Catch-up and Drain both read slots
//! through this module, so a Server's eligibility, its names and its refusals are written
//! once.

mod lifecycle;
mod observe;
mod slot;

#[cfg(test)]
pub(crate) use lifecycle::retire_copies;
pub(crate) use lifecycle::{
    Eligibility, Sentence, participating, retire_if_ineligible, retire_ingress_proxy,
};
pub(crate) use observe::{Live, ServerNames};
pub(crate) use slot::copies_on;
