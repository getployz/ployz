//! An Environment's Settings as `get` shows them: every Setting of every Service,
//! with its value and default.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
}

/// An Environment's Settings in Working State.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct EnvironmentView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// Every Setting asked for, by Service name then Setting.
    pub settings: Vec<SettingRow>,
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
    let mut settings = Vec::new();
    for service in services {
        for setting in ServiceSetting::ALL {
            if only.is_none_or(|only| only == setting) {
                settings.push(SettingRow {
                    path: SettingPath::of(&service.slug, setting)?,
                    value: setting.value(&service.config),
                    default: setting.default(),
                    apply: setting.apply(),
                });
            }
        }
    }
    Ok(EnvironmentView {
        environment: environment.summary,
        settings,
    })
}
