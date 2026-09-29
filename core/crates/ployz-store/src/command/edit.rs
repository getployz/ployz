//! Edits: `set` and `unset` of Settings in an Environment's Working State.
//!
//! An edit is blind by default: it applies to the latest Working State under the
//! Environment's lock, so edits to different Settings never overwrite each other.
//! A caller may pass the revision it read; the edit then refuses if Working State moved.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::Revision;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::sealing::SealingKey;
use crate::settings::{Apply, ServiceSetting, SettingPath, Target};
use crate::variables::{self, VariableKey};
use crate::storage::Tx;

/// Apply every change to one Environment, all or none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    /// The Environment to edit.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Refuse with `conflict` unless Working State is still at this revision.
    #[serde(default)]
    pub expect: Option<Revision>,
    /// The changes, applied in order.
    pub changes: Vec<Change>,
}

/// One change, addressed as `SERVICE.SETTING`, or as `SERVICE` for a patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// Set a value. Text is accepted for any Setting type. A variable
    /// (`SERVICE.env.KEY`) takes text, `{"secret": true}` to keep its secret, or
    /// `{"secret": "…"}` to seal a new one.
    Set {
        /// The Setting.
        path: SettingPath,
        /// Its new value.
        value: Value,
    },
    /// Return a Setting to its default, or delete a variable.
    Unset {
        /// The Setting.
        path: SettingPath,
    },
    /// Set several Settings of one Service from an object shaped like `get`'s
    /// `values`. Omitted Settings stay as they are; `null` never clears one.
    Patch {
        /// The Service, as `SERVICE`.
        path: SettingPath,
        /// Its Settings by name.
        value: Value,
    },
}

/// The Environment after an edit, and which Setting paths it changed: staged until
/// a Deploy, or applied immediately. An edit that changed nothing keeps the revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Edited {
    /// The Environment, at its revision after the edit.
    pub environment: EnvironmentSummary,
    /// Settings changed in Working State, waiting for a Deploy.
    pub staged: Vec<SettingPath>,
    /// Settings that took effect at once.
    pub immediate: Vec<SettingPath>,
}

pub(crate) fn edit(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    edit: &Edit,
) -> Result<Edited, RpcError> {
    if edit.changes.is_empty() {
        return Err(error::invalid(
            "An edit needs at least one change",
            json!({}),
        ));
    }
    let mut environment = scope::lock(tx, who, &edit.environment)?;
    if let Some(expect) = edit.expect
        && expect != environment.summary.revision
    {
        return Err(error::conflict(
            format!(
                "Working State moved from revision {expect} to {}",
                environment.summary.revision
            ),
            json!({ "revision": environment.summary.revision }),
        ));
    }
    let before = environment.working.clone();
    let (mut staged, mut immediate) = (Vec::new(), Vec::new());
    for (path, value) in expand(&edit.changes)? {
        let service = path.service();
        let (changed, apply) = match (path.target(), value) {
            (Some(Target::Setting(setting)), value) => {
                let config = &mut environment.service_mut(service)?.config;
                let was = setting.value(config);
                match value {
                    Some(value) => setting.set(config, value)?,
                    None => setting.unset(config)?,
                }
                (setting.value(config) != was, setting.apply())
            }
            (Some(Target::Variable(key)), Some(value)) => (
                variables::set(&mut environment, service, key, value, sealing)?,
                Apply::Staged,
            ),
            (Some(Target::Exported(key)), Some(value)) => (
                variables::set_exported(&mut environment, service, key, value)?,
                Apply::Staged,
            ),
            (Some(Target::Variable(key)), None) => (
                variables::unset(&mut environment, service, key, false)?,
                Apply::Staged,
            ),
            (Some(Target::Exported(key)), None) => (
                variables::unset(&mut environment, service, key, true)?,
                Apply::Staged,
            ),
            (None, _) => return Err(name_a_setting(&path)),
        };
        if !changed {
            continue;
        }
        let list = match apply {
            Apply::Staged => &mut staged,
            Apply::Immediate => &mut immediate,
        };
        if !list.contains(&path) {
            list.push(path);
        }
    }
    if environment.working != before {
        scope::save_working(tx, &mut environment)?;
    }
    Ok(Edited {
        environment: environment.summary,
        staged,
        immediate,
    })
}

/// Every change as one path to set (with its value) or unset.
fn expand(changes: &[Change]) -> Result<Vec<(SettingPath, Option<Value>)>, RpcError> {
    let mut expanded = Vec::new();
    for change in changes {
        match change {
            Change::Set { path, value } => expanded.push((path.clone(), Some(value.clone()))),
            Change::Unset { path } => expanded.push((path.clone(), None)),
            Change::Patch { path, value } => {
                if path.target().is_some() {
                    return Err(error::invalid(
                        "A patch addresses a Service: set SERVICE --patch",
                        json!({ "example": path.service() }),
                    ));
                }
                let Some(object) = value.as_object() else {
                    return Err(error::invalid(
                        "A patch is a JSON object of Settings",
                        json!({ "example": { "replicas": 3 } }),
                    ));
                };
                for (name, value) in object {
                    if name == "env" {
                        let Some(variables) = value.as_object() else {
                            return Err(error::invalid(
                                "env is a JSON object of variables",
                                json!({ "example": { "env": { "LOG_LEVEL": "info" } } }),
                            ));
                        };
                        for (key, value) in variables {
                            let key = VariableKey::parse(key)?;
                            let path = SettingPath::at(path.service(), Target::Variable(key));
                            expanded.push((path, Some(value.clone())));
                        }
                        continue;
                    }
                    let setting = ServiceSetting::parse(name)?;
                    let path = SettingPath::of(path.service(), setting);
                    expanded.push((path, Some(value.clone())));
                }
            }
        }
    }
    Ok(expanded)
}

fn name_a_setting(path: &SettingPath) -> RpcError {
    error::invalid(
        "Name a Setting: SERVICE.SETTING",
        json!({
            "valid_children": ServiceSetting::ALL.map(ServiceSetting::name),
            "example": format!("{}.replicas", path.service()),
        }),
    )
}
