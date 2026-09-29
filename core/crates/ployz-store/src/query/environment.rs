//! An Environment's Settings as `get` shows them. Depth decides how much: the
//! whole Environment lists only Settings that differ from their default (all of
//! them with `all`); one Service lists every Setting and its `values` object; one
//! Setting lists itself. Variables list as `SERVICE.env.KEY`, secrets as
//! `{"secret": true}`: no read shows a secret.

use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::Actor;
use crate::builders;
use crate::id::EnvironmentId;
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
}

pub(crate) fn environment(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &EnvironmentQuery,
) -> Result<EnvironmentView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let path = query.path.as_ref();
    let services = match path {
        Some(path) => vec![environment.service(path.service())?],
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
        .map(|service| values(tx, &environment.summary.id, service, &environment.working))
        .transpose()?;
    let whole = path.is_none() && !query.all;
    let mut settings = Vec::new();
    for service in services {
        let name = ServiceName::parse(service.slug.as_str())
            .map_err(|_| crate::error::corrupt("Service name"))?;
        let mut row = |target: Target, value: Value, default: Value, apply: Apply| {
            if only.is_none_or(|only| *only == target) && !(whole && value == default) {
                settings.push(SettingRow {
                    path: SettingPath::at(&name, target),
                    value,
                    default,
                    apply,
                });
            }
        };
        for setting in ServiceSetting::ALL {
            if !setting.applies(&service.config) {
                continue;
            }
            let value = if setting == ServiceSetting::PreferredBuilder {
                builders::preferred_value(tx, &environment.summary.id, &service.id)?
            } else {
                setting.value(&service.config)
            };
            row(
                Target::Setting(setting),
                value,
                setting.default(),
                setting.apply(),
            );
        }
        if let Some(Target::Variable(key) | Target::Exported(key)) = only {
            variables::find(service, key)?;
        }
        for variable in variables::sorted(service) {
            let key =
                VariableKey::parse(&variable.key).map_err(|_| crate::error::corrupt("variable"))?;
            row(
                Target::Variable(key.clone()),
                variables::shown(variable, &environment.working),
                Value::Null,
                Apply::Staged,
            );
            row(
                Target::Exported(key),
                Value::Bool(variable.exported),
                Value::Bool(false),
                Apply::Staged,
            );
        }
    }
    Ok(EnvironmentView {
        environment: environment.summary,
        settings,
        values,
    })
}

/// A Service's Settings as one object, the shape `set --patch` takes. Settings
/// without a value are left out.
pub(crate) fn values(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service: &SavedServiceIntent,
    intent: &SavedEnvironmentIntent,
) -> Result<Map<String, Value>, RpcError> {
    let mut values: Map<String, Value> = ServiceSetting::ALL
        .into_iter()
        .filter(|setting| setting.applies(&service.config))
        .map(|setting| (setting.name().to_owned(), setting.value(&service.config)))
        .filter(|(_, value)| !value.is_null())
        .collect();
    if ServiceSetting::PreferredBuilder.applies(&service.config) {
        let preferred = builders::preferred_value(tx, environment, &service.id)?;
        if !preferred.is_null() {
            values.insert(
                ServiceSetting::PreferredBuilder.name().to_owned(),
                preferred,
            );
        }
    }
    let env = variables::patch_values(service, intent);
    if !env.is_empty() {
        values.insert("env".to_owned(), Value::Object(env));
    }
    Ok(values)
}
