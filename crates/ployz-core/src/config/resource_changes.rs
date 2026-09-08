//! Validate and compare resource-owned settings without exposing sealed values.

use std::collections::BTreeMap;

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
        EnvironmentNodeType::VariableGroup => {
            let config: VariableGroupConfig = serde_json::from_value(value).map_err(|_| {
                ConfigError::at("variableGroup", "Invalid variable group configuration")
            })?;
            if config.version != 1 {
                return Err(ConfigError::at(
                    "variableGroup.version",
                    "Expected version 1",
                ));
            }
            for variable in &config.variables {
                if let VariableGroupConfigValue::Sealed {
                    has_value,
                    fingerprint,
                    encrypted_value,
                } = &variable.value
                    && (!has_value
                        || fingerprint.is_empty()
                        || encrypted_value.as_ref().is_some_and(|v| v.version != 1))
                {
                    return Err(ConfigError::at(
                        "variables.value",
                        "Invalid sealed variable",
                    ));
                }
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
    if node_type == EnvironmentNodeType::VariableGroup {
        let variables = |value: &Value| -> BTreeMap<String, Value> {
            at(value, "variables")
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| {
                    let mut comparable = v.clone();
                    comparable
                        .as_object_mut()
                        .expect("parsed variable")
                        .remove("key");
                    if let Some(value) = comparable.get_mut("value").and_then(Value::as_object_mut)
                    {
                        value.remove("encryptedValue");
                    }
                    (
                        at(v, "key").as_str().expect("parsed key").into(),
                        comparable,
                    )
                })
                .collect()
        };
        let before = variables(baseline.as_ref().unwrap_or(&Value::Null));
        let after = variables(&current);
        let keys: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
        for key in keys {
            if before.get(key) == after.get(key) {
                continue;
            }
            let display = |v: Option<&Value>| match v {
                None => Value::Null,
                Some(v) if at(v, "value.type") == "sealed" => json!({"kind":"secret"}),
                Some(v) => at(v, "value.value").clone(),
            };
            changes.push(resource_change(
                format!("variables.{key}"),
                display(before.get(key)),
                display(after.get(key)),
                false,
            ));
        }
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
        derived_from: None,
    }
}
