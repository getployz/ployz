//! Review projections, provenance, and executable discard descriptions shared with Cloud.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::EnvironmentNodeType;

/// Stable owner identity used across all comparison roles.
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

/// One owner with an authored configuration or an explicit absence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewNodeProjection {
    pub node: ReviewNodeIdentity,
    #[ts(type = "CompiledNodeConfig | null")]
    pub config: Option<Value>,
}

/// A revision token and the node facts observed at that revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewStateProjection {
    pub token: String,
    pub nodes: Vec<ReviewNodeProjection>,
}

/// Distinguishes no publication from a specific saved revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ReviewSavedProjection {
    NoSavedState {
        token: String,
        nodes: Vec<ReviewNodeProjection>,
    },
    SavedRevision {
        token: String,
        nodes: Vec<ReviewNodeProjection>,
        saved_state_snapshot_id: String,
    },
}

impl ReviewSavedProjection {
    pub(super) fn state(&self) -> ReviewStateProjection {
        match self {
            Self::NoSavedState { token, nodes } | Self::SavedRevision { token, nodes, .. } => {
                ReviewStateProjection {
                    token: token.clone(),
                    nodes: nodes.clone(),
                }
            }
        }
    }
}

/// Separate working, saved, applied, introduction, and runtime evidence for review.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSetInput {
    pub working: ReviewStateProjection,
    pub saved: ReviewSavedProjection,
    pub applied: ReviewStateProjection,
    pub node_introductions: ReviewStateProjection,
    pub runtime_observed: Option<ReviewStateProjection>,
    #[serde(default)]
    #[ts(optional)]
    pub runtime_observations: Option<ReviewRuntimeObservations>,
}

/// Adapter-supplied presence and setting evidence; not an authored restore baseline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewRuntimeObservations {
    pub token: String,
    #[serde(default)]
    #[ts(optional)]
    pub presence: Option<Vec<ReviewRuntimePresence>>,
    pub settings: Vec<ReviewRuntimeSetting>,
}

/// Whether a node is present in the supplied observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPresence {
    Present,
    Absent,
}

/// Applied and observed existence of the same owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReviewRuntimePresence {
    pub node: ReviewNodeIdentity,
    pub applied: ReviewPresence,
    pub observed: ReviewPresence,
}

/// Display-safe runtime setting evidence supplied by an observation adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewRuntimeSetting {
    pub node: ReviewNodeIdentity,
    pub setting: String,
    pub label: String,
    pub applied_value: Option<String>,
    pub observed_value: Option<String>,
}

/// The origin of a fact used in a change comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRole {
    Working,
    Saved,
    Applied,
    RuntimeObservation,
    NodeIntroduction,
}

/// A comparison role paired with the revision that supplied its evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewSource {
    pub role: ReviewRole,
    pub token: String,
}

/// The two evidence sources whose differences form a review slice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewProvenance {
    #[ts(type = "{ role: Exclude<ReviewRole, 'node_introduction'>; token: string }")]
    pub baseline: ReviewSource,
    #[ts(type = "{ role: Exclude<ReviewRole, 'node_introduction'>; token: string }")]
    pub target: ReviewSource,
}

/// The node and setting to which a change belongs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewSettingOwner {
    pub node: ReviewNodeIdentity,
    pub setting: String,
}

/// The saved revision that a discard operation is authorized to replace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReviewSavedBasis {
    SavedRevision { saved_state_snapshot_id: String },
}

/// Chooses a working edit or a saved revision as the discard destination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum ReviewDiscardTarget {
    Working,
    Saved { basis: ReviewSavedBasis },
}

/// Whether discarding restores a prior owner or removes a new owner.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewNodeDiscardKind {
    Restore,
    Delete,
}

/// One node-level discard operation with its destination and owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewNodeDiscardPlan {
    pub kind: ReviewNodeDiscardKind,
    pub node: ReviewNodeIdentity,
    #[serde(flatten)]
    pub target: ReviewDiscardTarget,
}

