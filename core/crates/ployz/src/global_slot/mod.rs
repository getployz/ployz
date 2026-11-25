//! A Global's slot on one Machine: how a whole-Cluster observation of it is read, and how
//! catch-up retires a copy from it. A Drain retires no Global.

mod lifecycle;
mod observe;
mod slot;

pub(crate) use lifecycle::{
    Eligibility, participating, retire_if_ineligible, retire_ingress_proxy, retirement_error,
};
pub(crate) use observe::{Live, ServerNames};
