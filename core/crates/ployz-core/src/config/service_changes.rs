//! Compare service settings without runtime I/O.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::{Place, ServiceConfig, Setting};
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
    /// The mounted Config's friendly name; its path keeps the exact identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub config_name: Option<crate::ConfigName>,
    /// The full hostname of each generated domain it stores, by prefix; the Store's to
    /// fill, as deployed or under the Cluster Domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hostnames: Option<BTreeMap<String, String>>,
    pub kind: ChangeKind,
    pub before: Value,
    pub after: Value,
    /// Whether Discard takes it; the Store's to fill from the paths it accepts.
    pub can_restore: bool,
    /// The Sync row it falls in, which joins it to what moved it; the Store's to fill
    /// from the `At` its comparison hands alongside and the node's lineage.
    #[serde(default)]
    pub row: Option<super::RowId>,
}

impl ServiceSettingChange {
    /// Its path as people read it among `rows`, the others shown with it, as
    /// [`shown_paths`] reads it.
    #[must_use]
    pub fn shown_path(&self, rows: &[Self]) -> String {
        let among = rows
            .iter()
            .map(|row| (row.path.as_str(), row.config_name.as_ref()));
        shown_among(&self.path, self.config_name.as_ref(), among)
    }
}

/// Setting paths shown together, each with the name of the Config it addresses, as
/// people read them: a Config as `configs.NAME`, a mount as `SERVICE.configs.NAME`, by
/// the id the path keeps only when it has no name, and with a short id when another of
/// `paths` would read the same.
#[must_use]
pub fn shown_paths(paths: &[(&str, Option<&crate::ConfigName>)]) -> Vec<String> {
    paths
        .iter()
        .map(|&(path, name)| shown_among(path, name, paths.iter().copied()))
        .collect()
}

/// `path`, named `name`, as [`shown_paths`] reads it among `others`.
fn shown_among<'a>(
    path: &str,
    name: Option<&crate::ConfigName>,
    mut others: impl Iterator<Item = (&'a str, Option<&'a crate::ConfigName>)>,
) -> String {
    let Some((label, id)) = named(path, name) else {
        return path.to_owned();
    };
    let shared = others.any(|(other, name)| {
        named(other, name).is_some_and(|(same, other_id)| same == label && other_id != id)
    });
    match shared {
        true => format!("{label} (@{})", id.get(..8).unwrap_or(id)),
        false => label,
    }
}

/// `path` with the Config it addresses named `name`, and that Config's id.
fn named<'a>(path: &'a str, name: Option<&crate::ConfigName>) -> Option<(String, &'a str)> {
    let name = name?;
    if let Some(rest) = path.strip_prefix("configs.@") {
        return Some(match rest.split_once('.') {
            Some((id, field)) => (format!("configs.{name}.{field}"), id),
            None => (format!("configs.{name}"), rest),
        });
    }
    let (service, id) = path.split_once(".configs.@")?;
    Some((format!("{service}.configs.{name}"), id))
}

/// The order a review lists a Service's setting rows in, which Cloud and the CLI show as
/// they come; the Settings in landing order are not it. Routes are listed apart.
const REVIEW_ORDER: [Setting; 17] = [
    Setting::Source,
    Setting::Healthcheck,
    Setting::Branch,
    Setting::PreDeployCommand,
    Setting::StartCommand,
    Setting::RestartPolicy,
    Setting::MaxRetries,
    Setting::UpdateOrder,
    Setting::Replicas,
    Setting::CpuLimit,
    Setting::MemLimit,
    Setting::PrivateDns,
    Setting::ManagedHostnames,
    Setting::BuildMethod,
    Setting::DockerfilePath,
    Setting::BuildCommand,
    Setting::Routes,
];

/// Compare settings against an available authored baseline, keeping derived effects separate.
/// Each change comes with where in its node its row is.
#[must_use]
pub fn compare_service_settings(
    current: &ServiceConfig,
    baseline: Option<&ServiceConfig>,
) -> Vec<(ServiceSettingChange, Place)> {
    // A git branch that comes or goes with its source is the source's change.
    let switched = baseline.is_some_and(|baseline| {
        std::mem::discriminant(&baseline.settings.source)
            != std::mem::discriminant(&current.settings.source)
    });
    let mut changes = Vec::new();
    for setting in REVIEW_ORDER {
        // Each route is a row of its own, found below.
        if setting == Setting::Routes || (setting == Setting::Branch && switched) {
            continue;
        }
        let read = |config: &ServiceConfig| setting.read(&config.settings, "");
        let (mut before, mut after) = (baseline.map_or(Value::Null, read), read(current));
        // The source is one row, as Sync moves it and Discard takes it, but for its git
        // branch; an empty one is no source.
        if setting == Setting::Source {
            for cell in [&mut before, &mut after] {
                if *cell == setting.default() {
                    *cell = Value::Null;
                }
            }
        }
        if before == after || (baseline.is_none() && after == setting.default()) {
            continue;
        }
        changes.push(change(Place::Setting(setting), before, after));
    }
    let current = json!(current);
    let baseline = baseline.map_or(Value::Null, |value| json!(value));
    changes.extend(compare_related_settings(&current, &baseline));
    changes
}

pub(super) fn at<'a>(value: &'a Value, path: &str) -> &'a Value {
    path.split('.')
        .fold(value, |value, key| value.get(key).unwrap_or(&Value::Null))
}

/// A change in the row at `place` of its node.
pub(super) fn change(place: Place, before: Value, after: Value) -> (ServiceSettingChange, Place) {
    let kind = if before.is_null() {
        ChangeKind::Add
    } else if after.is_null() {
        ChangeKind::Remove
    } else {
        ChangeKind::Update
    };
    let change = ServiceSettingChange {
        path: place.path(),
        kind,
        before,
        after,
        can_restore: false,
        row: None,
        config_name: None,
        hostnames: None,
    };
    (change, place)
}

fn compare_related_settings(
    current: &Value,
    baseline: &Value,
) -> Vec<(ServiceSettingChange, Place)> {
    let mut changes = Vec::new();
    // Each family, the key of its id, and the place a row of it has.
    type Family = (&'static str, &'static str, fn(String) -> Place);
    let places: [Family; 3] = [
        ("routes", "id", Place::Route),
        ("mounts", "volumeResourceId", |volume_resource_id| {
            Place::Mount { volume_resource_id }
        }),
        ("configs", "configResourceId", |config_resource_id| {
            Place::ConfigMount { config_resource_id }
        }),
    ];
    for (family, identity, place) in places {
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
            let equal = match family {
                "mounts" => before["mountPath"] == after["mountPath"],
                "configs" => before["mountDir"] == after["mountDir"],
                _ => before == after,
            };
            if !equal {
                changes.push(change(place(id.clone()), before.clone(), after.clone()));
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
            Place::Variable(key.clone()),
            redacted_env(before),
            redacted_env(after),
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
        // Only a secret without a value has no fingerprint.
        json!({"kind": "secret", "valued": value["fingerprint"] != ""})
    } else if value["parts"].is_array() {
        json!({"kind": "literal", "value": value["value"]})
    } else {
        // Shown as `get` shows it: a plain `${{` reads escaped, unlike a reference.
        let text = value["value"].as_str().unwrap_or_default();
        json!({"kind": "literal", "value": text.replace("${{", "$${{")})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_review_order_lists_every_setting_once() {
        let mut listed = REVIEW_ORDER;
        listed.sort();
        assert_eq!(listed, Setting::ALL);
    }
}
