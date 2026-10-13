//! Stop this Machine's own Containers that a Serving Pointer superseded.
//!
//! A pointer's `retiring` list names the Containers its deploy replaced. Each
//! Machine stops only the ones it runs, and only once every Machine that is not
//! Down has acked the pointer's deploy with a route set that leaves the Container
//! out, so no ingress still sends it traffic. A Machine that never acks holds a
//! Container back for at most [`FORCE_AFTER`].
//!
//! Everything is re-derived from the `serving/%` and `ingress_routes` rows and
//! local Docker, so a daemon restart picks up where it left off; only the cap's
//! clock restarts.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    time::Duration,
};

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerLocator, ContainerWant, DeploymentLogId, Drain,
    MachineId, MachineName, MembershipObservation, QualifiedService, synthesize_membership,
};
use tokio::{task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{
        AdminClient, Error as CorrosionError, IngressRouteAck, MachineView, ReplicatedStore,
        ServingPointers, Subscription, membership_states_by_address,
    },
    docker::ContainerRuntime,
    machine::LocalMachine,
};

/// Longest a Machine that never acks can hold a superseded Container running.
const FORCE_AFTER: Duration = Duration::from_secs(300);
/// Re-check membership and the cap this often while a local Container waits.
const PENDING_TICK: Duration = Duration::from_secs(5);
/// Re-read even when no change was reported, so a stalled subscription cannot
/// freeze the loop.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const DEBOUNCE: Duration = Duration::from_millis(200);
const DEBOUNCE_LIMIT: Duration = Duration::from_secs(2);
const MEMBERSHIP_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// Drain plus Docker's stop grace and removal.
const RETIRE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the loop needs from this Machine.
pub(crate) trait RetireHost: Clone + Send + Sync + 'static {
    /// Every managed Container on this Machine, running or not.
    fn local_containers(&self) -> impl Future<Output = io::Result<BTreeSet<ContainerId>>> + Send;
    /// Machines that must ack before a Container retires: every one not Down.
    fn ackers(&self) -> impl Future<Output = BTreeMap<MachineId, MachineName>> + Send;
    /// Drain, stop, and remove one local Container.
    fn retire(&self, container: ContainerId) -> impl Future<Output = io::Result<()>> + Send;
}

/// The daemon's host: local Docker, the Machine view, and Corrosion membership.
#[derive(Clone)]
pub(crate) struct DaemonHost {
    pub(crate) machine_id: MachineId,
    pub(crate) machine: LocalMachine,
    pub(crate) containers: ContainerRuntime,
    pub(crate) machines: MachineView,
    pub(crate) admin: AdminClient,
}

impl RetireHost for DaemonHost {
    async fn local_containers(&self) -> io::Result<BTreeSet<ContainerId>> {
        let listed =
            tokio::time::timeout(LIST_TIMEOUT, self.containers.list_managed(&self.machine_id))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "listing Containers timed out")
                })?
                .map_err(io::Error::other)?;
        Ok(listed
            .into_iter()
            .map(|observation| observation.container_id)
            .collect())
    }

    async fn ackers(&self) -> BTreeMap<MachineId, MachineName> {
        let Some(machines) = self.machines.current() else {
            return BTreeMap::new();
        };
        let sample =
            tokio::time::timeout(MEMBERSHIP_SAMPLE_TIMEOUT, self.admin.membership_states()).await;
        let machines = machines.observations.clone();
        let Ok(Ok(states)) = sample else {
            tracing::warn!("membership sample failed; every Machine must ack before retiring");
            return machines
                .into_iter()
                .map(|machine| (machine.id, machine.name))
                .collect();
        };
        synthesize_membership(
            machines,
            &self.machine_id,
            &membership_states_by_address(states),
        )
        .into_iter()
        .filter(|observation| observation.membership != MembershipObservation::Down)
        .map(|observation| (observation.machine.id, observation.machine.name))
        .collect()
    }

    async fn retire(&self, container: ContainerId) -> io::Result<()> {
        let want = ContainerWant::Gone {
            container: ContainerLocator::Id(container),
            drain: Drain::DEFAULT,
            anonymous_volumes: AnonymousVolumes::default(),
        };
        tokio::time::timeout(RETIRE_TIMEOUT, self.machine.ensure_container(want))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "retiring timed out"))?
            .map(|_| ())
            .map_err(io::Error::other)
    }
}

