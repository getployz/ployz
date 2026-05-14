mod deploy;
mod image;
mod machine;
mod mesh;
mod volume_zfs;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ployz_model::MachineId;
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
