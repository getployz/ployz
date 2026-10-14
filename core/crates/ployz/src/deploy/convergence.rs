//! Placement convergence: move one replicated Service's Containers off Machines that no
//! longer admit it, one at a time and start-first. No hooks, no Deployment record, no
//! Config Store writes, and each moved Container keeps the image ID it ran.
//!
//! Reasons and failures are typed; their `Display` is the CLI's English. Convergence never
//! returns an error: whatever moved before a failure stays in its result.

use std::fmt;

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, DeploymentLogId, ExecutionError,
    InspectContainerRequest, Machine, MachineId, MachineName, MachineTarget, PullPolicy,
    QualifiedService, RawVolumeSource, ResolvedServiceSpec, ServiceMode,
    ServicePlacementEligibility, ServingPointer, op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};
use crate::serving_publish::PointerClient;

use super::DeploySnapshot;
use super::exec::MoveContainerError;

/// A Server as a report names it: its durable identity and the name it had then. Two
/// Servers may share a name, so the id is what a reader links by.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineRef {
    pub id: MachineId,
    pub name: MachineName,
}

impl From<&Machine> for MachineRef {
    fn from(machine: &Machine) -> Self {
        Self {
            id: machine.id,
            name: machine.name.clone(),
        }
    }
}

impl fmt::Display for MachineRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(f)
    }
}

/// One Container moved between Servers: started and serving on `to`, then removed from
/// `from`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Move {
    pub from: MachineRef,
    pub to: MachineRef,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Replacement {
    pub(crate) old: ContainerId,
    pub(crate) new: ContainerId,
    pub(crate) deploy: DeploymentLogId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OldCopy<'a> {
    Remove,
    Keep { pointer: &'a ServingPointer },
}

/// How converging one replicated Service ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Converged {
    /// Every Container that had to move did. Never empty.
    Moved {
        moves: Vec<Move>,
        replaced: Vec<Replacement>,
    },
    /// None of its active Containers had to move.
    NothingToMove,
    /// `moves` were made, then `failure` stopped the rest.
    Failed {
        moves: Vec<Move>,
        replaced: Vec<Replacement>,
        failure: MoveFailure,
    },
    /// Nothing moved, and why.
    Stays { reason: StayReason },
    /// It stopped after `moves` without deciding the rest, and the Drain stops with it.
    Stopped {
        moves: Vec<Move>,
        replaced: Vec<Replacement>,
        stop: DrainStop,
    },
}

/// Why a Drain ended early. Every other problem is one Service's evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrainStop {
    /// The caller cancelled, or the SDK session closed.
    Cancelled,
    /// The entry Server stopped answering.
    EntryUnreachable { detail: String },
}

impl fmt::Display for DrainStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::EntryUnreachable { detail } => {
                write!(f, "the entry Server stopped answering: {detail}")
            }
        }
    }
}

/// Why a Service's Containers stay where they are. Each holds what the snapshot can't
/// vouch for, or refuses what a move could lose.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StayReason {
    /// A Server's Containers could not be listed.
    Unobserved { server: MachineRef },
    /// Its Containers carry more than one Serving Shape.
    MidRollout,
    /// It is a Global Service.
    Global,
    /// It mounts a Bind Mount on `server`.
    BindMount { server: MachineRef },
    /// It mounts a Volume that lives on `server`.
    Volume { server: MachineRef },
    /// No Server can take a Container; `detail` is the planner's.
    NoDestination { detail: String },
    /// Its Serving Pointer could not be read, so a move could not be published.
    PointerUnread { detail: String },
}

impl fmt::Display for StayReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unobserved { server } => write!(f, "cannot observe {server}"),
            Self::MidRollout => f.write_str("mid-rollout: deploy it first"),
            Self::Global => f.write_str("Global Services run on every Server that accepts them"),
            Self::BindMount { server } => write!(f, "Bind Mount on {server}"),
            Self::Volume { server } => write!(f, "its Volume is on {server}"),
            Self::NoDestination { detail } => write!(f, "no eligible Server: {detail}"),
            Self::PointerUnread { detail } => {
                write!(f, "cannot read its Serving Pointer: {detail}")
            }
        }
    }
}

