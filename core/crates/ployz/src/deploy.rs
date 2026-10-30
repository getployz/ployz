use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use ployz_core::{
    BridgeEndpointCapacity, ContainerObservation, ContainerRuntimeObservation, DockerVolume,
    DockerVolumeId, DockerVolumeName, IngressHost, MachineFailure, MachineId, MachineName,
    MachineObservation, MembershipObservation, Namespace, PartialResult, PlacementConstraint,
    ProvisionedVolumeMaximumBytes, QualifiedService, RpcError, RpcErrorCode, ServiceName,
    ServiceObservation, VolumeInventory, VolumeObservationFailure, derive_services,
};
use thiserror::Error;

mod apply;
pub(crate) mod cleanup;
mod convergence;
mod exec;
pub(crate) use exec::MachineOperations;
pub(crate) mod pipeline;
mod planning;
mod progress;
mod render;
mod report;
mod schedule;

pub(crate) use apply::{ApplyError, Outcome, apply_requested};
pub(crate) use convergence::{Converged, LastIn, Moving, can_move, converge, named};
pub use convergence::{
    DrainStop, MachineRef, Move, MoveFailure, OldCopyEnd, StayReason, Unconfirmed,
};
pub use pipeline::DeployError;
pub(crate) use pipeline::plan_options;
pub(crate) use planning::capacity::endpoint_capacity_error;
pub use planning::{
    DeployPlan, SlotAction, SlotRepair, VolumeFate, data_loss_from_plan, plan_deploy,
    plan_ingress_repairs, plan_namespace_removal, preview_deploy, without_departed,
};
pub use ployz_core::{
    DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployPreview, DeployWarning,
    ExecutionError, FailedOperation, HealthFailure, HookFailure, MachineAction, ObservationKind,
    OperationPhase, OperationRow, OperationStatus, PlanOptions, PruneRefusal,
    ReplacementCompensation, ReplacementOperation, RestoreAttempt, ServiceAttempt, StopAttempt,
};
pub use schedule::{ServiceGroup, Step};

pub(crate) use progress::pending_rows;
pub(crate) use report::{Replica, service_row, starts, updates};

fn is_at_rest(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
        ContainerRuntimeObservation::Created
            | ContainerRuntimeObservation::Exited { .. }
            | ContainerRuntimeObservation::Dead
    )
}

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

    /// Each Machine whose Volumes this observer did not read in full.
    fn gaps(&self) -> impl Iterator<Item = MachineId> + '_ {
        self.machine_failures
            .iter()
            .map(|failure| failure.machine_id)
            .chain(self.omissions.iter().copied())
            .chain(
                self.named_failures
                    .iter()
                    .map(|failure| failure.id.machine_id),
            )
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

    /// Why the inspect of Docker Volume `name` on `machine_id` failed, if it did.
    pub(crate) fn named_gap(
        &self,
        machine_id: MachineId,
        name: &DockerVolumeName,
    ) -> Option<String> {
        self.named_failures
            .iter()
            .find(|failure| failure.id.machine_id == machine_id && &failure.id.name == name)
            .map(|failure| {
                format!(
                    "inspect of Docker Volume {name} failed: {}",
                    crate::ui::row(&failure.error)
                )
            })
    }

    pub(crate) fn deploy_warnings(&self) -> impl Iterator<Item = DeployWarning> + '_ {
        self.machine_failures
            .iter()
            .map(|failure| DeployWarning::ObservationFailed {
                gap: None,
                kind: ObservationKind::Volume,
                machine_id: failure.machine_id,
                machine_name: None,
                message: crate::ui::row(&failure.error),
            })
            .chain(
                self.omissions
                    .iter()
                    .map(|machine_id| DeployWarning::ObservationOmitted {
                        gap: None,
                        kind: ObservationKind::Volume,
                        machine_id: *machine_id,
                    }),
            )
            .chain(
                self.named_failures
                    .iter()
                    .map(|failure| DeployWarning::ObservationFailed {
                        gap: None,
                        kind: ObservationKind::Volume,
                        machine_id: failure.id.machine_id,
                        machine_name: None,
                        message: format!(
                            "Docker Volume {} could not be inspected: {}; resolve the reported Docker or storage error on that Server, then inspect the Volume again",
                            failure.id.name, failure.error
                        ),
                    }),
            )
    }
}

fn newest(containers: Vec<&ContainerObservation>) -> Option<&ContainerObservation> {
    containers.into_iter().max_by_key(|container| {
        (
            container.created_at_unix_nanos,
            container.container_id.as_str(),
        )
    })
}

