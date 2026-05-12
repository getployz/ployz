use ployz_types::model::{
    BranchEnvironmentRecord, DeployBaselineComponent, DeployId, DeployPreviewBaseline,
    InstanceStatusRecord, PreparedDeployRecord, PreparedDeployState,
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
pub struct BranchApplyPreparedRequest {
    pub prepared_deploy_id: DeployId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchEnvironmentStatusRequest {
    pub target_namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchEnvironmentPayload {
    pub environment: BranchEnvironmentRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchEnvironmentListPayload {
    pub environments: Vec<BranchEnvironmentRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployCandidateStartedPayload {
    pub status: InstanceStatusRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployFailurePayload {
    NoEligiblePlacementTargets,
    DeployBaselineChanged {
        expected_baseline: DeployPreviewBaseline,
        actual_baseline: DeployPreviewBaseline,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        baseline_changed_components: Vec<DeployBaselineComponent>,
    },
    PreparedDeployMissing {
        prepared_deploy_id: DeployId,
    },
    PreparedDeployNotApplicable {
        prepared_deploy_id: DeployId,
        prepared_deploy_state: PreparedDeployState,
    },
    PreparedDeployExpired {
        prepared_deploy_id: DeployId,
        prepared_deploy_state: PreparedDeployState,
        prepared_deploy_expires_at: u64,
    },
    PreparedDeployInvalid {
        prepared_deploy_id: DeployId,
    },
    DeployImageDigestRequired {
        service: String,
        image: String,
    },
    DeployImageAvailabilityMissing {
        service: String,
        slot_id: String,
        machine_id: String,
        image: String,
        digest: String,
    },
    DeployImageAvailabilityNotPresent {
        service: String,
        slot_id: String,
        machine_id: String,
        image: String,
        digest: String,
        state: String,
    },
}

impl DeployFailurePayload {
    #[must_use]
    pub fn reason(&self) -> DeployFailureReason {
        match self {
            Self::NoEligiblePlacementTargets => DeployFailureReason::NoEligiblePlacementTargets,
            Self::DeployBaselineChanged { .. } => DeployFailureReason::DeployBaselineChanged,
            Self::PreparedDeployMissing { .. } => DeployFailureReason::PreparedDeployMissing,
            Self::PreparedDeployNotApplicable { .. } => {
                DeployFailureReason::PreparedDeployNotApplicable
            }
            Self::PreparedDeployExpired { .. } => DeployFailureReason::PreparedDeployExpired,
            Self::PreparedDeployInvalid { .. } => DeployFailureReason::PreparedDeployInvalid,
            Self::DeployImageDigestRequired { .. } => {
                DeployFailureReason::DeployImageDigestRequired
            }
            Self::DeployImageAvailabilityMissing { .. } => {
                DeployFailureReason::DeployImageAvailabilityMissing
            }
            Self::DeployImageAvailabilityNotPresent { .. } => {
                DeployFailureReason::DeployImageAvailabilityNotPresent
            }
        }
    }
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
        let payload = DeployFailurePayload::DeployBaselineChanged {
            expected_baseline: expected.clone(),
            actual_baseline: actual.clone(),
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

    #[test]
    fn deploy_prepare_and_apply_prepared_payloads_roundtrip() {
        let baseline = test_baseline("sources");
        let preview = DeployPreview {
            namespace: Namespace::new("prod"),
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
            prepared_deploy_id: DeployId::new("prepare-1"),
            namespace: Namespace::new("prod"),
            manifest_hash: "manifest".into(),
            manifest_json: "{}".into(),
            preview,
            baseline,
            coordinator_machine_id: ployz_types::model::MachineId::new("machine-a"),
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
            DeployId::new("prepare-1")
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
        let payload = DeployFailurePayload::DeployImageAvailabilityMissing {
            service: "web".into(),
            slot_id: "slot-0001".into(),
            machine_id: "machine-a".into(),
            image: "sha256:abc".into(),
            digest: "sha256:abc".into(),
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

    #[test]
    fn deploy_failure_payload_rejects_unrelated_variant_fields() {
        let json = serde_json::json!({
            "reason": "prepared_deploy_missing",
            "prepared_deploy_id": "prepare-1",
            "state": "absent"
        });

        serde_json::from_value::<DeployFailurePayload>(json)
            .expect_err("prepared deploy missing cannot carry image state");
    }
}