/// Where a move stopped. Every stage but `OldNotRemoved` leaves the Container being moved
/// serving where it was; `OldNotRemoved` leaves it serving on `to`, and on `from` too
/// unless the old Container stopped.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum MoveFailure {
    /// No Server could take its next Container any more.
    NoDestination { from: MachineRef, detail: String },
    /// `from` predates image IDs.
    SourceTooOld { from: MachineRef },
    /// Reading its running image ID on `from` failed.
    ReadImage {
        from: MachineRef,
        to: MachineRef,
        detail: String,
    },
    /// Copying the image to `to` failed.
    CopyImage {
        from: MachineRef,
        to: MachineRef,
        detail: String,
    },
    /// The new Container on `to` never served. `replacement_removed` says none is left
    /// there; otherwise it may still be.
    NotServing {
        from: MachineRef,
        to: MachineRef,
        detail: String,
        replacement_removed: bool,
    },
    /// The new Container serves on `to`, but the old one on `from` could not be stopped
    /// (`old_stopped` false: both serve) or, once stopped, removed.
    OldNotRemoved {
        from: MachineRef,
        to: MachineRef,
        detail: String,
        old_stopped: bool,
    },
    /// Cancelled mid-move. `replacement_removed` says no new Container is left on `to`.
    Cancelled {
        from: MachineRef,
        to: MachineRef,
        replacement_removed: bool,
    },
    /// A fresh observation before the next move refuses it.
    Refused { reason: StayReason },
}

impl fmt::Display for MoveFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDestination { detail, .. } => write!(f, "no eligible Server: {detail}"),
            Self::SourceTooOld { from } => write!(
                f,
                "Server {from} does not report image IDs; upgrade it with `ployz server upgrade {from}`, then rerun"
            ),
            Self::ReadImage { from, detail, .. } => write!(f, "inspecting it on {from}: {detail}"),
            Self::CopyImage { to, detail, .. } => write!(f, "copying its image to {to}: {detail}"),
            Self::NotServing {
                from, to, detail, ..
            }
            | Self::OldNotRemoved {
                from, to, detail, ..
            } => write!(f, "moving it from {from} to {to}: {detail}"),
            Self::Cancelled { from, to, .. } => write!(
                f,
                "moving it from {from} to {to}: {}",
                ExecutionError::Cancelled
            ),
            Self::Refused { reason } => reason.fmt(f),
        }
    }
}

pub(crate) trait ConvergenceClient {
    async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError>;
    async fn move_one(
        &mut self,
        snapshot: &DeploySnapshot,
        container: &ContainerObservation,
        old: OldCopy<'_>,
        cancellation: &CancellationToken,
    ) -> Result<(Move, Option<Replacement>), MoveFailure>;
}

impl ConvergenceClient for Client {
    async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError> {
        let machines = self.machines().await?;
        self.deploy_snapshot(machines).await
    }

    async fn move_one(
        &mut self,
        snapshot: &DeploySnapshot,
        container: &ContainerObservation,
        old: OldCopy<'_>,
        cancellation: &CancellationToken,
    ) -> Result<(Move, Option<Replacement>), MoveFailure> {
        move_one(self, snapshot, container, old, cancellation).await
    }
}

