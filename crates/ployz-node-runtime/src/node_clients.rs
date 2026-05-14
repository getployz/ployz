use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_node_api::{
    NodeRequest, NodeResponse, NodeVolumeZfsPeerSendPayload, NodeVolumeZfsSnapshotPayload,
    NodeVolumeZfsTransferPayload, VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND,
    VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND, VOLUME_ZFS_TRANSFER_PAYLOAD_KIND,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeCommand {
    Probe,
    ReceiveImage { operation_id: String },
    ReceiveVolume { transfer_id: String },
    PromoteStorage { operation_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeClientError {
    pub node_id: String,
    pub message: String,
}

#[async_trait]
pub trait NodePeerClient: Send + Sync {
    fn node_id(&self) -> &str;
    async fn send(&self, command: NodeCommand) -> Result<(), NodeClientError>;
}

#[derive(Default, Clone)]
pub struct NodeClientRegistry<C> {
    clients: Arc<Mutex<BTreeMap<String, C>>>,
}

impl<C> NodeClientRegistry<C>
where
    C: Clone + NodePeerClient,
{
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn insert(&self, client: C) {
        self.clients
            .lock()
            .expect("node clients")
            .insert(client.node_id().to_string(), client);
    }

    pub async fn send(&self, node_id: &str, command: NodeCommand) -> Result<(), NodeClientError> {
        let client = self
            .clients
            .lock()
            .expect("node clients")
            .get(node_id)
            .cloned();
        let Some(client) = client else {
            return Err(NodeClientError {
                node_id: node_id.to_string(),
                message: "node client not registered".to_string(),
            });
        };
        client.send(command).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRpcPolicy {
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRpcErrorKind {
    Transport,
    Remote,
    MissingPayload,
    Decode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRpcError {
    pub kind: NodeRpcErrorKind,
    pub operation: &'static str,
    pub code: String,
    pub message: String,
}

impl NodeRpcError {
    #[must_use]
    pub fn new(
        operation: &'static str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::transport(operation, code, message)
    }

    #[must_use]
    pub fn transport(
        operation: &'static str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: NodeRpcErrorKind::Transport,
            operation,
            code: code.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn remote(
        operation: &'static str,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: NodeRpcErrorKind::Remote,
            operation,
            code: code.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn missing_payload(operation: &'static str, expected_kind: &'static str) -> Self {
        Self {
            kind: NodeRpcErrorKind::MissingPayload,
            operation,
            code: "NODE_RPC_MISSING_PAYLOAD".into(),
            message: format!("node response missing payload '{expected_kind}'"),
        }
    }

    #[must_use]
    pub fn decode(
        operation: &'static str,
        expected_kind: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: NodeRpcErrorKind::Decode,
            operation,
            code: "NODE_RPC_DECODE_PAYLOAD".into(),
            message: format!(
                "decode node response payload '{expected_kind}': {}",
                message.into()
            ),
        }
    }
}

impl std::fmt::Display for NodeRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]: {}", self.operation, self.code, self.message)
    }
}

impl std::error::Error for NodeRpcError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeZfsRpcOperation {
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
        if !response.is_ok() {
            return Err(NodeRpcError::remote(
                operation.operation_name(),
                response.code(),
                response.message(),
            ));
        }
        let Some(payload) = response.payload() else {
            return Err(NodeRpcError::missing_payload(
                operation.operation_name(),
                expected_kind,
            ));
        };
        let Some(kind) = payload.get("kind").and_then(serde_json::Value::as_str) else {
            return Err(NodeRpcError::missing_payload(
                operation.operation_name(),
                expected_kind,
            ));
        };
        if kind != expected_kind {
            return Err(NodeRpcError::missing_payload(
                operation.operation_name(),
                expected_kind,
            ));
        }
        serde_json::from_value(payload.clone()).map_err(|error| {
            NodeRpcError::decode(operation.operation_name(), expected_kind, error.to_string())
        })
    }
}

#[cfg(test)]
mod volume_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeVolumeTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, VolumeZfsRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
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
            send,
            transfer_get,
            peer_snapshot,
            peer_snapshot_guid,
            peer_start_send,
        ] = requests.as_slice()
        else {
            panic!("expected five requests");
        };
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

    fn default_response(operation: VolumeZfsRpcOperation) -> NodeResponse {
        let payload = match operation {
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
}
