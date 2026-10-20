//! Configs as `config list` and `config inspect` show them: every Config in Working
//! State, and every deployed one a staged delete dropped from it, each with its
//! files, its mounts and what the next Deploy does to it.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{ReviewLifecycleKind, SavedConfigIntent, SavedEnvironmentIntent};
use ployz_core::{ConfigFileName, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::Actor;
use crate::config_item::{ConfigMountAt, ConfigSummary, summary};
use crate::error;
use crate::id::ConfigId;
use crate::review;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;

/// Every Config of an Environment. They are part of one bounded document, so the
/// list comes whole.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ConfigsQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// An Environment's Configs, by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigsView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// Its Configs, by name.
    pub configs: Vec<ConfigListing>,
}

/// One Config, where it is mounted, and what the next Deploy does to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigListing {
    #[serde(flatten)]
    pub config: ConfigSummary,
    /// The Services mounting it in Working State.
    pub mounts: Vec<ConfigMountAt>,
    /// Whether a Deploy applied it.
    pub deployed: bool,
    /// What the next Deploy does to it; none when it is deployed as it is.
    pub change: Option<ReviewLifecycleKind>,
    /// Each Service whose mount of it the next Deploy changes, as `diff` shows it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ConfigMountChange>>", optional)]
    pub mount_changes: Vec<ConfigMountChange>,
}

/// A Service's mount of a Config that the next Deploy adds, moves or removes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigMountChange {
    pub service: ServiceName,
    /// Where the last Deploy mounted it; none when it was not mounted.
    pub before: Option<String>,
    /// Where the next Deploy mounts it; none when it unmounts it.
    pub after: Option<String>,
}

/// One Config by name or exact `@UUID`, with its files' text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ConfigItemQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name, or `@UUID` for one exact identity.
    pub config: crate::ConfigRef,
}

/// One Config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ConfigItemView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    #[serde(flatten)]
    pub config: ConfigListing,
    /// The lineage its Environment copies share.
    pub lineage: ConfigId,
    /// Each file's text as written, references by Service name.
    pub contents: BTreeMap<ConfigFileName, String>,
}

pub(crate) fn configs(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &ConfigsQuery,
) -> Result<ConfigsView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let configs = listed(tx, &environment)?
        .into_iter()
        .map(|(listing, _)| listing)
        .collect();
    Ok(ConfigsView {
        environment: environment.summary,
        configs,
    })
}

pub(crate) fn config(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &ConfigItemQuery,
) -> Result<ConfigItemView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let found = listed(tx, &environment)?
        .into_iter()
        .filter(|(_, node)| query.config.matches(node))
        .min_by_key(|(listing, _)| listing.change == Some(ReviewLifecycleKind::Delete));
    let Some((listing, node)) = found else {
        return Err(match &query.config {
            crate::ConfigRef::Name(name) => environment
                .config(name)
                .err()
                .unwrap_or_else(|| error::corrupt("Config listing")),
            crate::ConfigRef::Id(id) => error::not_found(
                "No Config with this identity",
                serde_json::json!({ "config": id }),
            ),
        });
    };
    let names = environment.shown_names();
    Ok(ConfigItemView {
        environment: environment.summary,
        lineage: ConfigId::parse(node.resource_lineage_id.as_str())
            .map_err(|_| error::corrupt("Config lineage"))?,
        contents: node
            .files
            .iter()
            .map(|(name, file)| {
                (
                    name.clone(),
                    ployz_core::config::render_variable_parts(&file.content, &names),
                )
            })
            .collect(),
        config: listing,
    })
}

fn listed(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
) -> Result<Vec<(ConfigListing, SavedConfigIntent)>, RpcError> {
    let review = review::review(tx, environment)?;
    let working = &environment.working;
    let names = environment.names();
    let removed = review
        .head
        .intent
        .configs
        .iter()
        .filter(|node| {
            working
                .configs
                .iter()
                .all(|kept| kept.resource_id != node.resource_id)
        })
        .cloned();
    let mut listed = working
        .configs
        .iter()
        .cloned()
        .chain(removed)
        .map(|node| {
            let listing = ConfigListing {
                config: summary(&node, &names)?,
                mounts: mounts(working, &node.resource_id)?,
                deployed: review
                    .head
                    .applied
                    .configs
                    .iter()
                    .any(|applied| applied.resource_id == node.resource_id),
                change: review
                    .view
                    .changes
                    .iter()
                    .find(|change| change.node.id == node.resource_id)
                    .map(|change| change.lifecycle),
                mount_changes: mount_changes(working, &review.head.intent, &node.resource_id)?,
            };
            Ok((listing, node))
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    listed.sort_by(|a, b| a.0.config.name.cmp(&b.0.config.name));
    Ok(listed)
}

/// Each Service whose mount of Config `config` differs between `head`, what the
/// last Deploy asked for, and `working`.
fn mount_changes(
    working: &SavedEnvironmentIntent,
    head: &SavedEnvironmentIntent,
    config: &str,
) -> Result<Vec<ConfigMountChange>, RpcError> {
    let mut seen = BTreeSet::new();
    let mut changes = Vec::new();
    for service in working.services.iter().chain(&head.services) {
        if !seen.insert(service.lineage_id.as_str()) {
            continue;
        }
        let dir = |intent: &SavedEnvironmentIntent| {
            intent
                .services
                .iter()
                .find(|other| other.lineage_id == service.lineage_id)?
                .config_attachments
                .iter()
                .find(|mount| mount.config_resource_id == config)
                .map(|mount| mount.mount_dir.to_string())
        };
        let (before, after) = (dir(head), dir(working));
        if before != after {
            changes.push(ConfigMountChange {
                service: ServiceName::parse(service.slug.as_str())
                    .map_err(|_| error::corrupt("Service name"))?,
                before,
                after,
            });
        }
    }
    changes.sort_by(|a, b| a.service.cmp(&b.service));
    Ok(changes)
}

fn mounts(working: &SavedEnvironmentIntent, config: &str) -> Result<Vec<ConfigMountAt>, RpcError> {
    let mut mounts = working
        .services
        .iter()
        .flat_map(|service| {
            service
                .config_attachments
                .iter()
                .filter(|mount| mount.config_resource_id == config)
                .map(move |mount| {
                    Ok(ConfigMountAt {
                        service: ServiceName::parse(service.slug.as_str())
                            .map_err(|_| error::corrupt("Service name"))?,
                        dir: mount.mount_dir.to_string(),
                    })
                })
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    mounts.sort_by(|a, b| a.service.cmp(&b.service));
    Ok(mounts)
}
