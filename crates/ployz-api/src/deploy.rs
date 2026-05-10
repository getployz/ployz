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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_service_source_fingerprint: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_service_source_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_service_source_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployFailureReason {
    NoEligiblePlacementTargets,
    ServiceSourceBaselineChanged,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_options_preserve_expected_service_source_fingerprint() {
        let options = DeployOptions {
            expected_service_source_fingerprint: Some("abc123".into()),
            ..DeployOptions::default()
        };

        let json = serde_json::to_value(&options).expect("serialize deploy options");

        assert_eq!(
            json["expected_service_source_fingerprint"],
            serde_json::json!("abc123")
        );
        let roundtrip: DeployOptions =
            serde_json::from_value(json).expect("deserialize deploy options");
        assert_eq!(
            roundtrip.expected_service_source_fingerprint.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn deploy_failure_payload_serializes_service_source_baseline_details() {
        let payload = DeployFailurePayload {
            reason: DeployFailureReason::ServiceSourceBaselineChanged,
            expected_service_source_fingerprint: Some("old".into()),
            actual_service_source_fingerprint: Some("new".into()),
        };

        let json = serde_json::to_value(&payload).expect("serialize deploy failure");

        assert_eq!(
            json["reason"],
            serde_json::json!("service_source_baseline_changed")
        );
        assert_eq!(
            json["expected_service_source_fingerprint"],
            serde_json::json!("old")
        );
        assert_eq!(
            json["actual_service_source_fingerprint"],
            serde_json::json!("new")
        );
    }
}
