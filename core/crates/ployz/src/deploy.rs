use ployz_core::error_chain;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use ployz_core::{
    BridgeEndpointCapacity, ContainerObservation, ContainerRuntimeObservation, DockerVolume,
    DockerVolumeId, DockerVolumeName, IngressHost, MachineFailure, MachineId, MachineName,
    MachineObservation, Namespace, PartialResult, PlacementConstraint,
    ProvisionedVolumeMaximumBytes, QualifiedService, Region, RpcError, RpcErrorCode, ServiceName,
    ServiceObservation, VolumeInventory, VolumeObservationFailure, derive_services,
};
use thiserror::Error;

use crate::coded::Coded;

mod approvals;
pub(crate) mod cleanup;
mod exec;
mod machine_calls;
pub(crate) use approvals::{Approvals, KeptVersions, RuleProofs, kept_query};
pub(crate) use exec::{MoveEnd, MoveError, health_error, move_container};
pub(crate) use machine_calls::{
    Answer, Approved, CopyLifecycle, MachineOperations, ProxyPlacement, RestartTolerant, set_each,
};
#[cfg(test)]
pub(crate) use machine_calls::{RedialFailure, Session};
pub(crate) mod pipeline;
mod planning;
mod progress;
mod schedule;

pub use pipeline::DeployError;
pub(crate) use planning::capacity::endpoint_capacity_error;
pub use planning::{
    DeployPlan, SlotAction, SlotRepair, VolumeFate, data_loss_from_plan, plan_deploy,
    plan_ingress_repairs, plan_namespace_removal, preview_deploy, without_departed,
};
pub(crate) use planning::{destinations, place_one, region_waits};
pub use ployz_core::{
    DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployPreview, DeployWarning,
    ExecutionError, FailedOperation, HealthFailure, HookFailure, MachineAction, ObservationKind,
    OperationPhase, OperationRow, OperationStatus, PlanOptions, PruneRefusal,
    ReplacementCompensation, ReplacementOperation, RestoreAttempt, ServiceAttempt, StopAttempt,
};
pub use schedule::{GroupRole, ServiceGroup, Stage};

pub(crate) use progress::pending_rows;
#[cfg(test)]
pub(crate) use schedule::depths;

/// The Store's decision on a requested traffic switch.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SwitchDecision {
    Committed,
    Refused,
}

