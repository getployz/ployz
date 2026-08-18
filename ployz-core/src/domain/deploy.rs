//! Deploy Intent, Plan Options, Preview, and Outcome shared by the CLI planner and `@ployz/sdk`.

use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
};

use serde::{Deserialize, Serialize};

use super::{
    ContainerRuntimeObservation, HealthObservation, RequestedServiceSpec, ResolvedServiceSpec,
    ServiceVolume,
};
use crate::{ContainerId, MachineId, MachineName, ProjectName, RpcError, ServiceName};
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
    /// Project that will own Containers this Deploy creates.
    pub project_name: ProjectName,
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
        project_name: ProjectName,
        target: Vec<RequestedServiceSpec>,
        apply: Vec<ServiceAttempt>,
        options: PlanOptions,
    ) -> Self {
        Self {
            project_name,
            target,
            apply,
            options,
            dependencies: BTreeMap::new(),
        }
    }

    /// Target and apply set from every spec, in that order.
    #[must_use]
    pub fn apply_all<'a>(
        project_name: ProjectName,
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
        Self::new(project_name, target, apply, options)
    }

    /// One-spec target with apply set to that name.
    #[must_use]
    pub fn apply_one(
        project_name: ProjectName,
        spec: RequestedServiceSpec,
        options: PlanOptions,
    ) -> Self {
        let apply = vec![ServiceAttempt {
            name: spec.name.clone(),
        }];
        Self::new(project_name, vec![spec], apply, options)
    }

    /// Target from every loaded spec; `apply` is this command's Service Attempts.
    #[must_use]
    pub fn from_named_specs(
        project_name: ProjectName,
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
        Self::new(
            project_name,
            services.values().cloned().collect(),
            apply,
            options,
        )
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
#[serde(tag = "type", rename_all = "snake_case")]
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
#[serde(tag = "type", rename_all = "snake_case")]
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
#[serde(tag = "type", rename_all = "snake_case")]
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

/// Observer-relative plan-plus-warnings offered for confirmation before one Deploy executes.
///
/// Live Observation shaped for a decision, not persisted state. Confirming executes
/// these operations; it does not re-plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeployPreview {
    /// Project this preview was planned for. Confirm uses this name; it does not re-plan.
    pub project_name: ProjectName,
    /// Pending rows for the operations this snapshot would execute.
    pub operations: Vec<OperationRow>,
    /// Observer-relative warnings for this snapshot, including ingress DNS misses.
    pub warnings: Vec<DeployWarning>,
}

impl DeployPreview {
    /// Plan rows plus observer-relative warnings. `noop` is empty operations.
    #[must_use]
    pub fn new(
        operations: Vec<OperationRow>,
        warnings: Vec<DeployWarning>,
        project_name: ProjectName,
    ) -> Self {
        Self {
            project_name,
            operations,
            warnings,
        }
    }

    /// True when this preview planned no operations.
    #[must_use]
    pub fn noop(&self) -> bool {
        self.operations.is_empty()
    }
}

/// Kind of Machine observation that failed or was omitted while gathering a snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    Container,
    Volume,
}

impl Display for ObservationKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container => f.write_str("container"),
            Self::Volume => f.write_str("volume"),
        }
    }
}

/// A warning attached to a Deploy Preview. Display matches the CLI warning body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeployWarning {
    /// Listing containers or volumes on `machine_id` returned `message`.
    ObservationFailed {
        kind: ObservationKind,
        machine_id: MachineId,
        message: String,
    },
    /// Listing containers or volumes on `machine_id` produced no terminal response.
    ObservationOmitted {
        kind: ObservationKind,
        machine_id: MachineId,
    },
    /// An Ingress Hostname misses this Cluster. The string is the CLI warning body.
    IngressHostname(String),
}

impl Display for DeployWarning {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ObservationFailed {
                kind,
                machine_id,
                message,
            } => write!(f, "{kind} observation failed on {machine_id}: {message}"),
            Self::ObservationOmitted { kind, machine_id } => {
                write!(f, "{kind} observation omitted {machine_id}")
            }
            Self::IngressHostname(message) => f.write_str(message),
        }
    }
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
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HealthFailure {
    Cancelled,
    TimedOut,
    Runtime {
        observation: ContainerRuntimeObservation,
    },
}

