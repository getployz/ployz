//! Deploy Intent, Plan Options, and Deploy Outcome shared by the CLI planner and `@ployz/sdk`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    ContainerRuntimeObservation, RequestedServiceSpec, ResolvedServiceSpec, ServiceVolume,
};
use crate::{ContainerId, MachineId, RpcError, ServiceName};
use thiserror::Error;

/// Planner knobs for one Deploy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanOptions {
    /// Recreate containers even when the resolved spec matches.
    pub force_recreate: bool,
    /// Skip waiting on container health after start or replace.
    pub skip_health_monitor: bool,
    /// Caller-supplied entropy keeps the planner pure while varying equal-priority placement.
    pub placement_seed: u64,
}

/// One Service Name this Deploy will apply from the target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceAttempt {
    /// Service Name to apply from `DeployIntent.target`.
    pub name: ServiceName,
}

/// Complete desired Services plus which of those Services this command applies.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeployIntent {
    /// Complete desired Services for this Cluster.
    pub target: Vec<RequestedServiceSpec>,
    /// Service Attempts this command applies. Empty means apply nothing.
    pub apply: Vec<ServiceAttempt>,
    /// Planner knobs for this Deploy.
    pub options: PlanOptions,
    // ponytail: planner graph, not wire. Split if Cloud ever sends depends_on.
    #[serde(default, skip)]
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

    /// Planner `depends_on` edges used to expand and order `apply`.
    #[must_use]
    pub fn dependencies(&self) -> &BTreeMap<ServiceName, Vec<ServiceName>> {
        &self.dependencies
    }
}

/// Evidence from executing a Deploy Plan: every operation completed, or the
/// completed prefix plus the failed operation and the unexecuted rest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

/// Why a Deploy Operation did not complete.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FailedOperation<E> {
    /// The named operation returned `error`.
    Operation {
        operation: DeployOperation,
        error: E,
    },
    /// Replacement started; health failed; `compensation` is what ran next.
    ReplacementHealth {
        operation: ReplacementOperation,
        error: E,
        compensation: ReplacementCompensation<E>,
    },
}

/// Compensation after a replacement health failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReplacementCompensation<E> {
    /// New container started first; `stop_new_container` is that stop attempt.
    StartFirst { stop_new_container: Result<(), E> },
    /// Old container stopped first; `restart_old_container` is that restart attempt.
    StopFirst {
        stop_new_container: Result<(), E>,
        restart_old_container: RestartAttempt<E>,
    },
}

/// Whether restarting the old container was attempted after stop-first replacement failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RestartAttempt<E> {
    /// Restart was not attempted.
    NotAttempted,
    /// Restart was attempted; `Ok(())` or the restart error.
    Attempted(Result<(), E>),
}

/// Replace one container with a newly resolved spec on the same Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplacementOperation {
    /// Machine that hosts both containers.
    pub machine_id: MachineId,
    /// Container being replaced.
    pub old_container_id: ContainerId,
    /// Spec for the replacement container.
    pub spec: ResolvedServiceSpec,
    /// Skip waiting on container health after the replacement starts.
    pub skip_health_monitor: bool,
}

/// One step in a Deploy Plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeployOperation {
    /// Create a Service Volume on `machine_id`.
    CreateVolume {
        machine_id: MachineId,
        volume: ServiceVolume,
    },
    /// Create and start a container from `spec`.
    RunContainer {
        machine_id: MachineId,
        spec: ResolvedServiceSpec,
        skip_health_monitor: bool,
    },
    /// Stop a running container.
    StopContainer {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    /// Remove a stopped container.
    RemoveContainer {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    /// Replace `old_container_id` with a new container from the replacement spec.
    ReplaceContainer(ReplacementOperation),
    /// Run the stop hook for a container.
    StopHook {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    /// Run the start hook for a replacement, with prior hook containers to clean up.
    RunHook {
        machine_id: MachineId,
        spec: ResolvedServiceSpec,
        old_hook_containers: Vec<(MachineId, ContainerId)>,
    },
}

/// Machine RPC invoked while executing one Deploy Operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MachineAction {
    CreateVolume,
    CreateContainer,
    StartContainer,
    InspectContainer,
    StopContainer,
    RemoveContainer,
}

/// Why health monitoring rejected a started container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HealthFailure {
    Cancelled,
    TimedOut,
    Runtime(ContainerRuntimeObservation),
}

/// Why a pre-deploy hook container did not succeed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HookFailure {
    Cancelled { stop_error: Option<RpcError> },
    TimedOut { stop_error: Option<RpcError> },
    Exit(i64),
}

/// Error from executing one Deploy Operation.
#[derive(Clone, Debug, Error, PartialEq, Serialize, Deserialize)]
pub enum ExecutionError {
    #[error("{action:?} failed: {}", error.message)]
    Machine {
        action: MachineAction,
        error: RpcError,
    },
    #[error("container {container_id} failed health monitoring: {failure:?}")]
    Health {
        container_id: ContainerId,
        failure: HealthFailure,
    },
    #[error("hook container {container_id} failed: {failure:?}")]
    Hook {
        container_id: ContainerId,
        failure: HookFailure,
    },
}