pub(crate) fn is_active_runtime(runtime: &ContainerRuntimeObservation) -> bool {
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
            |error| {
                RpcError::internal(format!(
                    "invalid Docker Volume snapshot: {}",
                    error_chain::row(&error)
                ))
                .with_details(error.details)
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
        let invalid = |message| RpcError::invalid_argument(message);
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
                    error_chain::row(&failure.error)
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
                    error_chain::row(&failure.error)
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
                message: error_chain::row(&failure.error),
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
    /// Only dead ones, the newest given: Docker starts none of them, so the daemon
    /// retires them and creates its own.
    Dead(&'snapshot ContainerObservation),
    /// The newest one crash-looping under Docker's restart policy, which keeps retrying
    /// it, with none running beside it.
    Restarting(&'snapshot ContainerObservation),
}

/// A Machine that left a Container listing unanswered within the read's deadline
/// ([`crate::connect::target_read_timeout`]) is an observation gap a Deploy goes on
/// without, whatever its membership reads: Suspect, undialed while its daemon is stopped
/// (#3396), behind a path dropping large packets (#3510), or Up with a daemon that took
/// the call and never replied, as a frozen one does while its Corrosion keeps it Up. A
/// Machine that answers within the deadline, even with an error, is no gap.
pub(crate) fn listing_gap(error: &RpcError) -> bool {
    error.target_unanswered()
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
    let message = error_chain::row(error);
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

/// A Machine the entry's WireGuard sees down: no handshake for long enough that nothing
/// the entry routes there arrives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeadLink {
    pub(crate) machine: MachineId,
    name: MachineName,
    /// The Server this command entered by, when it said which it is.
    entry: Option<MachineName>,
    /// Since the latest handshake; `None` when there never was one.
    since_handshake: Option<std::time::Duration>,
}

impl DeadLink {
    /// The link and what fixes it, as sentences, for a command that can run through
    /// another Server.
    pub(crate) fn explained(&self) -> String {
        format!("{}. {}", self.sentence("The"), self.remedy())
    }

    /// The link as a clause, opening with `the` when the entry didn't say which it is.
    fn sentence(&self, the: &str) -> String {
        let entry = match &self.entry {
            Some(entry) => format!("{entry}, the Server this command entered by,"),
            None => format!("{the} Server this command entered by"),
        };
        match self.since_handshake {
            Some(since) => format!(
                "{entry} has had no WireGuard handshake with {} for {}",
                self.name,
                span(since)
            ),
            None => format!(
                "{entry} has never had a WireGuard handshake with {}",
                self.name
            ),
        }
    }

    /// What fixes it, as a sentence, for a command that can run through another Server.
    pub(crate) fn remedy(&self) -> String {
        format!(
            "Allow {}, or run this through another Server with --connect.",
            open_wireguard()
        )
    }
}

/// The Machines `entry`, the Server this command entered by, sees down in its listing.
pub(crate) fn dead_links(
    machines: &[MachineObservation],
    entry: Option<MachineId>,
) -> Vec<DeadLink> {
    let entry = entry.and_then(|entry| {
        machines
            .iter()
            .find(|observed| observed.machine.id == entry)
            .map(|observed| observed.machine.name.clone())
    });
    machines
        .iter()
        .filter_map(|observed| match observed.wireguard? {
            ployz_core::WireGuardObservation::Down {
                since_handshake_secs,
            } => Some(DeadLink {
                machine: observed.machine.id,
                name: observed.machine.name.clone(),
                entry: entry.clone(),
                since_handshake: since_handshake_secs.map(std::time::Duration::from_secs),
            }),
            ployz_core::WireGuardObservation::Up
            | ployz_core::WireGuardObservation::Unrecognized => None,
        })
        .collect()
}

/// The Machine `selector` names, when the entry's listing sees it down.
pub(crate) async fn dead_link_to(
    client: &mut crate::connect::Client,
    selector: &ployz_core::MachineTarget,
) -> Option<DeadLink> {
    let machines = tokio::time::timeout(DIAGNOSIS_WAIT, client.machines())
        .await
        .ok()?
        .ok()?;
    let ployz_core::NameMatches::One(target) =
        selector.resolve(machines.iter().map(|observed| &observed.machine))
    else {
        return None;
    };
    let target = target.id;
    dead_links(&machines, client.entry_machine())
        .into_iter()
        .find(|link| link.machine == target)
}

/// How long a command that already failed waits for the entry's listing that explains
/// why, before it reports the failure without it.
pub(crate) const DIAGNOSIS_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// What a firewall must let through so two Servers handshake.
pub(crate) fn open_wireguard() -> String {
    format!(
        "UDP {} between them in every firewall, your provider's included",
        ployz_core::WIREGUARD_PORT
    )
}

impl fmt::Display for DeadLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.sentence("the"))
    }
}

