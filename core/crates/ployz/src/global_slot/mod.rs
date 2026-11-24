//! A Server's copies, names and own view, and removing its Ingress Proxy. Catch-up and
//! Drain both read Servers through this module, so these are written once.

mod lifecycle;
mod observe;
mod slot;

pub(crate) use lifecycle::{participating, retire_ingress_proxy};
pub(crate) use observe::{Live, ServerNames};
pub(crate) use slot::copies_on;