pub(crate) async fn converge<C: ConvergenceClient>(
    client: &mut C,
    service: &QualifiedService,
    old: OldCopy<'_>,
    cancellation: &CancellationToken,
) -> Converged {
    let entry_lost = |moves, replaced, error: ConnectError| Converged::Stopped {
        moves,
        replaced,
        stop: DrainStop::EntryUnreachable {
            detail: crate::ui::row(&error),
        },
    };
    let failed = |moves, replaced, failure| Converged::Failed {
        moves,
        replaced,
        failure,
    };
    let retiring = match old {
        OldCopy::Remove => &[][..],
        OldCopy::Keep { pointer } => &pointer.retiring,
    };
    let snapshot = match client.observe().await {
        Ok(snapshot) => snapshot,
        Err(error) => return entry_lost(Vec::new(), Vec::new(), error),
    };
    let initial = movable(&snapshot, service, retiring);
    if let Some(reason) = refusal(&snapshot, service, &initial) {
        return Converged::Stays { reason };
    }
    let ids = initial
        .iter()
        .map(|container| container.container_id)
        .collect::<Vec<_>>();
    let mut moves = Vec::new();
    let mut replaced = Vec::new();
    let mut first = Some(snapshot);
    for id in ids {
        let snapshot = match first.take() {
            Some(snapshot) => snapshot,
            None => match client.observe().await {
                Ok(snapshot) => snapshot,
                Err(error) => return entry_lost(moves, replaced, error),
            },
        };
        // A snapshot missing a Server's listing can't tell a gone Container from an unlisted one.
        if let Some(reason) = unobserved(&snapshot) {
            return failed(moves, replaced, MoveFailure::Refused { reason });
        }
        let fresh = movable(&snapshot, service, retiring);
        // Gone, exited, or admitted again since the first snapshot: it stays put, whatever
        // the rest would now refuse.
        let Some(container) = fresh
            .iter()
            .copied()
            .find(|container| container.container_id == id)
        else {
            continue;
        };
        if let Some(reason) = refusal(&snapshot, service, &fresh) {
            return failed(moves, replaced, MoveFailure::Refused { reason });
        }
        if cancellation.is_cancelled() {
            return Converged::Stopped {
                moves,
                replaced,
                stop: DrainStop::Cancelled,
            };
        }
        match client
            .move_one(&snapshot, container, old, cancellation)
            .await
        {
            Ok((step, kept)) => {
                moves.push(step);
                replaced.extend(kept);
            }
            Err(failure) => return failed(moves, replaced, failure),
        }
    }
    if moves.is_empty() {
        Converged::NothingToMove
    } else {
        Converged::Moved { moves, replaced }
    }
}

/// Why nothing of this Service may move now, if so. It holds whatever the snapshot can't
/// vouch for, and refuses before anything is removed when no Server can take a Container.
fn refusal(
    snapshot: &DeploySnapshot,
    service: &QualifiedService,
    stranded: &[&ContainerObservation],
) -> Option<StayReason> {
    if let Some(reason) = unobserved(snapshot) {
        return Some(reason);
    }
    let active = snapshot
        .containers
        .iter()
        .filter(|container| is_service_container(container, service))
        .collect::<Vec<_>>();
    let spec = &active.first()?.resolved_spec;
    if active
        .iter()
        .any(|container| container.resolved_spec.serving_shape() != spec.serving_shape())
    {
        return Some(StayReason::MidRollout);
    }
    // The snapshot carries no storage evidence: `stays` pins every storage-dependent spec
    // first. Observe storage once Volumes can move.
    let first = stranded.first()?;
    if let Some(reason) = stays(spec, &machine_ref(snapshot, &first.machine_id)) {
        return Some(reason);
    }
    super::planning::place_one(&spec.to_requested(), &service.namespace, snapshot)
        .err()
        .map(|error| StayReason::NoDestination {
            detail: crate::ui::row(&error),
        })
}

/// The first Server whose Containers the snapshot could not list, if any.
fn unobserved(snapshot: &DeploySnapshot) -> Option<StayReason> {
    snapshot
        .container_failures
        .iter()
        .map(|failure| &failure.machine_id)
        .chain(&snapshot.container_omissions)
        .next()
        .map(|id| StayReason::Unobserved {
            server: machine_ref(snapshot, id),
        })
}

fn is_service_container(container: &ContainerObservation, service: &QualifiedService) -> bool {
    container.kind == ContainerKind::ServiceContainer
        && container.namespace == service.namespace
        && container.resolved_spec.name == service.name
        && super::is_active_runtime(&container.runtime)
}

/// Why this Service's Containers can't move off `machine`, if they can't. It refuses
/// anything a move could lose.
fn stays(spec: &ResolvedServiceSpec, machine: &MachineRef) -> Option<StayReason> {
    if spec.mode == ServiceMode::Global {
        return Some(StayReason::Global);
    }
    spec.volume_graph()
        .mounted_volumes()
        .find_map(|volume| match volume.source.kind() {
            RawVolumeSource::Tmpfs { .. } => None,
            RawVolumeSource::Bind { .. } => Some(StayReason::BindMount {
                server: machine.clone(),
            }),
            RawVolumeSource::External { .. }
            | RawVolumeSource::Ordinary { .. }
            | RawVolumeSource::Provisioned { .. } => Some(StayReason::Volume {
                server: machine.clone(),
            }),
        })
}