/// `since` in its largest whole unit, as `6m`.
pub(crate) fn span(since: std::time::Duration) -> String {
    let seconds = since.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

/// A Machine whose Container listing a [`DeploySnapshot`] lacks, as
/// [`DeploySnapshot::listing_gaps`] reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ListingGap<'snapshot> {
    pub(crate) machine: MachineId,
    pub(crate) kind: ListingGapKind<'snapshot>,
    /// Whether its silence is a [`listing_gap`].
    pub(crate) silent: bool,
    /// Whether it went unanswered over a path the entry finds dropping large packets.
    pub(crate) large_packets: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ListingGapKind<'snapshot> {
    /// The listing failed with this error.
    Failed(&'snapshot RpcError),
    /// The listing produced no terminal response.
    Omitted,
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
    pub(crate) fn machine(&self, id: MachineId) -> Option<&MachineObservation> {
        self.machines
            .iter()
            .find(|machine| machine.machine.id == id)
    }

    pub(crate) fn machine_named(&self, name: &MachineName) -> Option<&MachineObservation> {
        self.machines
            .iter()
            .find(|machine| &machine.machine.name == name)
    }

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
                        container.machine_id == machine.machine.id && container.is_system_ingress()
                    })
                    .collect();
                let (restarting, held): (Vec<_>, Vec<_>) =
                    held.into_iter().partition(|container| {
                        container.runtime == ContainerRuntimeObservation::Restarting
                    });
                if !held.iter().all(|container| container.runtime.at_rest()) {
                    return None;
                }
                if let Some(restarting) = ployz_core::newest(restarting) {
                    return Some(EmptyIngress {
                        machine,
                        held: HeldIngress::Restarting(restarting),
                    });
                }
                let (dead, startable): (Vec<_>, Vec<_>) = held
                    .into_iter()
                    .partition(|container| container.runtime == ContainerRuntimeObservation::Dead);
                let held = match (ployz_core::newest(startable), ployz_core::newest(dead)) {
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

    /// Each Machine whose Container listing this snapshot lacks, failures before
    /// omissions, each read once for every use of it.
    pub(crate) fn listing_gaps(&self) -> impl Iterator<Item = ListingGap<'_>> {
        let failed = self.container_failures.iter().map(|failure| {
            let machine = self.machine(failure.machine_id);
            ListingGap {
                machine: failure.machine_id,
                kind: ListingGapKind::Failed(&failure.error),
                silent: machine.is_some() && listing_gap(&failure.error),
                large_packets: machine
                    .is_some_and(|machine| dropped_large_packets(machine, &failure.error)),
            }
        });
        let omitted = self.container_omissions.iter().map(|machine| ListingGap {
            machine: *machine,
            kind: ListingGapKind::Omitted,
            silent: false,
            large_packets: false,
        });
        failed.chain(omitted)
    }

    /// Whether `machine` left this snapshot's Container listing unanswered as a
    /// [`listing_gap`].
    pub(crate) fn silent(&self, machine: &MachineObservation) -> bool {
        self.listing_gaps()
            .any(|gap| gap.machine == machine.machine.id && gap.silent)
    }

    /// Why `machine`'s container listing is missing from this snapshot, if it is.
    fn unobserved(&self, machine: MachineId) -> Option<String> {
        self.listing_gaps()
            .find(|gap| gap.machine == machine)
            .map(|gap| match gap.kind {
                ListingGapKind::Failed(error) => error.to_string(),
                ListingGapKind::Omitted => "no answer".to_owned(),
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
            machine.can_be_asked()
                && machine.machine.accepts_ingress
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
            .filter(|empty| !matches!(empty.held, HeldIngress::Restarting(_)))
            .map(|empty| empty.machine.machine.public_ip)
            .collect()
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

    #[must_use]
    pub fn as_slice(&self) -> &[EliminatingConstraint] {
        &self.0
    }
}

/// `fsn`, `fsn or syd`, `fsn, lhr or syd`.
pub(crate) fn any_of(regions: &[&Region]) -> String {
    match regions {
        [rest @ .., last] if !rest.is_empty() => {
            let rest = rest
                .iter()
                .map(|region| region.as_str())
                .collect::<Vec<_>>();
            format!("{} or {last}", rest.join(", "))
        }
        regions => regions.iter().map(|region| region.as_str()).collect(),
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

/// Why a Server in a Service's region can't run the Service now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerAway {
    Down,
    /// It did not answer this Deploy's Container listing.
    Silent,
    TakesNoServices,
    /// Its membership is neither up nor down. A refusal doesn't name it.
    Unreached,
}

/// A region none of whose Servers can run a Service now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionAway {
    pub region: Region,
    /// The Servers in the region, each with why it can't run the Service. Empty when
    /// no Server is in the region.
    pub servers: Vec<(MachineName, ServerAway)>,
}

/// How a Service kept away from its Volume's Server can run beside the Volume, which
/// stays where it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BesideVolume {
    /// In the region the Volume's Server is in, with the replicas the Service asks for.
    RunIn {
        region: Region,
        replicas: Option<std::num::NonZeroU32>,
    },
    /// By giving the Volume's Server, which is in no region, the region the Service is
    /// kept in.
    Label,
}

/// Where Services that share a Volume have to run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SharedHome {
    /// The region of the Server that writes the Volume.
    Region(Region),
    /// The Server that writes the Volume, which is in no region.
    Server(MachineName),
    /// The Volume is not made yet, and its Services are kept in different regions.
    Unmade,
}

