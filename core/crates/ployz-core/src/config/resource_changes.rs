//! Validate and compare resource-owned settings without exposing sealed values.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::ConfigFileName;

use super::service_changes::{at, change};
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
        EnvironmentNodeType::Config => {
            let config: ConfigNodeConfig = serde_json::from_value(value)
                .map_err(|_| ConfigError::at("config", "Invalid Config configuration"))?;
            if config.version != 1 {
                return Err(ConfigError::at("config.version", "Expected version 1"));
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
) -> Result<Vec<(ServiceSettingChange, Option<At>)>, ConfigError> {
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
        for (path, row) in [("name", At::Name), ("storage", At::Storage)] {
            if at(&current, path) != at(baseline, path) {
                changes.push(change(
                    (path, Some(row)),
                    at(baseline, path).clone(),
                    at(&current, path).clone(),
                    true,
                ));
            }
        }
        if node_type == EnvironmentNodeType::Config {
            let files = |config: &Value| config.get("files").cloned().unwrap_or(Value::Null);
            let (now, then) = (files(&current), files(baseline));
            let names: BTreeSet<&String> = [&now, &then]
                .into_iter()
                .filter_map(Value::as_object)
                .flat_map(serde_json::Map::keys)
                .collect();
            for name in names {
                let entry =
                    |files: &Value| files.get(name.as_str()).cloned().unwrap_or(Value::Null);
                let (before, after) = (entry(&then), entry(&now));
                if before != after {
                    let file = ConfigFileName::parse(name.clone())
                        .map_err(|_| ConfigError::at("config.files", "Invalid Config file name"))?;
                    changes.push(change(
                        (&format!("files.{name}"), Some(At::File(file))),
                        before,
                        after,
                        true,
                    ));
                }
            }
        }
    } else {
        changes.push(change(
            ("node", Some(At::Node)),
            Value::Null,
            at(&current, "name").clone(),
            true,
        ));
    }
    Ok(changes)
}
