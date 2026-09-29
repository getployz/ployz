//! Edits: `set` and `unset` of Settings in an Environment's Working State.
//!
//! An edit is blind by default: it applies to the latest Working State under the
//! Environment's lock, so edits to different Settings never overwrite each other.
//! A caller may pass the revision it read; the edit then refuses if Working State moved.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Actor;
use crate::error;
use crate::id::Revision;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{Apply, ServiceSetting, SettingPath};
use crate::storage::Tx;

/// Apply every change to one Environment, all or none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// Set a value. Text is accepted for any Setting type.
    Set {
        /// The Setting.
        path: SettingPath,
        /// Its new value.
        value: Value,
    },
    /// Return a Setting to its default.
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edited {
    /// The Environment, at its revision after the edit.
    pub environment: EnvironmentSummary,
    /// Settings changed in Working State, waiting for a Deploy.
    pub staged: Vec<SettingPath>,
    /// Settings that took effect at once.
    pub immediate: Vec<SettingPath>,
}

pub(crate) fn edit(tx: &mut dyn Tx, who: &Actor, edit: &Edit) -> Result<Edited, RpcError> {
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
    for (path, setting, value) in expand(&edit.changes)? {
        let service = environment.service_mut(path.service())?;
        let was = setting.value(&service.config);
        match value {
            Some(value) => setting.set(&mut service.config, value)?,
            None => setting.unset(&mut service.config)?,
        }
        if setting.value(&service.config) == was {
            continue;
        }
        let list = match setting.apply() {
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

/// Every change as one Setting to set (with its value) or unset.
fn expand(
    changes: &[Change],
) -> Result<Vec<(SettingPath, ServiceSetting, Option<Value>)>, RpcError> {
    let mut expanded = Vec::new();
    for change in changes {
        match change {
            Change::Set { path, value } => {
                let (path, setting) = one(path)?;
                expanded.push((path, setting, Some(value.clone())));
            }
            Change::Unset { path } => {
                let (path, setting) = one(path)?;
                expanded.push((path, setting, None));
            }
            Change::Patch { path, value } => {
                if path.setting().is_some() {
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
                    let setting = ServiceSetting::parse(name)?;
                    let path = SettingPath::of(path.service(), setting);
                    expanded.push((path, setting, Some(value.clone())));
                }
            }
        }
    }
    Ok(expanded)
}

fn one(path: &SettingPath) -> Result<(SettingPath, ServiceSetting), RpcError> {
    let Some(setting) = path.setting() else {
        return Err(error::invalid(
            "Name a Setting: SERVICE.SETTING",
            json!({
                "valid_children": ServiceSetting::ALL.map(ServiceSetting::name),
                "example": format!("{}.replicas", path.service()),
            }),
        ));
    };
    Ok((path.clone(), setting))
}
