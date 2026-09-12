//! Publication identity and destructive review contracts.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use super::{ConfigError, EnvironmentNodeType};

/// The saved-state revision against which a publication was reviewed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PublicationBasis {
    NoSavedState,
    SavedRevision { saved_state_snapshot_id: String },
}

/// Check whether the reviewed publication basis still identifies the latest saved revision.
#[must_use]
pub fn publication_basis_matches(basis: &PublicationBasis, latest: Option<&str>) -> bool {
    match basis {
        PublicationBasis::NoSavedState => latest.is_none(),
        PublicationBasis::SavedRevision {
            saved_state_snapshot_id,
        } => latest == Some(saved_state_snapshot_id),
    }
}

/// Applied Service and Volume identities whose removal requires explicit authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestructivePublication {
    pub service_ids: Vec<String>,
    pub volume_ids: Vec<String>,
}

/// Node presence evidence supplied to destructive publication review.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationNode {
    node_type: EnvironmentNodeType,
    node_id: String,
    #[serde(default = "present_config")]
    config: Value,
}

fn present_config() -> Value {
    json!({})
}

/// Working, saved, and applied ownership facts used to find destructive edits.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestructivePublicationInput {
    working_nodes: Vec<PublicationNode>,
    saved_nodes: Vec<PublicationNode>,
    applied_nodes: Vec<PublicationNode>,
}

/// Find applied Service and Volume owners absent from working intent but still present in saved state.
#[must_use]
pub fn destructive_publication(input: DestructivePublicationInput) -> DestructivePublication {
    let key = |n: &PublicationNode| format!("{}:{}", n.node_type.as_str(), n.node_id);
    let working: BTreeSet<_> = input
        .working_nodes
        .iter()
        .filter(|n| !n.config.is_null())
        .map(key)
        .collect();
    let applied: BTreeSet<_> = input
        .applied_nodes
        .iter()
        .filter(|n| !n.config.is_null())
        .map(key)
        .collect();
    let mut result = DestructivePublication {
        service_ids: Vec::new(),
        volume_ids: Vec::new(),
    };
    for node in input
        .saved_nodes
        .iter()
        .filter(|n| !n.config.is_null() && !working.contains(&key(n)) && applied.contains(&key(n)))
    {
        match node.node_type {
            EnvironmentNodeType::Service => result.service_ids.push(node.node_id.clone()),
            EnvironmentNodeType::Volume => result.volume_ids.push(node.node_id.clone()),
            EnvironmentNodeType::VariableGroup => {}
        }
    }
    result.service_ids.sort();
    result.volume_ids.sort();
    result
}

/// Explain a changed or duplicated destructive owner set, or return None when it still matches.
#[must_use]
pub fn destructive_publication_mismatch(
    expected: DestructivePublication,
    reviewed: DestructivePublication,
) -> Option<&'static str> {
    for (expected, reviewed) in [
        (expected.service_ids, reviewed.service_ids),
        (expected.volume_ids, reviewed.volume_ids),
    ] {
        let expected_set: BTreeSet<_> = expected.iter().collect();
        let reviewed_set: BTreeSet<_> = reviewed.iter().collect();
        if expected_set.len() != expected.len() || reviewed_set.len() != reviewed.len() {
            return Some("A destructive Save review contains duplicate Environment Nodes.");
        }
        if expected_set != reviewed_set {
            return Some(
                "The set of deployed Environment Nodes awaiting removal changed after review.",
            );
        }
    }
    None
}

/// One node captured in the reviewed working surface.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewedWorkingNode {
    node_type: EnvironmentNodeType,
    node_id: String,
    node_lineage_id: String,
    config_version: u8,
    config: Value,
}

/// Working nodes and private revision markers used to detect stale review.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewedWorkingState {
    node_snapshots: Vec<ReviewedWorkingNode>,
    #[serde(default)]
    revision_markers: Vec<String>,
}

/// Canonicalize only the reviewed surface. Existing backing-row revision markers
/// detect changes hidden by redaction; ciphertext never participates in review.
///
/// # Errors
/// Returns ConfigError if the reviewed surface cannot be serialized as JSON.
pub fn canonical_reviewed_working_state(
    mut state: ReviewedWorkingState,
) -> Result<String, ConfigError> {
    state
        .node_snapshots
        .sort_by_key(|n| format!("{}:{}", n.node_type.as_str(), n.node_id));
    state.revision_markers.sort();
    fn reviewable(value: Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.into_iter().map(reviewable).collect()),
            Value::Object(values) => {
                let sorted: BTreeMap<_, _> = values
                    .into_iter()
                    .filter(|(key, _)| key != "encryptedValue" && key != "parts")
                    .map(|(key, value)| (key, reviewable(value)))
                    .collect();
                Value::Object(sorted.into_iter().collect())
            }
            value @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => value,
        }
    }
    serde_json::to_string(&reviewable(json!(state)))
        .map_err(|_| ConfigError::at("review", "Reviewed state must be JSON"))
}

/// Decode an explicit no-publication or saved-revision basis.
///
/// # Errors
/// Returns ConfigError for an invalid basis shape or malformed saved revision identity.
pub fn parse_publication_basis(value: Value) -> Result<PublicationBasis, ConfigError> {
    let basis: PublicationBasis = serde_json::from_value(value)
        .map_err(|_| ConfigError::at("basis", "Invalid publication basis"))?;
    if let PublicationBasis::SavedRevision {
        saved_state_snapshot_id,
    } = &basis
        && uuid::Uuid::parse_str(saved_state_snapshot_id).is_err()
    {
        return Err(ConfigError::at("basis", "Invalid publication identity"));
    }
    Ok(basis)
}

/// Whether to always publish or reuse an equivalent saved revision.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationRevisionPolicy {
    AlwaysCreate,
    ReuseLatestIfEquivalent,
}

/// Authored intent paired with its separately reviewed destructive authority.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicationCandidate {
    intent: Value,
    volume_deletion_authorizations: Value,
}

/// Decide whether canonical authored intent and destructive authority permit revision reuse.
///
/// # Errors
/// Returns ConfigError when either compared authored document fails admission.
pub fn reuse_publication(
    policy: PublicationRevisionPolicy,
    current: PublicationCandidate,
    latest: Option<PublicationCandidate>,
) -> Result<bool, ConfigError> {
    let Some(latest) = latest else {
        return Ok(false);
    };
    if matches!(policy, PublicationRevisionPolicy::AlwaysCreate) {
        return Ok(false);
    }
    Ok(
        super::canonicalize_environment_intent(super::parse_environment_intent(current.intent)?)
            == super::canonicalize_environment_intent(super::parse_environment_intent(
                latest.intent,
            )?)
            && current.volume_deletion_authorizations == latest.volume_deletion_authorizations,
    )
}