/// An authored configuration to restore for one setting owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewSettingDiscardPlan {
    pub kind: ReviewSettingDiscardKind,
    pub owner: ReviewSettingOwner,
    #[ts(type = "CompiledNodeConfig")]
    pub config: Value,
    #[serde(flatten)]
    pub target: ReviewDiscardTarget,
}

/// Owner creation, update, deletion, or no lifecycle difference.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewLifecycleKind {
    Create,
    Update,
    Delete,
    None,
}

/// The owner whose existence changed, independently of its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewLifecycleOwner {
    pub node: ReviewNodeIdentity,
}

/// An owner-level change with a stable review-row identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewLifecycleChange {
    pub id: String,
    pub owner: ReviewLifecycleOwner,
    pub kind: ReviewLifecycleKind,
}

/// One setting difference and its optional authored restore operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSettingChange {
    pub id: String,
    pub owner: ReviewSettingOwner,
    /// Runtime adapter labels are retained. Authored labels are rendered by the consumer.
    pub label: Option<String>,
    pub kind: ReviewSettingKind,
    pub baseline_value: Value,
    pub target_value: Value,
    pub baseline_source: Option<ReviewSource>,
    pub discard_plan: Option<ReviewSettingDiscardPlan>,
}

/// The two existence facts underlying a node comparison.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewNodePresence {
    pub baseline: ReviewPresence,
    pub target: ReviewPresence,
}

/// Lifecycle and setting differences for one owner, plus any node discard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewNodeChange {
    pub id: String,
    pub node: ReviewNodeIdentity,
    pub presence: ReviewNodePresence,
    pub lifecycle: ReviewLifecycleChange,
    pub settings: Vec<ReviewSettingChange>,
    pub discard_plan: Option<ReviewNodeDiscardPlan>,
}

/// The executable node and setting discards exposed by a review slice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewDiscardPlans {
    pub nodes: Vec<ReviewNodeDiscardPlan>,
    pub settings: Vec<ReviewSettingDiscardPlan>,
}

/// One provenance pair with grouped changes, counts, and discard operations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewChangeSlice {
    pub provenance: ReviewProvenance,
    pub groups: Vec<ReviewNodeChange>,
    pub lifecycle_count: usize,
    pub setting_count: usize,
    pub total_count: usize,
    pub discard_plans: ReviewDiscardPlans,
}

/// Unsaved edits, pending deployment changes, and runtime drift kept separate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewChangeSet {
    pub unsaved: ReviewChangeSlice,
    pub pending: ReviewChangeSlice,
    pub drift: ReviewChangeSlice,
    pub discard_all_plan: ReviewAggregateDiscardPlan,
}

/// A working-state owner reset used by aggregate discard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewWorkingNodePlan {
    pub kind: ReviewNodeDiscardKind,
    pub target: ReviewWorkingTarget,
    pub node: ReviewNodeIdentity,
}

/// The working operation associated with one aggregate discard owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewAggregateNodePlan {
    pub node: ReviewNodeIdentity,
    pub working: ReviewWorkingNodePlan,
}

/// A saved-state node operation emitted by aggregate discard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSavedNodeOperation {
    pub kind: ReviewSavedOperationKind,
    pub node_type: EnvironmentNodeType,
    pub node_id: String,
}

/// A saved revision and its reviewed node discard operations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ReviewSavedDiscardCommand {
    pub kind: ReviewSavedCommandKind,
    pub basis: ReviewSavedBasis,
    pub operations: Vec<ReviewSavedNodeOperation>,
}

/// Working resets and an optional saved command for discarding all reviewed edits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewAggregateDiscardPlan {
    pub nodes: Vec<ReviewAggregateNodePlan>,
    pub saved_command: Option<ReviewSavedDiscardCommand>,
}

/// The serialized discriminator for a setting restore.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSettingDiscardKind {
    RestoreSetting,
}

/// The direction or runtime origin of a setting difference.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSettingKind {
    Add,
    Update,
    Remove,
    Drift,
}

/// The serialized destination of an aggregate working-state reset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewWorkingTarget {
    Working,
}

/// The serialized discriminator for a saved node operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSavedOperationKind {
    Node,
}

/// The serialized discriminator for a saved discard command.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSavedCommandKind {
    Discard,
}
