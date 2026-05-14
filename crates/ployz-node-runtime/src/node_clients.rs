mod deploy;
mod image;
mod volume_zfs;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ployz_model::{
    MachineId, MachineSelfTransition, MachineStorageAuthorityPeer, NetworkId, StorageParticipation,
    StorageReplicaPolicy,
};
use ployz_node_api::{NodeRequest, NodeResponse};

pub use deploy::{
    DEPLOY_PARTICIPANT_RPC_POLICY, DEPLOY_VOLUME_CLONE_CLEANUP_RPC_POLICY,
    DEPLOY_VOLUME_CLONE_RPC_POLICY, DEPLOY_VOLUME_MOVE_POLL_INTERVAL,
    DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY, DEPLOY_VOLUME_MOVE_START_RPC_POLICY,
    DEPLOY_VOLUME_MOVE_WAIT_TIMEOUT, DeployNodeClient, DeployRpcOperation, DeployRpcTransport,
};
pub use image::{
    IMAGE_DISTRIBUTE_RPC_POLICY, IMAGE_RECEIVE_SESSION_RPC_POLICY,
    IMAGE_RECEIVED_IMPORT_RPC_POLICY, ImageNodeClient, ImageNodePayload, ImageNodeResponse,
    ImageRpcOperation, ImageRpcTransport,
};
pub use volume_zfs::{
    VolumeZfsNodeClient, VolumeZfsNodePayload, VolumeZfsNodeResponse, VolumeZfsRpcOperation,
    VolumeZfsRpcTransport,
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

pub const MESH_MACHINE_REMOVE_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(120);
pub const MESH_DESTRUCTIVE_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(120);
pub const MACHINE_TRANSITION_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(120);
pub const MACHINE_STORAGE_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(15);
pub const NODE_READINESS_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(10);
pub const NODE_STATUS_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(15);
pub const MACHINE_OPERATION_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(15);

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
pub enum NodeProbeRpcOperation {
    Ping,
    Status,
}

impl NodeProbeRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::Ping => "node_ping",
            Self::Status => "node_status",
        }
    }
}

#[async_trait]
pub trait NodeProbeRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn node_probe_request(
        &self,
        machine_id: &MachineId,
        operation: NodeProbeRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct NodeProbeNodeClient<T> {
    transport: T,
}

impl<T> NodeProbeNodeClient<T>
where
    T: NodeProbeRpcTransport,
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

    pub async fn ping(&self, machine_id: &MachineId) -> Result<(), NodeRpcError> {
        self.request_expect_ok(machine_id, NodeProbeRpcOperation::Ping, &NodeRequest::Ping)
            .await
    }

    pub async fn status(&self, machine_id: &MachineId) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            NodeProbeRpcOperation::Status,
            &NodeRequest::Status,
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: NodeProbeRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .node_probe_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }

    async fn request_expect_response(
        &self,
        machine_id: &MachineId,
        operation: NodeProbeRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        let response = self
            .transport
            .node_probe_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)?;
        Ok(response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshReadinessRpcOperation {
    Ready,
    SelfRecord,
}

impl MeshReadinessRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::Ready => "mesh_ready",
            Self::SelfRecord => "mesh_self_record",
        }
    }
}

#[async_trait]
pub trait MeshReadinessRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn mesh_readiness_request(
        &self,
        machine_id: &MachineId,
        operation: MeshReadinessRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MeshReadinessNodeClient<T> {
    transport: T,
}

impl<T> MeshReadinessNodeClient<T>
where
    T: MeshReadinessRpcTransport,
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

    pub async fn ready(
        &self,
        machine_id: &MachineId,
        json: bool,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            MeshReadinessRpcOperation::Ready,
            &NodeRequest::MeshReady { json },
        )
        .await
    }

    pub async fn self_record(&self, machine_id: &MachineId) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            MeshReadinessRpcOperation::SelfRecord,
            &NodeRequest::MeshSelfRecord,
        )
        .await
    }

    async fn request_expect_response(
        &self,
        machine_id: &MachineId,
        operation: MeshReadinessRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        let response = self
            .transport
            .mesh_readiness_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)?;
        Ok(response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineOperationRpcOperation {
    Get,
}

impl MachineOperationRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::Get => "machine_operation_get",
        }
    }
}

