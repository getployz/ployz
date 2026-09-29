//! An Environment's Settings as `get` shows them: every Setting of every Service,
//! with its value and default.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Actor;
use crate::error;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{Apply, ServiceSetting, SettingPath};
use crate::storage::Tx;

/// Read an Environment's Working State, narrowed to `SERVICE` or `SERVICE.SETTING`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentView {
    pub environment: EnvironmentSummary,
    pub settings: Vec<SettingRow>,
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
            return Err(error::not_found(
                format!(
                    "No Service named {} in Environment {}",
                    path.service, environment.summary.name
                ),
                json!({ "services": environment.working.services.iter().map(|service| &service.slug).collect::<Vec<_>>() }),
            ));
        }
    }
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
        .collect();
    Ok(EnvironmentView {
        environment: environment.summary,
        settings,
    })
}