/// An ingress Server with no running Ingress Proxy.
#[derive(Clone, Copy, Debug)]
pub struct EmptyIngress<'snapshot> {
    pub machine: &'snapshot MachineObservation,
    pub held: HeldIngress<'snapshot>,
}

/// The Ingress Proxy containers an [`EmptyIngress`] Server holds, all at rest or restarting.
#[derive(Clone, Copy, Debug)]
pub enum HeldIngress<'snapshot> {
    /// None, so one is created.
    None,
    /// The newest one Docker can start again, which starts with its own spec.
    Stopped(&'snapshot ContainerObservation),
    /// Only dead ones, the newest given: Docker starts none of them, and one may still
    /// hold the name a created one needs.
    Dead(&'snapshot ContainerObservation),
    /// The newest one crash-looping under Docker's restart policy, which keeps retrying
    /// it, with none running beside it.
    Restarting(&'snapshot ContainerObservation),
}

/// A Machine that left a Container listing unanswered is an observation gap a Deploy
/// goes on without when its silence is explained: it is Suspect, the entry couldn't dial
/// it, as while its daemon is stopped (#3396), or the entry's path probes find large
/// packets to it dropped, so no listing's reply comes back (#3510). An Up Machine's
/// silence after a dial that worked stays an error, as does any answer.
pub(crate) fn listing_gap(machine: &MachineObservation, error: &RpcError) -> bool {
    error.target_unanswered()
        && (machine.membership == MembershipObservation::Suspect
            || error.undialed()
            || dropped_large_packets(machine, error))
}

/// Whether `machine` left a call unanswered with `error` because the entry's path probes
/// find large packets to it dropped. A Machine the router couldn't dial got only small
/// packets, so its silence has another cause.
pub(crate) fn dropped_large_packets(machine: &MachineObservation, error: &RpcError) -> bool {
    error.target_unanswered()
        && !error.undialed()
        && machine
            .path
            .is_some_and(|path| path.large_packets == ployz_core::LargePackets::Dropped)
}

/// `error`, `machine`'s unanswered observation, as a notice words it: with
/// [`large_packets_note`]. Planning and execution word the same failure alike, so its
/// notice shows once.
pub(crate) fn unanswered_message(machine: &MachineObservation, error: &RpcError) -> String {
    let message = crate::ui::row(error);
    match large_packets_note(machine, error) {
        Some(note) => format!("{message}{note}"),
        None => message,
    }
}

/// Why `machine` left an observation unanswered with `error`, when the entry's path
/// probes find large packets to it dropped: a reply larger than that path carries
/// never arrives.
pub(crate) fn large_packets_note(machine: &MachineObservation, error: &RpcError) -> Option<String> {
    dropped_large_packets(machine, error).then(|| dropped_note(&machine.machine.name))
}

/// [`large_packets_note`]'s words for `machine`.
pub(crate) fn dropped_note(machine: impl std::fmt::Display) -> String {
    format!(
        "; large packets to {machine} are dropped on its mesh path from the Server this command \
         entered by"
    )
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
    /// Fresh telemetry by Machine, or `None` for snapshot paths that cannot create endpoints.
    pub capacity: Option<BTreeMap<MachineId, BridgeEndpointCapacity>>,
}

impl DeploySnapshot {
    /// The ingress Servers whose Ingress Proxy a Deploy brings back: eligible for the
    /// Ingress Proxy's placement, observed in full, and holding none running.
    #[must_use]
    pub fn empty_ingress(&self) -> Vec<EmptyIngress<'_>> {
        self.ingress_eligible()
            .filter(|machine| self.unobserved(machine.machine.id).is_none())
            .filter_map(|machine| {
                let held: Vec<_> = self
                    .containers
                    .iter()
                    .filter(|container| {
                        container.machine_id == machine.machine.id
                            && crate::ingress::is_system_ingress(container)
                    })
                    .collect();
                let (restarting, held): (Vec<_>, Vec<_>) =
                    held.into_iter().partition(|container| {
                        container.runtime == ContainerRuntimeObservation::Restarting
                    });
                if !held.iter().all(|container| is_at_rest(&container.runtime)) {
                    return None;
                }
                if let Some(restarting) = newest(restarting) {
                    return Some(EmptyIngress {
                        machine,
                        held: HeldIngress::Restarting(restarting),
                    });
                }
                let (dead, startable): (Vec<_>, Vec<_>) = held
                    .into_iter()
                    .partition(|container| container.runtime == ContainerRuntimeObservation::Dead);
                let held = match (newest(startable), newest(dead)) {
                    (Some(stopped), _) => HeldIngress::Stopped(stopped),
                    (None, Some(dead)) => HeldIngress::Dead(dead),
                    (None, None) => HeldIngress::None,
                };
                Some(EmptyIngress { machine, held })
            })
            .collect()
    }

    /// The ingress Servers whose containers went unobserved, so whether their Ingress
    /// Proxy runs is unknown, each with why. One the entry Machine stopped listing
    /// left the Cluster, and its ingress role with it, so there is nothing to check.
    fn unobserved_ingress(&self) -> Vec<(&MachineObservation, String)> {
        self.ingress_eligible()
            .filter(|machine| !self.unlisted(machine.machine.id))
            .filter_map(|machine| Some((machine, self.unobserved(machine.machine.id)?)))
            .collect()
    }

    /// Whether the entry Machine no longer listed `machine` when its Containers were
    /// asked for: it left the Cluster after this snapshot's membership read, as after
    /// `ployz server rm`.
    fn unlisted(&self, machine: MachineId) -> bool {
        self.container_failures
            .iter()
            .any(|failure| failure.machine_id == machine && failure.error.server_unlisted(&machine))
    }

    /// Whether `machine` left this snapshot's Container listing unanswered as a
    /// [`listing_gap`].
    pub(crate) fn silent(&self, machine: &MachineObservation) -> bool {
        self.container_failures.iter().any(|failure| {
            failure.machine_id == machine.machine.id && listing_gap(machine, &failure.error)
        })
    }

    /// Why `machine`'s container listing is missing from this snapshot, if it is.
    fn unobserved(&self, machine: MachineId) -> Option<String> {
        self.container_failures
            .iter()
            .find(|failure| failure.machine_id == machine)
            .map(|failure| failure.error.to_string())
            .or_else(|| {
                self.container_omissions
                    .contains(&machine)
                    .then(|| "no answer".to_owned())
            })
    }

    /// The Servers an Ingress Proxy belongs on. One whose daemon says it is leaving
    /// is on its way out with its ingress role, so it gets no proxy and owes no check.
    fn ingress_eligible(&self) -> impl Iterator<Item = &MachineObservation> {
        let placement = self
            .system_ingress()
            .as_ref()
            .and_then(ServiceObservation::observed_global_slot_spec)
            .map(|spec| spec.placement.clone())
            .unwrap_or_default();
        self.machines.iter().filter(move |machine| {
            machine.invites_rpc()
                && machine.machine.accepts_ingress
                && !machine.machine.runtime.leaving
                && ployz_core::machine_matches_placement(&machine.machine, &placement)
        })
    }

    fn system_ingress(&self) -> Option<ServiceObservation> {
        self.services_in(&Namespace::system())
            .into_iter()
            .find(|service| service.identity == QualifiedService::system_ingress())
    }

    /// One entry per Server in [`Self::empty_ingress`] a Deploy brings back: its public
    /// address, or `None` when it has none recorded. They cannot answer port 80 until
    /// the Deploy ends.
    #[must_use]
    pub fn ingress_returning(&self) -> Vec<Option<std::net::IpAddr>> {
        self.empty_ingress()
            .into_iter()
            .filter(|empty| {
                !matches!(
                    empty.held,
                    HeldIngress::Dead(_) | HeldIngress::Restarting(_)
                )
            })
            .map(|empty| empty.machine.machine.public_ip)
            .collect()
    }

    /// This snapshot as seen from `machine` alone, so planning there cannot fail on
    /// another Machine.
    fn only(&self, machine: &MachineObservation) -> Self {
        Self {
            machines: vec![machine.clone()],
            storage_capacity: self.storage_capacity.clone(),
            containers: self
                .containers
                .iter()
                .filter(|container| container.machine_id == machine.machine.id)
                .cloned()
                .collect(),
            volume_snapshot: self.volume_snapshot.clone(),
            container_failures: Vec::new(),
            container_omissions: Vec::new(),
            capacity: self.capacity.clone(),
        }
    }

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
        self.unobserved_required().is_empty()
    }

    /// The required Machines whose Containers or Volumes this observer did not read in full.
    #[must_use]
    pub fn unobserved_required(&self) -> Vec<&MachineObservation> {
        let gaps = self
            .container_failures
            .iter()
            .map(|failure| failure.machine_id)
            .chain(self.container_omissions.iter().copied())
            .chain(self.volume_snapshot.gaps())
            .collect::<BTreeSet<_>>();
        self.machines
            .iter()
            .filter(|machine| machine.invites_rpc() && gaps.contains(&machine.machine.id))
            .collect()
    }
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
    /// Machines that did not answer this Deploy's Container listing.
    Unanswered {
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
            Self::Unanswered { names } => {
                write_machine_names(f, names)?;
                f.write_str(" did not answer when this Deploy listed its containers")
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
    #[error("{}", ployz_core::no_writer_message(.name, .copies, *.operator))]
    NoWriter {
        /// The Volume without a writer, as its owner named it.
        name: String,
        /// Each copy and its role, in Machine order.
        copies: Vec<ployz_core::KnownCopy>,
        /// Who runs the Deploy, which decides the way back the message names.
        operator: ployz_core::Operator,
    },
    /// The Volume's writer is on a Machine that accepts no services, as after a drain.
    #[error(
        "Volume {volume} is on {machine}, which doesn't run services. Turn them back on with ployz server set {machine} --accepts-services=true"
    )]
    VolumeServerTakesNoServices {
        /// The Volume, as its owner named it.
        volume: String,
        /// The Machine holding the Volume's writer.
        machine: MachineName,
    },
    /// No observed copy is a writer, and a silent Machine may hold it.
    #[error(
        "{machine} did not answer, so Volume {name}'s copies are unknown. Bring {machine} back, or deploy the other services by name"
    )]
    CopiesUnknown {
        /// The Volume, as its owner named it.
        name: String,
        /// The Machine whose storage was not read.
        machine: MachineName,
    },
    /// A Docker Volume this Deploy would create, which a Machine that did not answer
    /// its listing may already hold.
    #[error(
        "{machine} did not answer, so whether it holds Docker volume {name} is unknown. Bring {machine} back, or deploy the other services by name"
    )]
    VolumeHolderUnknown {
        /// The Volume, as its owner named it.
        name: String,
        machine: MachineName,
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
        name: DockerVolumeName,
        /// Machine holding the existing Volume.
        machine: MachineName,
    },
    /// A Ployz-driver Volume exists with a bound below the request or different labels.
    #[error(
        "Managed volume {name} on Server '{machine}' is smaller than the requested {maximum_bytes} or has different labels; a Deploy neither grows nor replaces a Volume's data"
    )]
    ExistingProvisionedVolumeMismatch {
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
    #[error("{}", .refusal.message(.hostname, .owner))]
    HostnameConflict {
        hostname: IngressHost,
        owner: QualifiedService,
        refusal: HostnameRefusal,
    },
    /// A Deploy would both run and retire the same Service.
    #[error("Service {service} can't be deployed and removed in one Deploy")]
    RemovalInTarget { service: ServiceName },
    /// A Deploy named a Service to retire without selecting it.
    #[error("Service {service} is removed but not selected")]
    RemovalNotSelected { service: ServiceName },
    /// Services of a reserved Namespace are never retired by name.
    #[error("Services in Namespace {namespace} can't be removed by a Deploy")]
    RemovalInReservedNamespace { namespace: Namespace },
}

