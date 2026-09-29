//! Pure authored configuration rules used by native clients and Cloud's canvas.

mod branch_changes;
mod branch_changes_types;
mod branch_plan;
mod change_set;
mod change_set_types;
mod environment;
mod environment_compile;
mod environment_restore;
mod live_values;
mod lowering;
mod resource_changes;
mod runtime_outcome;
mod service;
mod service_changes;
mod validation;
mod variables;

pub use branch_changes::*;
pub use branch_changes_types::*;
pub use branch_plan::*;
pub use change_set::*;
pub use change_set_types::*;
pub use environment::*;
pub use environment_compile::*;
pub use environment_restore::*;
pub use live_values::*;
pub use lowering::*;
pub use resource_changes::*;
pub use runtime_outcome::*;
pub use service::*;
pub use service_changes::*;
pub use validation::*;
pub use variables::*;

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigRequest {
    ParseSetting { value: serde_json::Value },
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
    }
}