/// One local Container a pointer supersedes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Candidate {
    service: QualifiedService,
    deploy: DeploymentLogId,
    container: ContainerId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Verdict {
    Acked { machines: usize },
    Forced { unacked: Vec<MachineName> },
}

/// What the loop remembers between rounds. Losing it on restart only restarts
/// the cap's clock and one Docker listing.
#[derive(Default)]
struct Retirer {
    first_seen: BTreeMap<Candidate, Instant>,
    /// Retiring ids this Machine does not run; ids are never reused.
    absent: BTreeSet<ContainerId>,
    /// Retiring ids Docker last listed here.
    present: BTreeSet<ContainerId>,
    in_flight: BTreeSet<ContainerId>,
}

impl Retirer {
    /// Retiring ids Docker has not yet been asked about.
    fn unknown(&self, pointers: &ServingPointers) -> BTreeSet<ContainerId> {
        self.candidates(pointers)
            .into_iter()
            .map(|candidate| candidate.container)
            .filter(|id| !self.present.contains(id))
            .collect()
    }

    fn note_local(&mut self, pointers: &ServingPointers, local: &BTreeSet<ContainerId>) {
        for id in self.unknown(pointers) {
            if local.contains(&id) {
                self.present.insert(id);
            } else {
                self.absent.insert(id);
            }
        }
    }

    fn candidates(&self, pointers: &ServingPointers) -> Vec<Candidate> {
        pointers
            .iter()
            .flat_map(|(service, pointer)| {
                pointer
                    .retiring
                    .iter()
                    .filter(|id| !pointer.containers.contains(id) && !self.absent.contains(id))
                    .map(|container| Candidate {
                        service: service.clone(),
                        deploy: pointer.deploy.clone(),
                        container: *container,
                    })
            })
            .collect()
    }

    /// Candidates every acker has acked away, or that waited past the cap.
    fn decide(
        &mut self,
        candidates: Vec<Candidate>,
        acks: &BTreeMap<(MachineId, QualifiedService), IngressRouteAck>,
        ackers: &BTreeMap<MachineId, MachineName>,
        now: Instant,
    ) -> Vec<(Candidate, Verdict)> {
        let mut decided = Vec::new();
        for candidate in candidates {
            let first_seen = *self.first_seen.entry(candidate.clone()).or_insert(now);
            if self.in_flight.contains(&candidate.container) {
                continue;
            }
            let unacked: Vec<MachineName> = ackers
                .iter()
                .filter(|(machine, _)| {
                    !acks
                        .get(&((*machine).clone(), candidate.service.clone()))
                        .is_some_and(|ack| {
                            ack.deploy == candidate.deploy
                                && !ack.containers.contains(&candidate.container)
                        })
                })
                .map(|(_, name)| name.clone())
                .collect();
            let verdict = if !ackers.is_empty() && unacked.is_empty() {
                Verdict::Acked {
                    machines: ackers.len(),
                }
            } else if now.duration_since(first_seen) >= FORCE_AFTER {
                Verdict::Forced { unacked }
            } else {
                continue;
            };
            decided.push((candidate, verdict));
        }
        decided
    }
}

/// Run the retire loop until `shutdown`.
pub(crate) async fn run<H: RetireHost>(
    host: H,
    replicated: ReplicatedStore,
    shutdown: CancellationToken,
) {
    let mut retirer = Retirer::default();
    let mut stopping = JoinSet::new();
    loop {
        let subscriptions = tokio::time::timeout(REFRESH_INTERVAL, async {
            tokio::try_join!(
                replicated.subscribe_serving_pointer_changes(),
                replicated.subscribe_ingress_route_changes(),
            )
        });
        let (mut pointers, mut acks) = tokio::select! {
            subscribed = subscriptions => match subscribed {
                Ok(Ok(subscriptions)) => subscriptions,
                Ok(Err(error)) => {
                    tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire subscription failed, retrying");
                    tokio::select! {
                        () = tokio::time::sleep(RETRY_INTERVAL) => continue,
                        () = shutdown.cancelled() => return,
                    }
                }
                Err(_) => continue,
            },
            () = shutdown.cancelled() => return,
        };
        loop {
            round(&host, &replicated, &mut retirer, &mut stopping).await;
            let tick = if retirer.first_seen.is_empty() {
                REFRESH_INTERVAL
            } else {
                PENDING_TICK
            };
            match wait(
                &mut pointers,
                &mut acks,
                &mut stopping,
                &mut retirer,
                tick,
                &shutdown,
            )
            .await
            {
                Wake::Round => {}
                Wake::Resubscribe(error) => {
                    tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire subscription ended, reopening");
                    tokio::select! {
                        () = tokio::time::sleep(RETRY_INTERVAL) => break,
                        () = shutdown.cancelled() => return,
                    }
                }
                Wake::Shutdown => return,
            }
        }
    }
}

