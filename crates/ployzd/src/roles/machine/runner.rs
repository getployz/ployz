use std::future::Future;

use ployz_core::deploy::{ContainerRuntimeSpec, ImageReference};
use ployz_core::ids::ContainerId;
use ployz_core::machine_runtime::ContainerHealth;
use std::net::IpAddr;

use ployz_core::machine_runtime::{ManagedContainerHealthStatus, ManagedContainerIdentity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingManagedContainer {
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
    pub state: ExistingManagedContainerState,
    pub health_status: Option<ManagedContainerHealthStatus>,
    pub resolved_image_identity: Option<String>,
    pub created_at_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingManagedContainerState {
    Running {
        ip: Option<IpAddr>,
        health: ContainerHealth,
    },
    StartableStopped,
    NotStartable {
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateManagedContainer {
    pub image: ImageReference,
    pub runtime: ContainerRuntimeSpec,
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
    Restart {
        container_id: ContainerId,
        message: String,
    },
    Remove {
        container_id: ContainerId,
        message: String,
    },
    RemoveVolume {
        docker_volume_name: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineLogQuery {
    pub tail_lines: Option<u16>,
    pub since_unix_seconds: Option<u64>,
    pub timestamps: MachineLogTimestamps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineLogTimestamps {
    Include,
    Omit,
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

    fn restart_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &ManagedContainerIdentity,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;

    fn remove_volume(
        &self,
        docker_volume_name: &str,
    ) -> impl Future<Output = Result<(), MachineContainerRunnerError>> + Send;
}

pub trait MachineLogReader {
    fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        query: MachineLogQuery,
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
    Ambiguous {
        operation_id: ployz_core::ids::OperationId,
        step_id: ployz_core::ids::StepId,
        container_ids: Vec<ContainerId>,
    },
}

#[must_use]
pub fn decide_container_run(
    expected: &ManagedContainerIdentity,
    existing: impl IntoIterator<Item = ExistingManagedContainer>,
) -> MachineContainerRunDecision {
    let mut matches = existing
        .into_iter()
        .filter(|container| container.identity == *expected);

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
        state,
        ..
    } = first;

    match state {
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
    }
}
