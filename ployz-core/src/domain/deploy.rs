//! Deploy Intent, Plan Options, Preview, and Outcome shared by the CLI planner and `@ployz/sdk`.

use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    num::NonZeroU64,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

use super::{
    ContainerRuntimeObservation, HealthObservation, RequestedServiceSpec, ResolvedServiceSpec,
    ServiceVolume,
};
use crate::{
    ContainerId, DockerVolumeId, MachineId, MachineName, ProjectName, QualifiedService, RpcError,
    ServiceName, ServiceVolumeReference,
};
use thiserror::Error;

/// Planner knobs for one Deploy.
///
/// `selected` is the Service list this command applies. Empty means full
/// reconciliation of `DeployIntent.target`. Non-empty means partial. There is
/// no independent prune flag.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanOptions {
    /// Recreate containers even when the resolved spec matches.
    pub force_recreate: bool,
    /// Skip waiting on container health after start or replace.
    pub skip_health_monitor: bool,
    /// Caller-supplied entropy keeps the planner pure while varying equal-priority placement.
    pub placement_seed: u64,
    /// Service Names this command applies. Empty means full reconciliation.
    #[serde(default)]
    pub selected: Vec<ServiceAttempt>,
}

/// One Service Name this Deploy will apply from the target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceAttempt {
    /// Service Name to apply from `DeployIntent.target`.
    pub name: ServiceName,
}

/// A positive maximum byte count for one Provisioned Volume.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProvisionedVolumeMaximumBytes(NonZeroU64);

impl ProvisionedVolumeMaximumBytes {
    /// Construct a Provisioned Volume bound from a positive byte count.
    #[must_use]
    pub const fn new(bytes: NonZeroU64) -> Self {
        Self(bytes)
    }

    /// The positive maximum byte count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl Display for ProvisionedVolumeMaximumBytes {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Serialize for ProvisionedVolumeMaximumBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ProvisionedVolumeMaximumBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map(Self)
            .map_err(D::Error::custom)
    }
}

/// One Provisioned Volume declaration in a Deploy Intent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedVolume {
    /// Service whose local Volume Reference is declared.
    pub service: ServiceName,
    /// Service-local reference that resolves to the Docker Volume declaration.
    pub reference: ServiceVolumeReference,
    /// Required positive storage bound in bytes.
    pub maximum_bytes: ProvisionedVolumeMaximumBytes,
}

/// Complete desired Services plus which of those Services this command applies.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeployIntent {
    /// Project that will own Containers this Deploy creates.
    pub project_name: ProjectName,
    /// Complete desired Services for this Cluster.
    pub target: Vec<RequestedServiceSpec>,
    /// Bounded Provisioned Volumes referenced by Services in `target`.
    pub provisioned_volumes: Vec<ProvisionedVolume>,
    /// Planner knobs for this Deploy, including the selected Service list.
    pub options: PlanOptions,
    // ponytail: planner graph, not wire. Split if Cloud ever sends depends_on.
    #[serde(default, skip)]
    dependencies: BTreeMap<ServiceName, Vec<ServiceDependency>>,
    #[serde(default, skip)]
    service_profiles: BTreeMap<ServiceName, Vec<String>>,
    #[serde(default, skip)]
    requested_profiles: Vec<String>,
    #[serde(default, skip)]
    compose_refusal: Option<ComposePruneRefusal>,
}

impl DeployIntent {
    /// Complete desired Services and planning options.
    ///
    /// Empty `options.selected` means full reconciliation of `target`. Names in
    /// `selected` that are absent from `target` are not planned; they are not a
    /// prune.
    #[must_use]
    pub fn new(
        project_name: ProjectName,
        target: Vec<RequestedServiceSpec>,
        options: PlanOptions,
    ) -> Self {
        Self {
            project_name,
            target,
            provisioned_volumes: Vec::new(),
            options,
            dependencies: BTreeMap::new(),
            service_profiles: BTreeMap::new(),
            requested_profiles: Vec::new(),
            compose_refusal: None,
        }
    }