/// Why a hostname another Service publishes can't be claimed in this Deploy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostnameRefusal {
    /// The owner keeps the hostname, or this Deploy leaves the owner alone.
    StillPublished,
    /// The owner's staged spec drops the hostname, but this Deploy doesn't include the owner.
    OwnerNotIncluded,
    /// `to` takes the hostname but already waits for the owner, `because`, so the owner
    /// can't wait for `to` and the hostname would answer nowhere for `to`'s whole rollout.
    GainerWaitsForOwner { to: ServiceName, because: String },
    /// `to`, another Service this Deploy requests, claims the hostname too.
    RequestedTwice { to: ServiceName },
}

impl HostnameRefusal {
    /// Decided from this Deploy's intent alone, so the same Deploy is refused the same
    /// way until something staged changes. The others rest on an observed publisher,
    /// which another Deploy can make drop the hostname, so those keep their retry.
    fn same_again(&self) -> bool {
        match self {
            Self::StillPublished | Self::OwnerNotIncluded | Self::GainerWaitsForOwner { .. } => {
                false
            }
            Self::RequestedTwice { .. } => true,
        }
    }

    fn message(&self, hostname: &IngressHost, owner: &QualifiedService) -> String {
        match self {
            Self::StillPublished => format!(
                "hostname {hostname} is already published by {owner}: remove it from that Service first, or use another hostname"
            ),
            Self::OwnerNotIncluded => format!(
                "hostname {hostname} is published by {owner} until {dropper} deploys: deploy {dropper} too",
                dropper = owner.name
            ),
            Self::GainerWaitsForOwner { to, because } => format!(
                "hostname {hostname} is published by {owner} and can't move to {to} in the same Deploy: {because}; remove it from {dropper} and deploy, then add it to {to}",
                dropper = owner.name
            ),
            Self::RequestedTwice { to } => format!(
                "hostname {hostname} is requested by both {owner} and {to}: keep it on one of them"
            ),
        }
    }
}

