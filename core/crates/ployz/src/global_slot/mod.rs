//! A Global's slot on one Machine: which copies it holds, how a whole-Cluster observation
//! of it is read, and how catch-up retires a copy from it. Catch-up and Drain both read
//! Servers through this module, so their names and copies are written once. A Drain
//! retires no Global.

mod lifecycle;
mod observe;
mod slot;

pub(crate) use lifecycle::{
    Eligibility, participating, retire_if_ineligible, retire_ingress_proxy, retirement_error,
};
pub(crate) use observe::{Live, ServerNames};
pub(crate) use slot::copies_on;
