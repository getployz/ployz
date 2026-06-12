//! Node-local NATS RPC protocol types.

use crate::docker::labels::{ManagedContainerIdentity, ManagedContainerLabels};
use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfEndpointRoute, WireGuardEbpfNodeReady,
    WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest, WireGuardPeer, WireGuardPeerEndpoint,
    WireGuardPublicKey,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::ManagedContainerKind;
use ployz_core::ops::{FailureMessage, OperatorHint, RoutePort};
use serde::{Deserialize, Serialize};

/// Shared node RPC response envelope: every endpoint answers either with its
/// success payload or with `{ node_id, error }`. The serialized shape is
/// identical to the previous per-endpoint enums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeRpcResponse<T, E> {
    Ok(T),
    DomainError { node_id: NodeId, error: E },
}

/// Success payloads carry the responding node id so the request side can
/// reject answers from the wrong node.
pub trait NodeRpcResponder {
    fn responder_node_id(&self) -> &NodeId;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerEndpointRequest {
    pub port: RoutePort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContainerRunSpec {
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeRunContainerOutcome {
    Created { container_id: ContainerId },
    ReusedRunning { container_id: ContainerId },
    StartedExisting { container_id: ContainerId },
}

impl NodeRunContainerOutcome {
    #[must_use]
    pub fn container_id(&self) -> &ContainerId {
        match self {
            Self::Created { container_id }
            | Self::ReusedRunning { container_id }
            | Self::StartedExisting { container_id } => container_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeEnsureEndpointNetworkRpcRequest {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeEnsureEndpointNetworkRpcOk {
    pub node_id: NodeId,
}

impl NodeRpcResponder for NodeEnsureEndpointNetworkRpcOk {
    fn responder_node_id(&self) -> &NodeId {
        let Self { node_id } = self;
        node_id
    }
}

pub type NodeEnsureEndpointNetworkRpcResponse =
    NodeRpcResponse<NodeEnsureEndpointNetworkRpcOk, NodeEnsureEndpointNetworkDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeEnsureEndpointNetworkDomainError {
    EnsureFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContainerRunRpcRequest {
    pub image: ImageReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ContainerEndpointRequest>,
    pub container: NodeContainerRunSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContainerRunRpcOk {
    pub node_id: NodeId,
    pub outcome: NodeRunContainerOutcome,
}

impl NodeRpcResponder for NodeContainerRunRpcOk {
    fn responder_node_id(&self) -> &NodeId {
        let Self { node_id, .. } = self;
        node_id
    }
}

pub type NodeContainerRunRpcResponse =
    NodeRpcResponse<NodeContainerRunRpcOk, NodeContainerRunDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeContainerRunDomainError {
    OperationStepConflict {
        container_id: ContainerId,
        expected: ManagedContainerLabels,
        actual: ManagedContainerLabels,
    },
    OperationStepAmbiguous {
        operation_id: OperationId,
        step_id: StepId,
        container_ids: Vec<ContainerId>,
    },
    CreatedContainerStartFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    ExistingContainerStartFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    OperationStepContainerNotStartable {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContainerRemoveRpcRequest {
    pub operation_id: OperationId,
    pub container_id: ContainerId,
    pub expected_identity: ManagedContainerIdentity,
}

/// Shared success payload for container remove/stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContainerRpcOk {
    pub node_id: NodeId,
    pub container_id: ContainerId,
}

impl NodeRpcResponder for NodeContainerRpcOk {
    fn responder_node_id(&self) -> &NodeId {
        let Self { node_id, .. } = self;
        node_id
    }
}

pub type NodeContainerRemoveRpcResponse =
    NodeRpcResponse<NodeContainerRpcOk, NodeContainerRemoveDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeContainerRemoveDomainError {
    RemoveFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContainerStopRpcRequest {
    pub operation_id: OperationId,
    pub container_id: ContainerId,
    pub expected_identity: ManagedContainerIdentity,
}

pub type NodeContainerStopRpcResponse =
    NodeRpcResponse<NodeContainerRpcOk, NodeContainerStopDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeContainerStopDomainError {
    StopFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLogsTailRpcRequest {
    pub container_id: ContainerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLogsTailResult {
    pub node_id: NodeId,
    pub container_id: ContainerId,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLogsTailRpcOk {
    pub value: NodeLogsTailResult,
}

impl NodeRpcResponder for NodeLogsTailRpcOk {
    fn responder_node_id(&self) -> &NodeId {
        let Self { value } = self;
        &value.node_id
    }
}

pub type NodeLogsTailRpcResponse = NodeRpcResponse<NodeLogsTailRpcOk, NodeLogsTailDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeLogsTailDomainError {
    NotFound {
        container_id: ContainerId,
    },
    ReadFailed {
        container_id: ContainerId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeWireGuardEbpfPrepareRpcRequest {
    pub phase: NodeWireGuardEbpfPreparePhase,
    pub operation_id: OperationId,
    pub nodes: Vec<NodeId>,
    pub endpoint_routes: Vec<WireGuardEbpfEndpointRoute>,
    pub peer_endpoints: Vec<WireGuardPeerEndpoint>,
    pub peers: Vec<WireGuardPeer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeWireGuardEbpfPreparePhase {
    ReadPublicKey,
    PrepareDataplane,
}

impl From<WireGuardEbpfPrepareRequest> for NodeWireGuardEbpfPrepareRpcRequest {
    fn from(value: WireGuardEbpfPrepareRequest) -> Self {
        Self {
            phase: NodeWireGuardEbpfPreparePhase::PrepareDataplane,
            operation_id: value.operation_id,
            nodes: value.nodes,
            endpoint_routes: value.endpoint_routes,
            peer_endpoints: value.peer_endpoints,
            peers: value.peers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeWireGuardEbpfPrepareRpcResponse {
    Ok {
        readiness: WireGuardEbpfNodeReady,
    },
    PublicKey {
        node_id: NodeId,
        public_key: WireGuardPublicKey,
    },
    DomainError {
        node_id: NodeId,
        error: NodeWireGuardEbpfPrepareDomainError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeWireGuardEbpfPrepareDomainError {
    Unavailable {
        component: WireGuardEbpfComponent,
        message: FailureMessage,
    },
}

impl From<WireGuardEbpfPrepareError> for NodeWireGuardEbpfPrepareDomainError {
    fn from(value: WireGuardEbpfPrepareError) -> Self {
        match value {
            WireGuardEbpfPrepareError::Unavailable {
                component, message, ..
            } => Self::Unavailable { component, message },
            WireGuardEbpfPrepareError::InvalidReport { message } => Self::Unavailable {
                component: WireGuardEbpfComponent::WireGuard,
                message,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn failure_message(value: &str) -> FailureMessage {
        FailureMessage::try_new(value).expect("valid failure message")
    }

    #[test]
    fn ensure_endpoint_network_response_wire_shape_is_pinned() {
        let ok = NodeEnsureEndpointNetworkRpcResponse::Ok(NodeEnsureEndpointNetworkRpcOk {
            node_id: node_id("node_a"),
        });
        let ok_json = json!({ "status": "ok", "node_id": "node_a" });
        assert_eq!(
            serde_json::to_value(&ok).expect("response serializes"),
            ok_json
        );
        assert_eq!(
            serde_json::from_value::<NodeEnsureEndpointNetworkRpcResponse>(ok_json)
                .expect("response deserializes"),
            ok
        );

        let domain_error = NodeEnsureEndpointNetworkRpcResponse::DomainError {
            node_id: node_id("node_a"),
            error: NodeEnsureEndpointNetworkDomainError::EnsureFailed {
                message: failure_message("ensure failed"),
            },
        };
        let domain_error_json = json!({
            "status": "domain_error",
            "node_id": "node_a",
            "error": { "error": "ensure_failed", "message": "ensure failed" },
        });
        assert_eq!(
            serde_json::to_value(&domain_error).expect("response serializes"),
            domain_error_json
        );
        assert_eq!(
            serde_json::from_value::<NodeEnsureEndpointNetworkRpcResponse>(domain_error_json)
                .expect("response deserializes"),
            domain_error
        );
    }

    #[test]
    fn container_run_response_wire_shape_is_pinned() {
        let ok = NodeContainerRunRpcResponse::Ok(NodeContainerRunRpcOk {
            node_id: node_id("node_a"),
            outcome: NodeRunContainerOutcome::Created {
                container_id: container_id("ctr_123"),
            },
        });
        let ok_json = json!({
            "status": "ok",
            "node_id": "node_a",
            "outcome": { "outcome": "created", "container_id": "ctr_123" },
        });
        assert_eq!(
            serde_json::to_value(&ok).expect("response serializes"),
            ok_json
        );
        assert_eq!(
            serde_json::from_value::<NodeContainerRunRpcResponse>(ok_json)
                .expect("response deserializes"),
            ok
        );
    }

    #[test]
    fn container_remove_and_stop_response_wire_shapes_are_pinned() {
        let removed = NodeContainerRemoveRpcResponse::Ok(NodeContainerRpcOk {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_old"),
        });
        let removed_json = json!({
            "status": "ok",
            "node_id": "node_a",
            "container_id": "ctr_old",
        });
        assert_eq!(
            serde_json::to_value(&removed).expect("response serializes"),
            removed_json
        );
        assert_eq!(
            serde_json::from_value::<NodeContainerRemoveRpcResponse>(removed_json)
                .expect("response deserializes"),
            removed
        );

        let stop_failed = NodeContainerStopRpcResponse::DomainError {
            node_id: node_id("node_a"),
            error: NodeContainerStopDomainError::StopFailed {
                container_id: container_id("ctr_old"),
                message: failure_message("container stop failed: busy"),
                inspect_hint: OperatorHint::try_new("ployz container inspect ctr_old")
                    .expect("valid hint"),
            },
        };
        let stop_failed_json = json!({
            "status": "domain_error",
            "node_id": "node_a",
            "error": {
                "error": "stop_failed",
                "container_id": "ctr_old",
                "message": "container stop failed: busy",
                "inspect_hint": "ployz container inspect ctr_old",
            },
        });
        assert_eq!(
            serde_json::to_value(&stop_failed).expect("response serializes"),
            stop_failed_json
        );
        assert_eq!(
            serde_json::from_value::<NodeContainerStopRpcResponse>(stop_failed_json)
                .expect("response deserializes"),
            stop_failed
        );
    }

    #[test]
    fn logs_tail_response_wire_shape_is_pinned() {
        let ok = NodeLogsTailRpcResponse::Ok(NodeLogsTailRpcOk {
            value: NodeLogsTailResult {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_failed"),
                text: "panic\n".to_owned(),
                truncated: false,
            },
        });
        let ok_json = json!({
            "status": "ok",
            "value": {
                "node_id": "node_a",
                "container_id": "ctr_failed",
                "text": "panic\n",
                "truncated": false,
            },
        });
        assert_eq!(
            serde_json::to_value(&ok).expect("response serializes"),
            ok_json
        );
        assert_eq!(
            serde_json::from_value::<NodeLogsTailRpcResponse>(ok_json)
                .expect("response deserializes"),
            ok
        );
    }
}
