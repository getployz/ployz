//! Pure authored configuration rules used by native clients and Cloud's canvas.

mod branch_plan;
mod change_set;
mod change_set_types;
mod env_file;
mod environment;
mod environment_compile;
mod generated;
mod live_values;
mod lowering;
mod resource_changes;
mod runtime_outcome;
mod service;
mod service_changes;
mod start_command;
mod sync;
mod validation;
mod variables;

pub use crate::sealed::ClientSealedValue;
pub use branch_plan::*;
pub use change_set::*;
pub use change_set_types::*;
pub use env_file::*;
pub use environment::*;
pub use environment_compile::*;
pub use generated::*;
pub use live_values::*;
pub use lowering::*;
pub use resource_changes::*;
pub use runtime_outcome::*;
pub use service::*;
pub use service_changes::*;
pub use start_command::*;
pub use sync::*;
pub use validation::*;
pub use variables::*;

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigRequest {
    ParseSetting { value: serde_json::Value },
    ParseEnvFile { text: String },
    ParseRegion { value: String },
    ParseRegionName { value: String },
    FirstServerRegion { servers: Vec<ServerRegion> },
}

/// A Server's Machine Subnet, when its join was approved and its `region` label value, as a
/// caller sees them.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerRegion {
    subnet: crate::MachineSubnet,
    #[serde(default)]
    joined: Option<crate::authority::Timestamp>,
    region: Option<String>,
}

/// The JSON ABI behind the SDK's WASM config export; it validates before policy.
///
/// # Errors
/// Returns ConfigError for an unknown request or a Setting value core refuses.
/// Messages identify the rejected setting without echoing its value.
pub fn config_request(input: serde_json::Value) -> Result<serde_json::Value, ConfigError> {
    let input = serde_json::from_value(input)
        .map_err(|_| ConfigError::at("request", "Invalid configuration request"))?;
    match input {
        ConfigRequest::ParseSetting { value } => parse_service_setting(value),
        ConfigRequest::ParseEnvFile { text } => {
            Ok(serde_json::to_value(parse_env_file(&text)?).expect("env entries are JSON"))
        }
        ConfigRequest::ParseRegion { value } => crate::Region::parse(value)
            .map(|region| serde_json::Value::String(region.into()))
            .map_err(|_| {
                ConfigError::at(
                    "region",
                    "A region is up to 63 letters, digits, hyphens, underscores and dots.",
                )
            }),
        ConfigRequest::ParseRegionName { value } => crate::Region::display_name(&value)
            .map(|name| serde_json::Value::String(name.to_owned()))
            .ok_or_else(|| ConfigError::at("name", crate::Region::NAME_RULE)),
        ConfigRequest::FirstServerRegion { servers } => Ok(serde_json::to_value(
            crate::Region::of_first_server(servers.iter().map(|server| crate::JoinedServer {
                subnet: server.subnet,
                joined: server.joined,
                label: server.region.as_deref(),
            })),
        )
        .expect("a region is JSON")),
    }
}
