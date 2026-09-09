mod build;
mod build_inputs;
mod capture;
mod convert;
mod loader;
mod model;
mod ports;
mod remote_build;
mod secrets;

pub use build::{
    BuildOptions, BuildService, BuiltService, CapturedBuild, capture_build, execute_build,
    plan_build,
};
pub use capture::{CapturedCompose, ComposeSource};
pub(crate) use convert::duration_millis;
pub use loader::parse_normalized;
pub use loader::{LoadOptions, load_project};
pub(crate) use loader::{compose_identity, has_explicit_nondefault_compose_file};
pub use model::{BuildSpec, ComposeError, ComposeProject};
pub(crate) use ports::parse_extension_port;

pub(crate) fn parse_bytes(value: &str) -> Option<u64> {
    convert::bytes_u64(&serde_norway::Value::String(value.into()))
}
