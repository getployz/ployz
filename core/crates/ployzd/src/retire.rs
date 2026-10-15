//! The retire loop: stops this Machine's own Containers a Serving Pointer
//! superseded, once every Machine not Down acked the pointer's deploy without them,
//! or `FORCE_AFTER` after it first saw them. It re-derives everything from rows
//! and Docker, so a restart resumes it, and it never starts, places or moves a
//! Container.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    time::Duration,
};

use ployz_core::{
    ContainerId, DeploymentLogId, Drain, Machine, MachineId, MachineName, ManagementAddress,
    MembershipEvidence, QualifiedService,
};
use tokio::{task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{
        AdminClient, Error as CorrosionError, IngressRouteAck, IngressRouteRow, MachineView,
        ReplicatedObservations, ReplicatedStore, ServingPointers, Subscription,
        membership_states_by_address,
    },
    docker::ContainerRuntime,
    machine::LocalMachine,
};

const FORCE_AFTER: Duration = Duration::from_secs(300);
const PENDING_TICK: Duration = Duration::from_secs(5);
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const DEBOUNCE: Duration = Duration::from_millis(200);
const DEBOUNCE_LIMIT: Duration = Duration::from_secs(2);
const MEMBERSHIP_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) trait RetireHost: Clone + Send + Sync + 'static {
    fn local_containers(&self) -> impl Future<Output = io::Result<BTreeSet<ContainerId>>> + Send;
    fn membership(&self) -> impl Future<Output = Membership> + Send;
    type Withdrawal: Send;
    fn withdraw(&self, container: ContainerId) -> Self::Withdrawal;
    fn drain(&self) -> impl Future<Output = ()> + Send;
    /// Stop and remove the drained Container under the Machine's exclusive
    /// admission turn, if `still_retired` answers true under that turn.
    fn remove(
        &self,
        container: ContainerId,
        still_retired: impl Future<Output = Result<bool, CorrosionError>> + Send + 'static,
    ) -> impl Future<Output = Outcome> + Send;
}

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
        let docker = self.containers.local_docker();
        let listed = tokio::time::timeout(LIST_TIMEOUT, docker.managed_container_ids())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "listing Containers timed out"))?
            .map_err(io::Error::other)?;
        Ok(listed.into_iter().collect())
    }

    async fn membership(&self) -> Membership {
        let Some(machines) = self.machines.current() else {
            return Membership {
                ackers: Ackers::Unread,
                down: BTreeSet::new(),
            };
        };
        sample_ackers(machines.as_ref().clone(), &self.machine_id, &self.admin).await
    }

    type Withdrawal = crate::docker::Withdrawal;

    fn withdraw(&self, container: ContainerId) -> Self::Withdrawal {
        self.containers.withdraw(container)
    }

    async fn drain(&self) {
        tokio::time::sleep(Drain::DEFAULT.as_duration()).await;
    }

    async fn remove(
        &self,
        container: ContainerId,
        still_retired: impl Future<Output = Result<bool, CorrosionError>> + Send + 'static,
    ) -> Outcome {
        remove_drained(&self.machine, container, still_retired).await
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Ackers {
    Known(BTreeMap<MachineId, MachineName>),
    Incomplete {
        named: BTreeMap<MachineId, MachineName>,
        unnamed: Vec<MachineId>,
    },
    Unread,
}

impl Ackers {
    fn unacked(&self, acked: impl Fn(&MachineId) -> bool) -> Vec<String> {
        let unacked_names = |machines: &BTreeMap<MachineId, MachineName>| {
            machines
                .iter()
                .filter(|(machine, _)| !acked(machine))
                .map(|(_, name)| name.to_string())
                .collect::<Vec<_>>()
        };
        match self {
            Self::Known(machines) => unacked_names(machines),
            Self::Incomplete { named, unnamed } => {
                let mut unacked = unacked_names(named);
                unacked.extend(
                    unnamed
                        .iter()
                        .filter(|machine| !acked(machine))
                        .map(|machine| machine.as_str().to_owned()),
                );
                unacked
            }
            Self::Unread => vec!["unknown, the Machine list was never read".to_owned()],
        }
    }

    fn lists(&self, machine: &MachineId) -> bool {
        match self {
            Self::Known(named) => named.contains_key(machine),
            Self::Incomplete { named, unnamed } => {
                named.contains_key(machine) || unnamed.contains(machine)
            }
            Self::Unread => false,
        }
    }
}

/// One round's membership: the Machines that must ack, and the Machines the
/// sample reported Down, whose ingress routes no longer count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Membership {
    pub(crate) ackers: Ackers,
    pub(crate) down: BTreeSet<MachineId>,
}

impl Membership {
    /// Machines outside the ackers, not reported Down, that may still route
    /// the Candidate. Each holds it like an unacked Machine.
    fn unlisted_routers(
        &self,
        candidate: &Candidate,
        routes: &BTreeMap<(MachineId, QualifiedService), IngressRouteRow>,
    ) -> Vec<String> {
        routes
            .iter()
            .filter(|((machine, service), row)| {
                candidate.routed_by(service, row)
                    && !self.down.contains(machine)
                    && !self.ackers.lists(machine)
            })
            .map(|((machine, _), _)| machine.as_str().to_owned())
            .collect()
    }
}

/// Machines whose ingress must ack: every one not reported Down, or every one
/// when membership cannot be sampled.
pub(crate) async fn sample_ackers(
    machines: ReplicatedObservations<Machine, MachineId>,
    local: &MachineId,
    admin: &AdminClient,
) -> Membership {
    let states =
        match tokio::time::timeout(MEMBERSHIP_SAMPLE_TIMEOUT, admin.membership_states()).await {
            Ok(Ok(states)) => Some(membership_states_by_address(states)),
            _ => {
                tracing::warn!("membership sample failed; every Machine must ack");
                None
            }
        };
    required_ackers(machines, local, states.as_ref())
}

fn required_ackers(
    machines: ReplicatedObservations<Machine, MachineId>,
    local: &MachineId,
    states: Option<&BTreeMap<ManagementAddress, MembershipEvidence>>,
) -> Membership {
    let (down, up): (Vec<Machine>, Vec<Machine>) = machines
        .observations
        .into_iter()
        .partition(|machine| &machine.id != local && reported_down(machine, states));
    let named = up
        .into_iter()
        .map(|machine| (machine.id, machine.name))
        .collect();
    let ackers = if machines.incomplete_ids.is_empty() {
        Ackers::Known(named)
    } else {
        Ackers::Incomplete {
            named,
            unnamed: machines.incomplete_ids,
        }
    };
    Membership {
        ackers,
        down: down.into_iter().map(|machine| machine.id).collect(),
    }
}

