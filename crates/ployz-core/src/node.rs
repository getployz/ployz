//! Node-facing domain models.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;

use crate::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use crate::ops::RoutePort;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContainerRuntimeState {
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint: Option<ContainerEndpoint>,
    },
    Exited,
}

impl ContainerRuntimeState {
    #[must_use]
    pub const fn running_unroutable() -> Self {
        Self::Running { endpoint: None }
    }

    #[must_use]
    pub fn running_at(endpoint: ContainerEndpoint) -> Self {
        Self::Running {
            endpoint: Some(endpoint),
        }
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        match self {
            Self::Running { .. } => true,
            Self::Exited => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerEndpoint {
    pub ip: IpAddr,
    pub port: RoutePort,
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

impl ManagedContainerObservation {
    #[must_use]
    pub fn is_running_service(&self) -> bool {
        self.kind == ManagedContainerKind::Service && self.state.is_running()
    }

    #[must_use]
    pub fn is_running_service_revision(
        &self,
        service_id: &ServiceId,
        revision_id: &RevisionId,
    ) -> bool {
        self.is_running_service()
            && self.service_id == *service_id
            && self.revision_id == *revision_id
    }

    #[must_use]
    pub fn running_service_endpoint(&self) -> Option<&ContainerEndpoint> {
        if self.kind != ManagedContainerKind::Service {
            return None;
        }

        match &self.state {
            ContainerRuntimeState::Running { endpoint } => endpoint.as_ref(),
            ContainerRuntimeState::Exited => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "NodeContainerObservationSnapshotWire",
    into = "NodeContainerObservationSnapshotWire"
)]
pub struct NodeContainerObservationSnapshot {
    node_id: NodeId,
    containers: Vec<ManagedContainerObservation>,
}

impl NodeContainerObservationSnapshot {
    pub fn try_new(
        node_id: NodeId,
        containers: impl IntoIterator<Item = ManagedContainerObservation>,
    ) -> Result<Self, NodeContainerObservationSnapshotError> {
        let containers: Vec<_> = containers.into_iter().collect();
        if let Some(container) = containers
            .iter()
            .find(|container| container.node_id != node_id)
        {
            return Err(NodeContainerObservationSnapshotError::NodeMismatch {
                expected: node_id,
                actual: container.node_id.clone(),
                container_id: container.container_id.clone(),
            });
        }

        Ok(Self {
            node_id,
            containers,
        })
    }

    #[must_use]
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    #[must_use]
    pub fn containers(&self) -> &[ManagedContainerObservation] {
        &self.containers
    }

    pub fn with_container_replaced(
        &self,
        observation: ManagedContainerObservation,
    ) -> Result<Self, NodeContainerObservationSnapshotError> {
        if observation.node_id != self.node_id {
            return Err(NodeContainerObservationSnapshotError::NodeMismatch {
                expected: self.node_id.clone(),
                actual: observation.node_id,
                container_id: observation.container_id,
            });
        }

        let mut containers = self.containers.clone();
        containers.retain(|container| container.container_id != observation.container_id);
        containers.push(observation);

        Self::try_new(self.node_id.clone(), containers)
    }

    #[must_use]
    pub fn container(&self, container_id: &ContainerId) -> Option<&ManagedContainerObservation> {
        self.containers
            .iter()
            .find(|container| &container.container_id == container_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeContainerObservationSnapshotError {
    NodeMismatch {
        expected: NodeId,
        actual: NodeId,
        container_id: ContainerId,
    },
}

impl fmt::Display for NodeContainerObservationSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeMismatch {
                expected,
                actual,
                container_id,
            } => write!(
                formatter,
                "container {} belongs to node {}, not snapshot node {}",
                container_id.as_str(),
                actual.as_str(),
                expected.as_str()
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeContainerObservationSnapshotWire {
    node_id: NodeId,
    containers: Vec<ManagedContainerObservation>,
}

impl TryFrom<NodeContainerObservationSnapshotWire> for NodeContainerObservationSnapshot {
    type Error = NodeContainerObservationSnapshotError;

    fn try_from(value: NodeContainerObservationSnapshotWire) -> Result<Self, Self::Error> {
        Self::try_new(value.node_id, value.containers)
    }
}

impl From<NodeContainerObservationSnapshot> for NodeContainerObservationSnapshotWire {
    fn from(value: NodeContainerObservationSnapshot) -> Self {
        Self {
            node_id: value.node_id,
            containers: value.containers,
        }
    }
}
