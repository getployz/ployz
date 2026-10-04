//! Placement convergence: move one replicated Service's Containers off Machines that no
//! longer admit it, one at a time and start-first. No hooks, no Deployment record, no
//! Config Store writes, and each moved Container keeps the image ID it ran.
//!
//! Reasons and failures are typed; their `Display` is the CLI's English. Convergence never
//! returns an error: whatever moved before a failure stays in its result.

use std::fmt;

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ExecutionError, InspectContainerRequest,
    Machine, MachineId, MachineName, MachineTarget, PullPolicy, QualifiedService, RawVolumeSource,
    ResolvedServiceSpec, ServiceMode, ServicePlacementEligibility, ServiceVolumeReference, op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};

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

/// What Placement convergence did for one replicated Service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Convergence {
    /// Every Container on a Machine its spec rules out moved, in order. Never empty.
    Moved { moves: Vec<Move> },
    /// None of its active Containers sits on a Machine its spec rules out.
    NothingToMove,
    /// `moves` were made, then `failure` stopped the rest.
    Failed {
        moves: Vec<Move>,
        failure: MoveFailure,
    },
    /// Refused before anything moved.
    Stays { reason: StayReason },
}

impl Convergence {
    /// The entry stopped answering while this Service was handled; a Drain stops after it.
    pub(crate) fn lost_entry(&self) -> Option<&str> {
        match self {
            Self::Failed {
                failure: MoveFailure::Unobservable { detail },
                ..
            }
            | Self::Stays {
                reason: StayReason::EntryUnobservable { detail },
            } => Some(detail),
            Self::Moved { .. } | Self::NothingToMove | Self::Failed { .. } | Self::Stays { .. } => {
                None
            }
        }
    }
}

/// Why a Service's Containers stay where they are. Each holds what the snapshot can't
/// vouch for, or refuses what a move could lose.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StayReason {
    /// The entry did not answer the observation.
    EntryUnobservable { detail: String },
    /// A Server's Containers could not be listed.
    Unobserved { server: MachineRef },
    /// Its Containers carry more than one Serving Shape.
    MidRollout,
    /// Its eligibility on `server` is unknown, for lack of storage evidence.
    EligibilityUnknown { server: MachineRef },
    /// It is a Global Service.
    Global,
    /// It mounts a Bind Mount on `server`.
    BindMount { server: MachineRef },
    /// It mounts `volume`, which lives on `server`.
    Volume {
        volume: ServiceVolumeReference,
        server: MachineRef,
    },
    /// No Server can take a Container; `detail` is the planner's.
    NoDestination { detail: String },
}

impl fmt::Display for StayReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntryUnobservable { detail } => write!(f, "cannot observe the Cluster: {detail}"),
            Self::Unobserved { server } => write!(f, "cannot observe {server}"),
            Self::MidRollout => f.write_str("mid-rollout: deploy it first"),
            Self::EligibilityUnknown { server } => write!(f, "eligibility on {server} is unknown"),
            Self::Global => f.write_str("Global Services run on every Server that accepts them"),
            Self::BindMount { server } => write!(f, "Bind Mount on {server}"),
            Self::Volume { volume, server } => write!(f, "Volume {volume} is on {server}"),
            Self::NoDestination { detail } => write!(f, "no eligible Server: {detail}"),
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
    /// Cancelled before the next move began.
    CancelledBeforeMove { from: MachineRef },
    /// The entry stopped answering before the next move.
    Unobservable { detail: String },
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
            Self::CancelledBeforeMove { from } => {
                write!(f, "cancelled before moving it off {from}")
            }
            Self::Unobservable { detail } => write!(f, "cannot observe the Cluster: {detail}"),
            Self::Refused { reason } => reason.fmt(f),
        }
    }
}

/// What convergence needs from a Cluster: a fresh snapshot and one move.
pub(crate) trait ConvergenceClient {
    async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError>;
    async fn move_one(
        &mut self,
        snapshot: &DeploySnapshot,
        container: &ContainerObservation,
        cancellation: &CancellationToken,
    ) -> Result<Move, MoveFailure>;
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
        cancellation: &CancellationToken,
    ) -> Result<Move, MoveFailure> {
        move_one(self, snapshot, container, cancellation).await
    }
}

