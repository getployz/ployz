use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ployz_model::{
    DeployId, ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest,
    InstanceId, MachineId, SlotId,
};
use ployz_node_api::{
    DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND, DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND,
    IMAGE_DISTRIBUTE_PAYLOAD_KIND, IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND,
    IMAGE_RECEIVE_SESSION_PAYLOAD_KIND, IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND,
    NodeDeployCandidateStartedPayload, NodeDeployNamespaceSnapshotPayload,
    NodeImageDistributePayload, NodeImageDistributeValidationPayload,
    NodeImageReceiveSessionPayload, NodeImageReceivedImportPayload, NodeRequest, NodeResponse,
    NodeVolumeZfsClonePayload, NodeVolumeZfsInspectPayload, NodeVolumeZfsPeerSendPayload,
    NodeVolumeZfsSnapshotPayload, NodeVolumeZfsTransferPayload, VOLUME_ZFS_CLONE_PAYLOAD_KIND,
    VOLUME_ZFS_INSPECT_PAYLOAD_KIND, VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND,
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

impl NodeRpcPolicy {
    #[must_use]
    pub const fn from_secs(timeout_secs: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

pub const DEPLOY_PARTICIPANT_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(10 * 60);
pub const DEPLOY_VOLUME_CLONE_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(2 * 60 * 60);
pub const DEPLOY_VOLUME_CLONE_CLEANUP_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(30 * 60);
pub const DEPLOY_VOLUME_MOVE_START_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(60);
pub const DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(60);
pub const DEPLOY_VOLUME_MOVE_WAIT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
pub const DEPLOY_VOLUME_MOVE_POLL_INTERVAL: Duration = Duration::from_secs(2);
pub const IMAGE_RECEIVE_SESSION_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(15);
pub const IMAGE_DISTRIBUTE_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(30 * 60);
pub const IMAGE_RECEIVED_IMPORT_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(30 * 60);

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

#[derive(Debug, Clone)]
pub struct NodeServiceResponse<P> {
    success: bool,
    code: String,
    message: String,
    payload: Option<P>,
}

impl<P> NodeServiceResponse<P> {
    fn from_node_response<F>(
        response: NodeResponse,
        decode_payload: F,
    ) -> Result<Self, NodeRpcError>
    where
        F: Fn(serde_json::Value) -> Result<P, NodeRpcError>,
    {
        let (success, code, message, payload) = match response {
            NodeResponse::Success {
                code,
                message,
                payload,
            } => (true, code, message, payload),
            NodeResponse::Error {
                code,
                message,
                payload,
            } => (false, code, message, payload),
        };
        let payload = match (success, payload) {
            (true, Some(payload)) => Some(decode_payload(payload)?),
            (true, None) => None,
            (false, Some(payload)) => decode_payload(payload).ok(),
            (false, None) => None,
        };
        Ok(Self {
            success,
            code,
            message,
            payload,
        })
    }

    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.success
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn into_payload(self) -> Option<P> {
        self.payload
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployRpcOperation {
    InspectNamespace,
    StartCandidate,
    CloneVolume,
    CleanupVolumeClone,
    DrainInstance,
    RemoveInstance,
}

impl DeployRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::InspectNamespace => "deploy_node_inspect",
            Self::StartCandidate => "deploy_node_start_candidate",
            Self::CloneVolume => "deploy_node_clone_volume",
            Self::CleanupVolumeClone => "deploy_node_cleanup_uncommitted_volume_clone",
            Self::DrainInstance => "deploy_node_drain",
            Self::RemoveInstance => "deploy_node_remove",
        }
    }
}

#[async_trait]
pub trait DeployRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn deploy_request(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct DeployNodeClient<T> {
    transport: T,
}

impl<T> DeployNodeClient<T>
where
    T: DeployRpcTransport,
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

    pub async fn inspect_namespace(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
    ) -> Result<NodeDeployNamespaceSnapshotPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            DeployRpcOperation::InspectNamespace,
            &NodeRequest::DeployNodeInspectNamespace {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
            },
            DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        service: &str,
        slot_id: &SlotId,
        instance_id: &InstanceId,
        spec_json: &str,
        volumes_json: &str,
    ) -> Result<NodeDeployCandidateStartedPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            DeployRpcOperation::StartCandidate,
            &NodeRequest::DeployNodeStartCandidate {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                service: service.to_string(),
                slot_id: slot_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
                spec_json: spec_json.to_string(),
                volumes_json: volumes_json.to_string(),
            },
            DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn clone_volume(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
        quota: &str,
        mode: &str,
        owner: &str,
    ) -> Result<NodeVolumeZfsClonePayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            DeployRpcOperation::CloneVolume,
            &NodeRequest::DeployNodeCloneVolume {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                volume: volume.to_string(),
                source_namespace: source_namespace.to_string(),
                source_volume: source_volume.to_string(),
                snapshot: snapshot.to_string(),
                quota: quota.to_string(),
                mode: mode.to_string(),
                owner: owner.to_string(),
            },
            VOLUME_ZFS_CLONE_PAYLOAD_KIND,
        )
        .await
    }

    pub async fn cleanup_volume_clone(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_ok(
            machine_id,
            DeployRpcOperation::CleanupVolumeClone,
            &NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                volume: volume.to_string(),
                source_namespace: source_namespace.to_string(),
                source_volume: source_volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
    }

    pub async fn drain_instance(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<(), NodeRpcError> {
        self.request_ok(
            machine_id,
            DeployRpcOperation::DrainInstance,
            &NodeRequest::DeployNodeDrainInstance {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
            },
        )
        .await
    }

    pub async fn remove_instance(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<(), NodeRpcError> {
        self.request_ok(
            machine_id,
            DeployRpcOperation::RemoveInstance,
            &NodeRequest::DeployNodeRemoveInstance {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
            },
        )
        .await
    }

    async fn request_typed<P>(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
        expected_kind: &'static str,
    ) -> Result<P, NodeRpcError>
    where
        P: serde::de::DeserializeOwned,
    {
        let response = self
            .transport
            .deploy_request(machine_id, operation, request)
            .await?;
        decode_typed_payload(operation.operation_name(), response, expected_kind)
    }

    async fn request_ok(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .deploy_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRpcOperation {
    ReceiveSession,
    Distribute,
    ReceivedImport,
}

impl ImageRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::ReceiveSession => "image_receive_session",
            Self::Distribute => "image_distribute",
            Self::ReceivedImport => "image_received_import",
        }
    }

    #[must_use]
    pub fn rpc_policy(self) -> NodeRpcPolicy {
        match self {
            Self::ReceiveSession => IMAGE_RECEIVE_SESSION_RPC_POLICY,
            Self::Distribute => IMAGE_DISTRIBUTE_RPC_POLICY,
            Self::ReceivedImport => IMAGE_RECEIVED_IMPORT_RPC_POLICY,
        }
    }
}

#[async_trait]
pub trait ImageRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn image_request(
        &self,
        machine_id: &MachineId,
        operation: ImageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct ImageNodeClient<T> {
    transport: T,
}

impl<T> ImageNodeClient<T>
where
    T: ImageRpcTransport,
{
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub async fn receive_session(
        &self,
        machine_id: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        self.request_image(
            machine_id,
            ImageRpcOperation::ReceiveSession,
            &NodeRequest::ImageReceiveSession { request },
        )
        .await
    }

    pub async fn distribute(
        &self,
        machine_id: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        self.request_image(
            machine_id,
            ImageRpcOperation::Distribute,
            &NodeRequest::ImageDistribute { request },
        )
        .await
    }

    pub async fn received_import(
        &self,
        machine_id: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        self.request_image(
            machine_id,
            ImageRpcOperation::ReceivedImport,
            &NodeRequest::ImageReceivedImport { request },
        )
        .await
    }

    async fn request_image(
        &self,
        machine_id: &MachineId,
        operation: ImageRpcOperation,
        request: &NodeRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        let transport = self.transport.with_node_rpc_policy(operation.rpc_policy());
        let response = transport
            .image_request(machine_id, operation, request)
            .await?;
        NodeServiceResponse::from_node_response(response, |payload| {
            decode_image_payload(operation.operation_name(), payload)
        })
    }
}

pub type ImageNodeResponse = NodeServiceResponse<ImageNodePayload>;

#[derive(Debug, Clone)]
pub enum ImageNodePayload {
    Distribute(NodeImageDistributePayload),
    DistributeValidation(NodeImageDistributeValidationPayload),
    ReceiveSession(NodeImageReceiveSessionPayload),
    ReceivedImport(NodeImageReceivedImportPayload),
}

fn decode_image_payload(
    operation_name: &'static str,
    payload: serde_json::Value,
) -> Result<ImageNodePayload, NodeRpcError> {
    let Some(kind) = payload.get("kind").and_then(serde_json::Value::as_str) else {
        return Err(NodeRpcError::missing_payload(
            operation_name,
            "image payload",
        ));
    };
    match kind {
        IMAGE_DISTRIBUTE_PAYLOAD_KIND => {
            decode_payload_variant(operation_name, IMAGE_DISTRIBUTE_PAYLOAD_KIND, payload)
                .map(ImageNodePayload::Distribute)
        }
        IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND => decode_payload_variant(
            operation_name,
            IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND,
            payload,
        )
        .map(ImageNodePayload::DistributeValidation),
        IMAGE_RECEIVE_SESSION_PAYLOAD_KIND => {
            decode_payload_variant(operation_name, IMAGE_RECEIVE_SESSION_PAYLOAD_KIND, payload)
                .map(ImageNodePayload::ReceiveSession)
        }
        IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND => {
            decode_payload_variant(operation_name, IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND, payload)
                .map(ImageNodePayload::ReceivedImport)
        }
        _ => Err(NodeRpcError::missing_payload(
            operation_name,
            "image payload",
        )),
    }
}

fn decode_payload_variant<P>(
    operation_name: &'static str,
    expected_kind: &'static str,
    payload: serde_json::Value,
) -> Result<P, NodeRpcError>
where
    P: serde::de::DeserializeOwned,
{
    serde_json::from_value(payload)
        .map_err(|error| NodeRpcError::decode(operation_name, expected_kind, error.to_string()))
}

fn decode_payload_kind<P>(
    operation_name: &'static str,
    expected_kind: &'static str,
    payload: serde_json::Value,
) -> Result<P, NodeRpcError>
where
    P: serde::de::DeserializeOwned,
{
    let Some(kind) = payload.get("kind").and_then(serde_json::Value::as_str) else {
        return Err(NodeRpcError::missing_payload(operation_name, expected_kind));
    };
    if kind != expected_kind {
        return Err(NodeRpcError::missing_payload(operation_name, expected_kind));
    }
    decode_payload_variant(operation_name, expected_kind, payload)
}

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

fn ensure_success(
    operation_name: &'static str,
    response: &NodeResponse,
) -> Result<(), NodeRpcError> {
    if response.is_ok() {
        return Ok(());
    }
    Err(NodeRpcError::remote(
        operation_name,
        response.code(),
        response.message(),
    ))
}

fn decode_typed_payload<P>(
    operation_name: &'static str,
    response: NodeResponse,
    expected_kind: &'static str,
) -> Result<P, NodeRpcError>
where
    P: serde::de::DeserializeOwned,
{
    ensure_success(operation_name, &response)?;
    let Some(payload) = response.payload() else {
        return Err(NodeRpcError::missing_payload(operation_name, expected_kind));
    };
    let Some(kind) = payload.get("kind").and_then(serde_json::Value::as_str) else {
        return Err(NodeRpcError::missing_payload(operation_name, expected_kind));
    };
    if kind != expected_kind {
        return Err(NodeRpcError::missing_payload(operation_name, expected_kind));
    }
    serde_json::from_value(payload.clone())
        .map_err(|error| NodeRpcError::decode(operation_name, expected_kind, error.to_string()))
}

#[cfg(test)]
mod deploy_tests {
    use std::collections::{BTreeMap, VecDeque};

    use ployz_model::{DrainState, InstancePhase, InstanceStatusRecord, Namespace};

    use super::*;

    #[derive(Clone, Default)]
    struct FakeDeployTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, DeployRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeDeployTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl DeployRpcTransport for FakeDeployTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn deploy_request(
            &self,
            machine_id: &MachineId,
            operation: DeployRpcOperation,
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
    async fn deploy_node_client_builds_requests_and_applies_policy() {
        let transport = FakeDeployTransport::default();
        let client = DeployNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");
        let deploy_id = DeployId::new("deploy-1");
        let instance_id = InstanceId::new("instance-1");

        client
            .inspect_namespace(&machine_id, "prod", &deploy_id)
            .await
            .expect("inspect namespace");
        client
            .start_candidate(
                &machine_id,
                "prod",
                &deploy_id,
                "web",
                &SlotId::new("slot-1"),
                &instance_id,
                "{\"image\":\"app\"}",
                "[{\"name\":\"data\"}]",
            )
            .await
            .expect("start candidate");
        client
            .clone_volume(
                &machine_id,
                "prod",
                &deploy_id,
                "data",
                "staging",
                "source-data",
                "snap",
                "10G",
                "0750",
                "999:999",
            )
            .await
            .expect("clone volume");
        client
            .cleanup_volume_clone(
                &machine_id,
                "prod",
                &deploy_id,
                "data",
                "staging",
                "source-data",
                "snap",
            )
            .await
            .expect("cleanup clone");
        client
            .drain_instance(&machine_id, "prod", &deploy_id, &instance_id)
            .await
            .expect("drain instance");
        client
            .remove_instance(&machine_id, "prod", &deploy_id, &instance_id)
            .await
            .expect("remove instance");

        let requests = transport.requests.lock().expect("requests");
        let [inspect, start, clone, cleanup, drain, remove] = requests.as_slice() else {
            panic!("expected six deploy requests");
        };
        assert_eq!(inspect.0, machine_id);
        assert_eq!(inspect.1, DeployRpcOperation::InspectNamespace);
        assert!(matches!(
            &inspect.2,
            NodeRequest::DeployNodeInspectNamespace {
                namespace,
                deploy_id,
            } if namespace == "prod" && deploy_id == "deploy-1"
        ));
        assert_eq!(start.1, DeployRpcOperation::StartCandidate);
        assert!(matches!(
            &start.2,
            NodeRequest::DeployNodeStartCandidate {
                namespace,
                deploy_id,
                service,
                slot_id,
                instance_id,
                spec_json,
                volumes_json,
            } if namespace == "prod"
                && deploy_id == "deploy-1"
                && service == "web"
                && slot_id == "slot-1"
                && instance_id == "instance-1"
                && spec_json == "{\"image\":\"app\"}"
                && volumes_json == "[{\"name\":\"data\"}]"
        ));
        assert_eq!(clone.1, DeployRpcOperation::CloneVolume);
        assert!(matches!(
            &clone.2,
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
            } if namespace == "prod"
                && deploy_id == "deploy-1"
                && volume == "data"
                && source_namespace == "staging"
                && source_volume == "source-data"
                && snapshot == "snap"
                && quota == "10G"
                && mode == "0750"
                && owner == "999:999"
        ));
        assert_eq!(cleanup.1, DeployRpcOperation::CleanupVolumeClone);
        assert!(matches!(
            &cleanup.2,
            NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
            } if namespace == "prod"
                && deploy_id == "deploy-1"
                && volume == "data"
                && source_namespace == "staging"
                && source_volume == "source-data"
                && snapshot == "snap"
        ));
        assert_eq!(drain.1, DeployRpcOperation::DrainInstance);
        assert!(matches!(
            &drain.2,
            NodeRequest::DeployNodeDrainInstance {
                namespace,
                deploy_id,
                instance_id,
            } if namespace == "prod" && deploy_id == "deploy-1" && instance_id == "instance-1"
        ));
        assert_eq!(remove.1, DeployRpcOperation::RemoveInstance);
        assert!(matches!(
            &remove.2,
            NodeRequest::DeployNodeRemoveInstance {
                namespace,
                deploy_id,
                instance_id,
            } if namespace == "prod" && deploy_id == "deploy-1" && instance_id == "instance-1"
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn deploy_node_client_maps_transport_remote_and_payload_errors() {
        let machine_id = MachineId::new("machine-a");
        let deploy_id = DeployId::new("deploy-1");
        let instance_id = InstanceId::new("instance-1");

        let transport = FakeDeployTransport::with_responses(vec![Err(NodeRpcError::transport(
            "deploy_node_inspect",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = DeployNodeClient::new(transport)
            .inspect_namespace(&machine_id, "prod", &deploy_id)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "deploy_node_inspect");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");

        let transport = FakeDeployTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_FAILED",
            "remote failed",
            None,
        ))]);
        let error = DeployNodeClient::new(transport)
            .drain_instance(&machine_id, "prod", &deploy_id, &instance_id)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "deploy_node_drain");
        assert_eq!(error.code, "REMOTE_FAILED");
        assert_eq!(error.message, "remote failed");

        let transport =
            FakeDeployTransport::with_responses(vec![Ok(NodeResponse::success("ok", None))]);
        let error = DeployNodeClient::new(transport)
            .clone_volume(
                &machine_id,
                "prod",
                &deploy_id,
                "data",
                "staging",
                "source-data",
                "snap",
                "10G",
                "0750",
                "999:999",
            )
            .await
            .expect_err("missing payload should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::MissingPayload);
        assert_eq!(error.operation, "deploy_node_clone_volume");
        assert_eq!(error.code, "NODE_RPC_MISSING_PAYLOAD");

        let transport = FakeDeployTransport::with_responses(vec![Ok(NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": "wrong-kind",
                "instances": []
            })),
        ))]);
        let error = DeployNodeClient::new(transport)
            .inspect_namespace(&machine_id, "prod", &deploy_id)
            .await
            .expect_err("wrong kind should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::MissingPayload);
        assert_eq!(error.operation, "deploy_node_inspect");

        let transport = FakeDeployTransport::with_responses(vec![Ok(NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND
            })),
        ))]);
        let error = DeployNodeClient::new(transport)
            .start_candidate(
                &machine_id,
                "prod",
                &deploy_id,
                "web",
                &SlotId::new("slot-1"),
                &instance_id,
                "{}",
                "[]",
            )
            .await
            .expect_err("invalid payload fields should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Decode);
        assert_eq!(error.operation, "deploy_node_start_candidate");
        assert_eq!(error.code, "NODE_RPC_DECODE_PAYLOAD");
    }

    #[test]
    fn deploy_rpc_policy_presets_match_operation_classes() {
        assert_eq!(
            DEPLOY_PARTICIPANT_RPC_POLICY.timeout,
            Duration::from_secs(10 * 60)
        );
        assert_eq!(
            DEPLOY_VOLUME_CLONE_RPC_POLICY.timeout,
            Duration::from_secs(2 * 60 * 60)
        );
        assert_eq!(
            DEPLOY_VOLUME_CLONE_CLEANUP_RPC_POLICY.timeout,
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            DEPLOY_VOLUME_MOVE_START_RPC_POLICY.timeout,
            Duration::from_secs(60)
        );
        assert_eq!(
            DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY.timeout,
            Duration::from_secs(60)
        );
        assert_eq!(
            DEPLOY_VOLUME_MOVE_WAIT_TIMEOUT,
            Duration::from_secs(24 * 60 * 60)
        );
        assert_eq!(DEPLOY_VOLUME_MOVE_POLL_INTERVAL, Duration::from_secs(2));
    }

    fn default_response(operation: DeployRpcOperation) -> NodeResponse {
        let payload = match operation {
            DeployRpcOperation::InspectNamespace => serde_json::json!({
                "kind": DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND,
                "instances": []
            }),
            DeployRpcOperation::StartCandidate => serde_json::json!({
                "kind": DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND,
                "status": instance_status()
            }),
            DeployRpcOperation::CloneVolume => serde_json::json!({
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
            }),
            DeployRpcOperation::CleanupVolumeClone
            | DeployRpcOperation::DrainInstance
            | DeployRpcOperation::RemoveInstance => {
                return NodeResponse::success("ok", None);
            }
        };
        NodeResponse::success("ok", Some(payload))
    }

    fn instance_status() -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId::new("instance-1"),
            namespace: Namespace::new("prod"),
            service: "web".into(),
            slot_id: SlotId::new("slot-1"),
            machine_id: MachineId::new("machine-a"),
            revision_hash: "rev".into(),
            deploy_id: DeployId::new("deploy-1"),
            docker_container_id: "container-1".into(),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 1,
            updated_at: 2,
        }
    }
}

