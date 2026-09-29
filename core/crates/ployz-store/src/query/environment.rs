//! An Environment's Settings as `get` shows them. Depth decides how much: the
//! whole Environment lists only Settings that differ from their default (all of
//! them with `all`); one Service lists every Setting and its `values` object; one
//! Setting lists itself.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Actor;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{self, Apply, ServiceSetting, SettingPath};
use crate::storage::Tx;

/// Read an Environment's Working State, narrowed to `SERVICE` or `SERVICE.SETTING`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    #[serde(default)]
    pub path: Option<String>,
    /// Include Settings at their default across the whole Environment.
    #[serde(default)]
    pub all: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentView {
    pub environment: EnvironmentSummary,
    pub settings: Vec<SettingRow>,
    /// For one Service: its Settings as one object, the shape `set --patch` takes.
    /// Settings without a value are left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Map<String, Value>>,
}

/// One Setting's current Working State value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingRow {
    pub path: String,
    pub value: Value,
    pub default: Value,
    pub apply: Apply,
}

pub(super) fn run(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &EnvironmentQuery,
) -> Result<EnvironmentView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let path = query.path.as_deref().map(SettingPath::parse).transpose()?;
    let mut services = environment.working.services.iter().collect::<Vec<_>>();
    services.sort_by(|a, b| a.slug.cmp(&b.slug));
    if let Some(path) = &path {
        services.retain(|service| service.slug == path.service.as_str());
        if services.is_empty() {
            return Err(settings::no_service(
                &path.service,
                &environment.summary.name,
                &environment.working,
            ));
        }
    }
    let values = path
        .as_ref()
        .filter(|path| path.setting.is_none())
        .and_then(|_| services.first())
        .map(|service| {
            ServiceSetting::ALL
                .into_iter()
                .map(|setting| (setting.name().to_owned(), setting.value(&service.config)))
                .filter(|(_, value)| !value.is_null())
                .collect()
        });
    let whole = path.is_none() && !query.all;
    let settings = services
        .into_iter()
        .flat_map(|service| {
            ServiceSetting::ALL
                .into_iter()
                .filter(|setting| {
                    path.as_ref()
                        .and_then(|path| path.setting)
                        .is_none_or(|only| only == *setting)
                })
                .map(|setting| SettingRow {
                    path: SettingPath::of(&service.slug, setting),
                    value: setting.value(&service.config),
                    default: setting.default(),
                    apply: setting.apply(),
                })
        })
        .filter(|row| !whole || row.value != row.default)
        .collect::<Vec<_>>();
    Ok(EnvironmentView {
        environment: environment.summary,
        settings,
        values,
    })
}
