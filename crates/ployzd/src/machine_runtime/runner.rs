use std::future::Future;

use ployz_core::deploy::ImageReference;
use ployz_core::ids::ContainerId;
use std::net::IpAddr;

use ployz_core::machine_runtime::ManagedContainerIdentity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingManagedContainer {
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
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
    pub identity: ManagedContainerIdentity,
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
        identity: ManagedContainerIdentity,
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
    pub expected: ManagedContainerIdentity,
    pub actual: ManagedContainerIdentity,
}

#[must_use]
pub fn decide_container_run(
    expected: &ManagedContainerIdentity,
    existing: impl IntoIterator<Item = ExistingManagedContainer>,
) -> MachineContainerRunDecision {
    let mut matches = existing.into_iter().filter(|container| {
        container.identity.operation_id == expected.operation_id
            && container.identity.step_id == expected.step_id
    });

    let Some(first) = matches.next() else {
        return MachineContainerRunDecision::Create {
            identity: expected.clone(),
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
        identity,
        state,
    } = first;

    if identity == *expected {
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
        actual: identity,
    })
}
