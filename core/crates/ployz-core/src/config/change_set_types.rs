//! One authored change set, shared by Cloud's canvas and review actions.
use super::{EnvironmentNodeType, ServiceSettingChange};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewNodeChange {
    pub node: ReviewNodeIdentity,
    pub lifecycle: ReviewLifecycleKind,
    /// What `settings` and discard compare against; `None` only when nothing exists to compare.
    pub comparison: Option<ReviewComparisonRole>,
    pub settings: Vec<ServiceSettingChange>,
}

/// Consumers render this list and discard by scope; they never merge state layers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewChangeSet {
    pub groups: Vec<ReviewNodeChange>,
    pub total_count: usize,
    /// Token of the Head this set was computed against; discard must present it back.
    pub head_token: String,
}
