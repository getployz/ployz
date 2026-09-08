//! Compare service settings and restore their authored values without runtime I/O.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::{ConfigError, EnvSource, ServiceConfig, parse_service_config};
use std::collections::{BTreeMap, BTreeSet};

/// Whether an owned setting appeared, changed, or disappeared.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Add,
    Update,
    Remove,
}

/// A redacted comparison row and the authored owner of any derived effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSettingChange {
    pub path: String,
    pub kind: ChangeKind,
    pub before: Value,
    pub after: Value,
    pub can_restore: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub derived_from: Option<EnvSource>,
}

const FIELDS: &[&str] = &[
    "name",
    "source.repository",
    "source.branch",
    "source.rootDir",
    "source.autoDeploy",
    "source.waitForCi",
    "source.image",
    "source.autoUpdate",
    "source.credentials",
    "preDeployCommand",
    "startCommand",
    "healthcheck",
    "restartPolicy",
    "maxRetries",
    "cron",
    "replicas",
    "cpuLimit",
    "memLimit",
    "privateDns",
    "managedHostname",
    "build",
    "variableGroupAttachments",
];

/// Compare settings against an available authored baseline, keeping derived effects separate.
#[must_use]
pub fn compare_service_settings(
    current: &ServiceConfig,
    baseline: Option<&ServiceConfig>,
) -> Vec<ServiceSettingChange> {
    let current = json!(current);
    let baseline = baseline.map_or(Value::Null, |value| json!(value));
    let mut changes = Vec::new();
    let source_changed =
        !baseline.is_null() && at(&current, "source.type") != at(&baseline, "source.type");
    if source_changed {
        changes.push(change(
            "source",
            at(&baseline, "source").clone(),
            at(&current, "source").clone(),
            true,
        ));
    }
    for path in FIELDS {
        if source_changed && path.starts_with("source.") {
            continue;
        }
        let before = at(&baseline, path);
        let after = at(&current, path);
        let repository_changed = *path == "source.repository"
            && !before.is_null()
            && !after.is_null()
            && (at(&baseline, "source.repositoryId") != at(&current, "source.repositoryId")
                || at(&baseline, "source.installationId") != at(&current, "source.installationId"));
        if before == after && !repository_changed {
            continue;
        }
        if baseline.is_null() && *after == default_value(path) {
            continue;
        }
        changes.push(change(
            path,
            before.clone(),
            after.clone(),
            !baseline.is_null(),
        ));
    }
    changes.extend(compare_related_settings(&current, &baseline));
    changes
}

/// Restore one setting from the supplied configuration baseline.
///
/// # Errors
/// Returns an error for an unsupported path or an invalid restored configuration.
pub fn restore_service_setting(
    current: ServiceConfig,
    baseline: &ServiceConfig,
    path: &str,
) -> Result<ServiceConfig, ConfigError> {
    if let Some(id) = path.strip_prefix("routes.") {
        let mut current = current;
        current.settings.routes.retain(|route| route.id != id);
        if let Some(route) = baseline.settings.routes.iter().find(|route| route.id == id) {
            current.settings.routes.push(route.clone());
        }
        return parse_service_config(json!(current));
    }
    if path != "source" && !FIELDS.contains(&path) {
        return Err(ConfigError::at(
            "path",
            "Setting has no authored restore operation",
        ));
    }
    let mut current = json!(current);
    let baseline = json!(baseline);
    if path == "source"
        || (path.starts_with("source.")
            && at(&current, "source.type") != at(&baseline, "source.type"))
    {
        current
            .as_object_mut()
            .expect("serialized service")
            .insert("source".into(), at(&baseline, "source").clone());
    } else if path == "source.repository" && at(&current, "source.type") == "git" {
        let source = current
            .get_mut("source")
            .and_then(Value::as_object_mut)
            .expect("serialized source");
        for field in ["repository", "repositoryId", "installationId"] {
            source.insert(
                field.into(),
                at(&baseline, "source")
                    .get(field)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    } else if let Some((parent, field)) = path.split_once('.') {
        let Some(value) = baseline.get(parent).and_then(|v| v.get(field)) else {
            return parse_service_config(current);
        };
        current
            .get_mut(parent)
            .and_then(Value::as_object_mut)
            .expect("serialized source")
            .insert(field.into(), value.clone());
    } else {
        current
            .as_object_mut()
            .expect("serialized service")
            .insert(path.into(), at(&baseline, path).clone());
    }
    parse_service_config(current)
}

pub(super) fn at<'a>(value: &'a Value, path: &str) -> &'a Value {
    path.split('.')
        .fold(value, |value, key| value.get(key).unwrap_or(&Value::Null))
}

