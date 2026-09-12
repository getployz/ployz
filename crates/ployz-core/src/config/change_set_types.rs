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

/// Submitted is the authored revision of the latest queued or running attempt.
/// It changes the editing baseline without advancing confirmed Applied State.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSetInput {
    pub working: ReviewStateProjection,
    pub saved: ReviewStateProjection,
    pub applied: ReviewStateProjection,
    pub submitted: Option<ReviewStateProjection>,
    pub node_introductions: ReviewStateProjection,
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
    pub settings: Vec<ServiceSettingChange>,
}

/// Consumers render this list and discard by scope; they never merge state layers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewChangeSet {
    pub groups: Vec<ReviewNodeChange>,
    pub total_count: usize,
    pub can_save: bool,
}
