use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use ployz_core::{
    BridgeEndpointCapacity, ContainerObservation, ContainerRuntimeObservation, DockerVolume,
    DockerVolumeId, DockerVolumeName, IngressHost, IngressLabelTooLong, MachineFailure, MachineId,
    MachineName, MachineObservation, MachineTarget, PartialResult, ProjectName,
    ProvisionedVolumeMaximumBytes, QualifiedService, RpcError, RpcErrorCode, ServiceName,
    ServiceObservation, VolumeInventory, VolumeObservationFailure, derive_services,
};
use thiserror::Error;

use crate::{
    compose::ComposeProject,
    dns::{DomainRequired, ExpandIngressError},
};

mod apply;
mod exec;
mod pipeline;
mod planning;
mod progress;
mod render;

pub(crate) use apply::{
    ApplyError, ConfirmGate, apply_requested, deploy_project, deploy_scale, deploy_spec,
    remove_project,
};
pub use pipeline::DeployError;
pub(crate) use pipeline::{ReconciliationHints, plan_options};
pub(crate) use planning::capacity::endpoint_capacity_error;
pub use planning::{
    DeployPlan, IngressContext, VolumeFate, data_loss_from_plan, plan_deploy, plan_project_removal,
    preview_deploy,
};
pub use ployz_core::compare_specs;
pub use ployz_core::{
    ComposePruneRefusal, DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployPreview,
    DeployWarning, ExecutionError, FailedOperation, HealthFailure, HookFailure, MachineAction,
    ObservationKind, OperationPhase, OperationRow, OperationStatus, PlanOptions, PruneRefusal,
    ReplacementCompensation, ReplacementOperation, RestartAttempt, ServiceAttempt, StopAttempt,
};

pub(crate) use progress::pending_rows;

fn is_active_runtime(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
        ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
    )
}

/// Flat Docker Volume evidence consumed by Deploy planning.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VolumeSnapshot {
    observations: Vec<DockerVolume>,
    named_failures: Vec<VolumeObservationFailure>,
    machine_failures: Vec<MachineFailure<RpcError>>,
    omissions: Vec<MachineId>,
}

impl VolumeSnapshot {
    pub(crate) fn from_partial(
        partial: PartialResult<VolumeInventory, RpcError>,
    ) -> Result<Self, RpcError> {
        let PartialResult {
            successes,
            failures: machine_failures,
            omissions,
        } = partial;
        let mut observations = Vec::new();
        let mut named_failures = Vec::new();
        for success in successes {
            observations.extend(success.value.volumes);
            named_failures.extend(success.value.failures);
        }
        Self::try_from_parts(observations, named_failures, machine_failures, omissions).map_err(
            |error| RpcError {
                code: RpcErrorCode::Internal,
                message: format!("invalid Docker Volume snapshot: {}", error.message),
                details: error.details,
            },
        )
    }