fn volume_outside_region(
    volume: &str,
    machine: &MachineName,
    region: &Region,
    beside: &BesideVolume,
) -> String {
    match beside {
        BesideVolume::RunIn { region: at, .. } => format!(
            "its Volume {volume} is in {at}. Move it with ployz volume move {volume} --to <a server in {region}>, or run it in {at}"
        ),
        BesideVolume::Label => format!(
            "its Volume {volume} is on {machine}, a Server in no region. Move it with ployz volume move {volume} --to <a server in {region}>, or give {machine} the region {region}"
        ),
    }
}

fn volumes_apart(volumes: &[(String, Region)]) -> String {
    all_of(
        volumes
            .iter()
            .map(|(volume, region)| format!("{volume} is in {region}")),
    )
}

fn shared_across_regions(
    volume: &str,
    service: &ServiceName,
    region: &Region,
    sharers: &[(ServiceName, Vec<Region>)],
    home: &SharedHome,
) -> String {
    let refused = (service.clone(), vec![region.clone()]);
    let mut each = std::iter::once(&refused).chain(sharers).collect::<Vec<_>>();
    // Where every sharer gets the same instruction, each one's refusal reads the same, so a
    // Deployment names it once.
    if !matches!(home, SharedHome::Region(_)) {
        each.sort();
    }
    let names = all_of(each.iter().map(|(name, _)| name.to_string()));
    let why = match home {
        SharedHome::Region(at) => {
            format!("{volume} is in {at} and {service} is set to {region}. Set {service} to {at}")
        }
        SharedHome::Server(machine) => format!(
            "{volume} is on {machine}, which has no region. Give {machine} a region, then set them all to it"
        ),
        SharedHome::Unmade => {
            let set = all_of(each.iter().map(|(name, regions)| {
                format!(
                    "{name} is set to {}",
                    all_of(regions.iter().map(ToString::to_string))
                )
            }));
            format!("{volume} isn't made yet. {set}. Set them all to one region")
        }
    };
    format!("{names} share the Volume {volume}, so they have to run in the same region. {why}")
}

fn all_of(each: impl Iterator<Item = String>) -> String {
    match each.collect::<Vec<_>>().as_slice() {
        [rest @ .., last] if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        each => each.concat(),
    }
}

fn region_unmatched(regions: &[RegionAway]) -> String {
    let (empty, away): (Vec<_>, Vec<_>) =
        regions.iter().partition(|region| region.servers.is_empty());
    let mut clauses = Vec::new();
    if !empty.is_empty() {
        let empty = empty.iter().map(|away| &away.region).collect::<Vec<_>>();
        clauses.push(format!("no Server in {}", any_of(&empty)));
    }
    clauses.extend(
        away.iter()
            .map(|away| servers_away(&away.region, &away.servers)),
    );
    clauses.join("; ")
}

