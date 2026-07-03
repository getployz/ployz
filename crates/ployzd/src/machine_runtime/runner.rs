use std::future::Future;

use ployz_core::deploy::ImageReference;
use ployz_core::ids::ContainerId;
use std::net::IpAddr;

use crate::docker::labels::{ManagedContainerIdentity, ManagedContainerLabels};
use crate::machine_runtime::protocol::MachineContainerRunSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingManagedContainer {
    pub container_id: ContainerId,
    pub labels: ManagedContainerLabels,
    pub state: ExistingManagedContainerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingManagedContainerState {
    Running { ip: Option<IpAddr> },
    StartableStopped,
    NotStartable { description: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateManagedContainer {
    pub image: ImageReference,
    pub labels: ManagedContainerLabels,
}

#[must_use]
pub fn managed_container_labels(spec: &MachineContainerRunSpec) -> ManagedContainerLabels {
    ManagedContainerLabels {
        namespace_id: spec.namespace_id.clone(),
        service_id: spec.service_id.clone(),
        namespace_revision_entry_id: spec.namespace_revision_entry_id.clone(),
        operation_id: spec.operation_id.clone(),
        step_id: spec.step_id.clone(),
        kind: spec.kind,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerRunnerError {
    ListExisting {
        message: String,
    },
    EnsureEndpointNetwork {
        message: String,
    },
    Create {
        message: String,
    },
    Start {
        container_id: ContainerId,
        message: String,
    },
    Stop {
        container_id: ContainerId,
        message: String,
    },
    Remove {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLogReaderError {
    NotFound {
        container_id: ContainerId,
    },
    ReadFailed {
        container_id: ContainerId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineLogTail {
    pub text: String,
    pub truncated: bool,
}

pub trait MachineContainerRunner {
    fn existing_managed_containers(
        &self,
    ) -> impl Future<Output = Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError>> + Send;

    fn ensure_endpoint_network(
        &self,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> impl Future<Output = Result<ContainerId, MachineContainerRunnerError>> + Send;

    fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;
}

pub trait MachineLogReader {
    fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        tail_lines: Option<u16>,
    ) -> impl Future<Output = Result<MachineLogTail, MachineLogReaderError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineContainerRunDecision {
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
    Conflict(MachineContainerRunConflict),
    Ambiguous {
        operation_id: ployz_core::ids::OperationId,
        step_id: ployz_core::ids::StepId,
        container_ids: Vec<ContainerId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineContainerRunConflict {
    pub container_id: ContainerId,
    pub expected: ManagedContainerLabels,
    pub actual: ManagedContainerLabels,
}

#[must_use]
pub fn decide_container_run(
    expected: &ManagedContainerLabels,
    existing: impl IntoIterator<Item = ExistingManagedContainer>,
) -> MachineContainerRunDecision {
    let mut matches = existing.into_iter().filter(|container| {
        container.labels.operation_id == expected.operation_id
            && container.labels.step_id == expected.step_id
    });

    let Some(first) = matches.next() else {
        return MachineContainerRunDecision::Create {
            labels: expected.clone(),
        };
    };

    let rest = matches.collect::<Vec<_>>();
    if !rest.is_empty() {
        let container_ids = std::iter::once(first.container_id)
            .chain(rest.into_iter().map(|container| container.container_id))
            .collect();
        return MachineContainerRunDecision::Ambiguous {
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
            ExistingManagedContainerState::Running { .. } => {
                MachineContainerRunDecision::ReuseRunning { container_id }
            }
            ExistingManagedContainerState::StartableStopped => {
                MachineContainerRunDecision::StartExisting { container_id }
            }
            ExistingManagedContainerState::NotStartable { .. } => {
                MachineContainerRunDecision::NotStartable {
                    container_id,
                    state,
                }
            }
        };
    }

    MachineContainerRunDecision::Conflict(MachineContainerRunConflict {
        container_id,
        expected: expected.clone(),
        actual: labels,
    })
}
