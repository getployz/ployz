//! An Environment's Settings as `get` shows them. Depth decides how much: the
//! whole Environment lists only Settings that differ from their default (all of
//! them with `all`); one Service lists every Setting and its `values` object; one
//! Setting lists itself.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::Actor;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{Apply, ServiceSetting, SettingPath};
use crate::storage::Tx;

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
    let only = path.and_then(SettingPath::setting);
    let values = path
        .filter(|path| path.setting().is_none())
        .and_then(|_| services.first())
        .map(|service| {
            ServiceSetting::ALL
                .into_iter()
                .map(|setting| (setting.name().to_owned(), setting.value(&service.config)))
                .filter(|(_, value)| !value.is_null())
                .collect()
        });
    let whole = path.is_none() && !query.all;
    let mut settings = Vec::new();
    for service in services {
        for setting in ServiceSetting::ALL {
            let value = setting.value(&service.config);
            let default = setting.default();
            if only.is_none_or(|only| only == setting) && !(whole && value == default) {
                settings.push(SettingRow {
                    path: SettingPath::of(&service.slug, setting)?,
                    value,
                    default,
                    apply: setting.apply(),
                });
            }
        }
    }
    Ok(EnvironmentView {
        environment: environment.summary,
        settings,
        values,
    })
}
