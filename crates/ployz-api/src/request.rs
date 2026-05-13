use crate::build::{BuildLocalRequest, BuildMachineRequest};
use crate::deploy::{
    BranchApplyPreparedRequest, BranchEnvironmentStatusRequest, BranchNamespaceRequest,
    DeployApplyPreparedRequest, DeployOptions, MigrateServiceRequest,
};
use crate::image::{
    ImageDistributeRequest, ImageInspectRequest, ImagePushRequest, ImageReceiveSessionRequest,
    ImageReceivedImportRequest, ImageStatusRequest,
};
use crate::machine::{MachineAddOptions, MachineInstallOptions, MachineStoragePromoteRequest};
use crate::mesh::MeshBootstrapRequest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebugTickTask {
    PeerSync,
    Endpoints,
    Heartbeat,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    /// Liveness probe used by deploy reachability checks. Returns immediately
    /// with no side effects so callers can confirm the peer daemon is alive
    /// at decision time.
    Ping,
    Status,
    Doctor,
    DebugTick {
        task: DebugTickTask,
        repeat: u32,
    },
    MeshList,
    MeshStatus {
        network: String,
    },
    MeshJoin {
        token: String,
    },
    MeshReady {
        json: bool,
    },
    MeshCreate {
        network: String,
    },
    MeshInit {
        network: String,
    },
    MeshStart {
        network: String,
    },
    MeshStop {
        force: bool,
    },
    MeshDestroy {
        network: String,
    },
    MachineList,
    MachineInit {
        target: String,
        network: String,
        install: MachineInstallOptions,
    },
    MachineAdd {
        targets: Vec<String>,
        options: MachineAddOptions,
    },
    MachineStoragePromote {
        request: MachineStoragePromoteRequest,
    },
    MachineUpdate {
        ids: Vec<String>,
        version: String,
    },
    MachineActivate {
        target: String,
    },
    MachineDrain {
        target: String,
    },
    MachineStandby {
        target: String,
        force: bool,
    },
    MachineRemove {
        id: String,
        force: bool,
    },
    MachineRtt,
    MeshPeerRttSnapshot,
    MachineOperationList,
    MachineOperationGet {
        id: String,
    },
    MachineInviteCreate {
        ttl_secs: u64,
    },
    MachineInviteRevoke {
        invite_id: String,
    },
    MachineInviteList,
    MachineInviteImport {
        token: String,
    },
    MeshBootstrap {
        request: MeshBootstrapRequest,
    },
    AcmeChallengeReady {
        hostname: String,
        token: String,
    },
    AcmeHttp01Status {
        hostname: String,
    },
    MeshSelfRecord,
    DeployPreview {
        manifest_json: String,
        options: DeployOptions,
    },
    DeployPrepare {
        manifest_json: String,
    },
    DeployApply {
        manifest_json: String,
        options: DeployOptions,
    },
    DeployApplyPrepared {
        request: DeployApplyPreparedRequest,
    },
    DeployExport {
        namespace: String,
    },
    BranchNamespace {
        request: BranchNamespaceRequest,
    },
    BranchApplyPrepared {
        request: BranchApplyPreparedRequest,
    },
    BranchEnvironmentStatus {
        request: BranchEnvironmentStatusRequest,
    },
    BranchEnvironmentList,
    MigrateService {
        request: MigrateServiceRequest,
    },
    ImageStatus {
        request: ImageStatusRequest,
    },
    ImageInspect {
        request: ImageInspectRequest,
    },
    ImagePush {
        request: ImagePushRequest,
    },
    ImageDistribute {
        request: ImageDistributeRequest,
    },
    ImageReceiveSession {
        request: ImageReceiveSessionRequest,
    },
    ImageReceivedImport {
        request: ImageReceivedImportRequest,
    },
    ImageOperationGet {
        id: String,
    },
    ImageOperationList,
    BuildLocal {
        request: BuildLocalRequest,
    },
    BuildMachine {
        request: BuildMachineRequest,
    },
    BuildOperationGet {
        id: String,
    },
    BuildOperationList,
    RuntimeSubscribe,
    VolumeZfsInspect {
        namespace: String,
        volume: String,
        machine: Option<String>,
    },
    VolumeZfsSnapshot {
        namespace: String,
        volume: String,
        snapshot: String,
    },
    VolumeZfsSend {
        namespace: String,
        volume: String,
        snapshot: String,
        target_machine: String,
        from_snapshot: Option<String>,
    },
    VolumeZfsTransferGet {
        id: String,
    },
    VolumeZfsTransferList,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{BuildEnvValue, BuildInputs, BuildLocalRequest, BuildMachineRequest};
    use crate::deploy::{
        BranchNamespaceMode, BranchResourceMode, BranchResourceModeOverride, MigrateServiceMode,
    };
    use crate::image::{
        ImageDistributeRequest, ImageInspectRequest, ImagePushRequest, ImageReceiveSessionRequest,
        ImageReceivedImportRequest,
    };
    use ployz_model::{BuildMethod, ImageDigest, MachineId};
    use std::collections::BTreeMap;

    #[test]
    fn migrate_service_request_serializes_stable_wire_shape() {
        let request = DaemonRequest::MigrateService {
            request: MigrateServiceRequest {
                namespace: "prod".into(),
                service: "db".into(),
                target_machine: "machine-b".into(),
                mode: MigrateServiceMode::RenderManifest,
            },
        };

        let json = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(
            json,
            serde_json::json!({
                "MigrateService": {
                    "request": {
                        "namespace": "prod",
                        "service": "db",
                        "target_machine": "machine-b",
                        "mode": "render_manifest"
                    }
                }
            })
        );
        let roundtrip: DaemonRequest = serde_json::from_value(json).expect("deserialize request");
        assert!(matches!(
            roundtrip,
            DaemonRequest::MigrateService {
                request: MigrateServiceRequest {
                    mode: MigrateServiceMode::RenderManifest,
                    ..
                }
            }
        ));
    }

    #[test]
    fn branch_namespace_request_serializes_stable_wire_shape() {
        let request = DaemonRequest::BranchNamespace {
            request: BranchNamespaceRequest {
                source_namespace: "prod".into(),
                target_namespace: "pr-39".into(),
                mode: BranchNamespaceMode::RenderManifest,
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
            },
        };

        let json = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(
            json,
            serde_json::json!({
                "BranchNamespace": {
                    "request": {
                        "source_namespace": "prod",
                        "target_namespace": "pr-39",
                        "mode": "render_manifest",
                        "default_service_mode": "branch",
                        "default_volume_mode": "fresh",
                        "services": [
                            {"name": "worker", "mode": "fresh"}
                        ],
                        "volumes": [
                            {"name": "data", "mode": "branch"}
                        ]
                    }
                }
            })
        );
        let roundtrip: DaemonRequest = serde_json::from_value(json).expect("deserialize request");
        assert!(matches!(
            roundtrip,
            DaemonRequest::BranchNamespace {
                request: BranchNamespaceRequest {
                    mode: BranchNamespaceMode::RenderManifest,
                    ..
                }
            }
        ));
    }

    #[test]
    fn deploy_prepare_and_apply_prepared_requests_roundtrip() {
        let prepare = DaemonRequest::DeployPrepare {
            manifest_json: "{}".into(),
        };
        let apply = DaemonRequest::DeployApplyPrepared {
            request: DeployApplyPreparedRequest {
                prepared_deploy_id: ployz_model::DeployId::new("prepare-1"),
            },
        };

        let prepare_json = serde_json::to_value(&prepare).expect("serialize prepare");
        let apply_json = serde_json::to_value(&apply).expect("serialize apply prepared");

        assert_eq!(
            prepare_json,
            serde_json::json!({
                "DeployPrepare": {
                    "manifest_json": "{}"
                }
            })
        );
        assert_eq!(
            apply_json,
            serde_json::json!({
                "DeployApplyPrepared": {
                    "request": {
                        "prepared_deploy_id": "prepare-1"
                    }
                }
            })
        );
        let prepare_roundtrip: DaemonRequest =
            serde_json::from_value(prepare_json).expect("deserialize prepare");
        let apply_roundtrip: DaemonRequest =
            serde_json::from_value(apply_json).expect("deserialize apply prepared");
        assert!(matches!(
            prepare_roundtrip,
            DaemonRequest::DeployPrepare { .. }
        ));
        assert!(matches!(
            apply_roundtrip,
            DaemonRequest::DeployApplyPrepared { .. }
        ));
    }

    #[test]
    fn migrate_service_modes_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&MigrateServiceMode::Apply).expect("serialize apply"),
            "\"apply\""
        );
        assert_eq!(
            serde_json::to_string(&MigrateServiceMode::Preview).expect("serialize preview"),
            "\"preview\""
        );
        assert_eq!(
            serde_json::to_string(&MigrateServiceMode::RenderManifest)
                .expect("serialize render manifest"),
            "\"render_manifest\""
        );
    }

    #[test]
    fn image_push_request_round_trips_source_targets_and_expected_digest() {
        let request = DaemonRequest::ImagePush {
            request: ImagePushRequest {
                source_image: "registry.example/api:sha".into(),
                target_machines: vec![MachineId::new("machine-a"), MachineId::new("machine-b")],
                platform: None,
                expected_digest: Some(digest()),
            },
        };

        let json = serde_json::to_value(&request).expect("serialize image request");
        assert_eq!(
            json,
            serde_json::json!({
                "ImagePush": {
                    "request": {
                        "source_image": "registry.example/api:sha",
                        "target_machines": ["machine-a", "machine-b"],
                        "expected_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    }
                }
            })
        );
        let roundtrip: DaemonRequest = serde_json::from_value(json).expect("deserialize request");

        let DaemonRequest::ImagePush { request } = roundtrip else {
            panic!("expected image push request");
        };
        assert_eq!(request.source_image, "registry.example/api:sha");
        assert_eq!(
            request.target_machines,
            vec![MachineId::new("machine-a"), MachineId::new("machine-b")]
        );
        assert_eq!(
            request.expected_digest.expect("expected digest").as_str(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn image_distribute_request_round_trips_digest_source_and_targets() {
        let request = DaemonRequest::ImageDistribute {
            request: ImageDistributeRequest {
                digest: digest(),
                source_machine: MachineId::new("machine-a"),
                target_machines: vec![MachineId::new("machine-b")],
                platform: Some(ployz_model::ImagePlatform {
                    os: "linux".into(),
                    architecture: "amd64".into(),
                    variant: None,
                }),
            },
        };

        let json = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(
            json,
            serde_json::json!({
                "ImageDistribute": {
                    "request": {
                        "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "source_machine": "machine-a",
                        "target_machines": ["machine-b"],
                        "platform": {
                            "os": "linux",
                            "architecture": "amd64"
                        }
                    }
                }
            })
        );
        let roundtrip: DaemonRequest = serde_json::from_value(json).expect("deserialize request");

        let DaemonRequest::ImageDistribute { request } = roundtrip else {
            panic!("expected image distribute request");
        };
        assert_eq!(
            request.digest.as_str(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(request.source_machine, MachineId::new("machine-a"));
        assert_eq!(request.target_machines, vec![MachineId::new("machine-b")]);
        assert_eq!(request.platform.expect("platform").architecture, "amd64");
    }

    #[test]
    fn image_receive_session_request_round_trips_operation_source_and_repository() {
        let request = DaemonRequest::ImageReceiveSession {
            request: ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId::new("machine-a"),
                repository: Some("ployz/image-push-1".into()),
            },
        };

        let json = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(
            json,
            serde_json::json!({
                "ImageReceiveSession": {
                    "request": {
                        "operation_id": "image-push-1",
                        "source_machine": "machine-a",
                        "repository": "ployz/image-push-1"
                    }
                }
            })
        );
        let roundtrip: DaemonRequest = serde_json::from_value(json).expect("deserialize request");

        let DaemonRequest::ImageReceiveSession { request } = roundtrip else {
            panic!("expected image receive session request");
        };
        assert_eq!(request.operation_id, "image-push-1");
        assert_eq!(request.source_machine, MachineId::new("machine-a"));
        assert_eq!(request.repository.as_deref(), Some("ployz/image-push-1"));
    }

    #[test]
    fn image_received_import_request_round_trips_import_coordinates() {
        let request = DaemonRequest::ImageReceivedImport {
            request: ImageReceivedImportRequest {
                operation_id: "image-distribute-1".into(),
                source_machine: MachineId::new("machine-a"),
                repository: "ployz/image-distribute-1".into(),
                reference: "image-distribute-1".into(),
                expected_digest: digest(),
                platform: None,
                repo_tags: vec!["example/app:latest".into()],
            },
        };

        let json = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(
            json,
            serde_json::json!({
                "ImageReceivedImport": {
                    "request": {
                        "operation_id": "image-distribute-1",
                        "source_machine": "machine-a",
                        "repository": "ployz/image-distribute-1",
                        "reference": "image-distribute-1",
                        "expected_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "repo_tags": ["example/app:latest"]
                    }
                }
            })
        );
        let roundtrip: DaemonRequest = serde_json::from_value(json).expect("deserialize request");

        let DaemonRequest::ImageReceivedImport { request } = roundtrip else {
            panic!("expected image received import request");
        };
        assert_eq!(request.operation_id, "image-distribute-1");
        assert_eq!(request.source_machine, MachineId::new("machine-a"));
        assert_eq!(request.repository, "ployz/image-distribute-1");
        assert_eq!(request.reference, "image-distribute-1");
        assert_eq!(request.repo_tags, vec!["example/app:latest"]);
    }

    #[test]
    fn image_inspect_request_round_trips_reference_and_machine() {
        let request = DaemonRequest::ImageInspect {
            request: ImageInspectRequest {
                digest: digest(),
                reference: Some(
                    "registry.example/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                ),
                machines: vec![MachineId::new("machine-a")],
            },
        };

        let json = serde_json::to_value(&request).expect("serialize image inspect request");
        let roundtrip: DaemonRequest =
            serde_json::from_value(json).expect("deserialize image inspect request");

        let DaemonRequest::ImageInspect { request } = roundtrip else {
            panic!("expected image inspect request");
        };
        assert_eq!(
            request.digest.as_str(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            request.reference.as_deref(),
            Some("registry.example/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(request.machines, vec![MachineId::new("machine-a")]);
    }

    #[test]
    fn build_machine_request_serializes_method_as_snake_case() {
        let request = BuildMachineRequest {
            method: BuildMethod::Railpack,
            context_path: ".".into(),
            image_name: "api".into(),
            machine_id: MachineId::new("builder-a"),
            platform: None,
            inputs: BuildInputs::default(),
        };

        let json = serde_json::to_value(&request).expect("serialize build request");

        assert_eq!(json["method"], "railpack");
    }

    #[test]
    fn build_machine_request_serializes_secret_aware_inputs() {
        let request = BuildMachineRequest {
            method: BuildMethod::Railpack,
            context_path: ".".into(),
            image_name: "api".into(),
            machine_id: MachineId::new("builder-a"),
            platform: None,
            inputs: BuildInputs {
                env: BTreeMap::from([
                    (
                        "NODE_ENV".into(),
                        BuildEnvValue::Plain {
                            value: "production".into(),
                        },
                    ),
                    (
                        "SENTRY_AUTH_TOKEN".into(),
                        BuildEnvValue::Secret {
                            value: "super-secret-token".into(),
                            fingerprint: Some("sentry-v1".into()),
                        },
                    ),
                ]),
                docker_build_args: BTreeMap::new(),
            },
        };

        let json = serde_json::to_value(&request).expect("serialize build request");

        assert_eq!(json["inputs"]["env"]["NODE_ENV"]["kind"], "plain");
        assert_eq!(json["inputs"]["env"]["NODE_ENV"]["value"], "production");
        assert_eq!(json["inputs"]["env"]["SENTRY_AUTH_TOKEN"]["kind"], "secret");
        assert_eq!(
            json["inputs"]["env"]["SENTRY_AUTH_TOKEN"]["fingerprint"],
            "sentry-v1"
        );
        assert!(format!("{:?}", request.inputs).contains("<redacted>"));
        assert!(!format!("{:?}", request.inputs).contains("super-secret-token"));
    }

    #[test]
    fn build_request_debug_redacts_input_values() {
        let request = DaemonRequest::BuildLocal {
            request: BuildLocalRequest {
                method: BuildMethod::Dockerfile,
                context_dir: "/work/app".into(),
                image_name: "example/app:latest".into(),
                platform: None,
                push_target: None,
                distribute_targets: Vec::new(),
                inputs: BuildInputs {
                    env: BTreeMap::from([(
                        "SENTRY_AUTH_TOKEN".into(),
                        BuildEnvValue::Secret {
                            value: "super-secret-token".into(),
                            fingerprint: None,
                        },
                    )]),
                    docker_build_args: BTreeMap::from([("PUBLIC_COMMIT".into(), "abc123".into())]),
                },
            },
        };

        let debug = format!("{request:?}");

        assert!(debug.contains("SENTRY_AUTH_TOKEN"));
        assert!(debug.contains("PUBLIC_COMMIT"));
        assert!(!debug.contains("super-secret-token"));
        assert!(!debug.contains("abc123"));
    }

    #[test]
    fn build_machine_request_roundtrips_as_daemon_request() {
        let request = DaemonRequest::BuildMachine {
            request: BuildMachineRequest {
                method: BuildMethod::Railpack,
                context_path: "/work/app".into(),
                image_name: "example/app:latest".into(),
                machine_id: MachineId::new("builder-a"),
                platform: None,
                inputs: BuildInputs {
                    env: BTreeMap::from([(
                        "SENTRY_AUTH_TOKEN".into(),
                        BuildEnvValue::Secret {
                            value: "super-secret-token".into(),
                            fingerprint: Some("sentry-v1".into()),
                        },
                    )]),
                    docker_build_args: BTreeMap::new(),
                },
            },
        };

        let json = serde_json::to_value(&request).expect("serialize build machine request");

        assert_eq!(json["BuildMachine"]["request"]["method"], "railpack");
        assert_eq!(
            json["BuildMachine"]["request"]["inputs"]["env"]["SENTRY_AUTH_TOKEN"]["kind"],
            "secret"
        );
        let roundtrip: DaemonRequest =
            serde_json::from_value(json).expect("deserialize build machine request");
        let DaemonRequest::BuildMachine { request } = roundtrip else {
            panic!("expected build machine request");
        };
        assert_eq!(request.method, BuildMethod::Railpack);
        assert_eq!(request.machine_id, MachineId::new("builder-a"));
        assert!(request.inputs.docker_build_args.is_empty());
    }

    #[test]
    fn build_env_unknown_variant_fails_deserialization() {
        let json = serde_json::json!({
            "kind": "opaque",
            "value": "x"
        });

        let error = serde_json::from_value::<BuildEnvValue>(json)
            .expect_err("unknown build env variant should fail");

        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn build_local_request_roundtrips_as_daemon_request() {
        let request = DaemonRequest::BuildLocal {
            request: BuildLocalRequest {
                method: BuildMethod::Dockerfile,
                context_dir: "/work/app".into(),
                image_name: "example/app:latest".into(),
                platform: None,
                push_target: None,
                distribute_targets: Vec::new(),
                inputs: BuildInputs::default(),
            },
        };

        let json = serde_json::to_value(&request).expect("serialize build local request");

        assert_eq!(json["BuildLocal"]["request"]["method"], "dockerfile");
        assert_eq!(json["BuildLocal"]["request"]["context_dir"], "/work/app");
        let roundtrip: DaemonRequest =
            serde_json::from_value(json).expect("deserialize build local request");
        let DaemonRequest::BuildLocal { request } = roundtrip else {
            panic!("expected build local request");
        };
        assert_eq!(request.method, BuildMethod::Dockerfile);
        assert_eq!(request.image_name, "example/app:latest");
        assert!(request.distribute_targets.is_empty());
    }

    fn digest() -> ImageDigest {
        ImageDigest("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into())
    }
}