#[cfg(test)]
mod image_tests {
    use std::collections::VecDeque;

    use ployz_model::{
        ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageDigest,
        ImageDistributeValidationFailure, ImagePlatform, ImagePresence, ImageRef,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct FakeImageTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, ImageRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeImageTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl ImageRpcTransport for FakeImageTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn image_request(
            &self,
            machine_id: &MachineId,
            operation: ImageRpcOperation,
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
    async fn image_node_client_builds_requests_and_applies_operation_policies() {
        let transport = FakeImageTransport::default();
        let client = ImageNodeClient::new(transport.clone());
        let target_machine = MachineId::new("machine-b");
        let source_machine = MachineId::new("machine-a");
        let digest = digest();
        let platform = ImagePlatform {
            os: "linux".into(),
            architecture: "amd64".into(),
            variant: Some("v8".into()),
        };
        let repo_tags = vec!["example/app:latest".into(), "example/app:v1".into()];

        client
            .receive_session(
                &target_machine,
                ImageReceiveSessionRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: source_machine.clone(),
                    repository: Some("ployz/image-push-1".into()),
                },
            )
            .await
            .expect("receive session");
        client
            .distribute(
                &source_machine,
                ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: source_machine.clone(),
                    target_machines: vec![target_machine.clone()],
                    platform: Some(platform.clone()),
                },
            )
            .await
            .expect("distribute");
        client
            .received_import(
                &target_machine,
                ImageReceivedImportRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: source_machine.clone(),
                    repository: "ployz/image-push-1".into(),
                    reference: "image-push-1".into(),
                    expected_digest: digest.clone(),
                    platform: Some(platform.clone()),
                    repo_tags: repo_tags.clone(),
                },
            )
            .await
            .expect("received import");

