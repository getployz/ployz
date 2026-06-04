//! Node-facing domain models.

use serde::{Deserialize, Serialize};

use crate::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedContainerKind {
    Service,
    Predeploy,
    Job,
}

impl ManagedContainerKind {
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Predeploy => "predeploy",
            Self::Job => "job",
        }
    }

    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "service" => Some(Self::Service),
            "predeploy" => Some(Self::Predeploy),
            "job" => Some(Self::Job),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerRuntimeState {
    Running,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerObservation {
    pub node_id: NodeId,
    pub container_id: ContainerId,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
    pub state: ContainerRuntimeState,
}