fn default_value(path: &str) -> Value {
    match path {
        "source.waitForCi" => json!(false),
        "source.autoUpdate" => json!({"type": "off"}),
        "source.credentials" => json!({"type": "none"}),
        "healthcheck" => json!({"type": "none"}),
        "restartPolicy" => json!("unless-stopped"),
        "maxRetries" => json!(10),
        "replicas" => json!(1),
        "build" => json!({"builder": "auto", "dockerfilePath": null, "watchPaths": []}),
        "variableGroupAttachments" => json!([]),
        _ => Value::Null,
    }
}

pub(super) fn change(
    path: &str,
    before: Value,
    after: Value,
    can_restore: bool,
) -> ServiceSettingChange {
    let kind = if before.is_null() {
        ChangeKind::Add
    } else if after.is_null() {
        ChangeKind::Remove
    } else {
        ChangeKind::Update
    };
    ServiceSettingChange {
        path: path.into(),
        kind,
        before,
        after,
        can_restore,
        derived_from: None,
    }
}

fn compare_related_settings(current: &Value, baseline: &Value) -> Vec<ServiceSettingChange> {
    let mut changes = Vec::new();
    for (family, identity) in [("routes", "id"), ("mounts", "volumeResourceId")] {
        let indexed = |value: &Value| -> BTreeMap<String, Value> {
            value[family]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row[identity].as_str().map(|id| (id.into(), row.clone())))
                .collect()
        };
        let before = indexed(baseline);
        let after = indexed(current);
        for id in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            let before = before.get(id).unwrap_or(&Value::Null);
            let after = after.get(id).unwrap_or(&Value::Null);
            let equal = if family == "mounts" {
                before["mountPath"] == after["mountPath"]
            } else {
                before == after
            };
            if !equal {
                changes.push(change(
                    &format!("{family}.{id}"),
                    before.clone(),
                    after.clone(),
                    family == "routes" && !baseline.is_null(),
                ));
            }
        }
    }
    let before = baseline["env"].as_object();
    let after = current["env"].as_object();
    let keys: BTreeSet<_> = before
        .into_iter()
        .flat_map(|env| env.keys())
        .chain(after.into_iter().flat_map(|env| env.keys()))
        .collect();
    for key in keys {
        let before = before.and_then(|env| env.get(key)).unwrap_or(&Value::Null);
        let after = after.and_then(|env| env.get(key)).unwrap_or(&Value::Null);
        if comparable_env(before) == comparable_env(after) {
            continue;
        }
        let mut row = change(
            &format!("env.{key}"),
            redacted_env(before),
            redacted_env(after),
            false,
        );
        // Detaching a group can reveal a local value. That is still a derived
        // effect of the attachment edit, not a second service-variable edit.
        let source = if after["source"].is_null() {
            &before["source"]
        } else {
            &after["source"]
        };
        row.derived_from = serde_json::from_value(source.clone()).ok();
        changes.push(row);
    }
    changes
}

fn comparable_env(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if value["kind"] == "secret" {
        json!({"kind": "secret", "variableId": value["variableId"], "fingerprint": value["fingerprint"]})
    } else if value["parts"].is_array() {
        json!({"kind": "template", "parts": value["parts"]})
    } else {
        json!({"kind": "literal", "value": value["value"]})
    }
}

fn redacted_env(value: &Value) -> Value {
    if value.is_null() {
        Value::Null
    } else if value["kind"] == "secret" {
        json!({"kind": "secret"})
    } else {
        json!({"kind": "literal", "value": value["value"]})
    }
}