    /// Full reconciliation of every spec: `target` is those specs and `selected` stays empty.
    #[must_use]
    pub fn apply_all<'a>(
        project_name: ProjectName,
        specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
        options: PlanOptions,
    ) -> Self {
        Self::new(project_name, specs.into_iter().cloned().collect(), options)
    }

    /// One-spec target; `selected` is that name, so the Deploy is partial.
    #[must_use]
    pub fn apply_one(
        project_name: ProjectName,
        spec: RequestedServiceSpec,
        mut options: PlanOptions,
    ) -> Self {
        options.selected = vec![ServiceAttempt {
            name: spec.name.clone(),
        }];
        Self::new(project_name, vec![spec], options)
    }

    /// Target from every loaded spec; `options.selected` is this command's Service list.
    #[must_use]
    pub fn from_named_specs(
        project_name: ProjectName,
        services: &BTreeMap<String, RequestedServiceSpec>,
        dependencies: &BTreeMap<String, Vec<ServiceDependency>>,
        options: PlanOptions,
    ) -> Self {
        let dependencies = dependencies
            .iter()
            .filter_map(|(name, deps)| {
                Some((
                    services.get(name)?.name.clone(),
                    deps.iter()
                        .filter(|dependency| services.contains_key(dependency.service.as_str()))
                        .cloned()
                        .collect(),
                ))
            })
            .collect();
        Self::new(project_name, services.values().cloned().collect(), options)
            .with_dependencies(dependencies)
    }

    /// `depends_on` edges used to expand and order `selected` inside the planner.
    #[must_use]
    pub fn with_dependencies(
        mut self,
        dependencies: BTreeMap<ServiceName, Vec<ServiceDependency>>,
    ) -> Self {
        self.dependencies = dependencies;
        self
    }

    /// Compose `profiles:` per Service Name in `target`.
    #[must_use]
    pub fn with_service_profiles(
        mut self,
        service_profiles: BTreeMap<ServiceName, Vec<String>>,
    ) -> Self {
        self.service_profiles = service_profiles;
        self
    }

    /// Profiles requested for this command. They decide what starts, not what exists.
    #[must_use]
    pub fn with_requested_profiles(mut self, requested_profiles: Vec<String>) -> Self {
        self.requested_profiles = requested_profiles;
        self
    }

    /// Compose-side reason pruning is refused, if any.
    #[must_use]
    pub fn with_compose_refusal(mut self, compose_refusal: Option<ComposePruneRefusal>) -> Self {
        self.compose_refusal = compose_refusal;
        self
    }

    /// Planner `depends_on` edges used to expand and order `selected`.
    #[must_use]
    pub fn dependencies(&self) -> &BTreeMap<ServiceName, Vec<ServiceDependency>> {
        &self.dependencies
    }

    /// Whether `name` starts given the requested profile list.
    #[must_use]
    pub fn service_starts(&self, name: &ServiceName) -> bool {
        profiles_enable_start(
            self.service_profiles
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            &self.requested_profiles,
        )
    }

    /// Why pruning is refused for this Intent and Snapshot completeness.
    #[must_use]
    pub fn prune_refusal(&self, snapshot_complete: bool) -> Option<PruneRefusal> {
        if !snapshot_complete {
            Some(PruneRefusal::IncompleteSnapshot)
        } else if !self.options.selected.is_empty() {
            Some(PruneRefusal::SelectedServices)
        } else {
            self.compose_refusal.map(PruneRefusal::from)
        }
    }
}

/// Compose condition on one Service dependency edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyCondition {
    /// Preserve dependency ordering only.
    ServiceStarted,
    /// Wait for every observed dependency Service Container to be healthy.
    ServiceHealthy,
}

/// One parsed dependency edge used by Deploy planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDependency {
    /// Service that the dependent Service requires.
    pub service: ServiceName,
    /// Condition the dependency must satisfy before the dependent starts.
    pub condition: DependencyCondition,
}

/// Unprofiled Services always start; otherwise any requested profile match starts them.
#[must_use]
pub fn profiles_enable_start(service_profiles: &[String], requested_profiles: &[String]) -> bool {
    service_profiles.is_empty()
        || service_profiles
            .iter()
            .any(|profile| requested_profiles.contains(profile))
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
    /// Wait for every observed Service Container of `dependency` before starting `dependent`.
    WaitHealthy {
        machine_id: MachineId,
        dependent: QualifiedService,
        dependency: QualifiedService,
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
    /// Destroy a named Docker Volume. Project removal may emit this; `preview_deploy` never does.
    RemoveVolume { id: DockerVolumeId },
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
    /// Visible Services in the Project that Compose no longer declares.
    #[serde(default)]
    pub would_remove: Vec<QualifiedService>,
    /// Compose-declared Docker Volumes owned by this Project that this Compose
    /// input no longer declares. They are not deleted.
    #[serde(default)]
    pub preserved_volumes: Vec<PreservedVolume>,
    /// Why pruning will not run. `None` means obsolete Services are removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_refusal: Option<PruneRefusal>,
}

/// A Compose-declared Docker Volume this Deploy keeps because it is omitted
/// from this Deploy's target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreservedVolume {
    /// Machine-local Docker Volume identity.
    pub id: DockerVolumeId,
    /// Machine Name from this observer's snapshot when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_name: Option<MachineName>,
}

