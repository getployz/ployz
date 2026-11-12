//! One authored change set, shared by Cloud's canvas and review actions.
use super::{At, ServiceSettingChange, Setting};
use crate::ConfigFileName;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Environment Node families with distinct configuration semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentNodeType {
    Service,
    Volume,
    Config,
}

impl EnvironmentNodeType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Volume => "volume",
            Self::Config => "config",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewNodeIdentity {
    #[serde(rename = "type")]
    pub node_type: EnvironmentNodeType,
    pub id: String,
}
impl ReviewNodeIdentity {
    pub(super) fn key(&self) -> String {
        format!("{}:{}", self.node_type.as_str(), self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewNodeProjection {
    pub node: ReviewNodeIdentity,
    #[ts(type = "CompiledNodeConfig | null")]
    pub config: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewStateProjection {
    pub token: String,
    pub nodes: Vec<ReviewNodeProjection>,
}

/// Head is `submitted` (the queued or running attempt's revision) when one exists, else `applied`.
/// Only nodes without Saved or Applied State may compare against their Introduction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSetInput {
    pub working: ReviewStateProjection,
    pub applied: ReviewStateProjection,
    pub saved: Option<ReviewStateProjection>,
    pub submitted: Option<ReviewStateProjection>,
    pub node_introductions: ReviewStateProjection,
}

/// The configuration a node's changed settings and field discards compare against.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewComparisonRole {
    /// Compare with the latest submitted revision, or the applied revision if none exists.
    Head,
    /// Compare the draft or a History revision with immutable Saved State.
    Saved,
    /// Compare an unsaved, unapplied node with its initial authored configuration.
    Introduction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewLifecycleKind {
    Create,
    Update,
    Delete,
}

/// Where in its node a change's row is. A mount names what it mounts, not its Volume or
/// Config lineage, which only the Store knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A Volume's or a Config's name.
    Name,
    Storage,
    File(ConfigFileName),
    Setting(Setting),
    /// The custom domain with this route id, which Sync moves as the `routes` Setting.
    Route(String),
    Variable(String),
    Mount {
        volume_resource_id: String,
    },
    ConfigMount {
        config_resource_id: String,
    },
}

impl Place {
    /// Its Sync row's place in the node; none for a mount, whose lineage is the Store's
    /// to find.
    #[must_use]
    pub fn at(&self) -> Option<At> {
        match self {
            Self::Name => Some(At::Name),
            Self::Storage => Some(At::Storage),
            Self::File(file) => Some(At::File(file.clone())),
            Self::Setting(setting) => Some(At::Setting(*setting)),
            Self::Route(_) => Some(At::Setting(Setting::Routes)),
            Self::Variable(key) => Some(At::Variable(key.clone())),
            Self::Mount { .. } | Self::ConfigMount { .. } => None,
        }
    }

    /// The `path` of its row, relative to its node.
    #[must_use]
    pub fn path(&self) -> String {
        match self {
            Self::Name => "name".to_owned(),
            Self::Storage => "storage".to_owned(),
            Self::File(file) => format!("files.{file}"),
            Self::Setting(setting) => setting.path().to_owned(),
            Self::Route(id) => format!("routes.{id}"),
            Self::Variable(key) => format!("env.{key}"),
            Self::Mount { volume_resource_id } => format!("mounts.{volume_resource_id}"),
            Self::ConfigMount { config_resource_id } => format!("configs.{config_resource_id}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewNodeChange {
    pub node: ReviewNodeIdentity,
    pub lifecycle: ReviewLifecycleKind,
    /// What `settings` and discard compare against; `None` only when nothing exists to compare.
    pub comparison: Option<ReviewComparisonRole>,
    /// Each change and where in its node its row is.
    pub settings: Vec<(ServiceSettingChange, Place)>,
}

/// Consumers render this list and discard by scope; they never merge state layers.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewChangeSet {
    pub groups: Vec<ReviewNodeChange>,
    pub total_count: usize,
    /// Token of the Head this set was computed against; discard must present it back.
    pub head_token: String,
}
