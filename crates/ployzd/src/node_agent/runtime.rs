use std::future::Future;

use ployz_core::deploy::ImageReference;
use ployz_core::ids::ContainerId;

use crate::docker::labels::ManagedContainerLabels;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingManagedContainer {
    pub container_id: ContainerId,
    pub labels: ManagedContainerLabels,
    pub state: ExistingManagedContainerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingManagedContainerState {
    Running,
    StartableStopped,
    NotStartable { description: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateManagedContainer {
    pub image: ImageReference,
    pub labels: ManagedContainerLabels,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeContainerRunnerError {
    ListExisting {
        message: String,
    },
    Create {
        message: String,
    },
    Start {
        container_id: ContainerId,
        message: String,
    },
}

pub trait NodeContainerRunner {
    fn existing_managed_containers(
        &self,
    ) -> impl Future<Output = Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError>> + Send;

    fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> impl Future<Output = Result<ContainerId, NodeContainerRunnerError>> + Send;

    fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> impl Future<Output = Result<(), NodeContainerRunnerError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeContainerRunDecision {
    Create {
        labels: ManagedContainerLabels,
    },
    ReuseRunning {
        container_id: ContainerId,
    },
    StartExisting {
        container_id: ContainerId,
    },
    NotStartable {
        container_id: ContainerId,
        state: ExistingManagedContainerState,
    },
    Conflict(NodeContainerRunConflict),
    Ambiguous {
        operation_id: ployz_core::ids::OperationId,
        step_id: ployz_core::ids::StepId,
        container_ids: Vec<ContainerId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeContainerRunConflict {
    pub container_id: ContainerId,
    pub expected: ManagedContainerLabels,
    pub actual: ManagedContainerLabels,
}

#[must_use]
pub fn decide_container_run(
    expected: &ManagedContainerLabels,
    existing: impl IntoIterator<Item = ExistingManagedContainer>,
) -> NodeContainerRunDecision {
    let mut matches = existing.into_iter().filter(|container| {
        container.labels.operation_id == expected.operation_id
            && container.labels.step_id == expected.step_id
    });

    let Some(first) = matches.next() else {
        return NodeContainerRunDecision::Create {
            labels: expected.clone(),
        };
    };

    let rest = matches.collect::<Vec<_>>();
    if !rest.is_empty() {
        let container_ids = std::iter::once(first.container_id)
            .chain(rest.into_iter().map(|container| container.container_id))
            .collect();
        return NodeContainerRunDecision::Ambiguous {
            operation_id: expected.operation_id.clone(),
            step_id: expected.step_id.clone(),
            container_ids,
        };
    }

    let ExistingManagedContainer {
        container_id,
        labels,
        state,
    } = first;

    if labels == *expected {
        return match state {
            ExistingManagedContainerState::Running => {
                NodeContainerRunDecision::ReuseRunning { container_id }
            }
            ExistingManagedContainerState::StartableStopped => {
                NodeContainerRunDecision::StartExisting { container_id }
            }
            ExistingManagedContainerState::NotStartable { .. } => {
                NodeContainerRunDecision::NotStartable {
                    container_id,
                    state,
                }
            }
        };
    }

    NodeContainerRunDecision::Conflict(NodeContainerRunConflict {
        container_id,
        expected: expected.clone(),
        actual: labels,
    })
}
