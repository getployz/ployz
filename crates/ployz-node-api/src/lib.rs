use ployz_api::image::{
    ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest,
};
use ployz_api::machine::{MachineSelfTransition, MachineStorageAuthorityPeer};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    MachineId, MachineMembership, NetworkId, StorageParticipation, StorageReplicaPolicy,
};
use serde::{Deserialize, Serialize};

pub type NodeResponse = ployz_api::DaemonResponse;

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

impl From<NodeRequest> for ployz_api::DaemonRequest {
    fn from(request: NodeRequest) -> Self {
        match request {
            NodeRequest::Ping => Self::Ping,
            NodeRequest::Status => Self::Status,
            NodeRequest::MeshReady { json } => Self::MeshReady { json },
            NodeRequest::MeshSelfRecord => Self::MeshSelfRecord,
            NodeRequest::MeshPeerPrepareDestroy {
                operation_id,
                network_id,
                coordinator_id,
                expected_machine_ids,
            } => Self::MeshPeerPrepareDestroy {
                operation_id,
                network_id,
                coordinator_id,
                expected_machine_ids,
            },
            NodeRequest::MeshPeerCancelDestroy { operation_id } => {
                Self::MeshPeerCancelDestroy { operation_id }
            }
            NodeRequest::MeshPeerExecuteDestroy {
                operation_id,
                network_id,
            } => Self::MeshPeerExecuteDestroy {
                operation_id,
                network_id,
            },
            NodeRequest::MeshPeerPrepareUpdate {
                operation_id,
                version,
            } => Self::MeshPeerPrepareUpdate {
                operation_id,
                version,
            },
            NodeRequest::MeshPeerExecuteUpdate {
                operation_id,
                version,
            } => Self::MeshPeerExecuteUpdate {
                operation_id,
                version,
            },
            NodeRequest::MeshPeerRemoveMachine {
                operation_id,
                network_id,
                machine_id,
            } => Self::MeshPeerRemoveMachine {
                operation_id,
                network_id,
                machine_id,
            },
            NodeRequest::MachineTransitionSelf { transition } => {
                Self::MachineTransitionSelf { transition }
            }
            NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers,
            } => Self::MachineStoragePromoteSelf {
                replicas,
                authority_peers,
            },
            NodeRequest::MachineStorageRestoreSelf {
                participation,
                replicas,
                authority_peers,
            } => Self::MachineStorageRestoreSelf {
                participation,
                replicas,
                authority_peers,
            },
            NodeRequest::MachineOperationGet { id } => Self::MachineOperationGet { id },
            NodeRequest::DeployNodeInspectNamespace {
                namespace,
                deploy_id,
            } => Self::DeployNodeInspectNamespace {
                namespace,
                deploy_id,
            },
            NodeRequest::DeployNodeStartCandidate {
                namespace,
                deploy_id,
                service,
                slot_id,
                instance_id,
                spec_json,
                volumes_json,
            } => Self::DeployNodeStartCandidate {
                namespace,
                deploy_id,
                service,
                slot_id,
                instance_id,
                spec_json,
                volumes_json,
            },
            NodeRequest::DeployNodeDrainInstance {
                namespace,
                deploy_id,
                instance_id,
            } => Self::DeployNodeDrainInstance {
                namespace,
                deploy_id,
                instance_id,
            },
            NodeRequest::DeployNodeRemoveInstance {
                namespace,
                deploy_id,
                instance_id,
            } => Self::DeployNodeRemoveInstance {
                namespace,
                deploy_id,
                instance_id,
            },
            NodeRequest::DeployNodeCloneVolume {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
                quota,
                mode,
                owner,
            } => Self::DeployNodeCloneVolume {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
                quota,
                mode,
                owner,
            },
            NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
            } => Self::DeployNodeCleanupUncommittedVolumeClone {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
            },
            NodeRequest::VolumeZfsInspect {
                namespace,
                volume,
                machine,
            } => Self::VolumeZfsInspect {
                namespace,
                volume,
                machine,
            },
            NodeRequest::VolumeZfsSnapshot {
                namespace,
                volume,
                snapshot,
            } => Self::VolumeZfsSnapshot {
                namespace,
                volume,
                snapshot,
            },
            NodeRequest::VolumeZfsSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                from_snapshot,
            } => Self::VolumeZfsSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                from_snapshot,
            },
            NodeRequest::VolumeZfsPeerSnapshot {
                namespace,
                volume,
                snapshot,
            } => Self::VolumeZfsPeerSnapshot {
                namespace,
                volume,
                snapshot,
            },
            NodeRequest::VolumeZfsPeerSnapshotGuid {
                namespace,
                volume,
                snapshot,
            } => Self::VolumeZfsPeerSnapshotGuid {
                namespace,
                volume,
                snapshot,
            },
            NodeRequest::VolumeZfsPeerStartSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                expected_guid,
                from_snapshot,
                from_snapshot_guid,
            } => Self::VolumeZfsPeerStartSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                expected_guid,
                from_snapshot,
                from_snapshot_guid,
            },
            NodeRequest::VolumeZfsTransferGet { id } => Self::VolumeZfsTransferGet { id },
            NodeRequest::ImageDistribute { request } => Self::ImageDistribute { request },
            NodeRequest::ImageReceiveSession { request } => Self::ImageReceiveSession { request },
            NodeRequest::ImageReceivedImport { request } => Self::ImageReceivedImport { request },
        }
    }
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
    fn node_request_converts_to_daemon_dispatch_request() {
        let request = NodeRequest::VolumeZfsTransferGet { id: "tx-1".into() };
        let daemon: ployz_api::DaemonRequest = request.into();

        assert!(matches!(
            daemon,
            ployz_api::DaemonRequest::VolumeZfsTransferGet { id } if id == "tx-1"
        ));
    }
}
