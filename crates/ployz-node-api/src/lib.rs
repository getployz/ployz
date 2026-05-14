use std::collections::BTreeMap;

use ployz_error::{Error, Result};
use ployz_model::{
    ImageAvailabilityRecord, ImageDigest, ImageDistributePayload, ImageDistributeRequest,
    ImageDistributeValidationFailure, ImageDistributeValidationPayload, ImagePlatform,
    ImageReceiveSessionPayload, ImageReceiveSessionRequest, ImageReceivedImportPayload,
    ImageReceivedImportRequest, ImageTransferTargetResult, InstanceStatusRecord, MachineId,
    MachineSelfTransition, MachineStorageAuthorityPeer, NetworkId, StorageParticipation,
    StorageReplicaPolicy,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND: &str = "deploy-namespace-snapshot";
pub const DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND: &str = "deploy-candidate-started";
pub const IMAGE_DISTRIBUTE_PAYLOAD_KIND: &str = "image-distribute";
pub const IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND: &str = "image-distribute-validation";
pub const IMAGE_RECEIVE_SESSION_PAYLOAD_KIND: &str = "image-receive-session";
pub const IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND: &str = "image-received-import";
pub const VOLUME_ZFS_CLONE_PAYLOAD_KIND: &str = "volume-zfs-clone";
pub const VOLUME_ZFS_INSPECT_PAYLOAD_KIND: &str = "volume-zfs-inspect";
pub const VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND: &str = "volume-zfs-snapshot";
pub const VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND: &str = "volume-zfs-peer-send";
pub const VOLUME_ZFS_TRANSFER_PAYLOAD_KIND: &str = "volume-zfs-transfer";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum NodeResponse {
    Success {
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
    },
    Error {
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
    },
}

impl NodeResponse {
    #[must_use]
    pub fn success(message: impl Into<String>, payload: Option<serde_json::Value>) -> Self {
        Self::Success {
            code: "OK".into(),
            message: message.into(),
            payload,
        }
    }

    #[must_use]
    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        payload: Option<serde_json::Value>,
    ) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
            payload,
        }
    }

    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Success { code, .. } | Self::Error { code, .. } => code,
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Success { message, .. } | Self::Error { message, .. } => message,
        }
    }

    #[must_use]
    pub fn payload(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Success { payload, .. } | Self::Error { payload, .. } => payload.as_ref(),
        }
    }

    pub fn payload_as<T>(&self, expected_kind: &'static str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let payload = self.payload().ok_or_else(|| {
            Error::operation(
                "node_rpc_missing_payload",
                format!("node response missing payload '{expected_kind}'"),
            )
        })?;
        let Some(kind) = payload.get("kind").and_then(serde_json::Value::as_str) else {
            return Err(Error::operation(
                "node_rpc_missing_payload_kind",
                format!("node response payload missing kind '{expected_kind}'"),
            ));
        };
        if kind != expected_kind {
            return Err(Error::operation(
                "node_rpc_unexpected_payload_kind",
                format!("node response payload kind '{kind}' did not match '{expected_kind}'"),
            ));
        }
        serde_json::from_value(payload.clone()).map_err(|error| {
            Error::operation(
                "node_rpc_decode_payload",
                format!("decode node response payload '{expected_kind}': {error}"),
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDeployNamespaceSnapshotPayload {
    pub instances: Vec<InstanceStatusRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDeployCandidateStartedPayload {
    pub status: InstanceStatusRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeImageDistributePayload {
    pub operation_id: String,
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub targets: Vec<ImageTransferTargetResult>,
}

impl From<NodeImageDistributePayload> for ImageDistributePayload {
    fn from(payload: NodeImageDistributePayload) -> Self {
        Self {
            operation_id: payload.operation_id,
            digest: payload.digest,
            source_machine: payload.source_machine,
            targets: payload.targets,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeImageDistributeValidationPayload {
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub target_machines: Vec<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    pub failure: ImageDistributeValidationFailure,
}

impl From<NodeImageDistributeValidationPayload> for ImageDistributeValidationPayload {
    fn from(payload: NodeImageDistributeValidationPayload) -> Self {
        Self {
            request: ImageDistributeRequest {
                digest: payload.digest,
                source_machine: payload.source_machine,
                target_machines: payload.target_machines,
                platform: payload.platform,
            },
            failure: payload.failure,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeImageReceiveSessionPayload {
    pub target_machine: MachineId,
    pub endpoint: String,
    pub token: String,
    pub expires_at_unix_secs: u64,
    pub headers: BTreeMap<String, String>,
}

impl From<NodeImageReceiveSessionPayload> for ImageReceiveSessionPayload {
    fn from(payload: NodeImageReceiveSessionPayload) -> Self {
        Self {
            target_machine: payload.target_machine,
            endpoint: payload.endpoint,
            token: payload.token,
            expires_at_unix_secs: payload.expires_at_unix_secs,
            headers: payload.headers,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeImageReceivedImportPayload {
    pub target_machine: MachineId,
    pub record: ImageAvailabilityRecord,
}

impl From<NodeImageReceivedImportPayload> for ImageReceivedImportPayload {
    fn from(payload: NodeImageReceivedImportPayload) -> Self {
        Self {
            target_machine: payload.target_machine,
            record: payload.record,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVolumeZfsClonePayload {
    pub namespace: String,
    pub volume: String,
    pub source_namespace: String,
    pub source_volume: String,
    pub machine_id: MachineId,
    pub source_dataset: String,
    pub target_dataset: String,
    pub snapshot: String,
    pub guid: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVolumeZfsInspectPayload {
    pub namespace: String,
    pub volume: String,
    pub machine_id: MachineId,
    pub dataset: String,
    pub mountpoint: String,
    pub quota: String,
    pub used_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<NodeVolumeZfsSnapshotInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVolumeZfsSnapshotInfo {
    pub name: String,
    pub guid: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVolumeZfsSnapshotPayload {
    pub namespace: String,
    pub volume: String,
    pub machine_id: MachineId,
    pub dataset: String,
    pub snapshot: String,
    pub guid: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVolumeZfsPeerSendPayload {
    pub bytes_transferred: u64,
    pub snapshot_guid: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVolumeZfsTransferPayload {
    pub transfer: NodeVolumeZfsTransferInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeVolumeZfsTransferInfo {
    pub id: String,
    pub namespace: String,
    pub volume: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub snapshot_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_name: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
    pub state: NodeVolumeZfsTransferState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NodeVolumeZfsTransferState {
    Running {
        stage: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Succeeded {
        stage: String,
        snapshot_guid: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        bytes_transferred: u64,
    },
    Failed {
        stage: String,
        last_error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
    },
    Interrupted {
        stage: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_snapshot_guid: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_transferred: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeRequest {
    Ping,
    Status,
    MeshReady {
        json: bool,
    },
    MeshSelfRecord,
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
    MachineTransitionSelf {
        transition: MachineSelfTransition,
    },
    MachineStoragePromoteSelf {
        replicas: StorageReplicaPolicy,
        authority_peers: Vec<MachineStorageAuthorityPeer>,
    },
    MachineStorageRestoreSelf {
        participation: StorageParticipation,
        replicas: StorageReplicaPolicy,
        authority_peers: Vec<MachineStorageAuthorityPeer>,
    },
    MachineOperationGet {
        id: String,
    },
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
    ImageDistribute {
        request: ImageDistributeRequest,
    },
    ImageReceiveSession {
        request: ImageReceiveSessionRequest,
    },
    ImageReceivedImport {
        request: ImageReceivedImportRequest,
    },
}

pub fn decode_node_request(payload: &[u8]) -> Result<NodeRequest> {
    serde_json::from_slice(payload)
        .map_err(|error| Error::operation("node_rpc_decode_request", error.to_string()))
}

pub fn encode_node_response(response: &NodeResponse) -> Result<Vec<u8>> {
    serde_json::to_vec(response)
        .map_err(|error| Error::operation("node_rpc_encode_response", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_request_ping_keeps_legacy_wire_shape() {
        let json = serde_json::to_value(NodeRequest::Ping).expect("serialize node request");

        assert_eq!(json, serde_json::json!("Ping"));
        let roundtrip: NodeRequest = serde_json::from_value(json).expect("deserialize request");
        assert!(matches!(roundtrip, NodeRequest::Ping));
    }

    #[test]
    fn node_response_payload_as_decodes_expected_kind() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct TestPayload {
            value: String,
        }

        let response = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": "test-payload",
                "value": "kept"
            })),
        );

        let payload: TestPayload = response
            .payload_as("test-payload")
            .expect("typed payload should decode");
        assert_eq!(
            payload,
            TestPayload {
                value: "kept".into()
            }
        );
    }

    #[test]
    fn node_response_payload_as_reports_payload_shape_errors() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct TestPayload {
            value: String,
        }

        let missing_payload = NodeResponse::success("ok", None);
        let error = missing_payload
            .payload_as::<TestPayload>("test-payload")
            .expect_err("missing payload should fail");
        assert!(error.to_string().contains("missing payload"));

        let missing_kind = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "value": "kept"
            })),
        );
        let error = missing_kind
            .payload_as::<TestPayload>("test-payload")
            .expect_err("missing kind should fail");
        assert!(error.to_string().contains("missing kind"));

        let non_string_kind = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": 7,
                "value": "kept"
            })),
        );
        let error = non_string_kind
            .payload_as::<TestPayload>("test-payload")
            .expect_err("non-string kind should fail");
        assert!(error.to_string().contains("missing kind"));

        let wrong_kind = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": "other-payload",
                "value": "kept"
            })),
        );
        let error = wrong_kind
            .payload_as::<TestPayload>("test-payload")
            .expect_err("wrong kind should fail");
        assert!(error.to_string().contains("did not match"));

        let invalid_fields = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": "test-payload",
                "value": 9
            })),
        );
        let error = invalid_fields
            .payload_as::<TestPayload>("test-payload")
            .expect_err("invalid payload fields should fail");
        assert!(error.to_string().contains("decode node response payload"));
    }

    #[test]
    fn node_volume_zfs_transfer_payload_preserves_tagged_wire_shape() {
        let response = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": VOLUME_ZFS_TRANSFER_PAYLOAD_KIND,
                "transfer": {
                    "id": "transfer-1",
                    "namespace": "prod",
                    "volume": "data",
                    "source_machine": "machine-a",
                    "target_machine": "machine-b",
                    "snapshot_name": "snap",
                    "started_at": 1,
                    "updated_at": 2,
                    "state": {
                        "status": "succeeded",
                        "stage": "complete",
                        "snapshot_guid": 42,
                        "bytes_transferred": 4096
                    }
                }
            })),
        );

        let payload: NodeVolumeZfsTransferPayload = response
            .payload_as(VOLUME_ZFS_TRANSFER_PAYLOAD_KIND)
            .expect("volume transfer payload should decode");
        assert_eq!(payload.transfer.id, "transfer-1");
        assert!(matches!(
            payload.transfer.state,
            NodeVolumeZfsTransferState::Succeeded {
                snapshot_guid: 42,
                bytes_transferred: 4096,
                ..
            }
        ));
    }

    #[test]
    fn node_deploy_and_clone_payloads_preserve_tagged_wire_shape() {
        let snapshot_response = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND,
                "instances": []
            })),
        );
        let snapshot: NodeDeployNamespaceSnapshotPayload = snapshot_response
            .payload_as(DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND)
            .expect("deploy namespace snapshot should decode");
        assert!(snapshot.instances.is_empty());

        let clone_response = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": VOLUME_ZFS_CLONE_PAYLOAD_KIND,
                "namespace": "prod",
                "volume": "data",
                "source_namespace": "staging",
                "source_volume": "source-data",
                "machine_id": "machine-a",
                "source_dataset": "pool/staging/source-data",
                "target_dataset": "pool/prod/data",
                "snapshot": "snap",
                "guid": 42
            })),
        );
        let clone: NodeVolumeZfsClonePayload = clone_response
            .payload_as(VOLUME_ZFS_CLONE_PAYLOAD_KIND)
            .expect("volume clone payload should decode");
        assert_eq!(clone.target_dataset, "pool/prod/data");
        assert_eq!(clone.guid, 42);
    }

    #[test]
    fn node_volume_zfs_inspect_payload_preserves_tagged_wire_shape() {
        let response = NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": VOLUME_ZFS_INSPECT_PAYLOAD_KIND,
                "namespace": "prod",
                "volume": "data",
                "machine_id": "machine-a",
                "dataset": "pool/prod/data",
                "mountpoint": "/var/lib/ployz/volumes/prod/data",
                "quota": "10G",
                "used_bytes": 4096,
                "snapshots": [
                    {
                        "name": "snap",
                        "guid": 42
                    }
                ]
            })),
        );

        let payload: NodeVolumeZfsInspectPayload = response
            .payload_as(VOLUME_ZFS_INSPECT_PAYLOAD_KIND)
            .expect("volume inspect payload should decode");
        assert_eq!(payload.machine_id, MachineId::new("machine-a"));
        assert_eq!(payload.snapshots.len(), 1);
        assert_eq!(payload.snapshots[0].guid, 42);
    }

    #[test]
    fn node_image_payloads_preserve_tagged_wire_shape() {
        let response = NodeResponse::success(
            "created",
            Some(serde_json::json!({
                "kind": IMAGE_RECEIVE_SESSION_PAYLOAD_KIND,
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1_777_646_000_u64,
                "headers": {
                    "x-ployz-image-operation": "image-push-1"
                }
            })),
        );

        let payload: NodeImageReceiveSessionPayload = response
            .payload_as(IMAGE_RECEIVE_SESSION_PAYLOAD_KIND)
            .expect("image receive session payload should decode");
        assert_eq!(payload.target_machine, MachineId::new("machine-b"));
        assert_eq!(payload.token, "token-1");
    }
}