/// Why a loaded Compose Project is incomplete for reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposePruneRefusal {
    /// Profiled Services were removed before planning.
    FilteredProfiles,
    /// The Project name was guessed from a directory while a non-default Compose file was named explicitly.
    GuessedProjectName,
}

impl From<ComposePruneRefusal> for PruneRefusal {
    fn from(reason: ComposePruneRefusal) -> Self {
        match reason {
            ComposePruneRefusal::FilteredProfiles => Self::FilteredProfiles,
            ComposePruneRefusal::GuessedProjectName => Self::GuessedProjectName,
        }
    }
}

/// Why a full reconciliation must not remove visible drift.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneRefusal {
    /// Required Container or Docker Volume evidence is missing from this Machine's visible fan-out.
    IncompleteSnapshot,
    /// The command named specific Services, so it is not a full reconciliation.
    SelectedServices,
    /// Profiled Services were removed before planning.
    FilteredProfiles,
    /// The Project name was guessed from a directory while a non-default Compose file was named explicitly.
    GuessedProjectName,
}

impl Display for PruneRefusal {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteSnapshot => f.write_str(
                "Pruning is disabled because the Deploy Snapshot is incomplete relative to this Machine's current visible fan-out; required Container or Docker Volume evidence is missing. This is not Cluster completeness.",
            ),
            Self::SelectedServices => f.write_str(
                "Pruning is disabled because this command named specific Services, so it is not a full reconciliation.",
            ),
            Self::FilteredProfiles => f.write_str(
                "Pruning is disabled because profiled Services were removed before planning, so the loaded Compose Project is incomplete for reconciliation.",
            ),
            Self::GuessedProjectName => f.write_str(
                "Pruning is disabled because the Project name was guessed from a directory while a non-default Compose file was named explicitly.",
            ),
        }
    }
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
            would_remove: Vec::new(),
            preserved_volumes: Vec::new(),
            prune_refusal: None,
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
    /// Conflict detection used this Machine's current view and does not claim uniqueness.
    ObserverRelativeHostnameConflict,
    /// `--skip-health` weakened one `service_healthy` edge to ordering only.
    SkippedDependencyHealth {
        dependent: QualifiedService,
        dependency: QualifiedService,
    },
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
            Self::ObserverRelativeHostnameConflict => f.write_str(
                "Hostname conflict detection is observer-relative to this Machine's current visible fan-out and does not claim uniqueness.",
            ),
            Self::SkippedDependencyHealth {
                dependent,
                dependency,
            } => write!(
                f,
                "--skip-health treats dependency {dependent} -> {dependency} as service_started"
            ),
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
    RemoveVolume,
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

/// Why a `service_healthy` dependency gate failed.
#[derive(Clone, Debug, Error, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DependencyHealthFailure {
    #[error("deploy cancelled")]
    Cancelled,
    #[error("no Service Containers were observed")]
    NoContainers,
    #[error("container observation failed: {}", error.message)]
    Observation { error: RpcError },
    #[error("container {container_id} failed health monitoring: {failure:?}")]
    Container {
        container_id: ContainerId,
        failure: HealthFailure,
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
    #[error("dependency {dependency} failed health gate: {failure}")]
    DependencyHealth {
        dependency: QualifiedService,
        failure: DependencyHealthFailure,
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
    RemovingVolume,
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
            | Self::WaitHealthy { machine_id, .. }
            | Self::RunContainer { machine_id, .. }
            | Self::StopContainer { machine_id, .. }
            | Self::RemoveContainer { machine_id, .. }
            | Self::StopHook { machine_id, .. }
            | Self::RunHook { machine_id, .. } => *machine_id,
            Self::ReplaceContainer(replacement) => replacement.machine_id,
            Self::RemoveVolume { id } => id.machine_id,
        }
    }

    /// Service Name when this operation carries a spec.
    #[must_use]
    pub fn service_name(&self) -> Option<&ServiceName> {
        match self {
            Self::WaitHealthy { dependent, .. } => Some(&dependent.name),
            Self::RunContainer { spec, .. } | Self::RunHook { spec, .. } => Some(&spec.name),
            Self::ReplaceContainer(replacement) => Some(&replacement.spec.name),
            Self::CreateVolume { .. }
            | Self::StopContainer { .. }
            | Self::RemoveContainer { .. }
            | Self::StopHook { .. }
            | Self::RemoveVolume { .. } => None,
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
            Self::CreateVolume { .. }
            | Self::WaitHealthy { .. }
            | Self::RunContainer { .. }
            | Self::RunHook { .. }
            | Self::RemoveVolume { .. } => None,
        }
    }
}
