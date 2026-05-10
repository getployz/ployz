use ployz_types::model::{DeployBaselineComponent, DeployPreviewBaseline, InstanceStatusRecord};
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
    pub expected_baseline: Option<DeployPreviewBaseline>,
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
    pub expected_baseline: Option<DeployPreviewBaseline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_baseline: Option<DeployPreviewBaseline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline_changed_components: Vec<DeployBaselineComponent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployFailureReason {
    NoEligiblePlacementTargets,
    DeployBaselineChanged,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::DeployPreviewBaselineComponents;

    fn test_baseline(service_sources: &str) -> DeployPreviewBaseline {
        DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: "manifest".into(),
            participants: "participants".into(),
            phases: "phases".into(),
            services: "services".into(),
            service_sources: service_sources.into(),
            volumes: "volumes".into(),
            volume_moves: "moves".into(),
            volume_clones: "clones".into(),
        })
    }

    #[test]
    fn deploy_options_preserve_expected_baseline() {
        let baseline = test_baseline("sources");
        let options = DeployOptions {
            expected_baseline: Some(baseline.clone()),
            ..DeployOptions::default()
        };

        let json = serde_json::to_value(&options).expect("serialize deploy options");

        assert_eq!(
            json["expected_baseline"]["fingerprint"],
            serde_json::json!(baseline.fingerprint)
        );
        let roundtrip: DeployOptions =
            serde_json::from_value(json).expect("deserialize deploy options");
        assert_eq!(roundtrip.expected_baseline, Some(baseline));
    }

    #[test]
    fn deploy_failure_payload_serializes_baseline_details() {
        let expected = test_baseline("old");
        let actual = test_baseline("new");
        let payload = DeployFailurePayload {
            reason: DeployFailureReason::DeployBaselineChanged,
            expected_baseline: Some(expected.clone()),
            actual_baseline: Some(actual.clone()),
            baseline_changed_components: vec![DeployBaselineComponent::ServiceSources],
        };

        let json = serde_json::to_value(&payload).expect("serialize deploy failure");

        assert_eq!(json["reason"], serde_json::json!("deploy_baseline_changed"));
        assert_eq!(
            json["expected_baseline"]["fingerprint"],
            serde_json::json!(expected.fingerprint)
        );
        assert_eq!(
            json["actual_baseline"]["fingerprint"],
            serde_json::json!(actual.fingerprint)
        );
        assert_eq!(
            json["baseline_changed_components"],
            serde_json::json!(["service_sources"])
        );
    }
}
