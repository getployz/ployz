//! HTTP/1 API role wiring over the machine mesh.
//!
//! The public API derives a caller only from an accepted TCP peer address.
//! Corrosion remains a machine-local dependency configured through the role
//! environment file rather than an API credential supplied by callers.

mod config;
mod controller;
mod controller_forwarding;
mod controller_kernel;
mod deploy_controller;
mod deploy_effect_http;
mod deploy_effects;
mod deploy_hosts;
mod diagnostics;
mod door;
mod endpoint_network;
mod founding;
mod join;
mod machine_endpoint_reporter;
mod mutations;
mod namespace_store;
mod node_workflows;
mod removals;
mod roster;
mod routes;
mod runtime;
mod server;
mod service_logs;
mod simple_deploy;
mod simple_deploy_store;
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
