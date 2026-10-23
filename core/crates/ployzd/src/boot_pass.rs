//! Once per daemon start, set Out each In copy on this Machine that a Deploy
//! would make anew, or that its Service has more of than it asks for, beside the
//! newest In copies every live proxy routes. A Machine
//! powered off during a Deploy comes back with its old copy In, and proxies would
//! route it again. The pass decides only after this Machine has caught up with a
//! peer's store and its own ingress has published since, and it never stops a
//! Container.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{
    CodecError, ContainerId, ContainerKind, ContainerLocator, ContainerObservation, ContainerWant,
    CopyRotation, EnsuredContainer, Gen, InspectContainerRequest, ListMachinesRequest,
    MACHINE_API_PORT, Machine, MachineId, MachineRpcClient, QualifiedService,
    RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, RequestedServiceSpec, ResolvedServiceSpec, ServiceMode,
    SpecChange, compare_specs, op,
};
use tokio::{sync::watch, time::Instant};
use tonic::transport::Endpoint;

use crate::{
    corrosion::{
        AdminClient, Error as CorrosionError, IngressRouteRow, MachineView, ReplicatedObservations,
        ReplicatedStore, wait_for_catch_up,
    },
    ingress::PublishedRoutes,
    machine::{LocalMachine, LocalMachineError},
    route_acks::{Ackers, Membership, routed, sample_ackers},
};

/// The executor's `ACK_WAIT`, which ployzd cannot import.
pub(crate) const BOOT_PASS_WAIT: Duration = Duration::from_secs(300);
const PEER_TIMEOUT: Duration = Duration::from_secs(2);
const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);
const PEER_RETRY: Duration = Duration::from_secs(1);
/// Retries end this long before the deadline, so the Outs they decide still go.
const OUT_MARGIN: Duration = Duration::from_secs(30);

pub(crate) trait BootHost: Send + Sync {
    fn machine_id(&self) -> MachineId;
    /// One round over the peers. The first answer wins.
    fn peer_targets(&self) -> impl Future<Output = Peers> + Send;
    fn local_actor(&self) -> impl Future<Output = Result<String, CorrosionError>> + Send;
    fn catch_up(
        &self,
        target: &BTreeMap<String, i64>,
    ) -> impl Future<Output = Result<(), CorrosionError>> + Send;
    fn observe(&self) -> impl Future<Output = Result<Observed, CorrosionError>> + Send;
    /// `container` as `machine` observes it now, with the environment and Config
    /// content a store row drops. `None` means it did not answer.
    fn inspect(
        &self,
        machine: MachineId,
        container: ContainerId,
    ) -> impl Future<Output = Option<ContainerObservation>> + Send;
    fn ensure(
        &self,
        want: ContainerWant,
    ) -> impl Future<Output = Result<EnsuredContainer, LocalMachineError>> + Send;
}

#[derive(Clone)]
pub(crate) enum Peers {
    /// This Machine founded the Cluster, and its Machine list names no other
    /// Machine and has no partly replicated row. An empty list counts too. A
    /// joiner is never alone: its first read can predate its peers' rows.
    Alone,
    /// The Machine list is unread, or no peer answered.
    Silent,
    /// A peer's store versions.
    Answered(BTreeMap<String, i64>),
}

#[derive(Clone)]
pub(crate) struct Observed {
    pub(crate) containers: Vec<ContainerObservation>,
    pub(crate) routes: BTreeMap<(MachineId, QualifiedService), IngressRouteRow>,
    pub(crate) membership: Membership,
}

/// A local In copy, and the gen of the newest In siblings every live proxy routes.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Superseded {
    pub(crate) service: QualifiedService,
    pub(crate) container: ContainerId,
    pub(crate) by: Gen,
}

/// A local In copy and the other In copies of its Service that every live proxy
/// routes.
pub(crate) struct Candidate<'a> {
    copy: &'a ContainerObservation,
    /// Those of the newest gen, newer than `copy`.
    newest: Vec<&'a ContainerObservation>,
    routed: Vec<&'a ContainerObservation>,
}

impl<'a> Candidate<'a> {
    /// Every copy whose full spec the judgement reads.
    fn rows(&self) -> impl Iterator<Item = &'a ContainerObservation> + '_ {
        std::iter::once(self.copy).chain(self.routed.iter().copied())
    }
}

