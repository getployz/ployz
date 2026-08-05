//! HTTP/1 API role wiring over the machine mesh.
//!
//! The public API derives a caller only from an accepted TCP peer address.
//! Corrosion remains a machine-local dependency configured through the role
//! environment file rather than an API credential supplied by callers.

mod config;
mod door;
mod endpoint_network;
mod founding;
mod join;
mod mutations;
mod roster;
mod runtime;
mod server;
mod store;
mod upgrade;

pub use config::{
    ApiRoleConfig, ApiRoleConfigError, ApiRoleMode, BootstrapSecret, DoorListenAddress,
    DoorMaterialPaths,
};
pub use roster::ApiListenerValidationError;
pub use runtime::{
    ApiRoleRuntimeError, ApiServer, ApiServerError, ApiServerServeError, run_from_environment,
};

#[cfg(test)]
#[path = "http/tests.rs"]
mod tests;
