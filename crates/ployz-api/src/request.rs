use crate::deploy::{DeployOptions, MigrateServiceRequest};
use crate::image::{
    ImageDistributeRequest, ImageInspectRequest, ImagePushRequest, ImageReceiveSessionRequest,
    ImageReceivedImportRequest, ImageStatusRequest,
};
use crate::machine::{
    MachineAddOptions, MachineInstallOptions, MachineStorageAuthorityPeer,
    MachineStoragePromoteRequest, MachineTransitionGoal,
};
use crate::mesh::MeshBootstrapRequest;
use ipnet::Ipv4Net;
use ployz_types::model::{
    MachineId, MachineMembership, NetworkId, StorageParticipation, StorageReplicaPolicy,
};
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
    MeshPeerPrepareDestroy {
        operation_id: String,
        network_id: NetworkId,
        coordinator_id: MachineId,
        expected_machine_ids: Vec<MachineId>,
    },
    MeshPeerCancelDestroy {
        operation_id: String,
    },
    MeshPeerExecuteDestroy {
        operation_id: String,
        network_id: NetworkId,
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
    MeshPeerPrepareUpdate {
        operation_id: String,
        version: String,
    },
    MeshPeerExecuteUpdate {
        operation_id: String,
        version: String,
    },
    MeshPeerRemoveMachine {
        operation_id: String,
        network_id: NetworkId,
        machine_id: MachineId,
    },
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
    MachineTransitionSelf {
        goal: MachineTransitionGoal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assigned_subnet: Option<Ipv4Net>,
        force: bool,
    },
    MachineStoragePromoteSelf {
        replicas: StorageReplicaPolicy,
        authority_peers: Vec<MachineStorageAuthorityPeer>,
    },
    MachineStorageRestoreSelf {
        participation: StorageParticipation,
        replicas: StorageReplicaPolicy,
        authority_peers: Vec<MachineMembership>,
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
    DeployApply {
        manifest_json: String,
        options: DeployOptions,
    },
    DeployExport {
        namespace: String,
    },
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
    BuildOperationGet {
        id: String,
    },
    BuildOperationList,
    DeployNodeInspectNamespace {
        namespace: String,
        deploy_id: String,
    },
    DeployNodeStartCandidate {
        namespace: String,
        deploy_id: String,
        service: String,
        slot_id: String,
        instance_id: String,
        spec_json: String,
        volumes_json: String,
    },
    DeployNodeDrainInstance {
        namespace: String,
        deploy_id: String,
        instance_id: String,
    },
    DeployNodeRemoveInstance {
        namespace: String,
        deploy_id: String,
        instance_id: String,
    },
    DeployNodeCloneVolume {
        namespace: String,
        deploy_id: String,
        volume: String,
        source_namespace: String,
        source_volume: String,
        snapshot: String,
        quota: String,
        mode: String,
        owner: String,
    },
    DeployNodeCleanupUncommittedVolumeClone {
        namespace: String,
        deploy_id: String,
        volume: String,
        source_namespace: String,
        source_volume: String,
        snapshot: String,
    },
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
    VolumeZfsPeerSnapshot {
        namespace: String,
        volume: String,
        snapshot: String,
    },
    VolumeZfsPeerSnapshotGuid {
        namespace: String,
        volume: String,
        snapshot: String,
    },
    VolumeZfsPeerStartSend {
        namespace: String,
        volume: String,
        snapshot: String,
        target_machine: String,
        expected_guid: u64,
        from_snapshot: Option<String>,
        from_snapshot_guid: Option<u64>,
    },
    VolumeZfsTransferGet {
        id: String,
    },
    VolumeZfsTransferList,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::BuildMachineRequest;
    use crate::deploy::MigrateServiceMode;
    use crate::image::{
        ImageDistributeRequest, ImageInspectRequest, ImagePushRequest, ImageReceiveSessionRequest,
        ImageReceivedImportRequest,
    };
    use ployz_types::model::{BuildMethod, ImageDigest};

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
                target_machines: vec![MachineId("machine-a".into()), MachineId("machine-b".into())],
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
            vec![MachineId("machine-a".into()), MachineId("machine-b".into())]
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
                source_machine: MachineId("machine-a".into()),
                target_machines: vec![MachineId("machine-b".into())],
                platform: Some(ployz_types::model::ImagePlatform {
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
        assert_eq!(request.source_machine, MachineId("machine-a".into()));
        assert_eq!(request.target_machines, vec![MachineId("machine-b".into())]);
        assert_eq!(request.platform.expect("platform").architecture, "amd64");
    }

    #[test]
    fn image_receive_session_request_round_trips_operation_source_and_repository() {
        let request = DaemonRequest::ImageReceiveSession {
            request: ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("machine-a".into()),
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
        assert_eq!(request.source_machine, MachineId("machine-a".into()));
        assert_eq!(request.repository.as_deref(), Some("ployz/image-push-1"));
    }

    #[test]
    fn image_received_import_request_round_trips_import_coordinates() {
        let request = DaemonRequest::ImageReceivedImport {
            request: ImageReceivedImportRequest {
                operation_id: "image-distribute-1".into(),
                source_machine: MachineId("machine-a".into()),
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
        assert_eq!(request.source_machine, MachineId("machine-a".into()));
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
                machines: vec![MachineId("machine-a".into())],
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
        assert_eq!(request.machines, vec![MachineId("machine-a".into())]);
    }

    #[test]
    fn build_machine_request_serializes_method_as_snake_case() {
        let request = BuildMachineRequest {
            method: BuildMethod::Railpack,
            context_path: ".".into(),
            image_name: "api".into(),
            machine_id: MachineId("builder-a".into()),
            platform: None,
            build_args: Default::default(),
        };

        let json = serde_json::to_value(&request).expect("serialize build request");

        assert_eq!(json["method"], "railpack");
    }

    fn digest() -> ImageDigest {
        ImageDigest("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into())
    }
}