/// Each local In copy that has a newer In sibling every live proxy routes. A
/// Machine list that is only partly replicated, or never read, yields nothing, and
/// a copy on a Down Machine serves no one. This Machine's own acks come from `own`,
/// what its ingress published since it started, because its row in the store may
/// predate a power-off.
fn candidates<'a>(
    local: &MachineId,
    Observed {
        containers,
        routes,
        membership,
    }: &'a Observed,
    own: &BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
) -> Vec<Candidate<'a>> {
    if !matches!(&membership.ackers, Ackers::Known(proxies) if !proxies.is_empty()) {
        return Vec::new();
    }
    let serving = |copy: &&ContainerObservation| {
        copy.kind == ContainerKind::ServiceContainer
            && *copy.rotation() == CopyRotation::In
            && !membership.down.contains(&copy.machine_id)
    };
    containers
        .iter()
        .filter(serving)
        .filter(|copy| copy.machine_id == *local)
        .filter_map(|copy| {
            let service = copy.identity();
            let mut routes = routed(routes.clone(), &service);
            routes.insert(*local, own.get(&service).cloned().unwrap_or_default());
            let acked = |sibling: &ContainerId| {
                membership
                    .ackers
                    .unacked(|proxy| {
                        routes
                            .get(proxy)
                            .is_some_and(|routed| routed.contains(sibling))
                    })
                    .is_empty()
            };
            let routed = containers
                .iter()
                .filter(serving)
                .filter(|sibling| {
                    sibling.container_id != copy.container_id
                        && sibling.identity() == service
                        && acked(&sibling.container_id)
                })
                .collect::<Vec<_>>();
            let newest = routed.iter().map(|sibling| sibling.copy_gen()).max()?;
            let newest = routed
                .iter()
                .copied()
                .filter(|sibling| sibling.copy_gen() == newest && newest > copy.copy_gen())
                .collect::<Vec<_>>();
            (!newest.is_empty()).then_some(Candidate {
                copy,
                newest,
                routed,
            })
        })
        .collect()
}

/// The local In copies to set Out, and how many copies it could not judge. The
/// candidates come from the view read just before the Outs, while `specs` holds
/// each copy's full spec as its owner confirmed it, read since an earlier view, so a
/// copy the fetch never read cannot decide anything.
///
/// A local copy C is judged only when its newest siblings, all started by one
/// Deploy, agree on that Deploy's input W, compared without the update order the
/// planner resolves per copy. C goes Out when a Deploy of W would make a new
/// Container in its place. Otherwise, when W is Replicated with R replicas, let M be
/// C and every other routed In copy of the Service whose confirmed spec W would not
/// recreate. C goes Out when more than R - 1 copies of M rank above it, by gen
/// descending and then id ascending. Every Machine ranks the same view the same
/// way, so two Machines never both drop a copy the other keeps. A Machine sets Out
/// only its own copies, so a surplus copy on a Machine that is not restarting stays
/// until it restarts or the next Deploy. A copy whose spec went unconfirmed leaves
/// M, which only ever keeps C In. A Global Service uses only the first rule.
///
/// R is the replica count in W's spec. A replicas-only Deploy that starts no copy
/// leaves every spec with the old count, so after scaling down without starting a
/// copy, a returning Machine still counts the old R.
pub(crate) fn superseded(
    candidates: &[Candidate<'_>],
    specs: &BTreeMap<ContainerId, ResolvedServiceSpec>,
) -> (Vec<Superseded>, usize) {
    let mut unjudged = 0;
    let superseded = candidates
        .iter()
        .filter_map(|candidate| {
            let copy = candidate.copy;
            judge(candidate, specs)
                .unwrap_or_else(|why| {
                    tracing::info!(
                        "boot pass left {}/{} in: {why}",
                        copy.identity(),
                        copy.container_id
                    );
                    unjudged += 1;
                    false
                })
                .then(|| Superseded {
                    service: copy.identity(),
                    container: copy.container_id,
                    by: candidate
                        .newest
                        .first()
                        .map_or(copy.copy_gen(), |newest| newest.copy_gen()),
                })
        })
        .collect();
    (superseded, unjudged)
}

/// What a Deploy asked of a copy. The planner resolves each copy's update order on
/// its own: a replacement whose predecessor holds a host port stops first, while a
/// new copy starts first. So copies one Deploy started can differ there alone.
pub(crate) fn deploy_input(spec: &ResolvedServiceSpec) -> RequestedServiceSpec {
    let mut requested = spec.to_requested();
    requested.update.order = None;
    requested
}

/// Whether [`superseded`] sets `copy` Out, or why it cannot decide.
fn judge(
    Candidate {
        copy,
        newest,
        routed,
    }: &Candidate<'_>,
    specs: &BTreeMap<ContainerId, ResolvedServiceSpec>,
) -> Result<bool, &'static str> {
    let spec = |copy: &ContainerObservation| specs.get(&copy.container_id);
    let current = spec(copy).ok_or("this Machine could not read it")?;
    let mut newest = newest.iter().map(|sibling| spec(sibling).map(deploy_input));
    let wanted = newest
        .next()
        .flatten()
        .ok_or("its newest sibling's owner did not confirm it")?;
    if !newest.all(|sibling| sibling.as_ref() == Some(&wanted)) {
        return Err("its newest siblings disagree or did not answer");
    }
    if compare_specs(current, &wanted) == SpecChange::NeedsRecreate {
        return Ok(true);
    }
    let ServiceMode::Replicated { replicas } = wanted.mode else {
        return Ok(false);
    };
    let rank =
        |copy: &ContainerObservation| (std::cmp::Reverse(copy.copy_gen()), copy.container_id);
    let above = routed
        .iter()
        .filter(|sibling| {
            rank(sibling) < rank(copy)
                && spec(sibling)
                    .is_some_and(|spec| compare_specs(spec, &wanted) != SpecChange::NeedsRecreate)
        })
        .count();
    Ok(u32::try_from(above).is_ok_and(|above| above >= replicas.get()))
}

