use std::collections::BTreeMap;

use ployz_core::{
    ContainerId, ContainerObservation, ContainerRuntimeObservation, DockerVolumeId,
    DockerVolumeName, MachineId, MachineObservation, RequestedServiceSpec, ResolvedServiceSpec,
    ServiceId, ServiceName, ServiceVolume,
};
use thiserror::Error;

mod apply;
mod exec;
mod pipeline;
mod planning;

pub(crate) use apply::{apply_requested, deploy_project, deploy_scale, deploy_spec};
pub use exec::{ExecutionError, HealthFailure, HookFailure, MachineAction, execute_plan};
pub use planning::plan_deploy;
pub use ployz_core::compare_specs;

fn is_active_runtime(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
        ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
    )
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

/// One Service Name this Deploy will apply from the target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceAttempt {
    pub name: ServiceName,
}

/// Complete desired Services plus which of those Services this command applies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployIntent {
    pub target: Vec<RequestedServiceSpec>,
    pub apply: Vec<ServiceAttempt>,
    pub options: PlanOptions,
    dependencies: BTreeMap<ServiceName, Vec<ServiceName>>,
}

impl DeployIntent {
    /// Complete desired Services, the Service Attempts this command applies, and planning options.
    ///
    /// Empty `apply` means apply nothing. Names in `apply` that are absent from
    /// `target` are not planned; they are not a prune.
    #[must_use]
    pub fn new(
        target: Vec<RequestedServiceSpec>,
        apply: Vec<ServiceAttempt>,
        options: PlanOptions,
    ) -> Self {
        Self {
            target,
            apply,
            options,
            dependencies: BTreeMap::new(),
        }
    }

    /// Target and apply set from every spec, in that order.
    #[must_use]
    pub fn apply_all<'a>(
        specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
        options: PlanOptions,
    ) -> Self {
        let target: Vec<_> = specs.into_iter().cloned().collect();
        let apply = target
            .iter()
            .map(|spec| ServiceAttempt {
                name: spec.name.clone(),
            })
            .collect();
        Self::new(target, apply, options)
    }

    /// One-spec target with apply set to that name.
    #[must_use]
    pub fn apply_one(spec: RequestedServiceSpec, options: PlanOptions) -> Self {
        let apply = vec![ServiceAttempt {
            name: spec.name.clone(),
        }];
        Self::new(vec![spec], apply, options)
    }

    /// Target from every loaded spec; `apply` is this command's Service Attempts.
    #[must_use]
    pub fn from_named_specs(
        services: &BTreeMap<String, RequestedServiceSpec>,
        dependencies: &BTreeMap<String, Vec<String>>,
        apply: Vec<ServiceAttempt>,
        options: PlanOptions,
    ) -> Self {
        let dependencies = dependencies
            .iter()
            .filter_map(|(name, deps)| {
                Some((
                    services.get(name)?.name.clone(),
                    deps.iter()
                        .filter_map(|dep| services.get(dep).map(|spec| spec.name.clone()))
                        .collect(),
                ))
            })
            .collect();
        Self::new(services.values().cloned().collect(), apply, options)
            .with_dependencies(dependencies)
    }

    /// `depends_on` edges used to expand and order `apply` inside the planner.
    #[must_use]
    pub fn with_dependencies(
        mut self,
        dependencies: BTreeMap<ServiceName, Vec<ServiceName>>,
    ) -> Self {
        self.dependencies = dependencies;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployPlan {
    pub operations: Vec<DeployOperation>,
}

impl DeployPlan {
    #[must_use]
    pub fn new(operations: Vec<DeployOperation>) -> Self {
        Self { operations }
    }

    pub fn failure_outcome<E>(&self, completed_count: usize, error: E) -> Option<DeployOutcome<E>> {
        Self::failure_outcome_from(&self.operations, completed_count, error)
    }

    pub(super) fn failure_outcome_from<E>(
        operations: &[DeployOperation],
        completed_count: usize,
        error: E,
    ) -> Option<DeployOutcome<E>> {
        let completed = operations.get(..completed_count)?;
        let (failed, unexecuted) = operations.get(completed_count..)?.split_first()?;
        Some(DeployOutcome::Failed {
            completed: completed.to_vec(),
            failed: FailedOperation::Operation {
                operation: failed.clone(),
                error,
            },
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
            &self.operations,
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
        Some(DeployOutcome::Failed {
            completed: completed.to_vec(),
            failed: FailedOperation::ReplacementHealth {
                operation: operation.clone(),
                error,
                compensation,
            },
            unexecuted: unexecuted.to_vec(),
        })
    }

    #[must_use]
    pub fn success_outcome<E>(&self) -> DeployOutcome<E> {
        DeployOutcome::Success {
            completed: self.operations.clone(),
        }
    }
}

/// Evidence from executing a Deploy Plan: every operation completed, or the
/// completed prefix plus the failed operation and the unexecuted rest.
#[derive(Clone, Debug, Eq, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "Failed must own the named op and unexecuted rest; boxing would not change the states"
)]
pub enum DeployOutcome<E> {
    /// Every planned operation completed.
    Success { completed: Vec<DeployOperation> },
    /// Execution stopped at `failed`; `unexecuted` is the rest of the plan.
    Failed {
        completed: Vec<DeployOperation>,
        failed: FailedOperation<E>,
        unexecuted: Vec<DeployOperation>,
    },
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
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanError {
    #[error("no machines available that satisfy all constraints")]
    NoEligibleMachines,
    #[error("service name matches multiple Service IDs: {matches:?}")]
    AmbiguousService { matches: Vec<ServiceId> },
    #[error("service mode cannot be changed")]
    ServiceModeCannotChange,
    #[error("mounted Service Volumes disagree about Docker Volume {name}")]
    ConflictingDockerVolumeDefinitions { name: DockerVolumeName },
    #[error("plan service '{service}': {source}")]
    Service {
        service: String,
        #[source]
        source: Box<PlanError>,
    },
    #[error(
        "Docker Volume {name} cannot be shared by global service '{global}' and replicated service '{replicated}'"
    )]
    MixedVolumeModes {
        name: DockerVolumeName,
        global: String,
        replicated: String,
    },
    #[error("dependency cycle at service '{service}'")]
    DependencyCycle { service: String },
}