#[async_trait]
pub trait MachineOperationRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_operation_request(
        &self,
        machine_id: &MachineId,
        operation: MachineOperationRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineOperationNodeClient<T> {
    transport: T,
}

impl<T> MachineOperationNodeClient<T>
where
    T: MachineOperationRpcTransport,
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

    pub async fn get(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            MachineOperationRpcOperation::Get,
            &NodeRequest::MachineOperationGet {
                id: operation_id.to_string(),
            },
        )
        .await
    }

    async fn request_expect_response(
        &self,
        machine_id: &MachineId,
        operation: MachineOperationRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        let response = self
            .transport
            .machine_operation_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)?;
        Ok(response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineLifecycleRpcOperation {
    TransitionSelf,
}

impl MachineLifecycleRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::TransitionSelf => "machine_transition_self",
        }
    }
}

#[async_trait]
pub trait MachineLifecycleRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_lifecycle_request(
        &self,
        machine_id: &MachineId,
        operation: MachineLifecycleRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineLifecycleNodeClient<T> {
    transport: T,
}

impl<T> MachineLifecycleNodeClient<T>
where
    T: MachineLifecycleRpcTransport,
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

    pub async fn transition_self(
        &self,
        machine_id: &MachineId,
        transition: MachineSelfTransition,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineLifecycleRpcOperation::TransitionSelf,
            &NodeRequest::MachineTransitionSelf { transition },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MachineLifecycleRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .machine_lifecycle_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineUpdateRpcOperation {
    PrepareUpdate,
    ExecuteUpdate,
}

impl MachineUpdateRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::PrepareUpdate => "machine_update_prepare",
            Self::ExecuteUpdate => "machine_update_execute",
        }
    }
}

#[async_trait]
pub trait MachineUpdateRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_update_request(
        &self,
        machine_id: &MachineId,
        operation: MachineUpdateRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineUpdateNodeClient<T> {
    transport: T,
}

impl<T> MachineUpdateNodeClient<T>
where
    T: MachineUpdateRpcTransport,
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

    pub async fn prepare_update(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        version: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineUpdateRpcOperation::PrepareUpdate,
            &NodeRequest::MeshPeerPrepareUpdate {
                operation_id: operation_id.to_string(),
                version: version.to_string(),
            },
        )
        .await
    }

    pub async fn execute_update(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        version: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineUpdateRpcOperation::ExecuteUpdate,
            &NodeRequest::MeshPeerExecuteUpdate {
                operation_id: operation_id.to_string(),
                version: version.to_string(),
            },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MachineUpdateRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .machine_update_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineStorageRpcOperation {
    StoragePromoteSelf,
    StorageRestoreSelf,
}

impl MachineStorageRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::StoragePromoteSelf => "machine_storage_promote_self",
            Self::StorageRestoreSelf => "machine_storage_restore_self",
        }
    }
}

#[async_trait]
pub trait MachineStorageRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_request(
        &self,
        machine_id: &MachineId,
        operation: MachineStorageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineStorageNodeClient<T> {
    transport: T,
}

impl<T> MachineStorageNodeClient<T>
where
    T: MachineStorageRpcTransport,
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

    pub async fn promote_storage_self(
        &self,
        machine_id: &MachineId,
        replicas: StorageReplicaPolicy,
        authority_peers: &[MachineStorageAuthorityPeer],
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineStorageRpcOperation::StoragePromoteSelf,
            &NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers: authority_peers.to_vec(),
            },
        )
        .await
    }

    pub async fn restore_storage_self(
        &self,
        machine_id: &MachineId,
        participation: &StorageParticipation,
        replicas: StorageReplicaPolicy,
        authority_peers: &[MachineStorageAuthorityPeer],
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineStorageRpcOperation::StorageRestoreSelf,
            &NodeRequest::MachineStorageRestoreSelf {
                participation: participation.clone(),
                replicas,
                authority_peers: authority_peers.to_vec(),
            },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MachineStorageRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .machine_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshRpcOperation {
    PrepareDestroy,
    CancelDestroy,
    ExecuteDestroy,
    RemoveMachine,
}

impl MeshRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::PrepareDestroy => "mesh_peer_prepare_destroy",
            Self::CancelDestroy => "mesh_peer_cancel_destroy",
            Self::ExecuteDestroy => "mesh_peer_execute_destroy",
            Self::RemoveMachine => "mesh_peer_remove_machine",
        }
    }
}