/// `row`'s full spec as its owner answers for it, retried until `retry_until`.
/// `None` when the owner never answers, or answers that the copy is not the In
/// copy at the gen the row shows.
async fn confirmed<H: BootHost>(
    host: &H,
    row: &ContainerObservation,
    retry_until: Instant,
) -> Option<ResolvedServiceSpec> {
    let (machine, container) = (row.machine_id, row.container_id);
    loop {
        if let Some(answer) = host.inspect(machine, container).await {
            if answer.machine_id == machine
                && *answer.rotation() == CopyRotation::In
                && answer.copy_gen() == row.copy_gen()
            {
                return Some(answer.into_parts().resolved_spec);
            }
            tracing::info!(
                "boot pass trusts no row for {container}: {machine} no longer reports it In at gen {}",
                row.copy_gen()
            );
            return None;
        }
        if Instant::now() + PEER_RETRY > retry_until {
            tracing::info!("boot pass could not read {container} from {machine}");
            return None;
        }
        tokio::time::sleep(PEER_RETRY).await;
    }
}

/// Runs the whole pass under one [`BOOT_PASS_WAIT`] deadline, and stops it there.
pub(crate) async fn run<H: BootHost>(
    host: H,
    first_synced: watch::Receiver<bool>,
    route_publishes: watch::Receiver<PublishedRoutes>,
) {
    let deadline = Instant::now() + BOOT_PASS_WAIT;
    let mut sent = 0;
    let pass = pass(&host, first_synced, route_publishes, deadline, &mut sent);
    match tokio::time::timeout_at(deadline, pass).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "boot pass skipped, could not read the store");
        }
        Err(_) => {
            // An admitted Out finishes even when the pass stops waiting for it.
            tracing::warn!(
                "boot pass stopped after {}s; {sent} Outs already sent may still finish",
                BOOT_PASS_WAIT.as_secs()
            );
        }
    }
}

