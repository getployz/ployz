//! Deploy-facing node runtime command types.

use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::ManagedContainerKind;
use ployz_core::ops::RoutePort;
use serde::{Deserialize, Serialize};

use crate::docker::labels::ManagedContainerIdentity;

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
#[serde(deny_unknown_fields)]
pub struct NodeRunContainerRequest {
    pub node_id: NodeId,
    pub image: ImageReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ContainerEndpointRequest>,
    pub container: NodeContainerRunSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRemoveContainerRequest {
    pub node_id: NodeId,
    pub operation_id: OperationId,
    pub container_id: ContainerId,
    pub expected_identity: ManagedContainerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeStopContainerRequest {
    pub node_id: NodeId,
    pub operation_id: OperationId,
    pub container_id: ContainerId,
    pub expected_identity: ManagedContainerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLogsTailRequest {
    pub node_id: NodeId,
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
