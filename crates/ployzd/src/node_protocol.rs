//! Node-local NATS RPC protocol types.

use crate::docker::labels::ManagedContainerLabels;
use crate::node_runtime_types::{
    ContainerEndpointRequest, NodeContainerRunSpec, NodeEnsureEndpointNetworkRequest,
    NodeLogsTailRequest, NodeLogsTailResult, NodeRemoveContainerRequest, NodeRunContainerOutcome,
    NodeRunContainerRequest, NodeStopContainerRequest,
};
use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfEndpointRoute, WireGuardEbpfNodeReady,
    WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest, WireGuardPeer, WireGuardPeerEndpoint,
    WireGuardPublicKey,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, StepId};
use ployz_core::ops::{FailureMessage, OperatorHint};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeEnsureEndpointNetworkRpcRequest {
    pub operation_id: OperationId,
}

impl From<NodeEnsureEndpointNetworkRequest> for NodeEnsureEndpointNetworkRpcRequest {
    fn from(value: NodeEnsureEndpointNetworkRequest) -> Self {
        Self {
            operation_id: value.operation_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeEnsureEndpointNetworkRpcResponse {
    Ok {
        node_id: NodeId,
    },
    DomainError {
        node_id: NodeId,
        error: NodeEnsureEndpointNetworkDomainError,
    },
}

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

impl From<NodeRunContainerRequest> for NodeContainerRunRpcRequest {
    fn from(value: NodeRunContainerRequest) -> Self {
        Self {
            image: value.image,
            endpoint: value.endpoint,
            container: value.container,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeContainerRunRpcResponse {
    Ok {
        node_id: NodeId,
        outcome: NodeRunContainerOutcome,
    },
    DomainError {
        node_id: NodeId,
        error: NodeContainerRunDomainError,
    },
}

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
    pub expected_identity: crate::docker::labels::ManagedContainerIdentity,
}

impl From<NodeRemoveContainerRequest> for NodeContainerRemoveRpcRequest {
    fn from(value: NodeRemoveContainerRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            container_id: value.container_id,
            expected_identity: value.expected_identity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeContainerRemoveRpcResponse {
    Ok {
        node_id: NodeId,
        container_id: ContainerId,
    },
    DomainError {
        node_id: NodeId,
        error: NodeContainerRemoveDomainError,
    },
}

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
    pub expected_identity: crate::docker::labels::ManagedContainerIdentity,
}

impl From<NodeStopContainerRequest> for NodeContainerStopRpcRequest {
    fn from(value: NodeStopContainerRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            container_id: value.container_id,
            expected_identity: value.expected_identity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeContainerStopRpcResponse {
    Ok {
        node_id: NodeId,
        container_id: ContainerId,
    },
    DomainError {
        node_id: NodeId,
        error: NodeContainerStopDomainError,
    },
}

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

impl From<NodeLogsTailRequest> for NodeLogsTailRpcRequest {
    fn from(value: NodeLogsTailRequest) -> Self {
        Self {
            container_id: value.container_id,
            tail_lines: value.tail_lines,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeLogsTailRpcResponse {
    Ok {
        value: NodeLogsTailResult,
    },
    DomainError {
        node_id: NodeId,
        error: NodeLogsTailDomainError,
    },
}

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