/// Waits for this Machine's first container sync, a peer's store versions,
/// catch-up to them, and a route publish that read the store after catch-up, then
/// decides once.
async fn pass<H: BootHost>(
    host: &H,
    mut first_synced: watch::Receiver<bool>,
    mut route_publishes: watch::Receiver<PublishedRoutes>,
    deadline: Instant,
    sent: &mut usize,
) -> Result<(), CorrosionError> {
    if first_synced.wait_for(|synced| *synced).await.is_err() {
        std::future::pending::<()>().await;
    }
    let mut target = loop {
        match host.peer_targets().await {
            Peers::Answered(target) => break target,
            Peers::Alone => {
                tracing::info!(
                    "boot pass skipped, this Machine is the only one in its Machine list"
                );
                return Ok(());
            }
            Peers::Silent => tokio::time::sleep(PEER_RETRY).await,
        }
    };
    // A power cut can lose this Machine's last commits, which only its own later
    // writes would replace, so its own actor is never a target.
    let local = actor_key(&host.local_actor().await?);
    target.retain(|actor, _| actor_key(actor) != local);
    host.catch_up(&target).await?;
    tracing::info!("boot pass caught up to the peer's store versions");
    // The publish in flight may have read the store before catch-up, so the one
    // after it is the first that surely read it after.
    let after = route_publishes.borrow_and_update().count + 2;
    let own = route_publishes
        .wait_for(|published| published.count >= after)
        .await
        .map(|published| published.routes.clone());
    let Ok(own) = own else {
        return std::future::pending().await;
    };
    tracing::info!("boot pass saw this Machine's routes published after catch-up");
    let local = host.machine_id();
    let observed = host.observe().await?;
    let rows = candidates(&local, &observed, &own)
        .iter()
        .flat_map(Candidate::rows)
        .map(|row| (row.container_id, row))
        .collect::<BTreeMap<_, _>>();
    let retry_until = deadline.checked_sub(OUT_MARGIN).unwrap_or(deadline);
    let specs = join_all(rows.into_iter().map(|(container, row)| async move {
        Some((container, confirmed(host, row, retry_until).await?))
    }))
    .await
    .into_iter()
    .flatten()
    .collect();
    // The fetch can take minutes, and a Deploy meanwhile may have kept this
    // Machine's copy and retired the siblings it was ranked against.
    let observed = host.observe().await?;
    let own = route_publishes.borrow().routes.clone();
    let (superseded, unjudged) = superseded(&candidates(&local, &observed, &own), &specs);
    if unjudged > 0 {
        tracing::info!(
            "boot pass could not judge {unjudged} In copies here, their specs went unconfirmed"
        );
    } else if superseded.is_empty() {
        tracing::info!("boot pass found no In copy here that a Deploy would replace");
    }
    for Superseded {
        service,
        container,
        by,
    } in superseded
    {
        let want = ContainerWant::Out {
            container: ContainerLocator::Id(container),
            allow_empty: false,
            expect: Some(CopyRotation::In),
        };
        *sent += 1;
        match host.ensure(want).await {
            Ok(EnsuredContainer::Ensured { .. }) => {
                tracing::info!(
                    "boot pass set {service}/{container} out, gen {by} is acked everywhere"
                );
            }
            Ok(EnsuredContainer::Refused { refusal, .. }) => {
                tracing::info!(
                    "boot pass left {service}/{container} in: {}",
                    refusal.message
                );
            }
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "boot pass could not set {service}/{container} out");
            }
        }
    }
    Ok(())
}

pub(crate) fn actor_key(actor: &str) -> String {
    actor.replace('-', "").to_ascii_lowercase()
}

pub(crate) fn alone(
    machines: &ReplicatedObservations<Machine, MachineId>,
    local: &MachineId,
    bootstrap: &[Machine],
) -> bool {
    bootstrap.is_empty()
        && machines.incomplete_ids.is_empty()
        && machines
            .observations
            .iter()
            .all(|machine| machine.id == *local)
}

pub(crate) struct DaemonHost {
    pub(crate) machine_id: MachineId,
    pub(crate) machine: LocalMachine,
    pub(crate) replicated: ReplicatedStore,
    pub(crate) machines: MachineView,
    pub(crate) admin: AdminClient,
}

impl BootHost for DaemonHost {
    fn machine_id(&self) -> MachineId {
        self.machine_id
    }

    async fn peer_targets(&self) -> Peers {
        let Some(machines) = self.machines.current() else {
            return Peers::Silent;
        };
        if alone(
            &machines,
            &self.machine_id,
            self.machine.record().bootstrap(),
        ) {
            return Peers::Alone;
        }
        for peer in machines
            .observations
            .iter()
            .filter(|peer| peer.id != self.machine_id)
        {
            let address =
                SocketAddr::new(IpAddr::V6(peer.management_address().0), MACHINE_API_PORT);
            match tokio::time::timeout(PEER_TIMEOUT, target_versions(address)).await {
                Ok(Ok(target)) => {
                    tracing::info!("boot pass read store versions from {}", peer.name);
                    return Peers::Answered(target);
                }
                Ok(Err(error)) => {
                    tracing::debug!(error = %ployz_core::error_chain::inline(&error), "boot pass could not ask {}", peer.name);
                }
                Err(_) => tracing::debug!("boot pass timed out asking {}", peer.name),
            }
        }
        Peers::Silent
    }

