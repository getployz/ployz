mod build;
mod configs;
mod convert;
mod extensions;
mod image;
mod loader;
mod model;
mod mounts;
mod ports;
mod secrets;

pub use build::{BuildOptions, BuildService, execute_build, plan_build};
pub(crate) use convert::duration_millis;
pub use convert::parse_normalized;
pub use loader::{LoadOptions, load_project};
pub use model::{BuildSpec, ComposeError, ComposeProject};
pub(crate) use ports::parse_extension_port;

pub(crate) fn parse_bytes(value: &str) -> Option<u64> {
    convert::bytes_u64(&serde_norway::Value::String(value.into()))
}
