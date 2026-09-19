//! Validate and compare resource-owned settings without exposing sealed values.

use serde_json::{Value, json};

use super::service_changes::at;
use super::*;

/// Decode a node configuration according to its owner family.
///
/// # Errors
/// Returns ConfigError for an invalid shape, unsupported version, or invalid sealed-value evidence.
pub fn parse_resource_config(
    node_type: EnvironmentNodeType,
    value: Value,
) -> Result<Value, ConfigError> {
    match node_type {
        EnvironmentNodeType::Service => Ok(json!(parse_service_config(value)?)),
        EnvironmentNodeType::Volume => {
            let config: VolumeConfig = serde_json::from_value(value)
                .map_err(|_| ConfigError::at("volume", "Invalid volume configuration"))?;
            if config.version != 2 {
                return Err(ConfigError::at("volume.version", "Expected version 2"));
            }
            Ok(json!(config))
        }
    }
}

/// Compare resource-owned settings while keeping sealed contents out of review.
///
/// # Errors
/// Returns ConfigError when either supplied configuration is invalid for the requested node family.
pub fn compare_resource_settings(
    node_type: EnvironmentNodeType,
    current: Value,
    baseline: Option<Value>,
) -> Result<Vec<ServiceSettingChange>, ConfigError> {
    if node_type == EnvironmentNodeType::Service {
        let current = parse_service_config(current)?;
        let baseline = baseline.map(parse_service_config).transpose()?;
        return Ok(compare_service_settings(&current, baseline.as_ref()));
    }
    let current = parse_resource_config(node_type, current)?;
    let baseline = baseline
        .map(|v| parse_resource_config(node_type, v))
        .transpose()?;
    let mut changes = Vec::new();
    if let Some(baseline) = &baseline {
        if at(&current, "name") != at(baseline, "name") {
            changes.push(resource_change(
                "name".into(),
                at(baseline, "name").clone(),
                at(&current, "name").clone(),
                false,
            ));
        }
    } else {
        changes.push(resource_change(
            "node".into(),
            Value::Null,
            at(&current, "name").clone(),
            true,
        ));
    }
    Ok(changes)
}

fn resource_change(
    path: String,
    before: Value,
    after: Value,
    can_restore: bool,
) -> ServiceSettingChange {
    ServiceSettingChange {
        kind: if before.is_null() {
            ChangeKind::Add
        } else if after.is_null() {
            ChangeKind::Remove
        } else {
            ChangeKind::Update
        },
        path,
        before,
        after,
        can_restore,
    }
}
