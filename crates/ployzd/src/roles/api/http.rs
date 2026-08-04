//! HTTP/1 API role wiring over the machine mesh.
//!
//! The public API derives a caller only from an accepted TCP peer address.
//! Corrosion remains a machine-local dependency configured through the role
//! environment file rather than an API credential supplied by callers.

mod config;
mod founding;
mod roster;
mod server;

pub use config::{ApiRoleConfig, ApiRoleConfigError, ApiRoleMode, BootstrapSecret};
pub use roster::ApiListenerValidationError;
pub use server::{
    ApiRoleRuntimeError, ApiServer, ApiServerError, ApiServerServeError, run_from_environment,
};

#[cfg(test)]
#[path = "http/tests.rs"]
mod tests;
