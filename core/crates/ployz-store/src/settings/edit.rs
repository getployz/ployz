//! Edits: `set` and `unset` of Settings in an Environment's Working State.
//!
//! An edit is blind by default: it applies to the latest Working State under the
//! Environment's lock, so edits to different Settings never overwrite each other.
//! A caller may pass the revision it read; the edit then refuses if Working State moved.

use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error;
use crate::id::{Revision, VolumeName};
use crate::policy::{self, Policy};
use crate::registry;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::sealing::SealingKey;
use crate::settings::{Apply, ServiceSetting, SettingPath, Target};
use crate::storage::Tx;
use crate::typed_address::{Instead, Typed};
use crate::variables::{self, VariableKey};
use crate::{Actor, Trusted};

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
    /// Variables this edit set to a value with Typed Addresses, each with what to set
    /// instead.
    // An older Cloud doesn't send it.
    #[serde(default)]
    pub typed_addresses: Vec<TypedAddresses>,
}

/// The Typed Addresses in one variable an edit set: whose they are, and what to set
/// instead.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TypedAddresses {
    /// The variable, as SERVICE.env.KEY.
    pub path: SettingPath,
    /// The Services whose private addresses it types.
    pub services: Vec<ServiceName>,
    /// What to set instead.
    pub instead: Instead,
}