    async fn local_actor(&self) -> Result<String, CorrosionError> {
        self.replicated.local_actor().await
    }

    async fn catch_up(&self, target: &BTreeMap<String, i64>) -> Result<(), CorrosionError> {
        wait_for_catch_up(&self.replicated, target).await
    }

    async fn observe(&self) -> Result<Observed, CorrosionError> {
        let (containers, routes) = tokio::try_join!(
            self.replicated.containers(),
            self.replicated.ingress_routes()
        )?;
        let membership = match self.machines.current() {
            Some(machines) => {
                sample_ackers(machines.as_ref().clone(), &self.machine_id, &self.admin).await
            }
            None => Membership {
                ackers: Ackers::Unread,
                down: Default::default(),
            },
        };
        Ok(Observed {
            containers: containers.observations,
            routes,
            membership,
        })
    }

    async fn inspect(
        &self,
        machine: MachineId,
        container: ContainerId,
    ) -> Option<ContainerObservation> {
        if machine == self.machine_id {
            return self
                .machine
                .containers()?
                .inspect_managed(&container, &machine)
                .await
                .inspect_err(|error| {
                    tracing::debug!(error = %ployz_core::error_chain::inline(error), "boot pass could not inspect {container}");
                })
                .ok();
        }
        let machines = self.machines.current()?;
        let peer = machines
            .observations
            .iter()
            .find(|peer| peer.id == machine)?;
        inspect_peer(peer, container, INSPECT_TIMEOUT).await
    }

    async fn ensure(&self, want: ContainerWant) -> Result<EnsuredContainer, LocalMachineError> {
        self.machine.ensure_container(want).await
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PeerError {
    #[error("could not reach the peer")]
    Connect(#[from] tonic::transport::Error),
    #[error("the peer refused the request")]
    Rpc(#[from] tonic::Status),
    #[error("the peer's answer did not decode")]
    Codec(#[from] CodecError),
    #[error("the peer's answer carried no store versions")]
    NoEnrollment,
}

/// A request with no iroh connection skips the Machine API's credential check.
pub(crate) async fn target_versions(
    address: SocketAddr,
) -> Result<BTreeMap<String, i64>, PeerError> {
    let channel = Endpoint::from_shared(format!("http://{address}"))?
        .connect()
        .await?;
    MachineRpcClient::new(channel)
        .list_machines(op::ListMachines::into_request(ListMachinesRequest {}).encode()?)
        .await?
        .into_inner()
        .decode_response()?
        .decode::<op::ListMachines>()?
        .enrollment
        .map(|enrollment| enrollment.target_versions)
        .ok_or(PeerError::NoEnrollment)
}

/// `container` as `peer` observes it, over the mesh within `wait`. The answer keeps the
/// environment and Config content. `None` when it didn't answer.
pub(crate) async fn inspect_peer(
    peer: &Machine,
    container: ContainerId,
    wait: Duration,
) -> Option<ContainerObservation> {
    let address = SocketAddr::new(IpAddr::V6(peer.management_address().0), MACHINE_API_PORT);
    match tokio::time::timeout(wait, inspect_on(address, container)).await {
        Ok(Ok(copy)) => Some(copy),
        Ok(Err(error)) => {
            tracing::debug!(error = %ployz_core::error_chain::inline(&error), "could not inspect {container} on {}", peer.name);
            None
        }
        Err(_) => {
            tracing::debug!("timed out inspecting {container} on {}", peer.name);
            None
        }
    }
}

/// Like [`target_versions`], over the mesh with no iroh connection. The answer
/// keeps the environment and Config content.
async fn inspect_on(
    address: SocketAddr,
    container_id: ContainerId,
) -> Result<ContainerObservation, PeerError> {
    let channel = Endpoint::from_shared(format!("http://{address}"))?
        .connect()
        .await?;
    let request = InspectContainerRequest { container_id };
    Ok(MachineRpcClient::new(channel)
        .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT)
        .inspect_container(op::InspectContainer::into_request(request).encode()?)
        .await?
        .into_inner()
        .decode_response()?
        .decode::<op::InspectContainer>()?
        .container)
}

#[cfg(test)]
#[path = "boot_pass_tests.rs"]
mod tests;
