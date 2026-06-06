//! Node-local NATS RPC protocol types.

use crate::docker::labels::ManagedContainerLabels;
use crate::node_runtime_types::{NodeRunContainerOutcome, NodeRunContainerRequest};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, StepId};
use ployz_core::ops::{FailureMessage, OperatorHint};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContainerRunRpcRequest {
    pub image: ImageReference,
    pub labels: ManagedContainerLabels,
}

impl From<NodeRunContainerRequest> for NodeContainerRunRpcRequest {
    fn from(value: NodeRunContainerRequest) -> Self {
        Self {
            image: value.image,
            labels: value.labels,
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
    StartedContainerUnhealthy {
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
}
