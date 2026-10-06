use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use ployz_core::{
    BridgeEndpointCapacity, ContainerObservation, ContainerRuntimeObservation, DockerVolume,
    DockerVolumeId, DockerVolumeName, IngressHost, MachineFailure, MachineId, MachineName,
    MachineObservation, Namespace, PartialResult, PlacementConstraint,
    ProvisionedVolumeMaximumBytes, QualifiedService, RpcError, RpcErrorCode, ServiceName,
    ServiceObservation, VolumeInventory, VolumeObservationFailure, derive_services,
};
use thiserror::Error;

mod apply;
mod convergence;
mod exec;
pub(crate) mod pipeline;
mod planning;
mod progress;
mod render;
mod report;

pub(crate) use apply::{Outcome, apply_requested};
pub(crate) use convergence::{Converged, converge};
pub use convergence::{DrainStop, MachineRef, Move, MoveFailure, StayReason};
pub use pipeline::DeployError;
pub(crate) use planning::capacity::endpoint_capacity_error;
pub use planning::{
    DeployPlan, VolumeFate, data_loss_from_plan, plan_deploy, plan_namespace_removal,
    preview_deploy,
};
pub use ployz_core::{
    DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployPreview, DeployWarning,
    ExecutionError, FailedOperation, HealthFailure, HookFailure, MachineAction, ObservationKind,
    OperationPhase, OperationRow, OperationStatus, PlanOptions, PruneRefusal,
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
                message: format!("invalid Docker Volume snapshot: {}", crate::ui::row(&error)),
                details: error.details,
                cause: Vec::new(),
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
            cause: Vec::new(),
        };
        let mut ids = BTreeSet::new();
        for volume in &self.observations {
            if !ids.insert(&volume.id) {
                return Err(invalid(format!(
                    "Docker Volume observation {} is repeated",
                    volume.id
                )));
            }
        }
        for failure in &self.named_failures {
            if !ids.insert(&failure.id) {
                return Err(invalid(format!(
                    "Docker Volume evidence {} is repeated or contradictory",
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
                "Docker Volume evidence {id} contradicts its Machine inventory gap"
            )));
        }
        Ok(())
    }

    /// Volumes a Machine could not inspect although it answered.
    #[must_use]
    pub fn named_failures(&self) -> &[VolumeObservationFailure] {
        &self.named_failures
    }

    /// Machines whose Volume listing failed.
    #[must_use]
    pub fn machine_failures(&self) -> &[MachineFailure<RpcError>] {
        &self.machine_failures
    }

    /// Machines that never answered the Volume listing.
    #[must_use]
    pub fn omissions(&self) -> &[MachineId] {
        &self.omissions
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
            .map(|failure| {
                format!(
                    "Docker Volume inventory failed: {}",
                    crate::ui::row(&failure.error)
                )
            })
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
            .map(|failure| (failure.id.clone(), failure.to_string()))
    }

    pub(crate) fn deploy_warnings(&self) -> impl Iterator<Item = DeployWarning> + '_ {
        self.machine_failures
            .iter()
            .map(|failure| DeployWarning::ObservationFailed {
                kind: ObservationKind::Volume,
                machine_id: failure.machine_id,
                message: crate::ui::row(&failure.error),
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
                        message: failure.to_string(),
                    }),
            )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeploySnapshot {
    pub machines: Vec<MachineObservation>,
    /// Fresh machine-local storage commitments and physical capacity, including read failures.
    pub storage_capacity: BTreeMap<MachineId, Result<ployz_core::StorageCapacity, RpcError>>,
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
    /// Observer-derived Services owned by `namespace`. Other Namespaces are excluded.
    #[must_use]
    pub fn services_in(&self, namespace: &Namespace) -> Vec<ServiceObservation> {
        derive_services(
            self.containers
                .iter()
                .filter(|container| container.namespace == *namespace)
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
        targets: Vec<PlacementConstraint>,
    },
    WorkNotAccepted {
        names: Vec<MachineName>,
    },
    MachineDown {
        names: Vec<MachineName>,
    },
    VolumeInventoryUnavailable {
        machine: MachineName,
        message: String,
    },
    VolumeAlreadyOn {
        volume: DockerVolumeName,
        located_on: Vec<MachineName>,
    },
    VolumeConflictsWithPlacement {
        volume: DockerVolumeName,
        located_on: Vec<MachineName>,
        requested: Vec<PlacementConstraint>,
    },
    SharedVolumeNoCommonMachine {
        volume: DockerVolumeName,
        requested: Vec<PlacementConstraint>,
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
                f.write_str("placement constraints ")?;
                write_quoted(f, targets)?;
                f.write_str(" matched no Machine")
            }
            Self::WorkNotAccepted { names } => {
                write_machine_names(f, names)?;
                f.write_str(" do not accept this work")
            }
            Self::MachineDown { names } => {
                write_machine_names(f, names)?;
                if names.len() == 1 {
                    f.write_str(" is down")
                } else {
                    f.write_str(" are down")
                }
            }
            Self::VolumeInventoryUnavailable { machine, message } => {
                write!(f, "Machine '{machine}' was skipped because its {message}")
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
                f.write_str(", which conflicts with placement constraints ")?;
                write_quoted(f, requested)
            }
            Self::SharedVolumeNoCommonMachine { volume, requested } => {
                f.write_str("placement constraints ")?;
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
    /// Complete provisioned storage demand cannot be admitted on this Machine.
    #[error("Storage on Machine {machine} cannot admit this Deploy.")]
    Storage {
        /// Durable identity of the Machine whose budget failed.
        machine_id: MachineId,
        /// Human-facing Machine name from the plan observation.
        machine: MachineName,
        /// Typed capacity failure and recovery details.
        #[source]
        source: ployz_core::StorageCapacityError,
    },
    /// The Service declares overlapping exclusive host publications.
    #[error("Service {service} declares conflicting host socket publications")]
    ConflictingHostPublications { service: ServiceName },
    /// Observed or earlier planned Containers occupy a required host socket.
    #[error("Service {service} cannot fit because of known host socket conflicts")]
    HostPortConflict { service: ServiceName },
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
    /// Every copy of a Managed Volume is a slot or mid-switch; nothing can mount it.
    #[error("{}", ployz_core::no_writer_message(.name, .copies))]
    NoWriter {
        /// The Docker Volume without a writer.
        name: DockerVolumeName,
        /// Each copy and its role, in Machine order.
        copies: Vec<ployz_core::KnownCopy>,
    },
    /// The selected Machine has no usable ZFS storage preparation.
    #[error(
        "Server '{machine}' is Docker only and cannot host Managed volumes; pick a Server with Managed volumes, or keep this data in a Docker volume instead (ployz volume add NAME --docker)"
    )]
    ProvisionedVolumeStorageRequired {
        /// Explicitly selected stateless Machine.
        machine: MachineName,
    },
    /// No observed automatically eligible Machine has usable ZFS storage preparation.
    #[error(
        "No available Server can host Managed volumes. Add a Server with Managed volumes, or keep this data in a Docker volume instead (ployz volume add NAME --docker)"
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
        "Volume {name} on Server '{machine}' already uses Docker storage. Automatic conversion to managed storage is unavailable"
    )]
    ExistingPlainVolume {
        /// Existing machine-local Docker Volume name.
        name: DockerVolumeName,
        /// Machine holding the existing Volume.
        machine: MachineName,
    },
    /// A Ployz-driver Volume exists with a different bound or malformed options.
    #[error(
        "Managed volume {name} on Server '{machine}' has a different size limit; a Volume's size is fixed once deployed, so its data is neither resized nor replaced (requested {maximum_bytes})"
    )]
    ExistingProvisionedVolumeMismatch {
        /// Existing machine-local Docker Volume name.
        name: DockerVolumeName,
        /// Machine holding the existing Volume.
        machine: MachineName,
        /// Bound requested by this Deploy Intent.
        maximum_bytes: ProvisionedVolumeMaximumBytes,
    },
    #[error("Could not plan Service {service}.")]
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
    #[error("Docker Volume {id} is unavailable: {message}")]
    DockerVolumeUnavailable { id: DockerVolumeId, message: String },
    #[error("hostname {hostname} is already published by {owner}")]
    HostnameConflict {
        hostname: IngressHost,
        owner: QualifiedService,
    },
}

