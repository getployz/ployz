//! Compare service settings and restore their authored values without runtime I/O.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::{
    At, ConfigError, ServiceConfig, ServiceImageCredentials, ServiceSource, Setting,
    parse_service_config,
};
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
    /// The Sync row it falls in, which joins it to what moved it; the Store's to fill
    /// from the `At` its comparison hands alongside and the node's lineage.
    #[serde(default)]
    pub row: Option<super::RowId>,
}

/// Each compared field and the Setting whose row it falls in.
const FIELDS: &[(&str, Setting)] = &[
    ("source.branch", Setting::Branch),
    ("preDeployCommand", Setting::PreDeployCommand),
    ("startCommand", Setting::StartCommand),
    ("restartPolicy", Setting::RestartPolicy),
    ("maxRetries", Setting::MaxRetries),
    ("replicas", Setting::Replicas),
    ("cpuLimit", Setting::CpuLimit),
    ("memLimit", Setting::MemLimit),
    ("privateDns", Setting::PrivateDns),
    ("managedHostnames", Setting::ManagedHostnames),
    ("build.buildMethod", Setting::BuildMethod),
    ("build.dockerfilePath", Setting::DockerfilePath),
    ("build.command", Setting::BuildCommand),
];

/// Compare settings against an available authored baseline, keeping derived effects separate.
/// Each change comes with where in its node its row is; none for a mount, whose Volume
/// lineage only the Store knows.
#[must_use]
pub fn compare_service_settings(
    current: &ServiceConfig,
    baseline: Option<&ServiceConfig>,
) -> Vec<(ServiceSettingChange, Option<At>)> {
    // The source is one row, as Sync moves it and Discard takes it, but for its git branch.
    let source = |config: &ServiceConfig| {
        Some(source_cell(&config.settings.source)).filter(|cell| *cell != default_value("source"))
    };
    let (before, after) = (baseline.and_then(source), source(current));
    // A git branch that comes or goes with its source is the source's change.
    let switched = baseline.is_some_and(|baseline| {
        std::mem::discriminant(&baseline.settings.source)
            != std::mem::discriminant(&current.settings.source)
    });
    let mut changes = Vec::new();
    if before != after {
        changes.push(change(
            ("source", Some(At::Setting(Setting::Source))),
            before.unwrap_or_default(),
            after.unwrap_or_default(),
            baseline.is_some(),
        ));
    }
    let current = json!(current);
    let baseline = baseline.map_or(Value::Null, |value| json!(value));
    // One row, as Sync moves it and Discard takes it, whichever of its parts changed.
    if at(&current, "healthcheck") != at(&baseline, "healthcheck")
        && (!baseline.is_null() || at(&current, "healthcheck.type") != "none")
    {
        changes.push(change(
            ("healthcheck", Some(At::Setting(Setting::Healthcheck))),
            at(&baseline, "healthcheck").clone(),
            at(&current, "healthcheck").clone(),
            !baseline.is_null(),
        ));
    }
    for &(path, setting) in FIELDS {
        let before = at(&baseline, path);
        let after = at(&current, path);
        if before == after || (switched && path == "source.branch") {
            continue;
        }
        if baseline.is_null() && *after == default_value(path) {
            continue;
        }
        changes.push(change(
            (path, Some(At::Setting(setting))),
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
        let routes = &mut current.settings.routes;
        let baseline = &baseline.settings.routes;
        if let Some((index, restored)) = baseline
            .iter()
            .enumerate()
            .find(|(_, route)| route.id == id)
        {
            if let Some(route) = routes.iter_mut().find(|route| route.id == id) {
                *route = restored.clone();
            } else {
                // Restore before later baseline routes and newly linked domains.
                let position = routes
                    .iter()
                    .position(|route| {
                        baseline
                            .iter()
                            .position(|prior| prior.id == route.id)
                            .is_none_or(|prior| prior > index)
                    })
                    .unwrap_or(routes.len());
                routes.insert(position, restored.clone());
            }
        } else {
            routes.retain(|route| route.id != id);
        }
        return parse_service_config(json!(current));
    }
    if path != "source" && path != "healthcheck" && !FIELDS.iter().any(|(field, _)| *field == path)
    {
        return Err(ConfigError::at(
            "path",
            "Setting has no authored restore operation",
        ));
    }
    if path == "source" {
        let mut current = current;
        current.settings.source =
            keep_branch(&current.settings.source, baseline.settings.source.clone());
        return parse_service_config(json!(current));
    }
    let mut current = json!(current);
    let baseline = json!(baseline);
    if path == "healthcheck" {
        current
            .as_object_mut()
            .expect("serialized service")
            .insert("healthcheck".into(), at(&baseline, "healthcheck").clone());
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

/// A source as its row holds it: all of it but its git branch, its credentials by
/// presence.
pub(super) fn source_cell(source: &ServiceSource) -> Value {
    let mut cell = json!(source);
    let fields = cell.as_object_mut().expect("a source is an object");
    fields.remove("branch");
    if let ServiceSource::Image { credentials, .. } = source {
        let configured = matches!(credentials, ServiceImageCredentials::Configured { .. });
        fields.insert("credentials".into(), json!(configured));
    }
    cell
}

/// `source` as a Service on `current` takes it: the git branch is the Service's own, so
/// it keeps the one it has, else the one `source` carries.
pub(super) fn keep_branch(current: &ServiceSource, mut source: ServiceSource) -> ServiceSource {
    if let (ServiceSource::Git { branch: own, .. }, ServiceSource::Git { branch, .. }) =
        (current, &mut source)
    {
        branch.clone_from(own);
    }
    source
}

pub(super) fn default_value(path: &str) -> Value {
    match path {
        "source" => json!({"type": "empty", "version": 1, "rootDir": "/"}),
        "healthcheck" => json!({"type": "none"}),
        "restartPolicy" => json!("unless-stopped"),
        "maxRetries" => json!(10),
        "replicas" => json!(1),
        "build.buildMethod" => json!("railpack"),
        _ => Value::Null,
    }
}

/// A change at `path`, in the row at `at` of its node.
pub(super) fn change(
    (path, at): (&str, Option<At>),
    before: Value,
    after: Value,
    can_restore: bool,
) -> (ServiceSettingChange, Option<At>) {
    let kind = if before.is_null() {
        ChangeKind::Add
    } else if after.is_null() {
        ChangeKind::Remove
    } else {
        ChangeKind::Update
    };
    let change = ServiceSettingChange {
        path: path.into(),
        kind,
        before,
        after,
        can_restore,
        row: None,
    };
    (change, at)
}

fn compare_related_settings(
    current: &Value,
    baseline: &Value,
) -> Vec<(ServiceSettingChange, Option<At>)> {
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
                let at = (family == "routes").then_some(At::Setting(Setting::Routes));
                changes.push(change(
                    (&format!("{family}.{id}"), at),
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
        changes.push(change(
            (&format!("env.{key}"), Some(At::Variable(key.clone()))),
            redacted_env(before),
            redacted_env(after),
            false,
        ));
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
    } else if value["parts"].is_array() {
        json!({"kind": "literal", "value": value["value"]})
    } else {
        // Shown as `get` shows it: a plain `${{` reads escaped, unlike a reference.
        let text = value["value"].as_str().unwrap_or_default();
        json!({"kind": "literal", "value": text.replace("${{", "$${{")})
    }
}
