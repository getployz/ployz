use ployz_types::model::InstanceStatusRecord;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrateServiceMode {
    Apply,
    Preview,
    RenderManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrateServiceRequest {
    pub namespace: String,
    pub service: String,
    pub target_machine: String,
    pub mode: MigrateServiceMode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_files: Vec<String>,
    #[serde(default)]
    pub prune: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployNamespaceSnapshotPayload {
    pub instances: Vec<InstanceStatusRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployCandidateStartedPayload {
    pub status: InstanceStatusRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployFailurePayload {
    pub reason: DeployFailureReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployFailureReason {
    NoEligiblePlacementTargets,
}