pub(crate) fn edit(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    edit: &Edit,
    trusted: &Trusted,
) -> Result<Edited, RpcError> {
    if edit.changes.is_empty() {
        return Err(error::invalid(
            "An edit needs at least one change",
            json!({}),
        ));
    }
    let mut environment = scope::lock(tx, who, &edit.environment)?;
    environment.expect(edit.expect)?;
    let before = environment.working.clone();
    let (mut staged, mut immediate) = (Vec::new(), Vec::new());
    let mut typed_addresses: Vec<TypedAddresses> = Vec::new();
    for (path, value) in expand(&edit.changes)? {
        // Only the last change to a variable decides what it types.
        typed_addresses.retain(|typed| typed.path != path);
        let service = path.settings_of()?;
        // A new credential applies at once; turning one on or off is staged.
        if let Some(Target::Setting(setting @ ServiceSetting::RegistryCredential)) = path.target() {
            let changed = match &value {
                Some(value) => registry::set(tx, who, &mut environment, service, value, sealing)?,
                None => {
                    let config = &mut environment.service_mut(service)?.config;
                    let was = setting.value(config, &Policy::default());
                    setting.unset(config)?;
                    registry::Changed {
                        staged: setting.value(config, &Policy::default()) != was,
                        rotated: false,
                    }
                }
            };
            for (changed, list) in [
                (changed.staged, &mut staged),
                (changed.rotated, &mut immediate),
            ] {
                if changed && !list.contains(&path) {
                    list.push(path.clone());
                }
            }
            continue;
        }
        if let Some(Target::Setting(ServiceSetting::Policy(setting))) = path.target() {
            let node = environment.service(service)?;
            if policy::set(tx, who, &environment.summary.id, node, *setting, value)?
                && !immediate.contains(&path)
            {
                immediate.push(path.clone());
            }
            continue;
        }
        let (changed, apply) = match (path.target(), value) {
            // A Private DNS name addresses one Service: none other has it as a name.
            (Some(Target::Setting(setting @ ServiceSetting::PrivateDns)), value) => {
                let node = environment.service(service)?;
                let name = match value {
                    Some(Value::String(text)) => ployz_core::ServiceName::parse(text.trim())
                        .map_err(|_| setting.invalid("expected a lowercase DNS label"))?,
                    Some(_) => return Err(setting.invalid("expected a lowercase DNS label")),
                    None => ployz_core::ServiceName::parse(node.slug.as_str())
                        .map_err(|_| error::corrupt("Service name"))?,
                };
                let id = node.id.clone();
                crate::service::refuse_taken(&environment, &name, Some(&id))?;
                let config = &mut environment.service_mut(service)?.config;
                let changed = config.private_dns != name;
                config.private_dns = name;
                (changed, Apply::Staged)
            }
            (Some(Target::Setting(setting)), value) => {
                let config = &mut environment.service_mut(service)?.config;
                let was = setting.value(config, &Policy::default());
                match value {
                    Some(value) => setting.set(config, value, trusted)?,
                    None => setting.unset(config)?,
                }
                (
                    setting.value(config, &Policy::default()) != was,
                    setting.apply(),
                )
            }
            (Some(Target::Source), _) => {
                return Err(error::invalid(
                    format!("{path}: set image or repository instead"),
                    json!({ "example": { "image": "nginx:1.27" } }),
                ));
            }
            (Some(Target::Variable(key)), Some(value)) => {
                let (changed, typed) =
                    variables::set(&mut environment, service, key, value, sealing)?;
                typed_addresses.extend(typed.map(|Typed { services, instead }| TypedAddresses {
                    path: path.clone(),
                    services,
                    instead,
                }));
                (changed, Apply::Staged)
            }
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
            (Some(Target::Mount(volume)), Some(value)) => {
                let Some(mount) = value.as_str() else {
                    return Err(error::invalid(
                        format!("{path}: expected an absolute path"),
                        json!({ "example": "/var/lib/data" }),
                    ));
                };
                (
                    crate::volume::attach(&mut environment, service, volume, mount)?,
                    Apply::Staged,
                )
            }
            (Some(Target::Mount(volume)), None) => (
                crate::volume::detach(&mut environment, service, volume)?,
                Apply::Staged,
            ),
            (Some(Target::ConfigMount(config)), Some(value)) => {
                let Some(dir) = value.as_str() else {
                    return Err(error::invalid(
                        format!("{path}: expected an absolute directory"),
                        json!({ "example": "/etc/app" }),
                    ));
                };
                (
                    crate::config_item::attach(&mut environment, service, config, dir)?,
                    Apply::Staged,
                )
            }
            (Some(Target::ConfigMount(config)), None) => (
                crate::config_item::detach(&mut environment, service, config)?,
                Apply::Staged,
            ),
            (None, _) => return Err(crate::settings::name_a_setting(path.node())),
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
    crate::git::check_sources(&before, &environment.working, trusted)?;
    if environment.working != before {
        scope::save_working(tx, &mut environment)?;
    }
    Ok(Edited {
        environment: environment.summary,
        staged,
        immediate,
        typed_addresses,
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
                let service = path.settings_of()?;
                if path.target().is_some() {
                    return Err(error::invalid(
                        "A patch addresses a Service: set SERVICE --patch",
                        json!({ "example": service }),
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
                            let path = SettingPath::at(service, Target::Variable(key));
                            expanded.push((path, Some(value.clone())));
                        }
                        continue;
                    }
                    if name == "mounts" {
                        let Some(mounts) = value.as_object() else {
                            return Err(error::invalid(
                                "mounts is a JSON object of paths by Volume name",
                                json!({ "example": { "mounts": { "data": "/var/lib/data" } } }),
                            ));
                        };
                        for (volume, value) in mounts {
                            let volume = VolumeName::parse(volume.as_str())?;
                            let path = SettingPath::at(service, Target::Mount(volume));
                            expanded.push((path, Some(value.clone())));
                        }
                        continue;
                    }
                    if name == "configs" {
                        let Some(mounts) = value.as_object() else {
                            return Err(error::invalid(
                                "configs is a JSON object of directories by Config name",
                                json!({ "example": { "configs": { "sentry": "/etc/sentry" } } }),
                            ));
                        };
                        for (config, value) in mounts {
                            let config = crate::settings::config_name(config)?;
                            let path = SettingPath::at(service, Target::ConfigMount(config));
                            expanded.push((path, Some(value.clone())));
                        }
                        continue;
                    }
                    let setting = ServiceSetting::parse(name)?;
                    let path = SettingPath::of(service, setting);
                    expanded.push((path, Some(value.clone())));
                }
            }
        }
    }
    Ok(expanded)
}