/// The Service's active Containers on Machines its own spec definitely rules out.
fn stranded<'a>(
    snapshot: &'a DeploySnapshot,
    service: &QualifiedService,
) -> Vec<&'a ContainerObservation> {
    snapshot
        .containers
        .iter()
        .filter(|container| {
            is_service_container(container, service)
                && snapshot
                    .machines
                    .iter()
                    .find(|machine| machine.machine.id == container.machine_id)
                    .is_some_and(|machine| {
                        matches!(
                            container
                                .resolved_spec
                                .to_requested()
                                .placement_eligibility_in_namespace(
                                    &service.namespace,
                                    &machine.machine,
                                    machine.storage.as_ref(),
                                ),
                            ServicePlacementEligibility::Ineligible(_)
                        )
                    })
        })
        .collect()
}

fn movable<'a>(
    snapshot: &'a DeploySnapshot,
    service: &QualifiedService,
    retiring: &[ContainerId],
) -> Vec<&'a ContainerObservation> {
    let mut movable = stranded(snapshot, service);
    movable.retain(|container| !retiring.contains(&container.container_id));
    movable
}

/// The snapshot's Machine `id`. Container listings fan out to the snapshot's Machines, so
/// every id a snapshot carries names one of them.
fn machine<'a>(snapshot: &'a DeploySnapshot, id: &MachineId) -> &'a Machine {
    &snapshot
        .machines
        .iter()
        .find(|machine| machine.machine.id == *id)
        .expect("snapshot evidence names the snapshot's Machines")
        .machine
}

fn machine_ref(snapshot: &DeploySnapshot, id: &MachineId) -> MachineRef {
    MachineRef::from(machine(snapshot, id))
}

