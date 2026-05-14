use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_node_api::{
    NodeRequest, NodeResponse, NodeVolumeZfsInspectPayload, NodeVolumeZfsPeerSendPayload,
    NodeVolumeZfsSnapshotPayload, NodeVolumeZfsTransferPayload, VOLUME_ZFS_INSPECT_PAYLOAD_KIND,
    VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND, VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
    VOLUME_ZFS_TRANSFER_PAYLOAD_KIND,
};

use super::{
    NodeRpcError, NodeRpcPolicy, NodeServiceResponse, decode_payload_kind, decode_typed_payload,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeZfsRpcOperation {
    Inspect,
    Snapshot,
    Send,
    TransferGet,
    PeerSnapshot,
    PeerSnapshotGuid,
    PeerStartSend,
}

impl VolumeZfsRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::Inspect => "volume_zfs_inspect",
            Self::Snapshot => "volume_zfs_snapshot",
            Self::Send => "volume_zfs_send",
            Self::TransferGet => "volume_zfs_transfer_get",
            Self::PeerSnapshot => "volume_zfs_peer_snapshot",
            Self::PeerSnapshotGuid => "volume_zfs_peer_snapshot_guid",
            Self::PeerStartSend => "volume_zfs_peer_start_send",
        }
    }
}

#[async_trait]
pub trait VolumeZfsRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn volume_zfs_request(
        &self,
        machine_id: &MachineId,
        operation: VolumeZfsRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct VolumeZfsNodeClient<T> {
    transport: T,
}

impl<T> VolumeZfsNodeClient<T>
where
    T: VolumeZfsRpcTransport,
{
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    #[must_use]
    pub fn with_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            transport: self.transport.with_node_rpc_policy(policy),
        }
    }

    pub async fn inspect(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        volume: &str,
        machine: Option<&str>,
    ) -> Result<VolumeZfsNodeResponse, NodeRpcError> {
        self.request_response(
            machine_id,
            VolumeZfsRpcOperation::Inspect,
            &NodeRequest::VolumeZfsInspect {
                namespace: namespace.to_string(),
                volume: volume.to_string(),
                machine: machine.map(str::to_string),
            },
        )
        .await
    }

    pub async fn snapshot(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsNodeResponse, NodeRpcError> {
        self.request_response(
            machine_id,
            VolumeZfsRpcOperation::Snapshot,
            &NodeRequest::VolumeZfsSnapshot {
                namespace: namespace.to_string(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
    }

    pub async fn send(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        volume: &str,
        snapshot: &str,
        target_machine: &MachineId,
        from_snapshot: Option<&str>,
    ) -> Result<NodeVolumeZfsTransferPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            VolumeZfsRpcOperation::Send,
            &NodeRequest::VolumeZfsSend {
                namespace: namespace.to_string(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
                target_machine: target_machine.as_str().to_string(),
                from_snapshot: from_snapshot.map(str::to_string),
            },
            VOLUME_ZFS_TRANSFER_PAYLOAD_KIND,
        )
        .await
    }

    pub async fn transfer_get(
        &self,
        machine_id: &MachineId,
        transfer_id: &str,
    ) -> Result<NodeVolumeZfsTransferPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            VolumeZfsRpcOperation::TransferGet,
            &NodeRequest::VolumeZfsTransferGet {
                id: transfer_id.to_string(),
            },
            VOLUME_ZFS_TRANSFER_PAYLOAD_KIND,
        )
        .await
    }

    pub async fn peer_snapshot(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        volume: &str,
        snapshot: &str,
    ) -> Result<NodeVolumeZfsSnapshotPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            VolumeZfsRpcOperation::PeerSnapshot,
            &NodeRequest::VolumeZfsPeerSnapshot {
                namespace: namespace.to_string(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
            VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
        )
        .await
    }

    pub async fn peer_snapshot_guid(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        volume: &str,
        snapshot: &str,
    ) -> Result<NodeVolumeZfsSnapshotPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            VolumeZfsRpcOperation::PeerSnapshotGuid,
            &NodeRequest::VolumeZfsPeerSnapshotGuid {
                namespace: namespace.to_string(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
            VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn peer_start_send(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        volume: &str,
        snapshot: &str,
        target_machine: &MachineId,
        expected_guid: u64,
        from_snapshot: Option<&str>,
        from_snapshot_guid: Option<u64>,
    ) -> Result<NodeVolumeZfsPeerSendPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            VolumeZfsRpcOperation::PeerStartSend,
            &NodeRequest::VolumeZfsPeerStartSend {
                namespace: namespace.to_string(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
                target_machine: target_machine.as_str().to_string(),
                expected_guid,
                from_snapshot: from_snapshot.map(str::to_string),
                from_snapshot_guid,
            },
            VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND,
        )
        .await
    }

    async fn request_typed<P>(
        &self,
        machine_id: &MachineId,
        operation: VolumeZfsRpcOperation,
        request: &NodeRequest,
        expected_kind: &'static str,
    ) -> Result<P, NodeRpcError>
    where
        P: serde::de::DeserializeOwned,
    {
        let response = self
            .transport
            .volume_zfs_request(machine_id, operation, request)
            .await?;
        decode_typed_payload(operation.operation_name(), response, expected_kind)
    }

    async fn request_response(
        &self,
        machine_id: &MachineId,
        operation: VolumeZfsRpcOperation,
        request: &NodeRequest,
    ) -> Result<VolumeZfsNodeResponse, NodeRpcError> {
        let response = self
            .transport
            .volume_zfs_request(machine_id, operation, request)
            .await?;
        NodeServiceResponse::from_node_response(response, |payload| {
            decode_volume_zfs_payload(operation, payload)
        })
    }
}

pub type VolumeZfsNodeResponse = NodeServiceResponse<VolumeZfsNodePayload>;

#[derive(Debug, Clone)]
pub enum VolumeZfsNodePayload {
    Inspect(NodeVolumeZfsInspectPayload),
    Snapshot(NodeVolumeZfsSnapshotPayload),
}

fn decode_volume_zfs_payload(
    operation: VolumeZfsRpcOperation,
    payload: serde_json::Value,
) -> Result<VolumeZfsNodePayload, NodeRpcError> {
    let operation_name = operation.operation_name();
    match operation {
        VolumeZfsRpcOperation::Inspect => {
            decode_payload_kind(operation_name, VOLUME_ZFS_INSPECT_PAYLOAD_KIND, payload)
                .map(VolumeZfsNodePayload::Inspect)
        }
        VolumeZfsRpcOperation::Snapshot => {
            decode_payload_kind(operation_name, VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND, payload)
                .map(VolumeZfsNodePayload::Snapshot)
        }
        VolumeZfsRpcOperation::Send
        | VolumeZfsRpcOperation::TransferGet
        | VolumeZfsRpcOperation::PeerSnapshot
        | VolumeZfsRpcOperation::PeerSnapshotGuid
        | VolumeZfsRpcOperation::PeerStartSend => Err(NodeRpcError::missing_payload(
            operation_name,
            "volume zfs payload",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::super::NodeRpcErrorKind;
    use super::*;

    #[derive(Clone, Default)]
    struct FakeVolumeTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, VolumeZfsRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeVolumeTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl VolumeZfsRpcTransport for FakeVolumeTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn volume_zfs_request(
            &self,
            machine_id: &MachineId,
            operation: VolumeZfsRpcOperation,
            request: &NodeRequest,
        ) -> Result<NodeResponse, NodeRpcError> {
            self.requests.lock().expect("requests").push((
                machine_id.clone(),
                operation,
                request.clone(),
            ));
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .unwrap_or_else(|| Ok(default_response(operation)))
        }
    }

    #[tokio::test]
    async fn volume_zfs_client_builds_requests_and_applies_policy() {
        let transport = FakeVolumeTransport::default();
        let client = VolumeZfsNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(9),
        });

        client
            .inspect(&MachineId::new("machine-a"), "prod", "data", None)
            .await
            .expect("inspect request");
        client
            .inspect(
                &MachineId::new("machine-a"),
                "prod",
                "data",
                Some("machine-b"),
            )
            .await
            .expect("inspect request with machine filter");
        client
            .snapshot(&MachineId::new("machine-a"), "prod", "data", "snap")
            .await
            .expect("snapshot request");
        client
            .send(
                &MachineId::new("machine-a"),
                "prod",
                "data",
                "snap",
                &MachineId::new("machine-b"),
                Some("base"),
            )
            .await
            .expect("send request");
        client
            .transfer_get(&MachineId::new("machine-a"), "transfer-1")
            .await
            .expect("transfer get");
        client
            .peer_snapshot(&MachineId::new("machine-c"), "prod", "data", "snap")
            .await
            .expect("peer snapshot");
        client
            .peer_snapshot_guid(&MachineId::new("machine-c"), "prod", "data", "base")
            .await
            .expect("peer snapshot guid");
        client
            .peer_start_send(
                &MachineId::new("machine-c"),
                "prod",
                "data",
                "snap",
                &MachineId::new("machine-b"),
                42,
                Some("base"),
                Some(24),
            )
            .await
            .expect("peer start send");

        let requests = transport.requests.lock().expect("requests");
        let [
            inspect,
            inspect_filtered,
            snapshot,
            send,
            transfer_get,
            peer_snapshot,
            peer_snapshot_guid,
            peer_start_send,
        ] = requests.as_slice()
        else {
            panic!("expected eight requests");
        };
        assert_eq!(inspect.0, MachineId::new("machine-a"));
        assert_eq!(inspect.1, VolumeZfsRpcOperation::Inspect);
        assert!(matches!(
            &inspect.2,
            NodeRequest::VolumeZfsInspect {
                namespace,
                volume,
                machine: None,
            } if namespace == "prod" && volume == "data"
        ));
        assert_eq!(inspect_filtered.0, MachineId::new("machine-a"));
        assert_eq!(inspect_filtered.1, VolumeZfsRpcOperation::Inspect);
        assert!(matches!(
            &inspect_filtered.2,
            NodeRequest::VolumeZfsInspect {
                namespace,
                volume,
                machine: Some(machine),
            } if namespace == "prod" && volume == "data" && machine == "machine-b"
        ));
        assert_eq!(snapshot.0, MachineId::new("machine-a"));
        assert_eq!(snapshot.1, VolumeZfsRpcOperation::Snapshot);
        assert!(matches!(
            &snapshot.2,
            NodeRequest::VolumeZfsSnapshot {
                namespace,
                volume,
                snapshot,
            } if namespace == "prod" && volume == "data" && snapshot == "snap"
        ));
        assert_eq!(send.0, MachineId::new("machine-a"));
        assert_eq!(send.1, VolumeZfsRpcOperation::Send);
        assert!(matches!(
            &send.2,
            NodeRequest::VolumeZfsSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                from_snapshot: Some(from_snapshot),
            } if namespace == "prod"
                && volume == "data"
                && snapshot == "snap"
                && target_machine == "machine-b"
                && from_snapshot == "base"
        ));
        assert_eq!(transfer_get.0, MachineId::new("machine-a"));
        assert_eq!(transfer_get.1, VolumeZfsRpcOperation::TransferGet);
        assert!(matches!(
            &transfer_get.2,
            NodeRequest::VolumeZfsTransferGet { id } if id == "transfer-1"
        ));
        assert_eq!(peer_snapshot.0, MachineId::new("machine-c"));
        assert_eq!(peer_snapshot.1, VolumeZfsRpcOperation::PeerSnapshot);
        assert!(matches!(
            &peer_snapshot.2,
            NodeRequest::VolumeZfsPeerSnapshot {
                namespace,
                volume,
                snapshot,
            } if namespace == "prod" && volume == "data" && snapshot == "snap"
        ));
        assert_eq!(peer_snapshot_guid.0, MachineId::new("machine-c"));
        assert_eq!(
            peer_snapshot_guid.1,
            VolumeZfsRpcOperation::PeerSnapshotGuid
        );
        assert!(matches!(
            &peer_snapshot_guid.2,
            NodeRequest::VolumeZfsPeerSnapshotGuid {
                namespace,
                volume,
                snapshot,
            } if namespace == "prod" && volume == "data" && snapshot == "base"
        ));
        assert_eq!(peer_start_send.0, MachineId::new("machine-c"));
        assert_eq!(peer_start_send.1, VolumeZfsRpcOperation::PeerStartSend);
        assert!(matches!(
            &peer_start_send.2,
            NodeRequest::VolumeZfsPeerStartSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                expected_guid,
                from_snapshot: Some(from_snapshot),
                from_snapshot_guid: Some(from_snapshot_guid),
            } if namespace == "prod"
                && volume == "data"
                && snapshot == "snap"
                && target_machine == "machine-b"
                && *expected_guid == 42
                && from_snapshot == "base"
                && *from_snapshot_guid == 24
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(9)
            }]
        );
    }

    #[tokio::test]
    async fn volume_zfs_client_preserves_remote_error_envelope_and_payload_best_effort() {
        let transport = FakeVolumeTransport::with_responses(vec![Ok(NodeResponse::error(
            "VOLUME_ZFS_SNAPSHOT_FAILED",
            "snapshot failed",
            Some(snapshot_payload()),
        ))]);
        let response = VolumeZfsNodeClient::new(transport)
            .snapshot(&MachineId::new("machine-a"), "prod", "data", "snap")
            .await
            .expect("remote error should preserve response");
        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_SNAPSHOT_FAILED");
        assert_eq!(response.message(), "snapshot failed");
        let Some(VolumeZfsNodePayload::Snapshot(payload)) = response.into_payload() else {
            panic!("expected snapshot payload");
        };
        assert_eq!(payload.machine_id, MachineId::new("machine-a"));
        assert_eq!(payload.guid, 42);

        let transport = FakeVolumeTransport::with_responses(vec![Ok(NodeResponse::error(
            "VOLUME_ZFS_INSPECT_FAILED",
            "inspect failed",
            Some(serde_json::json!({
                "kind": VOLUME_ZFS_INSPECT_PAYLOAD_KIND,
                "namespace": "prod"
            })),
        ))]);
        let response = VolumeZfsNodeClient::new(transport)
            .inspect(&MachineId::new("machine-a"), "prod", "data", None)
            .await
            .expect("malformed remote error payload should preserve response");
        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_INSPECT_FAILED");
        assert_eq!(response.message(), "inspect failed");
        assert!(response.into_payload().is_none());
    }

    #[tokio::test]
    async fn volume_zfs_client_rejects_success_payload_shape_errors() {
        for payload in [
            serde_json::json!({
                "namespace": "prod",
                "volume": "data"
            }),
            serde_json::json!({
                "kind": "wrong-volume-payload",
                "namespace": "prod",
                "volume": "data"
            }),
            inspect_payload(),
        ] {
            let transport = FakeVolumeTransport::with_responses(vec![Ok(NodeResponse::success(
                "ok",
                Some(payload),
            ))]);
            let error = VolumeZfsNodeClient::new(transport)
                .snapshot(&MachineId::new("machine-a"), "prod", "data", "snap")
                .await
                .expect_err("missing, unknown, or mismatched payload kind should fail");
            assert_eq!(error.kind, NodeRpcErrorKind::MissingPayload);
            assert_eq!(error.operation, "volume_zfs_snapshot");
        }

        let transport = FakeVolumeTransport::with_responses(vec![Ok(NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": VOLUME_ZFS_INSPECT_PAYLOAD_KIND,
                "namespace": "prod"
            })),
        ))]);
        let error = VolumeZfsNodeClient::new(transport)
            .inspect(&MachineId::new("machine-a"), "prod", "data", None)
            .await
            .expect_err("structurally invalid inspect payload should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Decode);
        assert_eq!(error.operation, "volume_zfs_inspect");
    }

    fn default_response(operation: VolumeZfsRpcOperation) -> NodeResponse {
        let payload = match operation {
            VolumeZfsRpcOperation::Inspect => inspect_payload(),
            VolumeZfsRpcOperation::Snapshot => snapshot_payload(),
            VolumeZfsRpcOperation::Send | VolumeZfsRpcOperation::TransferGet => {
                serde_json::json!({
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
                            "status": "running",
                            "stage": "send"
                        }
                    }
                })
            }
            VolumeZfsRpcOperation::PeerSnapshot | VolumeZfsRpcOperation::PeerSnapshotGuid => {
                serde_json::json!({
                    "kind": VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
                    "namespace": "prod",
                    "volume": "data",
                    "machine_id": "machine-c",
                    "dataset": "pool/prod/data",
                    "snapshot": "snap",
                    "guid": 42
                })
            }
            VolumeZfsRpcOperation::PeerStartSend => {
                serde_json::json!({
                    "kind": VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND,
                    "bytes_transferred": 4096,
                    "snapshot_guid": 42
                })
            }
        };
        NodeResponse::success("ok", Some(payload))
    }

    fn inspect_payload() -> serde_json::Value {
        serde_json::json!({
            "kind": VOLUME_ZFS_INSPECT_PAYLOAD_KIND,
            "namespace": "prod",
            "volume": "data",
            "machine_id": "machine-a",
            "dataset": "pool/prod/data",
            "mountpoint": "/var/lib/ployz/volumes/prod/data",
            "quota": "10G",
            "used_bytes": 4096,
            "snapshots": []
        })
    }

    fn snapshot_payload() -> serde_json::Value {
        serde_json::json!({
            "kind": VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
            "namespace": "prod",
            "volume": "data",
            "machine_id": "machine-a",
            "dataset": "pool/prod/data",
            "snapshot": "snap",
            "guid": 42
        })
    }
}