async fn round<H: RetireHost>(
    host: &H,
    replicated: &ReplicatedStore,
    retirer: &mut Retirer,
    stopping: &mut JoinSet<(ContainerId, Verdict, io::Result<()>)>,
) {
    let pointers = match replicated.serving_pointers().await {
        Ok(pointers) => pointers,
        Err(error) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire could not read Serving Pointers");
            return;
        }
    };
    if !retirer.unknown(&pointers).is_empty() {
        match host.local_containers().await {
            Ok(local) => retirer.note_local(&pointers, &local),
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire could not list local Containers");
                return;
            }
        }
    }
    let candidates = retirer.candidates(&pointers);
    retirer
        .first_seen
        .retain(|candidate, _| candidates.contains(candidate));
    if candidates.is_empty() {
        return;
    }
    let acks = match replicated.ingress_routes().await {
        Ok(acks) => acks,
        Err(error) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire could not read ingress route acks");
            return;
        }
    };
    let ackers = host.ackers().await;
    for (candidate, verdict) in retirer.decide(candidates, &acks, &ackers, Instant::now()) {
        if let Verdict::Forced { unacked } = &verdict {
            tracing::warn!(
                container = %candidate.container,
                service = %candidate.service,
                "forcing retire after {} s; never acked: {unacked:?}",
                FORCE_AFTER.as_secs(),
            );
        }
        retirer.in_flight.insert(candidate.container);
        let host = host.clone();
        stopping.spawn(async move {
            let result = host.retire(candidate.container).await;
            (candidate.container, verdict, result)
        });
    }
}

enum Wake {
    Round,
    Resubscribe(CorrosionError),
    Shutdown,
}

async fn wait(
    pointers: &mut Subscription,
    acks: &mut Subscription,
    stopping: &mut JoinSet<(ContainerId, Verdict, io::Result<()>)>,
    retirer: &mut Retirer,
    tick: Duration,
    shutdown: &CancellationToken,
) -> Wake {
    let first = tokio::select! {
        biased;
        () = shutdown.cancelled() => return Wake::Shutdown,
        Some(joined) = stopping.join_next() => {
            finish(retirer, joined);
            return Wake::Round;
        }
        changed = pointers.changed() => changed,
        changed = acks.changed() => changed,
        () = tokio::time::sleep(tick) => {
            if pointers.snapshot_in_progress() || acks.snapshot_in_progress() {
                return Wake::Resubscribe(CorrosionError::Protocol("subscription snapshot stalled".into()));
            }
            return Wake::Round;
        }
    };
    if let Err(error) = first {
        return Wake::Resubscribe(error);
    }
    let quiet = tokio::time::sleep(DEBOUNCE);
    tokio::pin!(quiet);
    let limit = tokio::time::sleep(DEBOUNCE_LIMIT);
    tokio::pin!(limit);
    loop {
        let changed = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Wake::Shutdown,
            () = &mut limit => return Wake::Round,
            changed = pointers.changed() => changed,
            changed = acks.changed() => changed,
            () = &mut quiet => return Wake::Round,
        };
        if let Err(error) = changed {
            return Wake::Resubscribe(error);
        }
        quiet.as_mut().reset(Instant::now() + DEBOUNCE);
    }
}

fn finish(
    retirer: &mut Retirer,
    joined: Result<(ContainerId, Verdict, io::Result<()>), tokio::task::JoinError>,
) {
    let Ok((container, verdict, result)) = joined else {
        return;
    };
    retirer.in_flight.remove(&container);
    retirer.present.remove(&container);
    match (result, verdict) {
        (Ok(()), Verdict::Acked { machines }) => {
            retirer.absent.insert(container);
            tracing::info!("retired {container} after acks from {machines} Machines");
        }
        (Ok(()), Verdict::Forced { unacked }) => {
            retirer.absent.insert(container);
            tracing::info!("retired {container} at the cap; never acked: {unacked:?}");
        }
        (Err(error), _) => tracing::warn!(
            container = %container,
            error = %ployz_core::error_chain::inline(&error),
            "failed to retire Container, retrying",
        ),
    }
}