struct MachineNames<'names>(&'names [MachineName]);

impl fmt::Display for MachineNames<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_machine_names(f, self.0)
    }
}

impl PlanError {
    /// Readable SDK failure with structured storage details preserved through service context.
    #[must_use]
    pub fn into_rpc_error(self) -> RpcError {
        match self {
            Self::Storage {
                machine_id,
                machine,
                source,
            } => {
                let mut error = source.into_rpc_error();
                error.message = format!("Machine {machine}: {}", error.message);
                let details = error
                    .details
                    .as_object_mut()
                    .expect("storage errors have object details");
                details.insert("machine_id".into(), serde_json::json!(machine_id));
                details.insert("machine".into(), serde_json::json!(machine));
                error
            }
            Self::Service { service, source } => {
                let mut error = source.into_rpc_error();
                error.message = format!("Service {service}: {}", error.message);
                error
            }
            error @ Self::NoWriter { .. } => {
                ployz_core::SwitchError::NoWriter.rpc_error(error.to_string())
            }
            error @ (Self::ConflictingHostPublications { .. }
            | Self::HostPortConflict { .. }
            | Self::CapacityUnknown
            | Self::InsufficientCapacity
            | Self::NoEligibleMachines { .. }
            | Self::ServiceModeCannotChange
            | Self::ConflictingDockerVolumeDefinitions { .. }
            | Self::DuplicateTargetService { .. }
            | Self::ProvisionedVolumeStorageRequired { .. }
            | Self::ProvisionedVolumeStorageUnavailable
            | Self::ProvisionedVolumeStorageUnknown { .. }
            | Self::ExistingPlainVolume { .. }
            | Self::ExistingProvisionedVolumeMismatch { .. }
            | Self::MixedVolumeModes { .. }
            | Self::DependencyCycle { .. }
            | Self::DockerVolumeUnavailable { .. }
            | Self::HostnameConflict { .. }) => RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: error.to_string(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        }
    }

    pub(crate) fn no_eligible_machines(constraints: Vec<EliminatingConstraint>) -> Self {
        Self::NoEligibleMachines {
            constraints: EliminatingConstraints::new(constraints),
        }
    }
}