struct MachineNames<'names>(&'names [MachineName]);

impl fmt::Display for MachineNames<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_machine_names(f, self.0)
    }
}

impl PlanError {
    /// The Server whose storage can't hold the Deploy's Volumes, as opposed to a
    /// conflict or capacity nobody could read. Looks through the Service a plan error
    /// may name.
    #[must_use]
    pub fn short_of_storage(&self) -> Option<MachineId> {
        if let Self::Service { source, .. } = self {
            return source.short_of_storage();
        }
        let Self::Storage {
            machine_id, source, ..
        } = self
        else {
            return None;
        };
        matches!(
            source,
            ployz_core::StorageCapacityError::InsufficientStorage { .. }
                | ployz_core::StorageCapacityError::PoolCannotGrow { .. }
        )
        .then_some(*machine_id)
    }

    /// Deploying the same thing again is refused this way again: the refusal was
    /// decided from this Deploy's intent alone, so it holds until something staged
    /// changes. A refusal the snapshot, placement or a Server's labels take part in
    /// stays false, since another Deploy landing or a label changing can clear it, as
    /// does every refusal not listed here: the retry it keeps is the safe default.
    #[must_use]
    pub fn same_again(&self) -> bool {
        match self {
            Self::ConflictingHostPublications { .. }
            | Self::DuplicateTargetService { .. }
            | Self::MixedVolumeModes { .. }
            | Self::DependencyCycle { .. }
            | Self::RemovalInTarget { .. }
            | Self::RemovalNotSelected { .. }
            | Self::RemovalInReservedNamespace { .. } => true,
            Self::HostnameConflict { refusal, .. } => refusal.same_again(),
            Self::Service { source, .. } => source.same_again(),
            // Two Services may disagree about one Docker Volume only on a Server both
            // are placed on, which its labels decide.
            Self::ConflictingDockerVolumeDefinitions { .. }
            | Self::Storage { .. }
            | Self::HostPortConflict { .. }
            | Self::CapacityUnknown
            | Self::InsufficientCapacity
            | Self::NoEligibleMachines { .. }
            | Self::ServiceModeCannotChange
            | Self::NoWriter { .. }
            | Self::VolumeServerTakesNoServices { .. }
            | Self::CopiesUnknown { .. }
            | Self::VolumeHolderUnknown { .. }
            | Self::ProvisionedVolumeStorageRequired { .. }
            | Self::ProvisionedVolumeStorageUnavailable
            | Self::ProvisionedVolumeStorageUnknown { .. }
            | Self::ExistingPlainVolume { .. }
            | Self::ExistingProvisionedVolumeMismatch { .. } => false,
        }
    }

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
            error @ Self::VolumeServerTakesNoServices { .. } => RpcError {
                code: RpcErrorCode::Conflict,
                message: error.to_string(),
                details: serde_json::json!({ "reason": "volume_server_takes_no_services" }),
                cause: Vec::new(),
            },
            error @ (Self::CopiesUnknown { .. } | Self::VolumeHolderUnknown { .. }) => RpcError {
                code: RpcErrorCode::Unavailable,
                message: error.to_string(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
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
            | Self::HostnameConflict { .. }
            | Self::RemovalInTarget { .. }
            | Self::RemovalNotSelected { .. }
            | Self::RemovalInReservedNamespace { .. }) => RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: error.to_string(),
                details: if error.same_again() {
                    serde_json::json!({"same_again": true})
                } else {
                    serde_json::Value::Null
                },
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

pub(super) fn observation_warnings(
    kind: ObservationKind,
    failures: &[MachineFailure<RpcError>],
    omissions: &[MachineId],
) -> Vec<DeployWarning> {
    failures
        .iter()
        .map(|failure| DeployWarning::ObservationFailed {
            gap: None,
            kind,
            machine_id: failure.machine_id,
            machine_name: None,
            message: crate::ui::row(&failure.error),
        })
        .chain(
            omissions
                .iter()
                .map(|machine| DeployWarning::ObservationOmitted {
                    gap: None,
                    kind,
                    machine_id: *machine,
                }),
        )
        .collect()
}