        let requests = transport.requests.lock().expect("requests");
        let [receive_session, distribute, received_import] = requests.as_slice() else {
            panic!("expected three image requests");
        };
        assert_eq!(receive_session.0, target_machine);
        assert_eq!(receive_session.1, ImageRpcOperation::ReceiveSession);
        assert!(matches!(
            &receive_session.2,
            NodeRequest::ImageReceiveSession { request }
                if request.operation_id == "image-push-1"
                    && request.source_machine == source_machine
                    && request.repository.as_deref() == Some("ployz/image-push-1")
        ));
        assert_eq!(distribute.0, source_machine);
        assert_eq!(distribute.1, ImageRpcOperation::Distribute);
        assert!(matches!(
            &distribute.2,
            NodeRequest::ImageDistribute { request }
                if request.digest == digest
                    && request.source_machine == source_machine
                    && request.target_machines.as_slice() == [target_machine.clone()]
                    && request.platform.as_ref() == Some(&platform)
        ));
        assert_eq!(received_import.0, target_machine);
        assert_eq!(received_import.1, ImageRpcOperation::ReceivedImport);
        assert!(matches!(
            &received_import.2,
            NodeRequest::ImageReceivedImport { request }
                if request.operation_id == "image-push-1"
                    && request.source_machine == source_machine
                    && request.repository == "ployz/image-push-1"
                    && request.reference == "image-push-1"
                    && request.expected_digest == digest
                    && request.platform.as_ref() == Some(&platform)
                    && request.repo_tags.as_slice() == repo_tags.as_slice()
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[
                IMAGE_RECEIVE_SESSION_RPC_POLICY,
                IMAGE_DISTRIBUTE_RPC_POLICY,
                IMAGE_RECEIVED_IMPORT_RPC_POLICY,
            ]
        );
    }

