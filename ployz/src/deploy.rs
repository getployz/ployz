use ployz_core::{
    ContainerId, ContainerObservation, ContainerRuntimeObservation, DockerVolumeId,
    DockerVolumeName, MachineId, MachineObservation, ResolvedServiceSpec, ServiceId, ServiceMode,
    ServiceVolume, ServiceVolumeReference,
};
use thiserror::Error;

mod comparison;
mod exec;
mod observe;
mod planning;

pub use comparison::compare_specs;
pub(crate) use exec::execute_operations;
pub use exec::{ExecutionError, HealthFailure, HookFailure, MachineAction, execute_plan};
pub use planning::plan_deploy;
pub(crate) use planning::volume_eligible_machine_ids;

fn is_active_runtime(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
        ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
    )
}

fn same_service_mode_kind(left: &ServiceMode, right: &ServiceMode) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeploySnapshot {
    pub machines: Vec<MachineObservation>,
    pub containers: Vec<ContainerObservation>,
    pub volumes: Vec<ObservedDockerVolume>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedDockerVolume {
    pub id: DockerVolumeId,
    pub driver: String,
    pub options: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlanOptions {
    pub force_recreate: bool,
    pub skip_health_monitor: bool,
    /// Caller-supplied entropy keeps the planner pure while varying equal-priority placement.
    pub placement_seed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployPlan {
    pub service_id: ServiceId,
    pub is_new_service: bool,
    pub operation: DeployOperation,
}

impl DeployPlan {
    #[must_use]
    pub fn operations(&self) -> &[DeployOperation] {
        self.operation.operations()
    }

    pub fn failure_outcome<E>(&self, completed_count: usize, error: E) -> Option<DeployOutcome<E>> {
        Self::failure_outcome_from(self.operations(), completed_count, error)
    }

    pub(super) fn failure_outcome_from<E>(
        operations: &[DeployOperation],
        completed_count: usize,
        error: E,
    ) -> Option<DeployOutcome<E>> {
        let completed = operations.get(..completed_count)?;
        let (failed, unexecuted) = operations.get(completed_count..)?.split_first()?;
        Some(DeployOutcome {
            completed: completed.to_vec(),
            failed: Some(FailedOperation::Operation {
                operation: failed.clone(),
                error,
            }),
            unexecuted: unexecuted.to_vec(),
        })
    }

    pub fn replacement_health_failure_outcome<E>(
        &self,
        completed_count: usize,
        error: E,
        compensation: ReplacementCompensation<E>,
    ) -> Option<DeployOutcome<E>> {
        Self::replacement_health_failure_outcome_from(
            self.operations(),
            completed_count,
            error,
            compensation,
        )
    }

    pub(super) fn replacement_health_failure_outcome_from<E>(
        operations: &[DeployOperation],
        completed_count: usize,
        error: E,
        compensation: ReplacementCompensation<E>,
    ) -> Option<DeployOutcome<E>> {
        let completed = operations.get(..completed_count)?;
        let (failed, unexecuted) = operations.get(completed_count..)?.split_first()?;
        let DeployOperation::ReplaceContainer(operation) = failed else {
            return None;
        };
        Some(DeployOutcome {
            completed: completed.to_vec(),
            failed: Some(FailedOperation::ReplacementHealth {
                operation: operation.clone(),
                error,
                compensation,
            }),
            unexecuted: unexecuted.to_vec(),
        })
    }

    #[must_use]
    pub fn success_outcome<E>(&self) -> DeployOutcome<E> {
        DeployOutcome {
            completed: self.operations().to_vec(),
            failed: None,
            unexecuted: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployOutcome<E> {
    pub completed: Vec<DeployOperation>,
    pub failed: Option<FailedOperation<E>>,
    pub unexecuted: Vec<DeployOperation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailedOperation<E> {
    Operation {
        operation: DeployOperation,
        error: E,
    },
    ReplacementHealth {
        operation: ReplacementOperation,
        error: E,
        compensation: ReplacementCompensation<E>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplacementCompensation<E> {
    StartFirst {
        stop_new_container: Result<(), E>,
    },
    StopFirst {
        stop_new_container: Result<(), E>,
        restart_old_container: RestartAttempt<E>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestartAttempt<E> {
    NotAttempted,
    Attempted(Result<(), E>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementOperation {
    pub machine_id: MachineId,
    pub old_container_id: ContainerId,
    pub spec: ResolvedServiceSpec,
    pub skip_health_monitor: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeployOperation {
    CreateVolume {
        machine_id: MachineId,
        volume: ServiceVolume,
    },
    RunContainer {
        machine_id: MachineId,
        spec: ResolvedServiceSpec,
        skip_health_monitor: bool,
    },
    StopContainer {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    RemoveContainer {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    ReplaceContainer(ReplacementOperation),
    StopHook {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    RunHook {
        machine_id: MachineId,
        spec: ResolvedServiceSpec,
        old_hook_containers: Vec<(MachineId, ContainerId)>,
    },
    Sequence {
        operations: Vec<DeployOperation>,
    },
}

impl DeployOperation {
    #[must_use]
    pub fn operations(&self) -> &[DeployOperation] {
        match self {
            Self::Sequence { operations } => operations,
            operation @ (Self::CreateVolume { .. }
            | Self::RunContainer { .. }
            | Self::StopContainer { .. }
            | Self::RemoveContainer { .. }
            | Self::ReplaceContainer(..)
            | Self::StopHook { .. }
            | Self::RunHook { .. }) => std::slice::from_ref(operation),
        }
    }

    fn flatten_into(&self, flattened: &mut Vec<Self>) {
        match self {
            Self::Sequence { operations } => {
                for operation in operations {
                    operation.flatten_into(flattened);
                }
            }
            operation @ (Self::CreateVolume { .. }
            | Self::RunContainer { .. }
            | Self::StopContainer { .. }
            | Self::RemoveContainer { .. }
            | Self::ReplaceContainer(..)
            | Self::StopHook { .. }
            | Self::RunHook { .. }) => flattened.push(operation.clone()),
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanError {
    #[error("no machines available that satisfy all constraints")]
    NoEligibleMachines,
    #[error("service name matches multiple Service IDs: {matches:?}")]
    AmbiguousService { matches: Vec<ServiceId> },
    #[error("service mode cannot be changed")]
    ServiceModeCannotChange,
    #[error("duplicate Service Volume Reference: {reference}")]
    DuplicateVolumeReference { reference: ServiceVolumeReference },
    #[error("mount references an undeclared Service Volume: {reference}")]
    UnknownVolumeReference { reference: ServiceVolumeReference },
    #[error("duplicate config name: {name}")]
    DuplicateConfigName { name: String },
    #[error("config mount references an undeclared config: {name}")]
    UnknownConfigName { name: String },
    #[error("mounted Service Volumes disagree about Docker Volume {name}")]
    ConflictingDockerVolumeDefinitions { name: DockerVolumeName },
}
