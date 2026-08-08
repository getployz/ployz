//! Route Binding HTTP and deploy integration.

mod adjudication;
mod http_attach;

pub(super) use http_attach::handle_attach;
