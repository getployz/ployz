mod deploy;
mod image;
mod machine;
mod mesh;
mod probe;
mod volume_zfs;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ployz_node_api::NodeResponse;

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
pub use machine::{
    MachineLifecycleNodeClient, MachineLifecycleRpcOperation, MachineLifecycleRpcTransport,
    MachineOperationNodeClient, MachineOperationRpcOperation, MachineOperationRpcTransport,
    MachineStorageNodeClient, MachineStorageRpcOperation, MachineStorageRpcTransport,
    MachineUpdateNodeClient, MachineUpdateRpcOperation, MachineUpdateRpcTransport,
};
pub use mesh::{
    MeshNodeClient, MeshReadinessNodeClient, MeshReadinessRpcOperation, MeshReadinessRpcTransport,
    MeshRpcOperation, MeshRpcTransport,
};
pub use probe::{NodeProbeNodeClient, NodeProbeRpcOperation, NodeProbeRpcTransport};
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
