mod build;
mod configs;
mod convert;
mod extensions;
mod image;
mod loader;
mod model;
mod mounts;
mod planning;
mod ports;
mod secrets;

pub use build::{BuildOptions, BuildService, execute_build, plan_build};
pub use convert::parse_normalized;
pub use loader::{LoadOptions, load_project};
pub use model::{BuildSpec, ComposeError, ComposeProject};
pub use planning::{ComposeDeployPlan, ComposePlanError, plan_compose_deploy};
pub(crate) use ports::parse_extension_port;

pub(crate) fn parse_bytes(value: &str) -> Option<u64> {
    convert::bytes_u64(&serde_norway::Value::String(value.into()))
}