    /// Build a validated complete snapshot containing only successful observations.
    ///
    /// # Errors
    ///
    /// Returns [`RpcErrorCode::InvalidArgument`] when an observation is duplicated.
    pub fn try_from_observations(
        observations: impl IntoIterator<Item = DockerVolume>,
    ) -> Result<Self, RpcError> {
        Self::try_from_parts(
            observations.into_iter().collect(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Build a validated snapshot from successful, failed, and omitted observations.
    ///
    /// # Errors
    ///
    /// Returns [`RpcErrorCode::InvalidArgument`] when evidence is duplicated or contradictory.
    pub fn try_from_parts(
        observations: Vec<DockerVolume>,
        named_failures: Vec<VolumeObservationFailure>,
        machine_failures: Vec<MachineFailure<RpcError>>,
        omissions: Vec<MachineId>,
    ) -> Result<Self, RpcError> {
        let snapshot = Self {
            observations,
            named_failures,
            machine_failures,
            omissions,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn validate(&self) -> Result<(), RpcError> {
        let invalid = |message| RpcError {
            code: RpcErrorCode::InvalidArgument,
            message,
            details: serde_json::Value::Null,
        };
        let mut ids = BTreeSet::new();
        for volume in &self.observations {
            if !ids.insert(&volume.id) {
                return Err(invalid(format!(
                    "Docker Volume observation {:?} is repeated",
                    volume.id
                )));
            }
        }
        for failure in &self.named_failures {
            if !ids.insert(&failure.id) {
                return Err(invalid(format!(
                    "Docker Volume evidence {:?} is repeated or contradictory",
                    failure.id
                )));
            }
        }

        let mut machine_gaps = BTreeSet::new();
        for failure in &self.machine_failures {
            if !machine_gaps.insert(failure.machine_id) {
                return Err(invalid(format!(
                    "Docker Volume inventory failure for Machine {} is repeated",
                    failure.machine_id
                )));
            }
        }
        for machine_id in &self.omissions {
            if !machine_gaps.insert(*machine_id) {
                return Err(invalid(format!(
                    "Docker Volume inventory gap for Machine {machine_id} is repeated or contradictory"
                )));
            }
        }
        if let Some(id) = ids
            .into_iter()
            .find(|id| machine_gaps.contains(&id.machine_id))
        {
            return Err(invalid(format!(
                "Docker Volume evidence {id:?} contradicts its Machine inventory gap"
            )));
        }
        Ok(())
    }

    /// Successful Docker Volume observations.
    #[must_use]
    pub fn observations(&self) -> &[DockerVolume] {
        &self.observations
    }

    pub(crate) fn known_ids(&self) -> impl Iterator<Item = &DockerVolumeId> {
        self.observations
            .iter()
            .map(|volume| &volume.id)
            .chain(self.named_failures.iter().map(|failure| &failure.id))
    }

    fn affects_required(&self, required: &BTreeSet<MachineId>) -> bool {
        affects_required(&self.machine_failures, &self.omissions, required)
            || self
                .named_failures
                .iter()
                .any(|failure| required.contains(&failure.id.machine_id))
    }

    pub(crate) fn machine_gap(&self, machine_id: MachineId) -> Option<String> {
        self.machine_failures
            .iter()
            .find(|failure| failure.machine_id == machine_id)
            .map(|failure| format!("Docker Volume inventory failed: {}", failure.error.message))
            .or_else(|| {
                self.omissions
                    .contains(&machine_id)
                    .then(|| "Docker Volume inventory produced no terminal response".into())
            })
    }

    pub(crate) fn named_gap(
        &self,
        mut relevant: impl FnMut(&DockerVolumeId) -> bool,
    ) -> Option<(DockerVolumeId, String)> {
        self.named_failures
            .iter()
            .find(|failure| relevant(&failure.id))
            .map(|failure| (failure.id.clone(), failure.error.message.clone()))
    }

    pub(crate) fn deploy_warnings(&self) -> impl Iterator<Item = DeployWarning> + '_ {
        self.machine_failures
            .iter()
            .map(|failure| DeployWarning::ObservationFailed {
                kind: ObservationKind::Volume,
                machine_id: failure.machine_id,
                message: failure.error.message.clone(),
            })
            .chain(
                self.omissions
                    .iter()
                    .map(|machine_id| DeployWarning::ObservationOmitted {
                        kind: ObservationKind::Volume,
                        machine_id: *machine_id,
                    }),
            )
            .chain(
                self.named_failures
                    .iter()
                    .map(|failure| DeployWarning::ObservationFailed {
                        kind: ObservationKind::Volume,
                        machine_id: failure.id.machine_id,
                        message: format!(
                            "Docker Volume {}: {}",
                            failure.id.name, failure.error.message
                        ),
                    }),
            )
    }

    pub(crate) fn listing_warnings(&self) -> impl Iterator<Item = String> + '_ {
        self.machine_failures
            .iter()
            .map(|failure| {
                format!(
                    "WARNING: Machine {} failed listing volumes: {}",
                    failure.machine_id, failure.error.message
                )
            })
            .chain(self.omissions.iter().map(|machine_id| {
                format!("WARNING: Machine {machine_id} was omitted listing volumes")
            }))
            .chain(self.named_failures.iter().map(|failure| {
                format!(
                    "WARNING: Machine {} Docker Volume {}: {}",
                    failure.id.machine_id, failure.id.name, failure.error.message
                )
            }))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeploySnapshot {
    pub machines: Vec<MachineObservation>,
    pub containers: Vec<ContainerObservation>,
    /// Successful, failed, and omitted Docker Volume observations by Machine.
    pub volume_snapshot: VolumeSnapshot,
    /// Target-specific Container listing failures from this observer's fan-out.
    pub container_failures: Vec<MachineFailure<RpcError>>,
    /// Required Container queries that produced no terminal response.
    pub container_omissions: Vec<MachineId>,
    /// Fresh targeted bridge observations used only for capacity admission.
    /// Fresh telemetry by Machine, or `None` for snapshot paths that cannot create endpoints.
    pub capacity: Option<BTreeMap<MachineId, BridgeEndpointCapacity>>,
}

impl DeploySnapshot {
    /// Observer-derived Services owned by `project`. Other Projects are excluded.
    #[must_use]
    pub fn services_in(&self, project: &ProjectName) -> Vec<ServiceObservation> {
        derive_services(
            self.containers
                .iter()
                .filter(|container| container.project_name == *project)
                .cloned(),
        )
    }

    /// Completeness relative to this Machine's current visible required fan-out.
    ///
    /// Required targets are Machines whose Membership Observation invites RPC.
    /// A partition may hide a Machine entirely; that absence is not detected here.
    #[must_use]
    pub fn is_observer_complete(&self) -> bool {
        let required = self
            .machines
            .iter()
            .filter(|machine| machine.membership.invites_rpc())
            .map(|machine| machine.machine.id)
            .collect::<BTreeSet<_>>();
        !affects_required(
            &self.container_failures,
            &self.container_omissions,
            &required,
        ) && !self.volume_snapshot.affects_required(&required)
    }
}

fn affects_required(
    failures: &[MachineFailure<RpcError>],
    omissions: &[MachineId],
    required: &BTreeSet<MachineId>,
) -> bool {
    failures
        .iter()
        .any(|failure| required.contains(&failure.machine_id))
        || omissions.iter().any(|machine| required.contains(machine))
}

/// Why [`PlanError::NoEligibleMachines`] found zero Machines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EliminatingConstraint {
    NoMachines,
    UnknownPlacement {
        targets: Vec<MachineTarget>,
    },
    MachineDown {
        names: Vec<MachineName>,
    },
    VolumeAlreadyOn {
        volume: DockerVolumeName,
        located_on: Vec<MachineName>,
    },
    VolumeConflictsWithPlacement {
        volume: DockerVolumeName,
        located_on: Vec<MachineName>,
        requested: Vec<MachineTarget>,
    },
    SharedVolumeNoCommonMachine {
        volume: DockerVolumeName,
        requested: Vec<MachineTarget>,
    },
}

/// Display list of [`EliminatingConstraint`] values for [`PlanError::NoEligibleMachines`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EliminatingConstraints(Vec<EliminatingConstraint>);

impl EliminatingConstraints {
    /// Build from the constraints that emptied the remaining Machine set.
    #[must_use]
    pub fn new(constraints: Vec<EliminatingConstraint>) -> Self {
        Self(if constraints.is_empty() {
            vec![EliminatingConstraint::NoMachines]
        } else {
            constraints
        })
    }

    /// Constraints in display order.
    #[must_use]
    pub fn as_slice(&self) -> &[EliminatingConstraint] {
        &self.0
    }
}

fn write_quoted<T: fmt::Display>(f: &mut fmt::Formatter<'_>, items: &[T]) -> fmt::Result {
    let mut first = true;
    for item in items {
        if !first {
            f.write_str(", ")?;
        }
        first = false;
        write!(f, "'{item}'")?;
    }
    Ok(())
}

fn write_machine_names(f: &mut fmt::Formatter<'_>, names: &[MachineName]) -> fmt::Result {
    match names {
        [name] => write!(f, "Machine '{name}'"),
        _ => {
            f.write_str("Machines ")?;
            write_quoted(f, names)
        }
    }
}

impl fmt::Display for EliminatingConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMachines => f.write_str("no Machines in the Deploy Snapshot"),
            Self::UnknownPlacement { targets } => {
                f.write_str("x-machines ")?;
                write_quoted(f, targets)?;
                f.write_str(" matched no Machine")
            }
            Self::MachineDown { names } => {
                write_machine_names(f, names)?;
                if names.len() == 1 {
                    f.write_str(" is down")
                } else {
                    f.write_str(" are down")
                }
            }
            Self::VolumeAlreadyOn { volume, located_on } => {
                write!(f, "Docker Volume '{volume}' is already on ")?;
                write_machine_names(f, located_on)
            }
            Self::VolumeConflictsWithPlacement {
                volume,
                located_on,
                requested,
            } => {
                write!(f, "Docker Volume '{volume}' is already on ")?;
                write_machine_names(f, located_on)?;
                f.write_str(", which conflicts with x-machines ")?;
                write_quoted(f, requested)
            }
            Self::SharedVolumeNoCommonMachine { volume, requested } => {
                f.write_str("x-machines ")?;
                write_quoted(f, requested)?;
                write!(f, " have no Machine in common for Docker Volume '{volume}'")
            }
        }
    }
}

impl fmt::Display for EliminatingConstraints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for constraint in &self.0 {
            if !first {
                f.write_str("; ")?;
            }
            first = false;
            write!(f, "{constraint}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanError {
    /// At least one relevant Machine did not return a fresh capacity observation.
    #[error("capacity unknown: an eligible Machine did not return fresh bridge telemetry")]
    CapacityUnknown,
    /// Fresh observations confirm the requested operations cannot fit.
    #[error("insufficient capacity on observed eligible Machines")]
    InsufficientCapacity,
    #[error("no machines available that satisfy all constraints: {constraints}")]
    NoEligibleMachines { constraints: EliminatingConstraints },
    #[error("service mode cannot be changed")]
    ServiceModeCannotChange,
    #[error("mounted Service Volumes disagree about Docker Volume {name}")]
    ConflictingDockerVolumeDefinitions { name: DockerVolumeName },
    /// Two target specs declare the same Service Name.
    #[error("duplicate target Service {service}")]
    DuplicateTargetService {
        /// Service Name repeated in the Deploy Intent target.
        service: ServiceName,
    },
    /// The selected Machine has no usable ZFS storage preparation.
    #[error(
        "Machine '{machine}' requires storage preparation before deploying a Provisioned Volume; enroll it with --storage zfs"
    )]
    ProvisionedVolumeStorageRequired {
        /// Explicitly selected stateless Machine.
        machine: MachineName,
    },
    /// No observed automatically eligible Machine has usable ZFS storage preparation.
    #[error(
        "no observed eligible Machine is storage-ready or has a Machine Pool; enroll one with --storage zfs before deploying a Provisioned Volume"
    )]
    ProvisionedVolumeStorageUnavailable,
    /// Storage capability was unavailable for every otherwise eligible Machine.
    #[error(
        "storage could not be checked on {}; retry after those Machines can report storage capability",
        MachineNames(.names)
    )]
    ProvisionedVolumeStorageUnknown {
        /// Placement-matching Machines without storage evidence.
        names: Vec<MachineName>,
    },
    /// An ordinary Docker Volume already owns the requested machine-local name.
    #[error(
        "Plain Docker Volume {name} already exists on Machine '{machine}'; conversion to a Provisioned Volume is outside the Provisioned Volume MVP"
    )]
    ExistingPlainVolume {
        /// Existing machine-local Docker Volume name.
        name: DockerVolumeName,
        /// Machine holding the existing Volume.
        machine: MachineName,
    },
    /// A Ployz-driver Volume exists with a different bound or malformed options.
    #[error(
        "Provisioned Volume {name} on Machine '{machine}' does not have the requested {maximum_bytes}-byte bound and will not be resized or replaced"
    )]
    ExistingProvisionedVolumeMismatch {
        /// Existing machine-local Docker Volume name.
        name: DockerVolumeName,
        /// Machine holding the existing Volume.
        machine: MachineName,
        /// Bound requested by this Deploy Intent.
        maximum_bytes: ProvisionedVolumeMaximumBytes,
    },
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
    #[error("Docker Volume {id:?} is unavailable: {message}")]
    DockerVolumeUnavailable { id: DockerVolumeId, message: String },
    #[error("hostname {hostname} is already published by {owner}")]
    HostnameConflict {
        hostname: IngressHost,
        owner: QualifiedService,
    },
    #[error(transparent)]
    DomainRequired(#[from] DomainRequired),
    #[error(transparent)]
    GeneratedLabel(#[from] IngressLabelTooLong),
}

impl From<ExpandIngressError> for PlanError {
    fn from(error: ExpandIngressError) -> Self {
        match error {
            ExpandIngressError::DomainRequired(error) => Self::DomainRequired(error),
            ExpandIngressError::LabelTooLong(error) => Self::GeneratedLabel(error),
        }
    }
}

struct MachineNames<'names>(&'names [MachineName]);

impl fmt::Display for MachineNames<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_machine_names(f, self.0)
    }
}

fn compose_deploy_intent(
    project: &ComposeProject,
    project_name: ProjectName,
    options: PlanOptions,
) -> DeployIntent {
    DeployIntent::from_named_specs(
        project_name,
        &project.services,
        &project.dependencies,
        options,
    )
    .with_service_profiles(project.service_profiles())
}

/// Plan a Compose project.
///
/// # Errors
///
/// Returns when placement, volumes, service identity, hostname assignment, or
/// the apply-set dependency graph cannot produce a preview.
pub fn plan_compose(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
    project_name: ProjectName,
) -> Result<DeployPreview, PlanError> {
    let intent = compose_deploy_intent(project, project_name, PlanOptions::default());
    preview_deploy(&intent, snapshot, IngressContext::default())
}

impl PlanError {
    pub(crate) fn no_eligible_machines(constraints: Vec<EliminatingConstraint>) -> Self {
        Self::NoEligibleMachines {
            constraints: EliminatingConstraints::new(constraints),
        }
    }
}