/// Each Server of `region` a refusal names, with why it can't run the Service. The
/// Servers it doesn't name are covered by one closing clause.
fn servers_away(region: &Region, servers: &[(MachineName, ServerAway)]) -> String {
    let mut clauses = servers
        .iter()
        .filter_map(|(name, away)| {
            let why = match away {
                ServerAway::Down => "is down",
                ServerAway::Silent => "did not answer",
                ServerAway::TakesNoServices => "takes no services",
                ServerAway::Unreached => return None,
            };
            Some(format!("{name} {why}"))
        })
        .collect::<Vec<_>>();
    if clauses.is_empty() {
        return format!("no Server in {region} is up");
    }
    if clauses.len() < servers.len() {
        clauses.push(format!("and no other Server in {region} is up"));
    }
    clauses.join(", ")
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanError {
    #[error("Storage on Machine {machine} cannot admit this Deploy.")]
    Storage {
        machine_id: MachineId,
        machine: MachineName,
        /// Typed capacity failure and recovery details.
        #[source]
        source: ployz_core::StorageCapacityError,
    },
    #[error("Service {service} declares conflicting host socket publications")]
    ConflictingHostPublications { service: ServiceName },
    #[error("Service {service} cannot fit because of known host socket conflicts")]
    HostPortConflict { service: ServiceName },
    #[error("capacity unknown: an eligible Machine did not return fresh bridge telemetry")]
    CapacityUnknown,
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
    #[error("{}", crate::volume_recovery::no_writer_message(.name, .copies, *.operator))]
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
    /// A Managed Volume no Server holds yet was placed on a Server that can't take it now.
    #[error(
        "Volume {volume} was placed on {machine}, which {}. {}",
        unfit.why(),
        unfit.remedy(volume, machine)
    )]
    FirstCopyUnfit {
        /// The Volume, as its owner named it.
        volume: String,
        /// The Server an earlier Deployment chose for its first copy, by name, or by ID
        /// when this Server's view of the Cluster doesn't list it.
        machine: String,
        /// Why that Server can't take it now.
        unfit: Unfit,
    },
    /// The Service mounts a born Volume, and every Server it could run on is too old to
    /// check where a Volume's data began.
    #[error(
        "Upgrade {} first. {service} mounts a volume that needs a newer Ployz on every server it runs on.",
        all_of(names.iter().map(ToString::to_string))
    )]
    VolumeOriginsUnchecked {
        service: ServiceName,
        /// The Servers left out for it, in snapshot order. Never empty.
        names: Vec<MachineName>,
    },
    /// The Service's new Containers carry a Free receipt, and every Server it could run on
    /// is too old to take one.
    #[error(
        "Upgrade {} first. {service} builds without approval, which needs a newer Ployz on the server it runs on.",
        all_of(names.iter().map(ToString::to_string))
    )]
    FreeReceiptsUntaken {
        service: ServiceName,
        /// The Servers left out for it, in snapshot order. Never empty.
        names: Vec<MachineName>,
    },
    /// No Server in a region the Service runs in can run it.
    #[error("{}", region_unmatched(regions))]
    RegionUnmatched {
        /// Every such region. Never empty.
        regions: Vec<RegionAway>,
    },
    /// The Service sets replicas in several regions and mounts a Volume, which is on
    /// one Server.
    #[error("{service} has a Volume, so its replicas run in one region")]
    VolumeInSeveralRegions { service: ServiceName },
    /// The Service is kept in a region, and its Volume is on a Server outside it.
    #[error("{}", volume_outside_region(volume, machine, region, beside))]
    VolumeOutsideRegion {
        /// The Volume, as its owner named it.
        volume: String,
        machine: MachineName,
        /// The region the Service is kept in.
        region: Region,
        beside: BesideVolume,
    },
    /// The Service mounts Volumes that no one region holds, one of them shared with a
    /// Service kept elsewhere, so it can run in none.
    #[error(
        "{}, and {service} runs in one region. Run the Services that share them in one region, or split {service}",
        volumes_apart(volumes)
    )]
    VolumesApart {
        service: ServiceName,
        /// Each located Volume it mounts, as its owner named it, with its region, by
        /// name. Two or more, in more than one region.
        volumes: Vec<(String, Region)>,
    },
    /// The Service is kept in a region, and another Service that mounts the Volume is kept
    /// away from it. A Volume is on one Server, so both can't run beside it.
    #[error("{}", shared_across_regions(volume, service, region, sharers, home))]
    VolumeSharedAcrossRegions {
        /// The Volume, as its owner named it.
        volume: String,
        service: ServiceName,
        /// The region the Service is kept in.
        region: Region,
        /// Each other Service that mounts it and is kept in a region, with its regions, by
        /// name. Never empty.
        sharers: Vec<(ServiceName, Vec<Region>)>,
        /// Where they all have to run.
        home: Box<SharedHome>,
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
    /// `to` takes up a Volume `from` drops, but `from` already waits for `to`, `because`,
    /// so neither could start once the other's old copy is gone.
    #[error(
        "Volume {volume} can't move from {from} to {to} in the same Deploy: {because}; remove it from {from} and deploy, then add it to {to}"
    )]
    VolumeHandoverCycle {
        /// The Volume, as its owner named it.
        volume: String,
        from: ServiceName,
        to: ServiceName,
        because: String,
    },
    #[error("{}", .refusal.message(.hostname, .owner))]
    HostnameConflict {
        hostname: IngressHost,
        owner: QualifiedService,
        refusal: HostnameRefusal,
    },
    #[error("Service {service} can't be deployed and removed in one Deploy")]
    RemovalInTarget { service: ServiceName },
    #[error("Service {service} is removed but not selected")]
    RemovalNotSelected { service: ServiceName },
    #[error("Services in Namespace {namespace} can't be removed by a Deploy")]
    RemovalInReservedNamespace { namespace: Namespace },
}