/// Why a pre-deploy hook container did not succeed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookFailure {
    Cancelled { stop_error: Option<RpcError> },
    TimedOut { stop_error: Option<RpcError> },
    Exit { code: i64 },
}

/// Error from executing one Deploy Operation.
#[derive(Clone, Debug, Error, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
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
    #[error("deploy cancelled")]
    Cancelled,
}

/// Live evidence of one in-flight Deploy. Not a Watch frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "Outcome owns the completed prefix, failed op, and unexecuted rest"
)]
pub enum DeployEvent {
    /// Full snapshot of every planned row.
    Progress {
        completed: u32,
        total: u32,
        rows: Vec<OperationRow>,
    },
    /// Terminal evidence for this Deploy.
    Outcome {
        outcome: DeployOutcome<ExecutionError>,
    },
}

/// One planned operation plus its current execution status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperationRow {
    /// Zero-based index in the Deploy Plan.
    pub index: u32,
    /// Machine this operation targets.
    pub machine_id: MachineId,
    /// Human-facing Machine Name when known from the snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_name: Option<MachineName>,
    /// Planned operation.
    pub operation: DeployOperation,
    /// Container display name when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Service Name when known from the spec or snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<ServiceName>,
    /// Current status of this row.
    pub status: OperationStatus,
}

/// Status of one operation in a Deploy Progress snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationStatus {
    Pending,
    Running { phase: OperationPhase },
    Completed,
    Failed { error: ExecutionError },
    Unexecuted,
}

/// Phase of a running operation. Wait phases carry clocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationPhase {
    Starting,
    CreatingVolume,
    CreatingContainer,
    StartingContainer,
    WaitingForHealth {
        container_id: ContainerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        health: Option<HealthObservation>,
        elapsed_ms: u64,
        deadline_ms: u64,
    },
    WaitingForHook {
        container_id: ContainerId,
        elapsed_ms: u64,
        deadline_ms: u64,
    },
    StoppingContainer,
    RemovingContainer,
    Compensating,
}

impl OperationRow {
    /// A not-yet-started row for one planned operation.
    #[must_use]
    pub fn pending(
        index: u32,
        operation: DeployOperation,
        machine_name: Option<MachineName>,
        display_name: Option<String>,
        service_name: Option<ServiceName>,
    ) -> Self {
        Self {
            index,
            machine_id: operation.machine_id(),
            machine_name,
            display_name,
            service_name: service_name.or_else(|| operation.service_name().cloned()),
            operation,
            status: OperationStatus::Pending,
        }
    }
}

impl DeployOperation {
    /// Machine this operation targets.
    #[must_use]
    pub fn machine_id(&self) -> MachineId {
        match self {
            Self::CreateVolume { machine_id, .. }
            | Self::RunContainer { machine_id, .. }
            | Self::StopContainer { machine_id, .. }
            | Self::RemoveContainer { machine_id, .. }
            | Self::StopHook { machine_id, .. }
            | Self::RunHook { machine_id, .. } => *machine_id,
            Self::ReplaceContainer(replacement) => replacement.machine_id,
        }
    }

    /// Service Name when this operation carries a spec.
    #[must_use]
    pub fn service_name(&self) -> Option<&ServiceName> {
        match self {
            Self::RunContainer { spec, .. } | Self::RunHook { spec, .. } => Some(&spec.name),
            Self::ReplaceContainer(replacement) => Some(&replacement.spec.name),
            Self::CreateVolume { .. }
            | Self::StopContainer { .. }
            | Self::RemoveContainer { .. }
            | Self::StopHook { .. } => None,
        }
    }

    /// Container ID when this operation names an existing container.
    #[must_use]
    pub fn container_id(&self) -> Option<ContainerId> {
        match self {
            Self::StopContainer { container_id, .. }
            | Self::RemoveContainer { container_id, .. }
            | Self::StopHook { container_id, .. } => Some(*container_id),
            Self::ReplaceContainer(replacement) => Some(replacement.old_container_id),
            Self::CreateVolume { .. } | Self::RunContainer { .. } | Self::RunHook { .. } => None,
        }
    }
}
