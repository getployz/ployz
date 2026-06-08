//! Deploy-facing node runtime command types.

use crate::docker::labels::ManagedContainerLabels;
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRunContainerRequest {
    pub node_id: NodeId,
    pub image: ImageReference,
    pub labels: ManagedContainerLabels,
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
