use ployz_core::PortPublication;

use super::{loader::helper, model::ComposeError};

pub(crate) fn parse_extension_port(value: &str) -> Result<PortPublication, ComposeError> {
    helper(&serde_json::json!({"version": 1, "port": value}))
}
