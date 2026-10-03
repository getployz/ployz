//! An Environment's Settings as `get` shows them. Depth decides how much: the
//! whole Environment lists only Settings that differ from their default (all of
//! them with `all`); one Service lists every Setting and its `values` object; one
//! Setting lists itself. Variables list as `SERVICE.env.KEY`, secrets as
//! `{"secret": true}`: no read shows a secret.

use ployz_core::config::{RowId, SavedEnvironmentIntent, SavedServiceIntent};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use crate::Actor;
use crate::id::VolumeName;
use crate::policy;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{Apply, ServiceSetting, SettingPath, Target};
use crate::storage::Tx;
use crate::variables::{self, VariableKey};

/// Read an Environment's Working State, narrowed to `SERVICE` or `SERVICE.SETTING`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentQuery {
    /// The Environment to read.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Narrow to one Service or one Setting; omitted means every Setting.
    #[serde(default)]
    pub path: Option<SettingPath>,
    /// Include Settings at their default across the whole Environment.
    #[serde(default)]
    pub all: bool,
}

/// An Environment's Settings in Working State.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// Every Setting asked for, by Service name then Setting.
    pub settings: Vec<SettingRow>,
    /// For one Service: its Settings as one object, the shape `set --patch` takes.
    /// Settings without a value are left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Map<String, Value>>,
    /// Every row the Environment marks Never sync, whichever Settings were asked for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<RowId>>", optional)]
    pub never_synced: Vec<RowId>,
}

/// One Setting's current Working State value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct SettingRow {
    /// The Setting.
    pub path: SettingPath,
    /// Its value in Working State; `null` when it has none.
    pub value: Value,
    /// The value `unset` restores.
    pub default: Value,
    /// Whether a change to it waits for a Deploy.
    pub apply: Apply,
    /// A variable's row, which [`crate::NeverSync`] names it by.
    // ponytail: only variables, the one Setting the dashboard marks from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub row: Option<crate::RowId>,
}

pub(crate) fn environment(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &EnvironmentQuery,
) -> Result<EnvironmentView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let path = query.path.as_ref();
    let services = match path {
        Some(path) => vec![environment.service(path.settings_of()?)?],
        None => {
            let mut services = environment.working.services.iter().collect::<Vec<_>>();
            services.sort_by(|a, b| a.slug.cmp(&b.slug));
            services
        }
    };
    let only = path.and_then(SettingPath::target);
    let values = path
        .filter(|path| path.target().is_none())
        .and_then(|_| services.first())
        .map(|service| values(tx, &environment, service))
        .transpose()?;
    let whole = path.is_none() && !query.all;
    let mut settings = Vec::new();
    for service in services {
        let name = ServiceName::parse(service.slug.as_str())
            .map_err(|_| crate::error::corrupt("Service name"))?;
        let mut row = |target: Target, value: Value, default: Value, apply: Apply| {
            if only.is_none_or(|only| *only == target) && !(whole && value == default) {
                let row = match &target {
                    Target::Variable(key) => Some(crate::branch::row_id(
                        &service.lineage_id,
                        &format!("variables.{key}"),
                    )?),
                    Target::Setting(_)
                    | Target::Source
                    | Target::Exported(_)
                    | Target::Mount(_) => None,
                };
                settings.push(SettingRow {
                    path: SettingPath::at(&name, target),
                    value,
                    default,
                    apply,
                    row,
                });
            }
            Ok::<_, RpcError>(())
        };
        let policy = policy::load(tx, &environment.summary.id, &service.id)?;
        for setting in ServiceSetting::ALL {
            if !setting.applies(&service.config) {
                continue;
            }
            // Unset, a Private DNS name is the Service's own.
            let default = if matches!(setting, ServiceSetting::PrivateDns) {
                json!(service.slug)
            } else {
                setting.default()
            };
            row(
                Target::Setting(setting),
                setting.value(&service.config, &policy),
                default,
                setting.apply(),
            )?;
        }
        if let Some(Target::Variable(key) | Target::Exported(key)) = only {
            variables::find(service, key)?;
        }
        for variable in variables::sorted(service) {
            let key =
                VariableKey::parse(&variable.key).map_err(|_| crate::error::corrupt("variable"))?;
            row(
                Target::Variable(key.clone()),
                variables::shown(variable, &environment.names()),
                Value::Null,
                Apply::Staged,
            )?;
            row(
                Target::Exported(key),
                Value::Bool(variable.exported),
                Value::Bool(false),
                Apply::Staged,
            )?;
        }
        if let Some(Target::Mount(volume)) = only {
            environment.volume(volume)?;
        }
        for (volume, path) in mounts(service, &environment.working) {
            let volume = VolumeName::parse(volume).map_err(|_| crate::error::corrupt("Volume"))?;
            row(
                Target::Mount(volume),
                Value::String(path),
                Value::Null,
                Apply::Staged,
            )?;
        }
    }
    Ok(EnvironmentView {
        never_synced: crate::branch::marked(tx, &environment.summary.id)?
            .into_iter()
            .collect(),
        environment: environment.summary,
        settings,
        values,
    })
}

/// A Service's Settings as one object, the shape `set --patch` takes. Settings
/// without a value are left out.
pub(crate) fn values(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
    service: &SavedServiceIntent,
) -> Result<Map<String, Value>, RpcError> {
    let policy = policy::load(tx, &environment.summary.id, &service.id)?;
    let intent = &environment.working;
    let mut values: Map<String, Value> = ServiceSetting::ALL
        .into_iter()
        .filter(|setting| setting.applies(&service.config))
        .map(|setting| {
            (
                setting.name().to_owned(),
                setting.value(&service.config, &policy),
            )
        })
        .filter(|(_, value)| !value.is_null())
        .collect();
    let env = variables::patch_values(service, &environment.names());
    if !env.is_empty() {
        values.insert("env".to_owned(), Value::Object(env));
    }
    let mounts: Map<String, Value> = mounts(service, intent)
        .into_iter()
        .map(|(volume, path)| (volume, Value::String(path)))
        .collect();
    if !mounts.is_empty() {
        values.insert("mounts".to_owned(), Value::Object(mounts));
    }
    Ok(values)
}

/// Where `service` mounts Volumes, as (Volume name, path), sorted by name.
pub(crate) fn mounts(
    service: &SavedServiceIntent,
    intent: &SavedEnvironmentIntent,
) -> Vec<(String, String)> {
    let mut mounts: Vec<(String, String)> = service
        .volume_attachments
        .iter()
        .filter_map(|mount| {
            let volume = intent
                .volumes
                .iter()
                .find(|volume| volume.resource_id == mount.volume_resource_id)?;
            Some((volume.name.clone(), mount.mount_path.clone()))
        })
        .collect();
    mounts.sort();
    mounts
}
