use ployz_types::model::{
    DeployBaselineComponent, DeployId, DeployPreviewBaseline, InstanceStatusRecord,
    PreparedDeployRecord, PreparedDeployState,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrateServiceMode {
    Apply,
    Preview,
    RenderManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchNamespaceMode {
    Apply,
    Prepare,
    Preview,
    RenderManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchResourceMode {
    Fresh,
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchResourceModeOverride {
    pub name: String,
    pub mode: BranchResourceMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchNamespaceRequest {
    pub source_namespace: String,
    pub target_namespace: String,
    pub mode: BranchNamespaceMode,
    pub default_service_mode: BranchResourceMode,
    pub default_volume_mode: BranchResourceMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<BranchResourceModeOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<BranchResourceModeOverride>,
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
pub struct DeployPreparePayload {
    pub prepared: PreparedDeployRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployApplyPreparedRequest {
    pub prepared_deploy_id: DeployId,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_deploy_id: Option<DeployId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_deploy_state: Option<PreparedDeployState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_deploy_expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployFailureReason {
    NoEligiblePlacementTargets,
    DeployBaselineChanged,
    PreparedDeployMissing,
    PreparedDeployNotApplicable,
    PreparedDeployExpired,
    PreparedDeployInvalid,
    DeployImageDigestRequired,
    DeployImageAvailabilityMissing,
    DeployImageAvailabilityNotPresent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{DeployPreview, DeployPreviewBaselineComponents, PreparedDeployState};
    use ployz_types::spec::Namespace;

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
            prepared_deploy_id: None,
            prepared_deploy_state: None,
            prepared_deploy_expires_at: None,
            service: None,
            slot_id: None,
            machine_id: None,
            image: None,
            digest: None,
            state: None,
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

    #[test]
    fn deploy_prepare_and_apply_prepared_payloads_roundtrip() {
        let baseline = test_baseline("sources");
        let preview = DeployPreview {
            namespace: Namespace("prod".into()),
            manifest_hash: "manifest".into(),
            baseline: Some(baseline.clone()),
            participants: Vec::new(),
            phases: Vec::new(),
            services: Vec::new(),
            service_sources: Vec::new(),
            service_source_fingerprint: String::new(),
            service_branch_sources: Vec::new(),
            image_availability: Vec::new(),
            volume_moves: Vec::new(),
            volume_clones: Vec::new(),
            volume_clone_preflights: Vec::new(),
            warnings: Vec::new(),
        };
        let prepared = PreparedDeployRecord {
            prepared_deploy_id: DeployId("prepare-1".into()),
            namespace: Namespace("prod".into()),
            manifest_hash: "manifest".into(),
            manifest_json: "{}".into(),
            preview,
            baseline,
            coordinator_machine_id: ployz_types::model::MachineId("machine-a".into()),
            state: PreparedDeployState::Prepared,
            created_at: 1,
            expires_at: 2,
            updated_at: 1,
        };
        let payload = DeployPreparePayload {
            prepared: prepared.clone(),
        };
        let request = DeployApplyPreparedRequest {
            prepared_deploy_id: prepared.prepared_deploy_id.clone(),
        };

        let payload_json = serde_json::to_value(&payload).expect("serialize prepare payload");
        let request_json = serde_json::to_value(&request).expect("serialize apply prepared");

        assert_eq!(
            payload_json["prepared"]["prepared_deploy_id"],
            serde_json::json!("prepare-1")
        );
        assert_eq!(
            request_json["prepared_deploy_id"],
            serde_json::json!("prepare-1")
        );
        let payload_roundtrip: DeployPreparePayload =
            serde_json::from_value(payload_json).expect("deserialize prepare payload");
        let request_roundtrip: DeployApplyPreparedRequest =
            serde_json::from_value(request_json).expect("deserialize apply prepared");
        assert_eq!(payload_roundtrip.prepared, prepared);
        assert_eq!(
            request_roundtrip.prepared_deploy_id,
            DeployId("prepare-1".into())
        );
    }

    #[test]
    fn branch_namespace_request_roundtrips_modes_and_overrides() {
        let cases = [
            (BranchNamespaceMode::Apply, "apply"),
            (BranchNamespaceMode::Prepare, "prepare"),
            (BranchNamespaceMode::Preview, "preview"),
            (BranchNamespaceMode::RenderManifest, "render_manifest"),
        ];

        for (mode, serialized) in cases {
            let request = BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode,
                default_service_mode: BranchResourceMode::Branch,
                default_volume_mode: BranchResourceMode::Fresh,
                services: vec![BranchResourceModeOverride {
                    name: "worker".into(),
                    mode: BranchResourceMode::Fresh,
                }],
                volumes: vec![BranchResourceModeOverride {
                    name: "data".into(),
                    mode: BranchResourceMode::Branch,
                }],
            };

            let json = serde_json::to_value(&request).expect("serialize branch request");

            assert_eq!(json["mode"], serde_json::json!(serialized));
            assert_eq!(json["default_service_mode"], serde_json::json!("branch"));
            assert_eq!(json["default_volume_mode"], serde_json::json!("fresh"));
            assert_eq!(json["services"][0]["mode"], serde_json::json!("fresh"));
            assert_eq!(json["volumes"][0]["mode"], serde_json::json!("branch"));
            let roundtrip: BranchNamespaceRequest =
                serde_json::from_value(json).expect("deserialize branch request");
            assert_eq!(roundtrip.source_namespace, request.source_namespace);
            assert_eq!(roundtrip.target_namespace, request.target_namespace);
            assert_eq!(roundtrip.mode, request.mode);
            assert_eq!(roundtrip.services, request.services);
            assert_eq!(roundtrip.volumes, request.volumes);
        }
    }

    #[test]
    fn deploy_failure_payload_serializes_image_availability_details() {
        let payload = DeployFailurePayload {
            reason: DeployFailureReason::DeployImageAvailabilityMissing,
            expected_baseline: None,
            actual_baseline: None,
            baseline_changed_components: Vec::new(),
            prepared_deploy_id: None,
            prepared_deploy_state: None,
            prepared_deploy_expires_at: None,
            service: Some("web".into()),
            slot_id: Some("slot-0001".into()),
            machine_id: Some("machine-a".into()),
            image: Some("sha256:abc".into()),
            digest: Some("sha256:abc".into()),
            state: None,
        };

        let json = serde_json::to_value(&payload).expect("serialize deploy failure");

        assert_eq!(
            json["reason"],
            serde_json::json!("deploy_image_availability_missing")
        );
        assert_eq!(json["service"], serde_json::json!("web"));
        assert_eq!(json["slot_id"], serde_json::json!("slot-0001"));
        assert_eq!(json["machine_id"], serde_json::json!("machine-a"));
        assert_eq!(json["digest"], serde_json::json!("sha256:abc"));
        assert!(json.get("state").is_none());
    }
}