#[async_trait]
pub trait MeshRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn mesh_request(
        &self,
        machine_id: &MachineId,
        operation: MeshRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MeshNodeClient<T> {
    transport: T,
}

impl<T> MeshNodeClient<T>
where
    T: MeshRpcTransport,
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

    pub async fn prepare_destroy(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        network_id: &NetworkId,
        coordinator_id: &MachineId,
        expected_machine_ids: &[MachineId],
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::PrepareDestroy,
            &NodeRequest::MeshPeerPrepareDestroy {
                operation_id: operation_id.to_string(),
                network_id: network_id.clone(),
                coordinator_id: coordinator_id.clone(),
                expected_machine_ids: expected_machine_ids.to_vec(),
            },
        )
        .await
    }

    pub async fn cancel_destroy(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::CancelDestroy,
            &NodeRequest::MeshPeerCancelDestroy {
                operation_id: operation_id.to_string(),
            },
        )
        .await
    }

    pub async fn execute_destroy(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        network_id: &NetworkId,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::ExecuteDestroy,
            &NodeRequest::MeshPeerExecuteDestroy {
                operation_id: operation_id.to_string(),
                network_id: network_id.clone(),
            },
        )
        .await
    }

    pub async fn remove_machine(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        network_id: &NetworkId,
        removed_machine_id: &MachineId,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::RemoveMachine,
            &NodeRequest::MeshPeerRemoveMachine {
                operation_id: operation_id.to_string(),
                network_id: network_id.clone(),
                machine_id: removed_machine_id.clone(),
            },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MeshRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .mesh_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
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

pub fn decode_node_response_payload<P>(
    operation_name: &'static str,
    response: NodeResponse,
    expected_kind: &'static str,
) -> Result<P, NodeRpcError>
where
    P: serde::de::DeserializeOwned,
{
    decode_typed_payload(operation_name, response, expected_kind)
}

#[cfg(test)]
mod node_probe_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeNodeProbeTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, NodeProbeRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeNodeProbeTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl NodeProbeRpcTransport for FakeNodeProbeTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn node_probe_request(
            &self,
            machine_id: &MachineId,
            operation: NodeProbeRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn node_probe_client_builds_ping_and_status_requests_and_applies_policy() {
        let transport = FakeNodeProbeTransport::default();
        let client = NodeProbeNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");

        client.ping(&machine_id).await.expect("ping");
        client.status(&machine_id).await.expect("status");

        let requests = transport.requests.lock().expect("requests");
        let [ping, status] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(ping.0, machine_id);
        assert_eq!(ping.1, NodeProbeRpcOperation::Ping);
        assert!(matches!(&ping.2, NodeRequest::Ping));
        assert_eq!(status.0, machine_id);
        assert_eq!(status.1, NodeProbeRpcOperation::Status);
        assert!(matches!(&status.2, NodeRequest::Status));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn node_probe_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeNodeProbeTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = NodeProbeNodeClient::new(transport)
            .ping(&machine_id)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "node_ping");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeNodeProbeTransport::with_responses(vec![Err(NodeRpcError::transport(
            "node_status",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = NodeProbeNodeClient::new(transport)
            .status(&machine_id)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "node_status");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod mesh_readiness_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMeshReadinessTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MeshReadinessRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMeshReadinessTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MeshReadinessRpcTransport for FakeMeshReadinessTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn mesh_readiness_request(
            &self,
            machine_id: &MachineId,
            operation: MeshReadinessRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn mesh_readiness_client_builds_ready_and_self_record_requests_and_applies_policy() {
        let transport = FakeMeshReadinessTransport::default();
        let client = MeshReadinessNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");

        client.ready(&machine_id, false).await.expect("mesh ready");
        client
            .self_record(&machine_id)
            .await
            .expect("mesh self record");

        let requests = transport.requests.lock().expect("requests");
        let [ready, self_record] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(ready.0, machine_id);
        assert_eq!(ready.1, MeshReadinessRpcOperation::Ready);
        assert!(matches!(&ready.2, NodeRequest::MeshReady { json: false }));
        assert_eq!(self_record.0, machine_id);
        assert_eq!(self_record.1, MeshReadinessRpcOperation::SelfRecord);
        assert!(matches!(&self_record.2, NodeRequest::MeshSelfRecord));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn mesh_readiness_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMeshReadinessTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MeshReadinessNodeClient::new(transport)
            .ready(&machine_id, false)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "mesh_ready");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMeshReadinessTransport::with_responses(vec![Err(
            NodeRpcError::transport("mesh_self_record", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MeshReadinessNodeClient::new(transport)
            .self_record(&machine_id)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "mesh_self_record");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod machine_operation_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineOperationTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineOperationRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineOperationTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineOperationRpcTransport for FakeMachineOperationTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_operation_request(
            &self,
            machine_id: &MachineId,
            operation: MachineOperationRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_operation_client_builds_get_request_and_applies_policy() {
        let transport = FakeMachineOperationTransport::default();
        let client =
            MachineOperationNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
                timeout: Duration::from_secs(11),
            });
        let machine_id = MachineId::new("machine-a");

        client
            .get(&machine_id, "operation-1")
            .await
            .expect("machine operation get");

        let requests = transport.requests.lock().expect("requests");
        let [get] = requests.as_slice() else {
            panic!("expected one request");
        };
        assert_eq!(get.0, machine_id);
        assert_eq!(get.1, MachineOperationRpcOperation::Get);
        assert!(matches!(
            &get.2,
            NodeRequest::MachineOperationGet { id } if id == "operation-1"
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn machine_operation_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMachineOperationTransport::with_responses(vec![Ok(
            NodeResponse::error("REMOTE_REFUSED", "peer refused", None),
        )]);
        let error = MachineOperationNodeClient::new(transport)
            .get(&machine_id, "operation-1")
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_operation_get");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineOperationTransport::with_responses(vec![Err(
            NodeRpcError::transport("machine_operation_get", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MachineOperationNodeClient::new(transport)
            .get(&machine_id, "operation-1")
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_operation_get");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod machine_lifecycle_tests {
    use std::collections::VecDeque;

    use ployz_model::MachineSelfTransition;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineLifecycleTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineLifecycleRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineLifecycleTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineLifecycleRpcTransport for FakeMachineLifecycleTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_lifecycle_request(
            &self,
            machine_id: &MachineId,
            operation: MachineLifecycleRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_lifecycle_node_client_builds_transition_request_and_applies_policy() {
        let transport = FakeMachineLifecycleTransport::default();
        let client =
            MachineLifecycleNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
                timeout: Duration::from_secs(11),
            });
        let machine_id = MachineId::new("machine-a");

        client
            .transition_self(&machine_id, MachineSelfTransition::Drain)
            .await
            .expect("transition self");

        let requests = transport.requests.lock().expect("requests");
        let [transition] = requests.as_slice() else {
            panic!("expected one request");
        };
        assert_eq!(transition.0, machine_id);
        assert_eq!(transition.1, MachineLifecycleRpcOperation::TransitionSelf);
        assert!(matches!(
            &transition.2,
            NodeRequest::MachineTransitionSelf {
                transition: MachineSelfTransition::Drain,
            }
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn machine_lifecycle_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMachineLifecycleTransport::with_responses(vec![Ok(
            NodeResponse::error("REMOTE_REFUSED", "peer refused", None),
        )]);
        let error = MachineLifecycleNodeClient::new(transport)
            .transition_self(&machine_id, MachineSelfTransition::Drain)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_transition_self");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineLifecycleTransport::with_responses(vec![Err(
            NodeRpcError::transport("machine_transition_self", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MachineLifecycleNodeClient::new(transport)
            .transition_self(&machine_id, MachineSelfTransition::Drain)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_transition_self");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod machine_update_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineUpdateTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineUpdateRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineUpdateTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineUpdateRpcTransport for FakeMachineUpdateTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_update_request(
            &self,
            machine_id: &MachineId,
            operation: MachineUpdateRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_update_node_client_builds_update_requests_and_applies_policy() {
        let transport = FakeMachineUpdateTransport::default();
        let client = MachineUpdateNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");

        client
            .prepare_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect("prepare update");
        client
            .execute_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect("execute update");

        let requests = transport.requests.lock().expect("requests");
        let [prepare, execute] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(prepare.0, machine_id);
        assert_eq!(prepare.1, MachineUpdateRpcOperation::PrepareUpdate);
        assert!(matches!(
            &prepare.2,
            NodeRequest::MeshPeerPrepareUpdate {
                operation_id,
                version,
            } if operation_id == "operation-1" && version == "v1.2.3"
        ));
        assert_eq!(execute.0, machine_id);
        assert_eq!(execute.1, MachineUpdateRpcOperation::ExecuteUpdate);
        assert!(matches!(
            &execute.2,
            NodeRequest::MeshPeerExecuteUpdate {
                operation_id,
                version,
            } if operation_id == "operation-1" && version == "v1.2.3"
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn machine_update_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMachineUpdateTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MachineUpdateNodeClient::new(transport)
            .prepare_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_update_prepare");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineUpdateTransport::with_responses(vec![Err(
            NodeRpcError::transport("machine_update_execute", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MachineUpdateNodeClient::new(transport)
            .execute_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_update_execute");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod machine_tests {
    use std::collections::VecDeque;

    use ployz_model::{
        MachineLifecycle, MachineMembership, MachineStorageRole, MachineTopology, OverlayIp,
        PublicKey, RegionRole,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineStorageRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineStorageRpcTransport for FakeMachineTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_request(
            &self,
            machine_id: &MachineId,
            operation: MachineStorageRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_storage_node_client_builds_storage_requests_and_applies_policy() {
        let transport = FakeMachineTransport::default();
        let client = MachineStorageNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(9),
        });
        let machine_id = MachineId::new("machine-a");
        let peers = [machine_record("machine-a"), machine_record("machine-b")];
        let peer_payloads = peers
            .iter()
            .map(MachineStorageAuthorityPeer::from)
            .collect::<Vec<_>>();

        client
            .promote_storage_self(&machine_id, StorageReplicaPolicy::R3, &peer_payloads)
            .await
            .expect("promote storage self");
        client
            .restore_storage_self(
                &machine_id,
                &StorageParticipation::Candidate,
                StorageReplicaPolicy::R3,
                &peer_payloads,
            )
            .await
            .expect("restore storage self");

        let requests = transport.requests.lock().expect("requests");
        let [promote, restore] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(promote.0, machine_id);
        assert_eq!(promote.1, MachineStorageRpcOperation::StoragePromoteSelf);
        assert!(matches!(
            &promote.2,
            NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers,
            } if replicas == &StorageReplicaPolicy::R3
                && authority_peers == &peer_payloads
        ));
        assert_eq!(restore.0, machine_id);
        assert_eq!(restore.1, MachineStorageRpcOperation::StorageRestoreSelf);
        assert!(matches!(
            &restore.2,
            NodeRequest::MachineStorageRestoreSelf {
                participation,
                replicas,
                authority_peers,
            } if participation == &StorageParticipation::Candidate
                && replicas == &StorageReplicaPolicy::R3
                && authority_peers == &peer_payloads
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(9)
            }]
        );
    }

    #[tokio::test]
    async fn machine_storage_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");
        let peers = [machine_record("machine-a")];
        let peer_payloads = peers
            .iter()
            .map(MachineStorageAuthorityPeer::from)
            .collect::<Vec<_>>();

        let transport = FakeMachineTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MachineStorageNodeClient::new(transport)
            .promote_storage_self(&machine_id, StorageReplicaPolicy::R3, &peer_payloads)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_storage_promote_self");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_RESTORE_REFUSED",
            "restore refused",
            None,
        ))]);
        let error = MachineStorageNodeClient::new(transport)
            .restore_storage_self(
                &machine_id,
                &StorageParticipation::Candidate,
                StorageReplicaPolicy::R3,
                &peer_payloads,
            )
            .await
            .expect_err("remote restore response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_storage_restore_self");
        assert_eq!(error.code, "REMOTE_RESTORE_REFUSED");
        assert_eq!(error.message, "restore refused");

        let transport = FakeMachineTransport::with_responses(vec![Err(NodeRpcError::transport(
            "machine_storage_restore_self",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = MachineStorageNodeClient::new(transport)
            .restore_storage_self(
                &machine_id,
                &StorageParticipation::Candidate,
                StorageReplicaPolicy::R3,
                &peer_payloads,
            )
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_storage_restore_self");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([id.len() as u8; 32]),
            overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
                .parse()
                .map(OverlayIp)
                .expect("valid overlay"),
            topology: MachineTopology::local(),
            region_role: RegionRole::HomeData,
            subnet: Some("10.42.0.0/24".parse().expect("valid subnet")),
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            lifecycle: MachineLifecycle::Active,
            storage_role: MachineStorageRole::default_authority(),
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod mesh_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMeshTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MeshRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMeshTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MeshRpcTransport for FakeMeshTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn mesh_request(
            &self,
            machine_id: &MachineId,
            operation: MeshRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn mesh_node_client_builds_mesh_membership_requests_and_applies_policy() {
        let transport = FakeMeshTransport::default();
        let client = MeshNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(7),
        });
        let machine_id = MachineId::new("machine-a");
        let network_id = NetworkId::new("network-a");
        let coordinator_id = MachineId::new("coordinator-a");
        let expected_machine_ids = vec![MachineId::new("machine-a"), MachineId::new("machine-b")];
        let removed_machine_id = MachineId::new("machine-b");

        client
            .prepare_destroy(
                &machine_id,
                "destroy-1",
                &network_id,
                &coordinator_id,
                &expected_machine_ids,
            )
            .await
            .expect("prepare destroy");
        client
            .cancel_destroy(&machine_id, "destroy-1")
            .await
            .expect("cancel destroy");
        client
            .execute_destroy(&machine_id, "destroy-1", &network_id)
            .await
            .expect("execute destroy");
        client
            .remove_machine(&machine_id, "remove-1", &network_id, &removed_machine_id)
            .await
            .expect("remove machine");

        let requests = transport.requests.lock().expect("requests");
        let [prepare, cancel, execute, remove] = requests.as_slice() else {
            panic!("expected four requests");
        };
        assert_eq!(prepare.0, machine_id);
        assert_eq!(prepare.1, MeshRpcOperation::PrepareDestroy);
        assert!(matches!(
            &prepare.2,
            NodeRequest::MeshPeerPrepareDestroy {
                operation_id,
                network_id: request_network_id,
                coordinator_id: request_coordinator_id,
                expected_machine_ids: request_expected_machine_ids,
            } if operation_id == "destroy-1"
                && request_network_id == &network_id
                && request_coordinator_id == &coordinator_id
                && request_expected_machine_ids == &expected_machine_ids
        ));
        assert_eq!(cancel.0, machine_id);
        assert_eq!(cancel.1, MeshRpcOperation::CancelDestroy);
        assert!(matches!(
            &cancel.2,
            NodeRequest::MeshPeerCancelDestroy { operation_id } if operation_id == "destroy-1"
        ));
        assert_eq!(execute.0, machine_id);
        assert_eq!(execute.1, MeshRpcOperation::ExecuteDestroy);
        assert!(matches!(
            &execute.2,
            NodeRequest::MeshPeerExecuteDestroy {
                operation_id,
                network_id: request_network_id,
            } if operation_id == "destroy-1" && request_network_id == &network_id
        ));
        assert_eq!(remove.0, machine_id);
        assert_eq!(remove.1, MeshRpcOperation::RemoveMachine);
        assert!(matches!(
            &remove.2,
            NodeRequest::MeshPeerRemoveMachine {
                operation_id,
                network_id: request_network_id,
                machine_id: request_machine_id,
            } if operation_id == "remove-1"
                && request_network_id == &network_id
                && request_machine_id == &removed_machine_id
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(7)
            }]
        );
    }

    #[tokio::test]
    async fn mesh_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");
        let network_id = NetworkId::new("network-a");

        let transport = FakeMeshTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MeshNodeClient::new(transport)
            .execute_destroy(&machine_id, "destroy-1", &network_id)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "mesh_peer_execute_destroy");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMeshTransport::with_responses(vec![Err(NodeRpcError::transport(
            "mesh_peer_prepare_destroy",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = MeshNodeClient::new(transport)
            .prepare_destroy(
                &machine_id,
                "destroy-1",
                &network_id,
                &MachineId::new("coordinator-a"),
                std::slice::from_ref(&machine_id),
            )
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "mesh_peer_prepare_destroy");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");

        let transport = FakeMeshTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REMOVE_REFUSED",
            "remove refused",
            None,
        ))]);
        let error = MeshNodeClient::new(transport)
            .remove_machine(&machine_id, "remove-1", &network_id, &machine_id)
            .await
            .expect_err("remote remove response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "mesh_peer_remove_machine");
        assert_eq!(error.code, "REMOTE_REMOVE_REFUSED");
        assert_eq!(error.message, "remove refused");
    }
}