impl Coded for PlanError {
    fn code(&self) -> RpcErrorCode {
        self.refusal().cli
    }
}

/// How each surface codes a plan refusal, and whether deploying the same thing again
/// is refused the same way. The surfaces disagree on some refusals; each code is what
/// that surface has always sent, so each is kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Refusal {
    /// The CLI's `--json` error.
    pub(crate) cli: RpcErrorCode,
    /// A Machine RPC reply, and a blocked Service's error.
    pub(crate) rpc: RpcErrorCode,
    /// The SDK's Deploy preparation failure.
    pub(crate) preparation: RpcErrorCode,
    pub(crate) same_again: bool,
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

/// Why the Server a Volume's first copy was placed on can't take it now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unfit {
    /// The Cluster as this plan sees it doesn't list the Server.
    Unlisted,
    /// It can't run the Service.
    Excluded,
    /// It has no room for the Volume.
    Full,
}

impl Unfit {
    fn why(self) -> &'static str {
        match self {
            Self::Unlisted => "the server running this deploy doesn't list",
            Self::Excluded => "can't run this service",
            Self::Full => "has no room for it",
        }
    }

    /// What to do. A Server this plan doesn't list may already hold the Volume and serve
    /// writes from it, so its advice never calls the Volume empty.
    fn remedy(self, volume: &str, machine: &str) -> String {
        let reset = format!("remove {volume} and add it again. It holds no data yet.");
        match self {
            Self::Unlisted => format!(
                "Bring {machine} back. If it's gone for good, check it held no data in {volume} before you remove {volume} and add it again."
            ),
            Self::Excluded => format!("Let {machine} run it, or {reset}"),
            Self::Full => format!("Free space on {machine}, or {reset}"),
        }
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

    /// Whether a Service this stops waits or is refused. It is refused when no later
    /// Deploy of the same config starts it: it shares a Volume with a Service kept in
    /// another region. Every other reason here waits, since a Server coming back or a
    /// Volume on a Server in the region starts it.
    #[must_use]
    pub fn blocked_kind(&self) -> ployz_core::BlockedKind {
        if matches!(
            self,
            Self::VolumesApart { .. } | Self::VolumeSharedAcrossRegions { .. }
        ) {
            ployz_core::BlockedKind::Refusal
        } else {
            ployz_core::BlockedKind::Wait
        }
    }

    /// Deploying the same thing again is refused this way again: the refusal was
    /// decided from this Deploy's intent alone, so it holds until something staged
    /// changes. A refusal the snapshot, placement or a Server's labels take part in
    /// stays false, since another Deploy landing or a label changing can clear it.
    #[must_use]
    pub fn same_again(&self) -> bool {
        self.refusal().same_again
    }

    /// This refusal's row: its code on each surface and whether it holds again.
    pub(crate) fn refusal(&self) -> Refusal {
        use RpcErrorCode::{Conflict, Internal, InvalidArgument as Invalid, Unavailable};
        use ployz_core::CodedRefusal as _;
        let row = |cli, rpc, preparation, same_again| Refusal {
            cli,
            rpc,
            preparation,
            same_again,
        };
        match self {
            Self::Service { source, .. } => source.refusal(),
            Self::ConflictingHostPublications { .. }
            | Self::VolumeInSeveralRegions { .. }
            | Self::DuplicateTargetService { .. }
            | Self::MixedVolumeModes { .. }
            | Self::DependencyCycle { .. }
            | Self::RemovalInTarget { .. }
            | Self::RemovalNotSelected { .. }
            | Self::RemovalInReservedNamespace { .. } => row(Invalid, Invalid, Invalid, true),
            // Two Services may disagree about one Docker Volume only on a Server both
            // are placed on, which its labels decide.
            Self::ConflictingDockerVolumeDefinitions { .. } => {
                row(Invalid, Invalid, Invalid, false)
            }
            Self::HostnameConflict { refusal, .. } => {
                row(Conflict, Invalid, Invalid, refusal.same_again())
            }
            // Rests on the Service holding the Volume today, which another Deploy can change.
            Self::VolumeHandoverCycle { .. } => row(Conflict, Invalid, Invalid, false),
            Self::HostPortConflict { .. }
            | Self::InsufficientCapacity
            | Self::NoEligibleMachines { .. }
            | Self::ServiceModeCannotChange
            | Self::ProvisionedVolumeStorageRequired { .. }
            | Self::ProvisionedVolumeStorageUnavailable
            | Self::ExistingPlainVolume { .. }
            | Self::ExistingProvisionedVolumeMismatch { .. } => {
                row(Conflict, Invalid, Invalid, false)
            }
            Self::CapacityUnknown | Self::ProvisionedVolumeStorageUnknown { .. } => {
                row(Unavailable, Invalid, Invalid, false)
            }
            Self::Storage { source, .. } => row(Conflict, source.code(), Internal, false),
            Self::NoWriter { .. } => row(
                Conflict,
                ployz_core::SwitchError::NoWriter.code(),
                Internal,
                false,
            ),
            Self::VolumeServerTakesNoServices { .. }
            | Self::FirstCopyUnfit { .. }
            | Self::VolumeOriginsUnchecked { .. }
            | Self::FreeReceiptsUntaken { .. } => row(Conflict, Conflict, Internal, false),
            Self::RegionUnmatched { .. }
            | Self::VolumeOutsideRegion { .. }
            | Self::VolumesApart { .. }
            | Self::VolumeSharedAcrossRegions { .. } => row(Conflict, Conflict, Invalid, false),
            Self::CopiesUnknown { .. } | Self::VolumeHolderUnknown { .. } => {
                row(Unavailable, Unavailable, Internal, false)
            }
        }
    }

    #[must_use]
    pub fn into_rpc_error(self) -> RpcError {
        let refusal = self.refusal();
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
            | Self::VolumeInSeveralRegions { .. }
            | Self::DuplicateTargetService { .. }
            | Self::MixedVolumeModes { .. }
            | Self::DependencyCycle { .. }
            | Self::RemovalInTarget { .. }
            | Self::RemovalNotSelected { .. }
            | Self::RemovalInReservedNamespace { .. }
            | Self::ConflictingDockerVolumeDefinitions { .. }
            | Self::HostnameConflict { .. }
            | Self::VolumeHandoverCycle { .. }
            | Self::HostPortConflict { .. }
            | Self::InsufficientCapacity
            | Self::NoEligibleMachines { .. }
            | Self::ServiceModeCannotChange
            | Self::ProvisionedVolumeStorageRequired { .. }
            | Self::ProvisionedVolumeStorageUnavailable
            | Self::ExistingPlainVolume { .. }
            | Self::ExistingProvisionedVolumeMismatch { .. }
            | Self::CapacityUnknown
            | Self::ProvisionedVolumeStorageUnknown { .. }
            | Self::CopiesUnknown { .. }
            | Self::VolumeHolderUnknown { .. }
            | Self::VolumeServerTakesNoServices { .. }
            | Self::FirstCopyUnfit { .. }
            | Self::VolumeOriginsUnchecked { .. }
            | Self::FreeReceiptsUntaken { .. }
            | Self::RegionUnmatched { .. }
            | Self::VolumeOutsideRegion { .. }
            | Self::VolumesApart { .. }
            | Self::VolumeSharedAcrossRegions { .. }) => {
                let rpc = RpcError::new(refusal.rpc, error.to_string());
                match error.rpc_details() {
                    Some(details) => rpc.with_details(details),
                    None if refusal.same_again => rpc.with_evidence(
                        &crate::sdk::preparation_failure::PreparationFailure::same_again(),
                    ),
                    None => rpc,
                }
            }
        }
    }

    /// What a Machine RPC reply carries in `details` beside the message.
    fn rpc_details(&self) -> Option<serde_json::Value> {
        Some(match self {
            Self::VolumeServerTakesNoServices { .. } => {
                serde_json::json!({ "reason": "volume_server_takes_no_services" })
            }
            Self::FirstCopyUnfit { .. } => serde_json::json!({ "reason": "first_copy_unfit" }),
            Self::VolumeOriginsUnchecked { names, .. }
            | Self::FreeReceiptsUntaken { names, .. } => serde_json::json!({
                "reason": "upgrade_required",
                "servers": names,
            }),
            Self::RegionUnmatched { regions } => serde_json::json!({
                "reason": "region_unmatched",
                "regions": regions.iter().map(|away| &away.region).collect::<Vec<_>>(),
            }),
            Self::VolumeOutsideRegion {
                volume,
                machine,
                region,
                beside,
            } => {
                let (volume_region, replicas) = match beside {
                    BesideVolume::RunIn { region, replicas } => (Some(region), *replicas),
                    BesideVolume::Label => (None, None),
                };
                serde_json::json!({
                    "reason": "volume_outside_region",
                    "volume": volume,
                    "server": machine,
                    "region": region,
                    "volume_region": volume_region,
                    "replicas": replicas,
                })
            }
            Self::VolumesApart { volumes, .. } => serde_json::json!({
                "reason": "volumes_apart",
                "volumes": volumes
                    .iter()
                    .map(|(volume, region)| serde_json::json!({ "volume": volume, "region": region }))
                    .collect::<Vec<_>>(),
            }),
            Self::VolumeSharedAcrossRegions {
                volume,
                service,
                region,
                sharers,
                home,
            } => serde_json::json!({
                "reason": "volume_shared_across_regions",
                "volume": volume,
                "service": service,
                "region": region,
                "sharers": sharers
                    .iter()
                    .map(|(sharer, regions)| serde_json::json!({ "service": sharer, "regions": regions }))
                    .collect::<Vec<_>>(),
                "home": match home.as_ref() {
                    SharedHome::Region(region) => serde_json::json!({ "region": region }),
                    SharedHome::Server(machine) => serde_json::json!({ "server": machine }),
                    SharedHome::Unmade => serde_json::json!("unmade"),
                },
            }),
            Self::Storage { .. }
            | Self::Service { .. }
            | Self::NoWriter { .. }
            | Self::ConflictingHostPublications { .. }
            | Self::VolumeInSeveralRegions { .. }
            | Self::DuplicateTargetService { .. }
            | Self::MixedVolumeModes { .. }
            | Self::DependencyCycle { .. }
            | Self::RemovalInTarget { .. }
            | Self::RemovalNotSelected { .. }
            | Self::RemovalInReservedNamespace { .. }
            | Self::ConflictingDockerVolumeDefinitions { .. }
            | Self::HostnameConflict { .. }
            | Self::VolumeHandoverCycle { .. }
            | Self::HostPortConflict { .. }
            | Self::InsufficientCapacity
            | Self::NoEligibleMachines { .. }
            | Self::ServiceModeCannotChange
            | Self::ProvisionedVolumeStorageRequired { .. }
            | Self::ProvisionedVolumeStorageUnavailable
            | Self::ExistingPlainVolume { .. }
            | Self::ExistingProvisionedVolumeMismatch { .. }
            | Self::CapacityUnknown
            | Self::ProvisionedVolumeStorageUnknown { .. }
            | Self::CopiesUnknown { .. }
            | Self::VolumeHolderUnknown { .. } => return None,
        })
    }

    pub(crate) fn no_eligible_machines(constraints: Vec<EliminatingConstraint>) -> Self {
        Self::NoEligibleMachines {
            constraints: EliminatingConstraints::new(constraints),
        }
    }
}