/// Converge `service`: replace each active Container on a Machine its own spec now rules
/// out with one on an eligible Machine, then remove the old one. Each move after the first
/// starts from a fresh snapshot, rechecked as the first was.
pub(crate) async fn converge<C: ConvergenceClient>(
    client: &mut C,
    service: &QualifiedService,
    cancellation: &CancellationToken,
) -> Convergence {
    let snapshot = match client.observe().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return Convergence::Stays {
                reason: StayReason::EntryUnobservable {
                    detail: error.to_string(),
                },
            };
        }
    };
    let initial = stranded(&snapshot, service);
    if let Some(reason) = refusal(&snapshot, service, &initial) {
        return Convergence::Stays { reason };
    }
    let ids = initial
        .iter()
        .map(|container| container.container_id)
        .collect::<Vec<_>>();
    let mut moves = Vec::new();
    let mut first = Some(snapshot);
    for id in ids {
        let snapshot = match first.take() {
            Some(snapshot) => snapshot,
            None => match client.observe().await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return Convergence::Failed {
                        moves,
                        failure: MoveFailure::Unobservable {
                            detail: error.to_string(),
                        },
                    };
                }
            },
        };
        // A snapshot missing a Server's listing can't tell a gone Container from an unlisted one.
        if let Some(reason) = unobserved(&snapshot) {
            return Convergence::Failed {
                moves,
                failure: MoveFailure::Refused { reason },
            };
        }
        let fresh = stranded(&snapshot, service);
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
            return Convergence::Failed {
                moves,
                failure: MoveFailure::Refused { reason },
            };
        }
        if cancellation.is_cancelled() {
            return Convergence::Failed {
                moves,
                failure: MoveFailure::CancelledBeforeMove {
                    from: machine_ref(&snapshot, &container.machine_id),
                },
            };
        }
        match client.move_one(&snapshot, container, cancellation).await {
            Ok(step) => moves.push(step),
            Err(failure) => return Convergence::Failed { moves, failure },
        }
    }
    if moves.is_empty() {
        Convergence::NothingToMove
    } else {
        Convergence::Moved { moves }
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
    let requested = spec.to_requested();
    if let Some(machine) = snapshot.machines.iter().find(|machine| {
        matches!(
            requested.placement_eligibility_in_namespace(
                &service.namespace,
                &machine.machine,
                machine.storage.as_ref(),
            ),
            ServicePlacementEligibility::Unknown(_)
        )
    }) {
        return Some(StayReason::EligibilityUnknown {
            server: MachineRef::from(&machine.machine),
        });
    }
    let first = stranded.first()?;
    if let Some(reason) = stays(spec, &machine_ref(snapshot, &first.machine_id)) {
        return Some(reason);
    }
    super::planning::place_one(&requested, &service.namespace, snapshot)
        .err()
        .map(|error| StayReason::NoDestination {
            detail: error.to_string(),
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
                volume: volume.reference.clone(),
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

/// Place, copy the exact image, start and serve, then remove the old Container.
async fn move_one(
    client: &mut Client,
    snapshot: &DeploySnapshot,
    container: &ContainerObservation,
    cancellation: &CancellationToken,
) -> Result<Move, MoveFailure> {
    let source = machine(snapshot, &container.machine_id);
    let from = MachineRef::from(source);
    let spec = &container.resolved_spec;
    let dest = super::planning::place_one(&spec.to_requested(), &container.namespace, snapshot)
        .map_err(|error| MoveFailure::NoDestination {
            from: from.clone(),
            detail: error.to_string(),
        })?;
    let dest = machine(snapshot, &dest);
    let to = MachineRef::from(dest);
    let image_id = image_id(client, source, &container.container_id)
        .await
        .map_err(|failure| match failure {
            NoImageId::Unreported => MoveFailure::SourceTooOld { from: from.clone() },
            NoImageId::Unread(detail) => MoveFailure::ReadImage {
                from: from.clone(),
                to: to.clone(),
                detail,
            },
        })?;
    crate::image::copy_running_image(client, source, dest, &spec.container.image, &image_id)
        .await
        .map_err(|error| MoveFailure::CopyImage {
            from: from.clone(),
            to: to.clone(),
            detail: error.to_string(),
        })?;
    // The image is on `dest` by now; a registry pull could fetch a different one.
    let mut spec = spec.clone();
    spec.container.pull_policy = PullPolicy::Never;
    match super::exec::move_container(
        &*client,
        &container.namespace,
        &spec,
        &dest.id,
        (&source.id, &container.container_id),
        cancellation,
    )
    .await
    {
        Ok(_) => Ok(Move { from, to }),
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
            detail: error.to_string(),
            replacement_removed,
        }),
        Err(MoveContainerError::OldNotRemoved { error, old_stopped }) => {
            Err(MoveFailure::OldNotRemoved {
                from,
                to,
                detail: error.to_string(),
                old_stopped,
            })
        }
    }
}

enum NoImageId {
    /// The source predates image IDs.
    Unreported,
    /// Inspecting the Container failed: the RPC error's message.
    Unread(String),
}

async fn image_id(
    client: &Client,
    source: &Machine,
    container: &ContainerId,
) -> Result<String, NoImageId> {
    client
        .invoke::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container,
            },
            &MachineTarget::from(&source.id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| NoImageId::Unread(error.message))?
        .image_id
        .ok_or(NoImageId::Unreported)
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
        Convergence, ConvergenceClient, DeploySnapshot, MachineRef, Move, MoveFailure, converge,
        refusal, stays,
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
            Some("Volume data is on machine-2")
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
        let mut unknown = base();
        unknown.containers = vec![container('1', 'a', provisioned)];
        assert_eq!(
            refused(&unknown).as_deref(),
            Some("eligibility on machine-b is unknown")
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

    /// Serves queued snapshots; every move succeeds onto machine-b.
    struct Scripted {
        snapshots: std::collections::VecDeque<Result<DeploySnapshot, ConnectError>>,
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
            _cancellation: &CancellationToken,
        ) -> Result<Move, MoveFailure> {
            Ok(Move {
                from: super::machine_ref(snapshot, &container.machine_id),
                to: MachineRef::from(&machine('b', true).machine),
            })
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

    fn one_move() -> Vec<Move> {
        vec![Move {
            from: MachineRef::from(&machine('a', false).machine),
            to: MachineRef::from(&machine('b', true).machine),
        }]
    }

    #[tokio::test]
    async fn a_lost_observation_keeps_the_moves_already_made() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted {
            snapshots: [
                Ok(two_on_a()),
                Err(ConnectError::Attempt("entry went away".into())),
            ]
            .into(),
        };
        let convergence = converge(&mut client, &service, &CancellationToken::new()).await;
        assert_eq!(
            convergence,
            Convergence::Failed {
                moves: one_move(),
                failure: MoveFailure::Unobservable {
                    detail: "connection attempt failed: entry went away".into()
                },
            }
        );
        assert!(convergence.lost_entry().is_some());
    }

    #[tokio::test]
    async fn a_fresh_snapshot_that_refuses_keeps_the_moves_already_made() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut unobserved = two_on_a();
        unobserved
            .container_omissions
            .push(machine('b', true).machine.id);
        let mut client = Scripted {
            snapshots: [Ok(two_on_a()), Ok(unobserved)].into(),
        };
        let convergence = converge(&mut client, &service, &CancellationToken::new()).await;
        let Convergence::Failed { moves, failure } = &convergence else {
            panic!("{convergence:?}");
        };
        assert_eq!(moves, &one_move());
        assert_eq!(failure.to_string(), "cannot observe machine-b");
        assert!(convergence.lost_entry().is_none());
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
        let mut client = Scripted {
            snapshots: [Ok(first), Ok(after)].into(),
        };
        let convergence = converge(&mut client, &service, &CancellationToken::new()).await;
        assert_eq!(convergence, Convergence::Moved { moves: one_move() });
    }

    #[tokio::test]
    async fn a_cancelled_convergence_says_it_stopped_before_moving() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted {
            snapshots: [Ok(two_on_a())].into(),
        };
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let convergence = converge(&mut client, &service, &cancelled).await;
        assert_eq!(
            convergence,
            Convergence::Failed {
                moves: Vec::new(),
                failure: MoveFailure::CancelledBeforeMove {
                    from: MachineRef::from(&machine('a', false).machine),
                },
            }
        );
    }

    #[tokio::test]
    async fn an_unobservable_entry_moves_nothing_and_says_so() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted {
            snapshots: [Err(ConnectError::Attempt("down".into()))].into(),
        };
        let convergence = converge(&mut client, &service, &CancellationToken::new()).await;
        let Convergence::Stays { reason } = &convergence else {
            panic!("{convergence:?}");
        };
        assert_eq!(
            reason.to_string(),
            "cannot observe the Cluster: connection attempt failed: down"
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
