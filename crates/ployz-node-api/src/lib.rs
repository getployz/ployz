use ployz_error::{Error, Result};
use ployz_model::{
    ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest, MachineId,
    MachineMembership, MachineSelfTransition, MachineStorageAuthorityPeer, NetworkId,
    StorageParticipation, StorageReplicaPolicy,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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
        authority_peers: Vec<MachineMembership>,
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
}