async fn move_one(
    client: &mut Client,
    snapshot: &DeploySnapshot,
    container: &ContainerObservation,
    old: OldCopy<'_>,
    cancellation: &CancellationToken,
) -> Result<(Move, Option<Replacement>), MoveFailure> {
    let source = machine(snapshot, &container.machine_id);
    let from = MachineRef::from(source);
    let spec = &container.resolved_spec;
    let dest = super::planning::place_one(&spec.to_requested(), &container.namespace, snapshot)
        .map_err(|error| MoveFailure::NoDestination {
            from: from.clone(),
            detail: crate::ui::row(&error),
        })?;
    let dest = machine(snapshot, &dest);
    let to = MachineRef::from(dest);
    let image_id = image_id(client, &container.container_id, &from, &to).await?;
    crate::image::copy_running_image(client, source, dest, &spec.container.image, &image_id)
        .await
        .map_err(|error| MoveFailure::CopyImage {
            from: from.clone(),
            to: to.clone(),
            detail: crate::ui::row(&error),
        })?;
    // The image is on `dest` by now; a registry pull could fetch a different one.
    let mut spec = spec.clone();
    spec.container.pull_policy = PullPolicy::Never;
    let service = QualifiedService {
        namespace: container.namespace.clone(),
        name: spec.name.clone(),
    };
    let mut reader = client.clone();
    let mut at_removal = AtRemoval::from(old);
    let moved = super::exec::move_container(
        &*client,
        &container.namespace,
        &spec,
        &dest.id,
        (&source.id, &container.container_id),
        async || {
            removes_old(
                &mut at_removal,
                &mut reader,
                &service,
                &container.container_id,
            )
            .await
        },
        cancellation,
    )
    .await;
    match moved {
        Ok(new) => match at_removal {
            AtRemoval::Remove => Ok((Move { from, to }, None)),
            AtRemoval::Pointed(deploy) => Ok((
                Move { from, to },
                Some(Replacement {
                    old: container.container_id,
                    new,
                    deploy,
                }),
            )),
            AtRemoval::Unread(detail) => Err(MoveFailure::OldNotRemoved {
                from,
                to,
                detail: format!("reading its Serving Pointer before removing it: {detail}"),
                old_stopped: false,
            }),
        },
        Err(MoveContainerError::NotServing {
            error: ExecutionError::Cancelled,
            replacement_removed,
        }) => Err(MoveFailure::Cancelled {
            from,
            to,
            replacement_removed,
        }),
        Err(MoveContainerError::NotServing {
            error,
            replacement_removed,
        }) => Err(MoveFailure::NotServing {
            from,
            to,
            detail: crate::ui::row(&error),
            replacement_removed,
        }),
        Err(MoveContainerError::OldNotRemoved { error, old_stopped }) => {
            Err(MoveFailure::OldNotRemoved {
                from,
                to,
                detail: crate::ui::row(&error),
                old_stopped,
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::deploy) enum AtRemoval {
    Remove,
    Pointed(DeploymentLogId),
    Unread(String),
}

impl From<OldCopy<'_>> for AtRemoval {
    fn from(old: OldCopy<'_>) -> Self {
        match old {
            OldCopy::Remove => Self::Remove,
            OldCopy::Keep { pointer } => Self::Pointed(pointer.deploy.clone()),
        }
    }
}

pub(in crate::deploy) async fn removes_old<C: PointerClient>(
    at_removal: &mut AtRemoval,
    reader: &mut C,
    service: &QualifiedService,
    old: &ContainerId,
) -> bool {
    if *at_removal == AtRemoval::Remove {
        *at_removal = at_removal_of(reader, service, old).await;
    }
    *at_removal == AtRemoval::Remove
}

async fn at_removal_of<C: PointerClient>(
    reader: &mut C,
    service: &QualifiedService,
    old: &ContainerId,
) -> AtRemoval {
    match reader.read_pointer(service).await {
        Ok(Some(pointer)) if pointer.containers.contains(old) => AtRemoval::Pointed(pointer.deploy),
        Ok(_) => AtRemoval::Remove,
        Err(error) => AtRemoval::Unread(crate::ui::row(&error)),
    }
}

/// The image ID the Container runs on `from`.
async fn image_id(
    client: &Client,
    container: &ContainerId,
    from: &MachineRef,
    to: &MachineRef,
) -> Result<String, MoveFailure> {
    client
        .invoke::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container,
            },
            &MachineTarget::from(&from.id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| MoveFailure::ReadImage {
            from: from.clone(),
            to: to.clone(),
            detail: crate::ui::row(&error),
        })?
        .image_id
        .ok_or_else(|| MoveFailure::SourceTooOld { from: from.clone() })
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerId, ContainerKind, ContainerObservation, ContainerObservationParts,
        ContainerRuntimeObservation, HealthObservation, Machine, MachineId, MachineName,
        MachineObservation, MembershipObservation, Namespace, QualifiedService,
        ResolvedServiceSpec, WireGuardPublicKey,
    };
    use serde_json::json;

    use tokio_util::sync::CancellationToken;

    use super::{
        Converged, ConvergenceClient, DeploySnapshot, DrainStop, MachineRef, Move, MoveFailure,
        OldCopy, Replacement, converge, refusal, stays,
    };
    use crate::connect::ConnectError;

    fn spec(mode: serde_json::Value, source: serde_json::Value) -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": mode,
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" },
            "volumes": [{ "reference": "data", "source": source }],
            "mounts": [{ "volume": "data", "target": "/data" }],
        }))
        .unwrap()
    }

    #[test]
    fn only_what_a_move_cannot_lose_stays() {
        let web = super::MachineRef::from(&machine('2', false).machine);
        let replicated = json!({ "mode": "replicated", "replicas": 2 });
        let tmpfs = json!({ "kind": "tmpfs", "size_bytes": 4096 });
        let stays_with = |mode: &serde_json::Value, source| {
            stays(&spec(mode.clone(), source), &web).map(|reason| reason.to_string())
        };
        assert_eq!(stays_with(&replicated, tmpfs.clone()), None);
        assert_eq!(
            stays_with(
                &replicated,
                json!({ "kind": "bind", "machine_path": "/srv" })
            )
            .as_deref(),
            Some("Bind Mount on machine-2")
        );
        assert_eq!(
            stays_with(&replicated, json!({ "kind": "external", "name": "app_db" })).as_deref(),
            Some("its Volume is on machine-2")
        );
        assert!(stays_with(&json!({ "mode": "global" }), tmpfs).is_some());
    }

    #[test]
    fn what_the_snapshot_cannot_vouch_for_is_held() {
        let replicated = json!({ "mode": "replicated", "replicas": 2 });
        let stateless = spec(replicated.clone(), json!({ "kind": "tmpfs" }));
        let service = QualifiedService::parse("app/api").unwrap();
        let refused = |snapshot: &DeploySnapshot| {
            let stranded = super::stranded(snapshot, &service);
            refusal(snapshot, &service, &stranded).map(|reason| reason.to_string())
        };
        let base = || DeploySnapshot {
            machines: vec![machine('a', false), machine('b', true)],
            containers: vec![container('1', 'a', stateless.clone())],
            ..DeploySnapshot::default()
        };
        assert_eq!(refused(&base()), None);

        let mut newer = stateless.clone();
        newer.container.image = "alpine:3.24".into();
        let mut mixed = base();
        mixed.containers.push(container('2', 'b', newer));
        assert_eq!(
            refused(&mixed).as_deref(),
            Some("mid-rollout: deploy it first")
        );

        let provisioned = spec(
            replicated,
            json!({
                "kind": "provisioned",
                "name": "app_data",
                "maximum_bytes": 1_073_741_824,
                "scope": { "namespace": "app", "logical_name": "data" }
            }),
        );
        let mut no_storage_evidence = base();
        no_storage_evidence.containers = vec![container('1', 'a', provisioned)];
        assert_eq!(
            refused(&no_storage_evidence).as_deref(),
            Some("its Volume is on machine-a"),
            "a Managed volume holds its Service whatever the snapshot knows of storage"
        );

        let mut unobserved = base();
        unobserved
            .container_omissions
            .push(machine('b', true).machine.id);
        assert_eq!(
            refused(&unobserved).as_deref(),
            Some("cannot observe machine-b")
        );

        let mut nowhere = base();
        nowhere.machines = vec![machine('a', false), machine('b', false)];
        assert!(
            refused(&nowhere).is_some_and(|reason| reason.starts_with("no eligible Server")),
            "{:?}",
            refused(&nowhere)
        );
    }

    struct Scripted {
        snapshots: std::collections::VecDeque<Result<DeploySnapshot, ConnectError>>,
        kept: Vec<bool>,
    }

    impl Scripted {
        fn new(snapshots: impl IntoIterator<Item = Result<DeploySnapshot, ConnectError>>) -> Self {
            Self {
                snapshots: snapshots.into_iter().collect(),
                kept: Vec::new(),
            }
        }
    }

    impl ConvergenceClient for Scripted {
        async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError> {
            self.snapshots
                .pop_front()
                .expect("one snapshot per observation")
        }

        async fn move_one(
            &mut self,
            snapshot: &DeploySnapshot,
            container: &ContainerObservation,
            old: OldCopy<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<(Move, Option<Replacement>), MoveFailure> {
            self.kept.push(old != OldCopy::Remove);
            let kept = match old {
                OldCopy::Remove => None,
                OldCopy::Keep { pointer } => Some(Replacement {
                    old: container.container_id,
                    new: id('f'),
                    deploy: pointer.deploy.clone(),
                }),
            };
            Ok((
                Move {
                    from: super::machine_ref(snapshot, &container.machine_id),
                    to: MachineRef::from(&machine('b', true).machine),
                },
                kept,
            ))
        }
    }

    fn two_on_a() -> DeploySnapshot {
        let stateless = spec(
            json!({ "mode": "replicated", "replicas": 2 }),
            json!({ "kind": "tmpfs" }),
        );
        DeploySnapshot {
            machines: vec![machine('a', false), machine('b', true)],
            containers: vec![
                container('1', 'a', stateless.clone()),
                container('2', 'a', stateless),
            ],
            ..DeploySnapshot::default()
        }
    }

    fn id(hex: char) -> ContainerId {
        ContainerId::parse(hex.to_string().repeat(64)).unwrap()
    }

    fn one_move() -> Vec<Move> {
        vec![Move {
            from: MachineRef::from(&machine('a', false).machine),
            to: MachineRef::from(&machine('b', true).machine),
        }]
    }

    #[tokio::test]
    async fn losing_the_entry_stops_and_keeps_the_moves_already_made() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted::new([
            Ok(two_on_a()),
            Err(ConnectError::Attempt("entry went away".into())),
        ]);
        assert_eq!(
            converge(
                &mut client,
                &service,
                OldCopy::Remove,
                &CancellationToken::new()
            )
            .await,
            Converged::Stopped {
                moves: one_move(),
                replaced: Vec::new(),
                stop: DrainStop::EntryUnreachable {
                    detail: "connection attempt failed: entry went away".into(),
                },
            }
        );

        let mut client = Scripted::new([Err(ConnectError::Attempt("down".into()))]);
        assert_eq!(
            converge(
                &mut client,
                &service,
                OldCopy::Remove,
                &CancellationToken::new()
            )
            .await,
            Converged::Stopped {
                moves: Vec::new(),
                replaced: Vec::new(),
                stop: DrainStop::EntryUnreachable {
                    detail: "connection attempt failed: down".into(),
                },
            },
            "an entry lost before the first look moves nothing"
        );
    }

    #[tokio::test]
    async fn a_fresh_snapshot_that_refuses_keeps_the_moves_already_made() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut unobserved = two_on_a();
        unobserved
            .container_omissions
            .push(machine('b', true).machine.id);
        let mut client = Scripted::new([Ok(two_on_a()), Ok(unobserved)]);
        let converged = converge(
            &mut client,
            &service,
            OldCopy::Remove,
            &CancellationToken::new(),
        )
        .await;
        let Converged::Failed {
            moves,
            replaced,
            failure,
        } = &converged
        else {
            panic!("{converged:?}");
        };
        assert_eq!(moves, &one_move());
        assert!(replaced.is_empty(), "a removed old copy needs no pointer");
        assert_eq!(failure.to_string(), "cannot observe machine-b");
    }

    #[tokio::test]
    async fn a_container_gone_since_the_first_snapshot_is_skipped_before_any_refusal() {
        let service = QualifiedService::parse("app/api").unwrap();
        let first = two_on_a();
        let stateless = first.containers.first().unwrap().resolved_spec.clone();
        // The moved Container now sits on machine-b, which takes nothing more: a refusal
        // would read "no eligible Server", but the one left to move is gone.
        let after = DeploySnapshot {
            machines: vec![machine('a', false), machine('b', false)],
            containers: vec![container('3', 'b', stateless)],
            ..DeploySnapshot::default()
        };
        let mut client = Scripted::new([Ok(first), Ok(after)]);
        assert_eq!(
            converge(
                &mut client,
                &service,
                OldCopy::Remove,
                &CancellationToken::new()
            )
            .await,
            Converged::Moved {
                moves: one_move(),
                replaced: Vec::new(),
            }
        );
    }

    #[tokio::test]
    async fn a_kept_old_copy_already_retiring_stays_put() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted::new([Ok(two_on_a()), Ok(two_on_a())]);
        let pointer = ployz_core::ServingPointer {
            deploy: ployz_core::DeploymentLogId::parse("dep-a").unwrap(),
            containers: vec![id('2')],
            previous: None,
            retiring: vec![id('1')],
        };
        let converged = converge(
            &mut client,
            &service,
            OldCopy::Keep { pointer: &pointer },
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(
            converged,
            Converged::Moved {
                moves: one_move(),
                replaced: vec![Replacement {
                    old: id('2'),
                    new: id('f'),
                    deploy: pointer.deploy.clone(),
                }],
            }
        );
        assert_eq!(client.kept, [true], "each move keeps its old copy");
    }

    #[tokio::test]
    async fn a_cancelled_convergence_stops_before_moving() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted::new([Ok(two_on_a())]);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            converge(&mut client, &service, OldCopy::Remove, &cancelled).await,
            Converged::Stopped {
                moves: Vec::new(),
                replaced: Vec::new(),
                stop: DrainStop::Cancelled,
            }
        );
    }

    fn machine(hex: char, accepts_services: bool) -> MachineObservation {
        MachineObservation::new(
            Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services,
                accepts_ingress: false,
                id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
                name: MachineName::parse(format!("machine-{hex}")).unwrap(),
                subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                    .parse()
                    .unwrap(),
                public_key: WireGuardPublicKey([hex as u8; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            MembershipObservation::Up,
        )
    }

    fn container(id: char, machine: char, spec: ResolvedServiceSpec) -> ContainerObservation {
        ContainerObservation::try_from(ContainerObservationParts {
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: "api".into(),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
            namespace: Namespace::parse("app").unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: spec,
            address: None,
            labels: Default::default(),
        })
        .unwrap()
    }
}