    #[tokio::test]
    async fn image_node_client_preserves_remote_error_payloads_and_maps_decode_errors() {
        let digest = digest();
        let target_machine = MachineId::new("machine-b");
        let source_machine = MachineId::new("machine-a");
        let platform = ImagePlatform {
            os: "linux".into(),
            architecture: "amd64".into(),
            variant: Some("v8".into()),
        };
        let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::error(
            "IMAGE_DISTRIBUTE_DUPLICATE_TARGET",
            "duplicate target",
            Some(serde_json::json!({
                "kind": IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND,
                "digest": digest.as_str(),
                "source_machine": "machine-a",
                "target_machines": ["machine-b", "machine-b"],
                "platform": {
                    "os": "linux",
                    "architecture": "amd64",
                    "variant": "v8"
                },
                "failure": {
                    "type": "duplicate_target",
                    "duplicate_target": "machine-b"
                }
            })),
        ))]);
        let response = ImageNodeClient::new(transport)
            .distribute(
                &source_machine,
                ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: source_machine.clone(),
                    target_machines: vec![target_machine.clone()],
                    platform: None,
                },
            )
            .await
            .expect("remote image response");
        assert!(!response.is_ok());
        assert_eq!(response.code(), "IMAGE_DISTRIBUTE_DUPLICATE_TARGET");
        assert_eq!(response.message(), "duplicate target");
        let Some(ImageNodePayload::DistributeValidation(payload)) = response.into_payload() else {
            panic!("expected distribute validation payload");
        };
        assert_eq!(payload.digest, digest.clone());
        assert_eq!(payload.source_machine, source_machine.clone());
        assert_eq!(
            payload.target_machines.as_slice(),
            &[target_machine.clone(), target_machine.clone()]
        );
        assert_eq!(payload.platform.as_ref(), Some(&platform));
        assert!(matches!(
            payload.failure,
            ImageDistributeValidationFailure::DuplicateTarget { duplicate_target }
                if duplicate_target == target_machine
        ));

        let transport = FakeImageTransport::with_responses(vec![Err(NodeRpcError::transport(
            "image_receive_session",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = ImageNodeClient::new(transport)
            .receive_session(
                &target_machine,
                ImageReceiveSessionRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: MachineId::new("machine-a"),
                    repository: None,
                },
            )
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "image_receive_session");

        let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": IMAGE_RECEIVE_SESSION_PAYLOAD_KIND,
                "target_machine": "machine-b"
            })),
        ))]);
        let error = ImageNodeClient::new(transport)
            .receive_session(
                &target_machine,
                ImageReceiveSessionRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: MachineId::new("machine-a"),
                    repository: None,
                },
            )
            .await
            .expect_err("invalid payload fields should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Decode);
        assert_eq!(error.operation, "image_receive_session");
    }

    #[tokio::test]
    async fn image_node_client_maps_payload_discriminator_errors_and_decodes_import_payload() {
        let digest = digest();
        let target_machine = MachineId::new("machine-b");
        let source_machine = MachineId::new("machine-a");

        for payload in [
            serde_json::json!({
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1_777_646_000_u64,
                "headers": {}
            }),
            serde_json::json!({
                "kind": "wrong-image-payload",
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1_777_646_000_u64,
                "headers": {}
            }),
        ] {
            let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::success(
                "ok",
                Some(payload),
            ))]);
            let error = ImageNodeClient::new(transport)
                .receive_session(
                    &target_machine,
                    ImageReceiveSessionRequest {
                        operation_id: "image-push-1".into(),
                        source_machine: source_machine.clone(),
                        repository: None,
                    },
                )
                .await
                .expect_err("payload discriminator should fail");
            assert_eq!(error.kind, NodeRpcErrorKind::MissingPayload);
            assert_eq!(error.operation, "image_receive_session");
        }

        let record = image_record(target_machine.clone(), digest.clone());
        let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::success(
            "imported",
            Some(serde_json::json!({
                "kind": IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND,
                "target_machine": target_machine.as_str(),
                "record": record
            })),
        ))]);
        let response = ImageNodeClient::new(transport)
            .received_import(
                &target_machine,
                ImageReceivedImportRequest {
                    operation_id: "image-push-1".into(),
                    source_machine,
                    repository: "ployz/image-push-1".into(),
                    reference: "image-push-1".into(),
                    expected_digest: digest.clone(),
                    platform: None,
                    repo_tags: vec!["example/app:latest".into()],
                },
            )
            .await
            .expect("received import payload should decode");
        assert!(response.is_ok());
        let Some(ImageNodePayload::ReceivedImport(payload)) = response.into_payload() else {
            panic!("expected received import payload");
        };
        assert_eq!(payload.target_machine, target_machine);
        assert_eq!(payload.record.machine_id, target_machine);
        assert_eq!(payload.record.digest, digest);
    }

    fn default_response(operation: ImageRpcOperation) -> NodeResponse {
        let payload = match operation {
            ImageRpcOperation::ReceiveSession => serde_json::json!({
                "kind": IMAGE_RECEIVE_SESSION_PAYLOAD_KIND,
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1_777_646_000_u64,
                "headers": {}
            }),
            ImageRpcOperation::Distribute => serde_json::json!({
                "kind": IMAGE_DISTRIBUTE_PAYLOAD_KIND,
                "operation_id": "image-distribute-1",
                "digest": digest().as_str(),
                "source_machine": "machine-a",
                "targets": []
            }),
            ImageRpcOperation::ReceivedImport => {
                return NodeResponse::success("imported", None);
            }
        };
        NodeResponse::success("ok", Some(payload))
    }

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid digest")
    }

    fn image_record(machine_id: MachineId, digest: ImageDigest) -> ImageAvailabilityRecord {
        ImageAvailabilityRecord {
            machine_id,
            digest: digest.clone(),
            presence: ImagePresence::Present {
                artifact: ImageArtifact {
                    image: ImageRef::digest_only(digest),
                    platform: None,
                    provenance: ImageArtifactProvenance::External {
                        source: Some("test".into()),
                    },
                    created_at: 1,
                },
                recorded_at: 2,
                source_operation_id: Some("image-push-1".into()),
            },
            updated_at: 3,
        }
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