fn reported_down(
    machine: &Machine,
    states: Option<&BTreeMap<ManagementAddress, MembershipEvidence>>,
) -> bool {
    states.is_some_and(|states| {
        states.get(&machine.management_address()) == Some(&MembershipEvidence::Down)
    })
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Candidate {
    service: QualifiedService,
    deploy: DeploymentLogId,
    serving: BTreeSet<ContainerId>,
    container: ContainerId,
}

impl Candidate {
    /// An ack proves its Machine moved off this Container only if it names the
    /// pointer's deploy, routes something the pointer serves, and not this
    /// Container. An ack that routes none of the pointer's Containers may be a
    /// projection that has not caught up yet.
    fn acked_away(&self, ack: &IngressRouteAck) -> bool {
        ack.deploy == self.deploy
            && !ack.containers.contains(&self.container)
            && (self.serving.is_empty() || !ack.containers.is_disjoint(&self.serving))
    }

    fn routed_by(&self, service: &QualifiedService, row: &IngressRouteRow) -> bool {
        match row {
            IngressRouteRow::Acked(ack) => ack.containers.contains(&self.container),
            IngressRouteRow::UnknownDeploy(containers) => containers.contains(&self.container),
            IngressRouteRow::UnknownContainers => *service == self.service,
        }
    }

    fn decided_deploy_still_retires(&self, pointers: &ServingPointers) -> bool {
        let pointed = pointers
            .iter()
            .any(|(_, pointer)| pointer.containers.contains(&self.container));
        let retiring = pointers.get(&self.service).is_some_and(|pointer| {
            pointer.deploy == self.deploy
                && !pointer.is_held()
                && pointer.retiring.contains(&self.container)
        });
        retiring && !pointed
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Verdict {
    Acked { machines: usize },
    Forced { unacked: Vec<String> },
}

#[derive(Default)]
struct Retirer {
    first_seen: BTreeMap<ContainerId, Instant>,
    /// Docker Container ids are random and never reused, so an absent id stays absent.
    absent: BTreeSet<ContainerId>,
    present: BTreeSet<ContainerId>,
    in_flight: BTreeSet<ContainerId>,
    retry_after: BTreeMap<ContainerId, Instant>,
}

impl Retirer {
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
        let pointed: BTreeSet<&ContainerId> = pointers
            .iter()
            .flat_map(|(_, pointer)| &pointer.containers)
            .collect();
        pointers
            .iter()
            .filter(|(_, pointer)| !pointer.is_held())
            .flat_map(|(service, pointer)| {
                pointer
                    .retiring
                    .iter()
                    .filter(|id| !pointed.contains(id) && !self.absent.contains(id))
                    .map(|container| Candidate {
                        service: service.clone(),
                        deploy: pointer.deploy.clone(),
                        serving: pointer.containers.iter().copied().collect(),
                        container: *container,
                    })
            })
            .collect()
    }

    fn forget_unpointed(&mut self, pointers: &ServingPointers) {
        let retiring: BTreeSet<&ContainerId> = pointers
            .iter()
            .flat_map(|(_, pointer)| &pointer.retiring)
            .collect();
        self.absent.retain(|id| retiring.contains(id));
        self.present.retain(|id| retiring.contains(id));
    }

    fn track(&mut self, candidates: &[Candidate], now: Instant) {
        let tracked = |container: &ContainerId| {
            candidates
                .iter()
                .any(|candidate| candidate.container == *container)
        };
        self.first_seen.retain(|container, _| tracked(container));
        self.retry_after.retain(|container, _| tracked(container));
        for candidate in candidates {
            self.first_seen.entry(candidate.container).or_insert(now);
        }
    }

    fn decide(
        &mut self,
        candidates: Vec<Candidate>,
        routes: &BTreeMap<(MachineId, QualifiedService), IngressRouteRow>,
        membership: &Membership,
        now: Instant,
    ) -> Vec<(Candidate, Verdict)> {
        let mut decided = Vec::new();
        for candidate in candidates {
            let first_seen = *self.first_seen.entry(candidate.container).or_insert(now);
            let retry_wait = self
                .retry_after
                .get(&candidate.container)
                .is_some_and(|after| now < *after);
            if self.in_flight.contains(&candidate.container) || retry_wait {
                continue;
            }
            let ackers = &membership.ackers;
            let mut unacked = ackers.unacked(|machine| {
                matches!(
                    routes.get(&(*machine, candidate.service.clone())),
                    Some(IngressRouteRow::Acked(ack)) if candidate.acked_away(ack)
                )
            });
            unacked.extend(membership.unlisted_routers(&candidate, routes));
            let acked_by = match ackers {
                Ackers::Known(machines) => {
                    (!machines.is_empty() && unacked.is_empty()).then_some(machines.len())
                }
                Ackers::Incomplete { .. } | Ackers::Unread => None,
            };
            let verdict = if let Some(machines) = acked_by {
                Verdict::Acked { machines }
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
    stopping: &mut JoinSet<(ContainerId, Verdict, Outcome)>,
) {
    let pointers = match replicated.serving_pointers().await {
        Ok(pointers) => pointers,
        Err(error) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire could not read Serving Pointers");
            return;
        }
    };
    retirer.forget_unpointed(&pointers);
    if !retirer.unknown(&pointers).is_empty() {
        match host.local_containers().await {
            Ok(local) => retirer.note_local(&pointers, &local),
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire could not list local Containers");
                retirer.track(&retirer.candidates(&pointers), Instant::now());
                return;
            }
        }
    }
    let candidates = retirer.candidates(&pointers);
    retirer.track(&candidates, Instant::now());
    if candidates.is_empty() {
        return;
    }
    let routes = match replicated.ingress_routes().await {
        Ok(routes) => routes,
        Err(error) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "retire could not read ingress route acks");
            return;
        }
    };
    let membership = host.membership().await;
    for (candidate, verdict) in retirer.decide(candidates, &routes, &membership, Instant::now()) {
        if let Verdict::Forced { unacked } = &verdict {
            tracing::warn!(
                container = %candidate.container,
                service = %candidate.service,
                "forcing retire after {} s; never acked: {unacked:?}",
                FORCE_AFTER.as_secs(),
            );
        }
        retirer.in_flight.insert(candidate.container);
        let (host, replicated) = (host.clone(), replicated.clone());
        stopping.spawn(async move {
            let outcome = retire(&host, &replicated, &candidate).await;
            (candidate.container, verdict, outcome)
        });
    }
}

pub(crate) enum Outcome {
    Retired,
    /// A pointer now serves the Container or no longer retires it.
    Kept,
    Failed(io::Error),
}

async fn retire<H: RetireHost>(
    host: &H,
    replicated: &ReplicatedStore,
    candidate: &Candidate,
) -> Outcome {
    match still_retired(replicated.clone(), candidate.clone()).await {
        Ok(true) => {}
        Ok(false) => return Outcome::Kept,
        Err(error) => return Outcome::Failed(io::Error::other(error)),
    }
    let _withdrawal = host.withdraw(candidate.container);
    host.drain().await;
    host.remove(
        candidate.container,
        still_retired(replicated.clone(), candidate.clone()),
    )
    .await
}

async fn still_retired(
    replicated: ReplicatedStore,
    candidate: Candidate,
) -> Result<bool, CorrosionError> {
    Ok(candidate.decided_deploy_still_retires(&replicated.serving_pointers().await?))
}

async fn remove_drained(
    machine: &LocalMachine,
    container: ContainerId,
    still_retired: impl Future<Output = Result<bool, CorrosionError>> + Send + 'static,
) -> Outcome {
    match machine.remove_drained_if(container, still_retired).await {
        Ok(true) => Outcome::Retired,
        Ok(false) => Outcome::Kept,
        Err(error) => Outcome::Failed(io::Error::other(error)),
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
    stopping: &mut JoinSet<(ContainerId, Verdict, Outcome)>,
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
    joined: Result<(ContainerId, Verdict, Outcome), tokio::task::JoinError>,
) {
    let Ok((container, verdict, outcome)) = joined else {
        return;
    };
    retirer.in_flight.remove(&container);
    retirer.present.remove(&container);
    match (outcome, verdict) {
        (Outcome::Retired, Verdict::Acked { machines }) => {
            retirer.absent.insert(container);
            tracing::info!("retired {container} after acks from {machines} Machines");
        }
        (Outcome::Retired, Verdict::Forced { unacked }) => {
            retirer.absent.insert(container);
            tracing::info!("retired {container} at the cap; never acked: {unacked:?}");
        }
        (Outcome::Kept, _) => {
            retirer
                .retry_after
                .insert(container, Instant::now() + PENDING_TICK);
            tracing::info!(
                container = %container,
                "kept retire: a pointer serves it or no longer retires it"
            );
        }
        (Outcome::Failed(error), _) => {
            retirer
                .retry_after
                .insert(container, Instant::now() + PENDING_TICK);
            tracing::warn!(
                container = %container,
                error = %ployz_core::error_chain::inline(&error),
                "failed to retire Container, retrying in {} s",
                PENDING_TICK.as_secs(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        future::Future,
        io,
        sync::{Arc, Mutex},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use ployz_core::{
        AdvertisedEndpoint, ContainerId, DeploymentLogId, Hold, Machine, MachineId, MachineName,
        MachineRuntime, MembershipEvidence, QualifiedService, ServingPointer, WireGuardPublicKey,
    };
    use tokio::{task::JoinSet, time::Instant};
    use tokio_util::sync::CancellationToken;

    use super::{
        Ackers, Candidate, FORCE_AFTER, Membership, Outcome, PENDING_TICK, RetireHost, Retirer,
        Verdict, finish, remove_drained, required_ackers, round, run,
    };
    use crate::corrosion::{
        Error as CorrosionError, IngressRouteAck, IngressRouteChange, IngressRouteRow,
        ReplicatedObservations, ReplicatedStore, ServingPointers,
        fake_cluster::FakeCluster,
        store_tests::{corrosion_db, identity_store},
    };

    type Acks = BTreeMap<(MachineId, QualifiedService), IngressRouteRow>;

    fn id(letter: &str) -> ContainerId {
        ContainerId::parse(letter.repeat(64)).unwrap()
    }

    fn deploy(name: &str) -> DeploymentLogId {
        DeploymentLogId::parse(name).unwrap()
    }

    fn service() -> QualifiedService {
        QualifiedService::parse("app/api").unwrap()
    }

    fn machine(name: &str, octet: u8) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::random(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{octet}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([octet; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("192.0.2.{octet}:51820").parse().unwrap(),
            )],
            runtime: MachineRuntime::default(),
            build_concurrency: None,
        }
    }

    fn pointers() -> ServingPointers {
        ServingPointers::from_iter([(
            service(),
            ServingPointer {
                deploy: deploy("d2"),
                containers: vec![id("b")],
                previous: Some(deploy("d1")),
                retiring: vec![id("a")],
                deciding: None,
            },
        )])
    }

    fn ack(deploy_name: &str, containers: &[ContainerId]) -> IngressRouteAck {
        IngressRouteAck {
            deploy: deploy(deploy_name),
            containers: containers.iter().copied().collect(),
        }
    }

    fn acked(deploy_name: &str, containers: &[ContainerId]) -> IngressRouteRow {
        IngressRouteRow::Acked(ack(deploy_name, containers))
    }

    fn named(machines: &[&Machine]) -> BTreeMap<MachineId, MachineName> {
        machines
            .iter()
            .map(|machine| (machine.id, machine.name.clone()))
            .collect()
    }

    fn ackers(machines: &[&Machine]) -> Membership {
        known(Ackers::Known(named(machines)))
    }

    fn known(ackers: Ackers) -> Membership {
        Membership {
            ackers,
            down: BTreeSet::new(),
        }
    }

    fn acked_away(machines: &[&Machine]) -> Acks {
        machines
            .iter()
            .map(|machine| ((machine.id, service()), acked("d2", &[id("b")])))
            .collect()
    }

    fn complete(machines: Vec<Machine>) -> ReplicatedObservations<Machine, MachineId> {
        ReplicatedObservations {
            observations: machines,
            incomplete_ids: Vec::new(),
        }
    }

    fn decide_now(
        retirer: &mut Retirer,
        pointers: &ServingPointers,
        acks: &Acks,
        ackers: &Membership,
        local: &[ContainerId],
    ) -> Vec<(Candidate, Verdict)> {
        retirer.note_local(pointers, &local.iter().copied().collect());
        let candidates = retirer.candidates(pointers);
        retirer.track(&candidates, Instant::now());
        retirer.decide(candidates, acks, ackers, Instant::now())
    }

    fn retired(decided: &[(Candidate, Verdict)]) -> Vec<ContainerId> {
        decided
            .iter()
            .map(|(candidate, _)| candidate.container)
            .collect()
    }

    #[test]
    fn retires_once_every_acker_acked_the_container_away() {
        let (one, two) = (machine("one", 1), machine("two", 2));
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acked_away(&[&one, &two]),
            &ackers(&[&one, &two]),
            &[id("a"), id("b")],
        );
        let [(candidate, verdict)] = decided.as_slice() else {
            panic!("expected one retirement, got {decided:?}");
        };
        assert_eq!(candidate.container, id("a"));
        assert_eq!(*verdict, Verdict::Acked { machines: 2 });
    }

    #[test]
    fn one_up_machine_without_an_ack_holds_the_container() {
        let (one, two) = (machine("one", 1), machine("two", 2));
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acked_away(&[&one]),
            &ackers(&[&one, &two]),
            &[id("a")],
        );
        assert!(decided.is_empty());
    }

    #[test]
    fn an_ack_that_still_routes_the_container_holds_it() {
        let one = machine("one", 1);
        let acks = Acks::from([((one.id, service()), acked("d2", &[id("a"), id("b")]))]);
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acks,
            &ackers(&[&one]),
            &[id("a")],
        );
        assert!(decided.is_empty());
    }

    #[test]
    fn an_ack_for_an_older_deploy_holds_the_container() {
        let one = machine("one", 1);
        let acks = Acks::from([((one.id, service()), acked("d1", &[id("b")]))]);
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acks,
            &ackers(&[&one]),
            &[id("a")],
        );
        assert!(decided.is_empty());
    }

    #[test]
    fn down_machines_need_not_ack_and_suspect_ones_must() {
        let (local, down, suspect) = (
            machine("local", 1),
            machine("down", 2),
            machine("suspect", 3),
        );
        let states = BTreeMap::from([
            (down.management_address(), MembershipEvidence::Down),
            (suspect.management_address(), MembershipEvidence::Suspect),
        ]);
        let required = required_ackers(
            complete(vec![local.clone(), down.clone(), suspect.clone()]),
            &local.id,
            Some(&states),
        );
        assert_eq!(
            required,
            Membership {
                down: BTreeSet::from([down.id]),
                ..ackers(&[&local, &suspect])
            }
        );
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acked_away(&[&local, &suspect]),
            &required,
            &[id("a")],
        );
        assert_eq!(retired(&decided), vec![id("a")]);
    }

    #[test]
    fn without_a_membership_sample_every_machine_must_ack() {
        let (local, other) = (machine("local", 1), machine("other", 2));
        let required = required_ackers(
            complete(vec![local.clone(), other.clone()]),
            &local.id,
            None,
        );
        assert_eq!(required, ackers(&[&local, &other]));
    }

    #[test]
    fn a_partly_replicated_machine_row_holds_retirement() {
        let (local, other) = (machine("local", 1), machine("other", 2));
        let required = required_ackers(
            ReplicatedObservations {
                observations: vec![local.clone()],
                incomplete_ids: vec![other.id],
            },
            &local.id,
            Some(&BTreeMap::new()),
        );
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acked_away(&[&local]),
            &required,
            &[id("a")],
        );
        assert!(decided.is_empty());
    }

    #[test]
    fn a_machine_the_membership_sample_omits_must_ack() {
        let (local, fresh) = (machine("local", 1), machine("fresh", 2));
        let required = required_ackers(
            complete(vec![local.clone(), fresh.clone()]),
            &local.id,
            Some(&BTreeMap::new()),
        );
        assert_eq!(required, ackers(&[&local, &fresh]));
    }

    #[tokio::test(start_paused = true)]
    async fn the_cap_names_never_acked_machines_while_rows_are_incomplete() {
        let (local, slow, partial) = (
            machine("local", 1),
            machine("slow", 2),
            machine("partial", 3),
        );
        let required = required_ackers(
            ReplicatedObservations {
                observations: vec![local.clone(), slow.clone()],
                incomplete_ids: vec![partial.id],
            },
            &local.id,
            Some(&BTreeMap::new()),
        );
        let mut retirer = Retirer::default();
        let acks = acked_away(&[&local]);
        assert!(decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]).is_empty());
        tokio::time::advance(FORCE_AFTER).await;
        let decided = decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]);
        assert_eq!(
            decided
                .into_iter()
                .map(|(_, verdict)| verdict)
                .collect::<Vec<_>>(),
            vec![Verdict::Forced {
                unacked: vec![slow.name.to_string(), partial.id.as_str().to_owned()]
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_cap_says_the_machine_list_was_never_read() {
        let mut retirer = Retirer::default();
        assert!(
            decide_now(
                &mut retirer,
                &pointers(),
                &Acks::new(),
                &known(Ackers::Unread),
                &[id("a")]
            )
            .is_empty()
        );
        tokio::time::advance(FORCE_AFTER).await;
        let decided = decide_now(
            &mut retirer,
            &pointers(),
            &Acks::new(),
            &known(Ackers::Unread),
            &[id("a")],
        );
        let [(_, Verdict::Forced { unacked })] = decided.as_slice() else {
            panic!("expected one forced retirement, got {decided:?}");
        };
        assert!(!unacked.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn forces_the_stop_at_the_cap_and_names_the_machines_that_never_acked() {
        let (one, two) = (machine("one", 1), machine("two", 2));
        let mut retirer = Retirer::default();
        let acks = acked_away(&[&one]);
        let required = ackers(&[&one, &two]);
        assert!(decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]).is_empty());
        tokio::time::advance(FORCE_AFTER - Duration::from_secs(1)).await;
        assert!(decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]).is_empty());
        tokio::time::advance(Duration::from_secs(1)).await;
        let decided = decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]);
        assert_eq!(
            decided
                .into_iter()
                .map(|(_, verdict)| verdict)
                .collect::<Vec<_>>(),
            vec![Verdict::Forced {
                unacked: vec![two.name.to_string()]
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_cap_counts_rounds_whose_ack_read_failed() {
        let (one, two) = (machine("one", 1), machine("two", 2));
        let mut retirer = Retirer::default();
        retirer.note_local(&pointers(), &BTreeSet::from([id("a")]));
        let candidates = retirer.candidates(&pointers());
        retirer.track(&candidates, Instant::now());
        tokio::time::advance(FORCE_AFTER).await;
        let decided = decide_now(
            &mut retirer,
            &pointers(),
            &acked_away(&[&one]),
            &ackers(&[&one, &two]),
            &[id("a")],
        );
        assert_eq!(
            decided
                .into_iter()
                .map(|(_, verdict)| verdict)
                .collect::<Vec<_>>(),
            vec![Verdict::Forced {
                unacked: vec![two.name.to_string()]
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_newer_deploy_still_retiring_the_container_keeps_its_cap_clock() {
        let (one, two) = (machine("one", 1), machine("two", 2));
        let mut retirer = Retirer::default();
        let required = ackers(&[&one, &two]);
        assert!(
            decide_now(
                &mut retirer,
                &pointers(),
                &Acks::new(),
                &required,
                &[id("a")]
            )
            .is_empty()
        );
        tokio::time::advance(FORCE_AFTER).await;
        let redeployed = ServingPointers::from_iter([(
            service(),
            ServingPointer {
                deploy: deploy("d3"),
                containers: vec![id("c")],
                previous: Some(deploy("d2")),
                retiring: vec![id("a"), id("b")],
                deciding: None,
            },
        )]);
        let decided = decide_now(
            &mut retirer,
            &redeployed,
            &Acks::new(),
            &required,
            &[id("a"), id("b")],
        );
        assert_eq!(retired(&decided), vec![id("a")]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pointed_container_is_never_retired_even_past_the_cap() {
        let one = machine("one", 1);
        let pointers = ServingPointers::from_iter([(
            service(),
            ServingPointer {
                deploy: deploy("d2"),
                containers: vec![id("b")],
                previous: None,
                retiring: vec![id("b")],
                deciding: None,
            },
        )]);
        let mut retirer = Retirer::default();
        let acks = acked_away(&[&one]);
        assert!(
            decide_now(&mut retirer, &pointers, &acks, &ackers(&[&one]), &[id("b")]).is_empty()
        );
        tokio::time::advance(FORCE_AFTER).await;
        assert!(
            decide_now(&mut retirer, &pointers, &acks, &ackers(&[&one]), &[id("b")]).is_empty()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_booting_container_no_pointer_names_is_never_retired() {
        let one = machine("one", 1);
        let mut retirer = Retirer::default();
        let acks = acked_away(&[&one]);
        let local = [id("a"), id("b"), id("c")];
        let decided = decide_now(&mut retirer, &pointers(), &acks, &ackers(&[&one]), &local);
        assert_eq!(retired(&decided), vec![id("a")]);
        tokio::time::advance(FORCE_AFTER).await;
        retirer.in_flight.clear();
        let decided = decide_now(&mut retirer, &pointers(), &acks, &ackers(&[&one]), &local);
        assert_eq!(retired(&decided), vec![id("a")]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retiring_id_with_no_local_container_is_skipped() {
        let one = machine("one", 1);
        let mut retirer = Retirer::default();
        let acks = acked_away(&[&one]);
        assert!(decide_now(&mut retirer, &pointers(), &acks, &ackers(&[&one]), &[]).is_empty());
        tokio::time::advance(FORCE_AFTER).await;
        assert!(decide_now(&mut retirer, &pointers(), &acks, &ackers(&[&one]), &[]).is_empty());
        assert!(retirer.unknown(&pointers()).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_machine_missing_from_the_view_that_still_routes_the_container_holds_it_until_the_cap()
     {
        let (local, joiner) = (machine("local", 1), machine("joiner", 3));
        let mut acks = acked_away(&[&local]);
        acks.insert((joiner.id, service()), acked("d1", &[id("a")]));
        let required = ackers(&[&local]);
        let mut retirer = Retirer::default();
        assert!(decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]).is_empty());
        tokio::time::advance(FORCE_AFTER - Duration::from_secs(1)).await;
        assert!(decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]).is_empty());
        tokio::time::advance(Duration::from_secs(1)).await;
        let decided = decide_now(&mut retirer, &pointers(), &acks, &required, &[id("a")]);
        assert_eq!(
            decided
                .into_iter()
                .map(|(_, verdict)| verdict)
                .collect::<Vec<_>>(),
            vec![Verdict::Forced {
                unacked: vec![joiner.id.as_str().to_owned()]
            }]
        );
    }

    #[test]
    fn a_down_machine_missing_from_the_view_does_not_hold_the_container() {
        let (local, gone) = (machine("local", 1), machine("gone", 3));
        let mut acks = acked_away(&[&local]);
        acks.insert((gone.id, service()), acked("d1", &[id("a")]));
        let membership = Membership {
            down: BTreeSet::from([gone.id]),
            ..ackers(&[&local])
        };
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acks,
            &membership,
            &[id("a")],
        );
        assert_eq!(retired(&decided), vec![id("a")]);
    }

    #[test]
    fn an_ack_that_routes_none_of_the_pointers_containers_holds_the_container() {
        let one = machine("one", 1);
        let acks = Acks::from([((one.id, service()), acked("d2", &[]))]);
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers(),
            &acks,
            &ackers(&[&one]),
            &[id("a")],
        );
        assert!(decided.is_empty());
    }

    #[test]
    fn an_empty_ack_counts_when_the_pointer_serves_nothing() {
        let one = machine("one", 1);
        let pointers = ServingPointers::from_iter([(
            service(),
            ServingPointer {
                deploy: deploy("d2"),
                containers: Vec::new(),
                previous: Some(deploy("d1")),
                retiring: vec![id("a")],
                deciding: None,
            },
        )]);
        let acks = Acks::from([((one.id, service()), acked("d2", &[]))]);
        let decided = decide_now(
            &mut Retirer::default(),
            &pointers,
            &acks,
            &ackers(&[&one]),
            &[id("a")],
        );
        assert_eq!(retired(&decided), vec![id("a")]);
    }

    type Hook = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

    #[derive(Clone)]
    struct FakeHost {
        local: Arc<Mutex<BTreeSet<ContainerId>>>,
        ackers: Membership,
        retired: Arc<Mutex<Vec<ContainerId>>>,
        listings: Arc<Mutex<usize>>,
        listing_fails: Arc<Mutex<bool>>,
        marked: Arc<Mutex<Vec<ContainerId>>>,
        unmarked: Arc<Mutex<Vec<ContainerId>>>,
        removals: Arc<Mutex<usize>>,
        remove_fails: bool,
        on_membership: Hook,
        on_drain: Hook,
    }

    impl FakeHost {
        fn new(local: &[ContainerId], ackers: Membership) -> Self {
            Self {
                local: Arc::new(Mutex::new(local.iter().copied().collect())),
                ackers,
                retired: Arc::default(),
                listings: Arc::default(),
                listing_fails: Arc::default(),
                marked: Arc::default(),
                unmarked: Arc::default(),
                removals: Arc::default(),
                remove_fails: false,
                on_membership: Arc::default(),
                on_drain: Arc::default(),
            }
        }
    }

    fn hook(run: impl FnOnce() + Send + 'static) -> Hook {
        Arc::new(Mutex::new(Some(Box::new(run))))
    }

    fn run_hook(hook: &Hook) {
        let hook = hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    struct FakeWithdrawal {
        unmarked: Arc<Mutex<Vec<ContainerId>>>,
        container: ContainerId,
    }

    impl Drop for FakeWithdrawal {
        fn drop(&mut self) {
            self.unmarked.lock().unwrap().push(self.container);
        }
    }

    impl RetireHost for FakeHost {
        async fn local_containers(&self) -> io::Result<BTreeSet<ContainerId>> {
            *self.listings.lock().unwrap() += 1;
            if *self.listing_fails.lock().unwrap() {
                return Err(io::Error::other("Docker is down"));
            }
            Ok(self.local.lock().unwrap().clone())
        }

        async fn membership(&self) -> Membership {
            run_hook(&self.on_membership);
            self.ackers.clone()
        }

        type Withdrawal = FakeWithdrawal;

        fn withdraw(&self, container: ContainerId) -> FakeWithdrawal {
            self.marked.lock().unwrap().push(container);
            FakeWithdrawal {
                unmarked: self.unmarked.clone(),
                container,
            }
        }

        async fn drain(&self) {
            run_hook(&self.on_drain);
        }

        async fn remove(
            &self,
            container: ContainerId,
            still_retired: impl Future<Output = Result<bool, CorrosionError>> + Send + 'static,
        ) -> Outcome {
            match still_retired.await {
                Ok(true) => {}
                Ok(false) => return Outcome::Kept,
                Err(error) => return Outcome::Failed(io::Error::other(error)),
            }
            *self.removals.lock().unwrap() += 1;
            if self.remove_fails {
                return Outcome::Failed(io::Error::other("Docker refused the stop"));
            }
            if !self.local.lock().unwrap().remove(&container) {
                return Outcome::Failed(io::Error::from(io::ErrorKind::NotFound));
            }
            self.retired.lock().unwrap().push(container);
            Outcome::Retired
        }
    }

    #[tokio::test]
    async fn the_loop_stops_only_its_own_container_once_acked_then_goes_quiet() {
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let (local, peer) = (machine("local", 1), machine("peer", 2));
        cluster.write_serving_pointer(
            "serving/app/api",
            &serde_json::to_string(&ServingPointer {
                deploy: deploy("d2"),
                containers: vec![id("b")],
                previous: Some(deploy("d1")),
                retiring: vec![id("a"), id("c")],
                deciding: None,
            })
            .unwrap(),
        );
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local, &peer]));
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(run(host.clone(), store.clone(), shutdown.clone()));

        for machine in [&local, &peer] {
            store
                .publish_ingress_routes(
                    &machine.id,
                    &BTreeMap::from([(
                        service(),
                        IngressRouteChange::Upsert(ack("d2", &[id("b")])),
                    )]),
                )
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while host.retired.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let listings = *host.listings.lock().unwrap();
        cluster.write_serving_pointer(
            "serving/app/other",
            &serde_json::to_string(&ServingPointer {
                deploy: deploy("d3"),
                containers: vec![id("b")],
                previous: None,
                retiring: Vec::new(),
                deciding: None,
            })
            .unwrap(),
        );
        tokio::time::sleep(Duration::from_millis(600)).await;

        assert_eq!(*host.retired.lock().unwrap(), vec![id("a")]);
        assert_eq!(*host.local.lock().unwrap(), BTreeSet::from([id("b")]));
        assert_eq!(*host.listings.lock().unwrap(), listings);
        shutdown.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_round_reads_pointers_and_acks_through_corrosion_column_names() {
        let db = corrosion_db();
        let local = machine("local", 1);
        db.execute(
            "INSERT INTO cluster (key, value) VALUES (?, ?)",
            rusqlite::params![
                ServingPointer::key(&service()),
                serde_json::to_string(&pointers().get(&service()).unwrap()).unwrap()
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO ingress_routes (machine_id, service_id, deploy, containers) VALUES (?, ?, ?, ?)",
            rusqlite::params![
                local.id.as_str(),
                service().to_string(),
                "d2",
                serde_json::to_string(&[id("b")]).unwrap()
            ],
        )
        .unwrap();
        let (store, server) = identity_store(db).await;
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local]));
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}

        assert_eq!(*host.retired.lock().unwrap(), vec![id("a")]);
        server.abort();
    }

    fn held_until(until: SystemTime) -> ServingPointer {
        let mut row = pointers().get(&service()).cloned().unwrap();
        row.deciding = Some(Hold {
            deploy: deploy("d3"),
            until_unix_seconds: until.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        });
        row
    }

    async fn acked_store_holding(
        row: &ServingPointer,
        local: &Machine,
    ) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
        let db = corrosion_db();
        insert_pointer(&db, row);
        db.execute(
            "INSERT INTO ingress_routes (machine_id, service_id, deploy, containers) VALUES (?, ?, ?, ?)",
            rusqlite::params![
                local.id.as_str(),
                service().to_string(),
                "d2",
                serde_json::to_string(&[id("b")]).unwrap()
            ],
        )
        .unwrap();
        identity_store(db).await
    }

    #[tokio::test]
    async fn a_copy_an_undecided_deploy_stopped_is_never_retired() {
        let local = machine("local", 1);
        let row = held_until(SystemTime::now() + Duration::from_secs(3600));
        let (store, server) = acked_store_holding(&row, &local).await;
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local]));
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}

        assert_holds_a(&host);
        server.abort();
    }

    #[tokio::test]
    async fn a_copy_whose_deploy_died_holding_it_is_retired_once_the_hold_lapses() {
        let local = machine("local", 1);
        let row = held_until(SystemTime::now() - Duration::from_secs(1));
        let (store, server) = acked_store_holding(&row, &local).await;
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local]));
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}

        assert_eq!(*host.retired.lock().unwrap(), vec![id("a")]);
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_hold_older_than_the_cap_is_never_retired() {
        let one = machine("one", 1);
        let pointers = ServingPointers::from_iter([(
            service(),
            held_until(SystemTime::now() + Duration::from_secs(3600)),
        )]);
        let mut retirer = Retirer::default();
        let required = ackers(&[&one, &machine("two", 2)]);
        let acks = acked_away(&[&one]);
        assert!(decide_now(&mut retirer, &pointers, &acks, &required, &[id("a")]).is_empty());
        tokio::time::advance(FORCE_AFTER * 2).await;
        assert!(decide_now(&mut retirer, &pointers, &acks, &required, &[id("a")]).is_empty());
    }

    async fn drain_waits_out(
        host: &FakeHost,
        store: &ReplicatedStore,
        mut retirer: Retirer,
        copy: ContainerId,
    ) -> bool {
        let mut stopping = JoinSet::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while host.local.lock().unwrap().contains(&copy) {
                round(host, store, &mut retirer, &mut stopping).await;
                while stopping.join_next().await.is_some() {}
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn an_orphaned_hold_lets_a_drain_finish_across_a_daemon_restart() {
        let local = machine("local", 1);
        let until = SystemTime::now() + Duration::from_secs(2);
        let row = held_until(until);
        let (store, server) = acked_store_holding(&row, &local).await;
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local]));
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}
        assert_holds_a(&host);

        while SystemTime::now() < until + Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let restarted = Retirer::default();
        assert!(
            drain_waits_out(&host, &store, restarted, id("a")).await,
            "the drain still waits on the held copy"
        );
        assert_eq!(*host.retired.lock().unwrap(), vec![id("a")]);
        server.abort();
    }

    async fn retired_with_extra_route(
        service_id: &str,
        deploy: &str,
        containers: &str,
    ) -> Vec<ContainerId> {
        let db = corrosion_db();
        let (local, joiner) = (machine("local", 1), machine("joiner", 2));
        insert_pointer(&db, pointers().get(&service()).unwrap());
        let insert = "INSERT INTO ingress_routes (machine_id, service_id, deploy, containers) VALUES (?, ?, ?, ?)";
        db.execute(
            insert,
            rusqlite::params![
                local.id.as_str(),
                service().to_string(),
                "d2",
                serde_json::to_string(&[id("b")]).unwrap()
            ],
        )
        .unwrap();
        db.execute(
            insert,
            rusqlite::params![joiner.id.as_str(), service_id, deploy, containers],
        )
        .unwrap();
        let (store, server) = identity_store(db).await;
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local]));
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}
        server.abort();
        host.retired.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn an_unlisted_route_row_without_a_deploy_still_holds_what_it_routes() {
        let routes_a = serde_json::to_string(&[id("a")]).unwrap();
        assert_eq!(
            retired_with_extra_route("app/api", "", &routes_a).await,
            vec![]
        );
        let routes_b = serde_json::to_string(&[id("b")]).unwrap();
        assert_eq!(
            retired_with_extra_route("app/api", "", &routes_b).await,
            vec![id("a")]
        );
    }

    #[tokio::test]
    async fn an_unlisted_route_row_without_readable_containers_holds_its_service() {
        assert_eq!(
            retired_with_extra_route("app/api", "d1", "{}").await,
            vec![]
        );
        assert_eq!(
            retired_with_extra_route("app/other", "d1", "{}").await,
            vec![id("a")]
        );
    }

    async fn host_after_own_route(deploy: &str, containers: &str) -> FakeHost {
        let db = corrosion_db();
        let local = machine("local", 1);
        insert_pointer(&db, pointers().get(&service()).unwrap());
        db.execute(
            "INSERT INTO ingress_routes (machine_id, service_id, deploy, containers) VALUES (?, ?, ?, ?)",
            rusqlite::params![local.id.as_str(), service().to_string(), deploy, containers],
        )
        .unwrap();
        let (store, server) = identity_store(db).await;
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local]));
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}
        server.abort();
        host
    }

    fn assert_holds_a(host: &FakeHost) {
        assert!(host.retired.lock().unwrap().is_empty());
        assert!(host.marked.lock().unwrap().is_empty());
        assert_eq!(*host.removals.lock().unwrap(), 0);
        assert!(host.local.lock().unwrap().contains(&id("a")));
    }

    #[tokio::test]
    async fn a_listed_route_row_without_a_deploy_never_acks_away() {
        let routes_b = serde_json::to_string(&[id("b")]).unwrap();
        let acked = host_after_own_route("d2", &routes_b).await;
        assert_eq!(*acked.retired.lock().unwrap(), vec![id("a")]);

        assert_holds_a(&host_after_own_route("", &routes_b).await);
    }

    #[tokio::test]
    async fn a_listed_route_row_without_readable_containers_never_acks_away() {
        assert_holds_a(&host_after_own_route("d2", "{}").await);
    }

    fn insert_pointer(db: &rusqlite::Connection, pointer: &ServingPointer) {
        db.execute(
            "INSERT OR REPLACE INTO cluster (key, value) VALUES (?, ?)",
            rusqlite::params![
                ServingPointer::key(&service()),
                serde_json::to_string(pointer).unwrap()
            ],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn the_cap_clock_starts_while_docker_cannot_list() {
        let db = corrosion_db();
        insert_pointer(&db, pointers().get(&service()).unwrap());
        let (one, two) = (machine("one", 1), machine("two", 2));
        let (store, server) = identity_store(db).await;
        let host = FakeHost {
            listing_fails: Arc::new(Mutex::new(true)),
            ..FakeHost::new(&[id("a"), id("b")], ackers(&[&one, &two]))
        };
        let mut retirer = Retirer::default();
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut retirer, &mut stopping).await;
        assert!(stopping.is_empty());

        let first_seen = retirer.first_seen.get(&id("a")).copied();
        assert!(first_seen.is_some());
        *host.listing_fails.lock().unwrap() = false;
        round(&host, &store, &mut retirer, &mut stopping).await;
        assert_eq!(retirer.first_seen.get(&id("a")).copied(), first_seen);
        server.abort();
    }

    #[tokio::test]
    async fn absent_ids_are_forgotten_once_no_pointer_retires_them() {
        let db = corrosion_db();
        insert_pointer(&db, pointers().get(&service()).unwrap());
        let one = machine("one", 1);
        let (store, server) = identity_store(db).await;
        let host = FakeHost::new(&[id("b")], ackers(&[&one]));
        let mut retirer = Retirer::default();
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut retirer, &mut stopping).await;
        assert!(retirer.absent.contains(&id("a")));
        server.abort();

        let db = corrosion_db();
        insert_pointer(
            &db,
            &ServingPointer {
                deploy: deploy("d3"),
                containers: vec![id("b")],
                previous: Some(deploy("d2")),
                retiring: Vec::new(),
                deciding: None,
            },
        );
        let (store, server) = identity_store(db).await;
        round(&host, &store, &mut retirer, &mut stopping).await;
        assert!(retirer.absent.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn present_ids_are_forgotten_once_no_pointer_retires_them() {
        let db = corrosion_db();
        insert_pointer(&db, pointers().get(&service()).unwrap());
        let one = machine("one", 1);
        let (store, server) = identity_store(db).await;
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&one]));
        let mut retirer = Retirer::default();
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut retirer, &mut stopping).await;
        assert!(retirer.present.contains(&id("a")));
        server.abort();

        let db = corrosion_db();
        insert_pointer(
            &db,
            &ServingPointer {
                deploy: deploy("d3"),
                containers: vec![id("b")],
                previous: Some(deploy("d2")),
                retiring: Vec::new(),
                deciding: None,
            },
        );
        let (store, server) = identity_store(db).await;
        round(&host, &store, &mut retirer, &mut stopping).await;
        assert!(retirer.present.is_empty());
        server.abort();
    }

    async fn acked_cluster(machines: &[&Machine]) -> (Arc<FakeCluster>, ReplicatedStore) {
        let cluster = Arc::new(FakeCluster::start());
        cluster.write_serving_pointer(
            &ServingPointer::key(&service()),
            &serde_json::to_string(pointers().get(&service()).unwrap()).unwrap(),
        );
        let store = cluster.store();
        for machine in machines {
            store
                .publish_ingress_routes(
                    &machine.id,
                    &BTreeMap::from([(
                        service(),
                        IngressRouteChange::Upsert(ack("d2", &[id("b")])),
                    )]),
                )
                .await
                .unwrap();
        }
        (cluster, store)
    }

    fn roll_back_to_a(cluster: &Arc<FakeCluster>) -> Hook {
        point_at_a(cluster, vec![id("b")])
    }

    fn point_at_a_still_retiring_it(cluster: &Arc<FakeCluster>) -> Hook {
        point_at_a(cluster, vec![id("a"), id("b")])
    }

    fn point_at_a(cluster: &Arc<FakeCluster>, retiring: Vec<ContainerId>) -> Hook {
        let cluster = cluster.clone();
        hook(move || {
            cluster.write_serving_pointer(
                &ServingPointer::key(&service()),
                &serde_json::to_string(&ServingPointer {
                    deploy: deploy("d3"),
                    containers: vec![id("a")],
                    previous: Some(deploy("d2")),
                    retiring,
                    deciding: None,
                })
                .unwrap(),
            );
        })
    }

    /// A real Local Machine on fake Docker, removing as `DaemonHost` does.
    #[derive(Clone)]
    struct MachineHost {
        machine: crate::machine::LocalMachine,
        containers: crate::docker::ContainerRuntime,
        local: BTreeSet<ContainerId>,
        ackers: Membership,
    }

    impl RetireHost for MachineHost {
        async fn local_containers(&self) -> io::Result<BTreeSet<ContainerId>> {
            Ok(self.local.clone())
        }

        async fn membership(&self) -> Membership {
            self.ackers.clone()
        }

        type Withdrawal = crate::docker::Withdrawal;

        fn withdraw(&self, container: ContainerId) -> Self::Withdrawal {
            self.containers.withdraw(container)
        }

        async fn drain(&self) {}

        async fn remove(
            &self,
            container: ContainerId,
            still_retired: impl Future<Output = Result<bool, CorrosionError>> + Send + 'static,
        ) -> Outcome {
            remove_drained(&self.machine, container, still_retired).await
        }
    }

    struct RealMachine {
        owner: crate::machine::RecordOwner,
        host: MachineHost,
        named: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
        requests: Arc<Mutex<Vec<(axum::http::Method, String)>>>,
        inspect_hold: Arc<Mutex<Option<Arc<tokio::sync::Barrier>>>>,
        on_stop: Arc<Mutex<Option<crate::docker::test_support::StopHook>>>,
        data_dir: std::path::PathBuf,
        a: ContainerId,
        b: ContainerId,
    }

    impl RealMachine {
        /// Running Containers A and B on an enrolled Local Machine, with A retired
        /// by `d2` and every acker routing B.
        async fn start(cluster: &FakeCluster) -> Self {
            use crate::docker::test_support::{FakeDocker, fake_runtime_with, spec_with_sources};
            let data_dir =
                std::env::temp_dir().join(format!("ployzd-retire-{}", MachineId::random()));
            let mut machine_store = crate::machine::LocalMachineStore::open(&data_dir).unwrap();
            machine_store
                .enroll(ployz_core::EnrollRequest {
                    role: ployz_core::EnrollAs::Founder {
                        initial_policy: Default::default(),
                        name: MachineName::parse("local").unwrap(),
                        cluster_network: "10.210.0.0/16".parse().unwrap(),
                        public_ip: None,
                        advertised_endpoints: vec![AdvertisedEndpoint(
                            "192.0.2.1:51820".parse().unwrap(),
                        )],
                    },
                    wireguard_mtu: None,
                })
                .unwrap();
            let owner = crate::machine::RecordOwner::spawn(machine_store).unwrap();
            let (containers, fake) = fake_runtime_with(FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                ..Default::default()
            })
            .await;
            let local_machine = crate::machine::LocalMachine::new(owner.clone())
                .with_containers(Some(containers.clone()))
                .with_zpool("true");
            let named = fake.named_containers.clone().unwrap();
            let ids = |named: &Mutex<BTreeMap<String, serde_json::Value>>| -> BTreeSet<String> {
                named
                    .lock()
                    .unwrap()
                    .values()
                    .map(|container| container["Id"].as_str().unwrap().to_owned())
                    .collect()
            };
            let mut created = Vec::new();
            for key in ["a", "b"] {
                let before = ids(&named);
                let new: ployz_core::NewContainer = serde_json::from_value(serde_json::json!({
                    "slot": {"namespace": "app", "kind": "service_container", "creation_key": key},
                    "resolved_spec": spec_with_sources(vec![]),
                    "answer": "started",
                }))
                .unwrap();
                local_machine
                    .ensure_container(ployz_core::ContainerWant::Running {
                        container: ployz_core::RunTarget::New(new),
                    })
                    .await
                    .unwrap();
                let id = ids(&named).difference(&before).next().unwrap().clone();
                created.push(ContainerId::parse(id).unwrap());
            }
            let [a, b] = created[..] else {
                unreachable!("two Containers were created")
            };
            cluster.write_serving_pointer(
                &ServingPointer::key(&service()),
                &serde_json::to_string(&ServingPointer {
                    deploy: deploy("d2"),
                    containers: vec![b],
                    previous: Some(deploy("d1")),
                    retiring: vec![a],
                    deciding: None,
                })
                .unwrap(),
            );
            let local = machine("local", 1);
            cluster
                .store()
                .publish_ingress_routes(
                    &local.id,
                    &BTreeMap::from([(service(), IngressRouteChange::Upsert(ack("d2", &[b])))]),
                )
                .await
                .unwrap();
            Self {
                owner,
                host: MachineHost {
                    machine: local_machine,
                    containers,
                    local: BTreeSet::from([a, b]),
                    ackers: ackers(&[&local]),
                },
                named,
                requests: fake.requests.clone(),
                inspect_hold: fake.inspect_hold.clone(),
                on_stop: fake.on_stop.clone(),
                data_dir,
                a,
                b,
            }
        }

        fn exists(&self, container: ContainerId) -> bool {
            let id = container.to_string();
            self.named
                .lock()
                .unwrap()
                .values()
                .any(|named| named["Id"] == id.as_str())
        }

        fn stopped(&self, container: ContainerId) -> bool {
            let stop = format!("/containers/{container}/stop");
            self.requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path)| *path == stop)
        }

        fn withdrawn_at_stop(&self) -> Arc<Mutex<Vec<StopRequest>>> {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let record = Arc::clone(&seen);
            let containers = self.host.containers.clone();
            *self.on_stop.lock().unwrap() = Some(Box::new(move |id: &str| {
                let id = ContainerId::parse(id).unwrap();
                let withdrawn = containers.stopping_ids_for_test().contains(&id);
                record.lock().unwrap().push(StopRequest {
                    container: id,
                    withdrawn,
                });
            }));
            seen
        }

        fn exit(&self, container: ContainerId) {
            let id = container.to_string();
            let mut named = self.named.lock().unwrap();
            let named = named
                .values_mut()
                .find(|named| named["Id"] == id.as_str())
                .unwrap();
            named["State"] = serde_json::json!({"Status": "exited", "ExitCode": 0});
        }
    }

    #[derive(Debug, PartialEq)]
    struct StopRequest {
        container: ContainerId,
        withdrawn: bool,
    }

    impl Drop for RealMachine {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.data_dir);
        }
    }

    async fn next_outcome(stopping: &mut JoinSet<(ContainerId, Verdict, Outcome)>) -> Outcome {
        let (_, _, outcome) = tokio::time::timeout(Duration::from_secs(10), stopping.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        outcome
    }

    #[tokio::test]
    async fn a_repoint_while_the_stop_waits_for_admission_keeps_the_container() {
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let machine = RealMachine::start(&cluster).await;
        let admission = machine.owner.admission_lock();
        // An image transfer holds the shared admission turn, as pull_image_from_machine does.
        let transfer = admission.clone().read_owned().await;
        let mut stopping = JoinSet::new();
        round(
            &machine.host,
            &store,
            &mut Retirer::default(),
            &mut stopping,
        )
        .await;
        assert_eq!(stopping.len(), 1);
        // A queued exclusive turn takes the free permits, so a shared one no longer fits.
        while admission.try_read().is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!machine.stopped(machine.a), "the stop waits for admission");

        cluster.write_serving_pointer(
            &ServingPointer::key(&service()),
            &serde_json::to_string(&ServingPointer {
                deploy: deploy("d3"),
                containers: vec![machine.a],
                previous: Some(deploy("d2")),
                retiring: vec![machine.b],
                deciding: None,
            })
            .unwrap(),
        );
        drop(transfer);

        assert!(matches!(next_outcome(&mut stopping).await, Outcome::Kept));
        assert!(!machine.stopped(machine.a), "d3 serves A");
        assert!(machine.exists(machine.a), "d3 serves A");
        assert!(machine.host.containers.stopping_ids_for_test().is_empty());
    }

    #[tokio::test]
    async fn a_kept_retire_leaves_a_container_withdrawn_while_an_explicit_stop_drains_it() {
        use ployz_core::{
            ContainerLocator, ContainerWant, DEPLOYMENT_ID_LABEL, Drain, StopOptions,
            service_containers, serving_containers,
        };
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let machine = RealMachine::start(&cluster).await;
        {
            let mut named = machine.named.lock().unwrap();
            for (id, ip, deploy) in [
                (machine.a, "10.210.0.2", "d1"),
                (machine.b, "10.210.0.3", "d2"),
            ] {
                let row = named
                    .values_mut()
                    .find(|row| row["Id"] == id.as_str())
                    .unwrap();
                row["NetworkSettings"] =
                    serde_json::json!({"Networks": {"ployz": {"IPAddress": ip}}});
                row.pointer_mut("/Config/Labels")
                    .and_then(serde_json::Value::as_object_mut)
                    .unwrap()
                    .insert(DEPLOYMENT_ID_LABEL.to_owned(), serde_json::json!(deploy));
            }
        }
        let machine_id = machine.owner.record().id();
        let observe = |container| {
            let containers = machine.host.containers.clone();
            async move {
                containers
                    .inspect_managed(&container, &machine_id)
                    .await
                    .unwrap()
            }
        };
        let transfer = machine.owner.admission_lock().read_owned().await;
        let mut retiring = JoinSet::new();
        round(
            &machine.host,
            &store,
            &mut Retirer::default(),
            &mut retiring,
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !machine
                .host
                .containers
                .stopping_ids_for_test()
                .contains(&machine.a)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let ensure = Arc::new(tokio::sync::Barrier::new(2));
        let stop = Arc::new(tokio::sync::Barrier::new(2));
        *machine.inspect_hold.lock().unwrap() = Some(ensure.clone());
        let explicit_stop = tokio::spawn({
            let local = machine.host.machine.clone();
            let a = machine.a;
            async move {
                local
                    .ensure_container(ContainerWant::Stopped {
                        container: ContainerLocator::Id(a),
                        drain: Drain::DEFAULT,
                        stop: StopOptions::default(),
                    })
                    .await
            }
        });
        ensure.wait().await;
        *machine.inspect_hold.lock().unwrap() = Some(stop.clone());
        ensure.wait().await;
        stop.wait().await;
        stop.wait().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!explicit_stop.is_finished(), "the explicit stop drains A");

        let pointer = ServingPointer {
            deploy: deploy("d3"),
            containers: vec![machine.a],
            previous: Some(deploy("d2")),
            retiring: vec![machine.b],
            deciding: None,
        };
        cluster.write_serving_pointer(
            &ServingPointer::key(&service()),
            &serde_json::to_string(&pointer).unwrap(),
        );
        drop(transfer);
        assert!(matches!(next_outcome(&mut retiring).await, Outcome::Kept));
        assert!(
            !explicit_stop.is_finished(),
            "the explicit stop still drains A"
        );

        assert_eq!(
            machine.host.containers.stopping_ids_for_test(),
            BTreeSet::from([machine.a])
        );
        let containers = service_containers([observe(machine.a).await, observe(machine.b).await]);
        let routed: Vec<_> = serving_containers(&containers, |_| Some(&pointer))
            .iter()
            .map(|container| container.as_observation().container_id)
            .collect();
        assert_eq!(routed, vec![machine.b], "B serves while A drains");

        tokio::time::timeout(Duration::from_secs(5), explicit_stop)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(machine.stopped(machine.a));
        assert!(machine.host.containers.stopping_ids_for_test().is_empty());
    }

    async fn repoint_during_the_last_inspect_before_the_stop(retiring_a: bool) {
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let machine = RealMachine::start(&cluster).await;
        let hold = Arc::new(tokio::sync::Barrier::new(2));
        *machine.inspect_hold.lock().unwrap() = Some(hold.clone());
        let mut stopping = JoinSet::new();
        round(
            &machine.host,
            &store,
            &mut Retirer::default(),
            &mut stopping,
        )
        .await;
        assert_eq!(stopping.len(), 1);
        hold.wait().await;

        let retiring = if retiring_a {
            vec![machine.a, machine.b]
        } else {
            vec![machine.b]
        };
        cluster.write_serving_pointer(
            &ServingPointer::key(&service()),
            &serde_json::to_string(&ServingPointer {
                deploy: deploy("d3"),
                containers: vec![machine.a],
                previous: Some(deploy("d2")),
                retiring,
                deciding: None,
            })
            .unwrap(),
        );
        hold.wait().await;

        assert!(matches!(next_outcome(&mut stopping).await, Outcome::Kept));
        assert!(!machine.stopped(machine.a), "d3 serves A");
        assert!(machine.exists(machine.a), "d3 serves A");
        assert!(machine.host.containers.stopping_ids_for_test().is_empty());
    }

    #[tokio::test]
    async fn a_repoint_during_the_last_inspect_before_the_stop_keeps_the_container() {
        repoint_during_the_last_inspect_before_the_stop(false).await;
    }

    #[tokio::test]
    async fn a_pointer_naming_the_container_during_the_last_inspect_keeps_it_while_still_retiring_it()
     {
        repoint_during_the_last_inspect_before_the_stop(true).await;
    }

    #[tokio::test]
    async fn a_newer_pointer_whose_ack_routes_the_container_keeps_it() {
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let machine = RealMachine::start(&cluster).await;
        let hold = Arc::new(tokio::sync::Barrier::new(2));
        *machine.inspect_hold.lock().unwrap() = Some(hold.clone());
        let mut stopping = JoinSet::new();
        round(
            &machine.host,
            &store,
            &mut Retirer::default(),
            &mut stopping,
        )
        .await;
        hold.wait().await;

        cluster.write_serving_pointer(
            &ServingPointer::key(&service()),
            &serde_json::to_string(&ServingPointer {
                deploy: deploy("d3"),
                containers: vec![id("c")],
                previous: Some(deploy("d2")),
                retiring: vec![machine.a, machine.b],
                deciding: None,
            })
            .unwrap(),
        );
        let Ackers::Known(required) = &machine.host.ackers.ackers else {
            unreachable!("RealMachine knows its ackers")
        };
        let acker = *required.keys().next().unwrap();
        store
            .publish_ingress_routes(
                &acker,
                &BTreeMap::from([(
                    service(),
                    IngressRouteChange::Upsert(ack("d3", &[machine.a, machine.b])),
                )]),
            )
            .await
            .unwrap();
        hold.wait().await;

        assert!(matches!(next_outcome(&mut stopping).await, Outcome::Kept));
        assert!(!machine.stopped(machine.a), "the d3 ack routes A");
        assert!(machine.exists(machine.a), "the d3 ack routes A");
        assert!(machine.host.containers.stopping_ids_for_test().is_empty());
    }

    #[tokio::test]
    async fn a_pointer_read_slower_than_a_second_still_lets_a_forced_retire_finish() {
        use axum::{Router, extract::State, routing::post};
        async fn slow_query(State(body): State<Arc<String>>) -> String {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            (*body).clone()
        }
        let encoded = serde_json::to_string(pointers().get(&service()).unwrap()).unwrap();
        let body = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"columns": ["serving_key", "value"]}),
            serde_json::json!({"row": [1, ["serving/app/api", encoded]]}),
            serde_json::json!({"eoq": {"time": 0.0}}),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/queries", post(slow_query))
            .with_state(Arc::new(body));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = ReplicatedStore::new(
            crate::corrosion::ApiClient::http1(address, &"a".repeat(64)).unwrap(),
        );
        let local = machine("local", 1);
        let mut retirer = Retirer::default();
        retirer.note_local(&pointers(), &BTreeSet::from([id("a")]));
        retirer.first_seen.insert(
            id("a"),
            Instant::now() - FORCE_AFTER - Duration::from_secs(1),
        );
        let decided = retirer.decide(
            retirer.candidates(&pointers()),
            &Acks::new(),
            &ackers(&[&local]),
            Instant::now(),
        );
        let [(candidate, Verdict::Forced { .. })] = &decided[..] else {
            panic!("expected one forced decision, got {decided:?}")
        };
        let host = FakeHost::new(&[id("a"), id("b")], ackers(&[&local]));

        let outcome = super::retire(&host, &store, candidate).await;
        server.abort();

        assert!(matches!(outcome, Outcome::Retired));
        assert_eq!(*host.retired.lock().unwrap(), vec![id("a")]);
    }

    #[tokio::test]
    async fn a_retired_container_is_withdrawn_when_docker_receives_its_stop() {
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let machine = RealMachine::start(&cluster).await;
        let withdrawn_at_stop = machine.withdrawn_at_stop();
        let mut stopping = JoinSet::new();
        round(
            &machine.host,
            &store,
            &mut Retirer::default(),
            &mut stopping,
        )
        .await;

        assert!(matches!(
            next_outcome(&mut stopping).await,
            Outcome::Retired
        ));
        assert_eq!(
            *withdrawn_at_stop.lock().unwrap(),
            vec![StopRequest {
                container: machine.a,
                withdrawn: true,
            }]
        );
        assert!(machine.host.containers.stopping_ids_for_test().is_empty());
    }

    #[tokio::test]
    async fn an_explicitly_stopped_container_is_withdrawn_when_docker_receives_its_stop() {
        use ployz_core::{ContainerLocator, ContainerWant, Drain, StopOptions};
        let cluster = FakeCluster::start();
        let machine = RealMachine::start(&cluster).await;
        let withdrawn_at_stop = machine.withdrawn_at_stop();

        machine
            .host
            .machine
            .ensure_container(ContainerWant::Stopped {
                container: ContainerLocator::Id(machine.a),
                drain: Drain::NONE,
                stop: StopOptions::default(),
            })
            .await
            .unwrap();

        assert_eq!(
            *withdrawn_at_stop.lock().unwrap(),
            vec![StopRequest {
                container: machine.a,
                withdrawn: true,
            }]
        );
        assert!(machine.host.containers.stopping_ids_for_test().is_empty());
    }

    #[tokio::test]
    async fn retiring_an_exited_container_clears_its_stopping_mark() {
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let machine = RealMachine::start(&cluster).await;
        machine.exit(machine.a);
        let mut stopping = JoinSet::new();
        round(
            &machine.host,
            &store,
            &mut Retirer::default(),
            &mut stopping,
        )
        .await;

        assert!(matches!(
            next_outcome(&mut stopping).await,
            Outcome::Retired
        ));
        assert!(!machine.exists(machine.a));
        assert!(machine.host.containers.stopping_ids_for_test().is_empty());
    }

    #[tokio::test]
    async fn a_repoint_while_the_round_decides_keeps_the_container() {
        let local = machine("local", 1);
        let (cluster, store) = acked_cluster(&[&local]).await;
        let host = FakeHost {
            on_membership: roll_back_to_a(&cluster),
            ..FakeHost::new(&[id("a"), id("b")], ackers(&[&local]))
        };
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        assert_eq!(stopping.len(), 1);
        while stopping.join_next().await.is_some() {}

        assert!(host.marked.lock().unwrap().is_empty());
        assert_eq!(*host.removals.lock().unwrap(), 0);
        assert_eq!(
            *host.local.lock().unwrap(),
            BTreeSet::from([id("a"), id("b")])
        );
    }

    #[tokio::test]
    async fn a_repoint_during_the_drain_keeps_the_container() {
        let local = machine("local", 1);
        let (cluster, store) = acked_cluster(&[&local]).await;
        let host = FakeHost {
            on_drain: roll_back_to_a(&cluster),
            ..FakeHost::new(&[id("a"), id("b")], ackers(&[&local]))
        };
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}

        assert_eq!(*host.marked.lock().unwrap(), vec![id("a")]);
        assert_eq!(*host.unmarked.lock().unwrap(), vec![id("a")]);
        assert_eq!(*host.removals.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_kept_container_is_journalled_by_its_container_field() {
        let local = machine("local", 1);
        let (cluster, store) = acked_cluster(&[&local]).await;
        let host = FakeHost {
            on_drain: roll_back_to_a(&cluster),
            ..FakeHost::new(&[id("a"), id("b")], ackers(&[&local]))
        };
        let (capture, _logging) = crate::diag::test_support::capture();
        let mut retirer = Retirer::default();
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut retirer, &mut stopping).await;
        while let Some(joined) = stopping.join_next().await {
            finish(&mut retirer, joined);
        }

        let journal = capture.text();
        let kept = journal
            .lines()
            .find(|line| line.contains("kept retire"))
            .unwrap_or_else(|| panic!("no kept line in {journal:?}"));
        assert!(kept.contains(&format!("container={}", id("a"))), "{kept}");
    }

    #[tokio::test]
    async fn a_pointer_naming_the_container_keeps_it_even_while_it_still_retires_it() {
        let local = machine("local", 1);
        let (cluster, store) = acked_cluster(&[&local]).await;
        let host = FakeHost {
            on_drain: point_at_a_still_retiring_it(&cluster),
            ..FakeHost::new(&[id("a"), id("b")], ackers(&[&local]))
        };
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}

        assert_eq!(*host.marked.lock().unwrap(), vec![id("a")]);
        assert_eq!(*host.unmarked.lock().unwrap(), vec![id("a")]);
        assert_eq!(*host.removals.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_failed_removal_clears_the_stopping_mark() {
        let local = machine("local", 1);
        let (_cluster, store) = acked_cluster(&[&local]).await;
        let host = FakeHost {
            remove_fails: true,
            ..FakeHost::new(&[id("a"), id("b")], ackers(&[&local]))
        };
        let mut stopping = JoinSet::new();
        round(&host, &store, &mut Retirer::default(), &mut stopping).await;
        while stopping.join_next().await.is_some() {}

        assert_eq!(*host.removals.lock().unwrap(), 1);
        assert_eq!(*host.unmarked.lock().unwrap(), vec![id("a")]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_retire_waits_a_pending_tick_before_the_next_attempt() {
        // A running blocking task stops the paused clock from jumping while
        // HTTP reads are in flight, so only `advance` moves it.
        let (release, held) = std::sync::mpsc::channel::<()>();
        let hold_clock = tokio::task::spawn_blocking(move || held.recv());
        let local = machine("local", 1);
        let (_cluster, store) = acked_cluster(&[&local]).await;
        let host = FakeHost {
            remove_fails: true,
            ..FakeHost::new(&[id("a"), id("b")], ackers(&[&local]))
        };
        let shutdown = CancellationToken::new();
        let start = Instant::now();
        let task = tokio::spawn(run(host.clone(), store, shutdown.clone()));
        for _ in 0..300 {
            tokio::time::advance(Duration::from_millis(100)).await;
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(2)))
                .await
                .unwrap();
        }
        let elapsed = start.elapsed();
        shutdown.cancel();
        task.await.unwrap();
        release.send(()).unwrap();
        hold_clock.await.unwrap().unwrap();

        let attempts = *host.removals.lock().unwrap();
        let bound = elapsed.as_millis() / PENDING_TICK.as_millis() + 1;
        assert!(attempts >= 2, "{attempts} attempts in {elapsed:?}");
        assert!(
            attempts as u128 <= bound,
            "{attempts} attempts in {elapsed:?}, bound {bound}"
        );
    }
}
