//! Once per daemon start, set Out each In copy on this Machine whose Service has
//! a newer In copy that replaces it and that every live proxy routes. A Machine
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

use ployz_core::{
    CodecError, ContainerId, ContainerKind, ContainerLocator, ContainerObservation, ContainerWant,
    CopyRotation, EnsuredContainer, EnvironmentValues, Gen, ListContainersRequest,
    ListMachinesRequest, MACHINE_API_PORT, MachineId, MachineRpcClient, QualifiedService,
    SpecChange, compare_specs, op,
};
use tokio::{sync::watch, time::Instant};
use tonic::transport::Endpoint;

use crate::{
    corrosion::{
        AdminClient, Error as CorrosionError, IngressRouteRow, MachineView, ReplicatedStore,
        wait_for_catch_up,
    },
    ingress::PublishedRoutes,
    machine::{LocalMachine, LocalMachineError},
    route_acks::{Ackers, Membership, routed, sample_ackers},
};

/// The executor's `ACK_WAIT`, which ployzd cannot import.
pub(crate) const BOOT_PASS_WAIT: Duration = Duration::from_secs(300);
const PEER_TIMEOUT: Duration = Duration::from_secs(2);
const FULL_COPIES_TIMEOUT: Duration = Duration::from_secs(10);
const PEER_RETRY: Duration = Duration::from_secs(1);

pub(crate) trait BootHost: Send + Sync {
    fn machine_id(&self) -> MachineId;
    /// One round over the peers. The first answer wins, and `None` means none answered.
    fn peer_targets(&self) -> impl Future<Output = Option<BTreeMap<String, i64>>> + Send;
    fn local_actor(&self) -> impl Future<Output = Result<String, CorrosionError>> + Send;
    fn catch_up(
        &self,
        target: &BTreeMap<String, i64>,
    ) -> impl Future<Output = Result<(), CorrosionError>> + Send;
    fn observe(&self) -> impl Future<Output = Result<Observed, CorrosionError>> + Send;
    /// `machine`'s managed Containers with their environment and Config content,
    /// which a store row drops. `None` means it did not answer.
    fn full_copies(
        &self,
        machine: MachineId,
    ) -> impl Future<Output = Option<Vec<ContainerObservation>>> + Send;
    fn ensure(
        &self,
        want: ContainerWant,
    ) -> impl Future<Output = Result<EnsuredContainer, LocalMachineError>> + Send;
}

pub(crate) struct Observed {
    pub(crate) containers: Vec<ContainerObservation>,
    pub(crate) routes: BTreeMap<(MachineId, QualifiedService), IngressRouteRow>,
    pub(crate) membership: Membership,
}

/// A local In copy, and the gen of the newest In sibling that replaces it and
/// every live proxy routes.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Superseded {
    pub(crate) service: QualifiedService,
    pub(crate) container: ContainerId,
    pub(crate) by: Gen,
}

/// Each local In copy beside its newer In siblings that every live proxy routes. A
/// Machine list that is only partly replicated, or never read, yields nothing, and a
/// copy on a Down Machine serves no one. This Machine's own acks come from `own`,
/// what its ingress published since it started, because its row in the store may
/// predate a power-off.
fn routed_newer<'a>(
    local: &MachineId,
    Observed {
        containers,
        routes,
        membership,
    }: &'a Observed,
    own: &BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
) -> Vec<(&'a ContainerObservation, Vec<&'a ContainerObservation>)> {
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
        .map(|copy| {
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
            let newer = containers
                .iter()
                .filter(serving)
                .filter(|sibling| {
                    sibling.container_id != copy.container_id
                        && sibling.identity() == service
                        && sibling.copy_gen() > copy.copy_gen()
                        && acked(&sibling.container_id)
                })
                .collect();
            (copy, newer)
        })
        .collect()
}

/// The local In copies to set Out: each beside a routed newer sibling that a
/// Deploy would act on, judged on the full specs in `full` as the Deploy judges
/// them. A copy whose owner is missing from `full` decides nothing.
pub(crate) fn superseded(
    local: &MachineId,
    observed: &Observed,
    own: &BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
    full: &BTreeMap<MachineId, Vec<ContainerObservation>>,
) -> Vec<Superseded> {
    let spec = |copy: &ContainerObservation| {
        full.get(&copy.machine_id)?
            .iter()
            .find(|full| full.container_id == copy.container_id)
            .map(|full| &full.resolved_spec)
    };
    routed_newer(local, observed, own)
        .into_iter()
        .filter_map(|(copy, newer)| {
            let current = spec(copy)?;
            let by = newer
                .into_iter()
                .filter(|sibling| {
                    spec(sibling).is_some_and(|wanted| {
                        compare_specs(current, &wanted.to_requested()) != SpecChange::UpToDate
                    })
                })
                .map(|sibling| sibling.copy_gen())
                .max()?;
            Some(Superseded {
                service: copy.identity(),
                container: copy.container_id,
                by,
            })
        })
        .collect()
}

/// Runs the whole pass under one [`BOOT_PASS_WAIT`] deadline, and stops it there.
pub(crate) async fn run<H: BootHost>(
    host: H,
    first_synced: watch::Receiver<bool>,
    route_publishes: watch::Receiver<PublishedRoutes>,
) {
    let deadline = Instant::now() + BOOT_PASS_WAIT;
    let mut sent = 0;
    let pass = pass(&host, first_synced, route_publishes, &mut sent);
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
    sent: &mut usize,
) -> Result<(), CorrosionError> {
    if first_synced.wait_for(|synced| *synced).await.is_err() {
        std::future::pending::<()>().await;
    }
    let mut target = loop {
        if let Some(target) = host.peer_targets().await {
            break target;
        }
        tokio::time::sleep(PEER_RETRY).await;
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
    let observed = host.observe().await?;
    let local = host.machine_id();
    let owners = routed_newer(&local, &observed, &own)
        .into_iter()
        .filter(|(_, newer)| !newer.is_empty())
        .flat_map(|(copy, newer)| {
            std::iter::once(copy.machine_id).chain(newer.into_iter().map(|copy| copy.machine_id))
        })
        .collect::<BTreeSet<_>>();
    let mut full = BTreeMap::new();
    for owner in owners {
        match host.full_copies(owner).await {
            Some(copies) => {
                full.insert(owner, copies);
            }
            None => tracing::info!(
                "boot pass could not read {owner}'s Containers, so its copies replace nothing here"
            ),
        }
    }
    let superseded = superseded(&local, &observed, &own, &full);
    if superseded.is_empty() {
        tracing::info!("boot pass found no In copy here that a newer one replaces everywhere");
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

fn actor_key(actor: &str) -> String {
    actor.replace('-', "").to_ascii_lowercase()
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

    async fn peer_targets(&self) -> Option<BTreeMap<String, i64>> {
        let machines = self.machines.current()?;
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
                    return Some(target);
                }
                Ok(Err(error)) => {
                    tracing::debug!(error = %ployz_core::error_chain::inline(&error), "boot pass could not ask {}", peer.name);
                }
                Err(_) => tracing::debug!("boot pass timed out asking {}", peer.name),
            }
        }
        None
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

    async fn full_copies(&self, machine: MachineId) -> Option<Vec<ContainerObservation>> {
        if machine == self.machine_id {
            return self
                .machine
                .containers()?
                .list_managed(&machine)
                .await
                .inspect_err(|error| {
                    tracing::debug!(error = %ployz_core::error_chain::inline(error), "boot pass could not list this Machine's Containers");
                })
                .ok();
        }
        let machines = self.machines.current()?;
        let peer = machines
            .observations
            .iter()
            .find(|peer| peer.id == machine)?;
        let address = SocketAddr::new(IpAddr::V6(peer.management_address().0), MACHINE_API_PORT);
        match tokio::time::timeout(FULL_COPIES_TIMEOUT, full_list(address)).await {
            Ok(Ok(copies)) => Some(copies),
            Ok(Err(error)) => {
                tracing::debug!(error = %ployz_core::error_chain::inline(&error), "boot pass could not list {}'s Containers", peer.name);
                None
            }
            Err(_) => {
                tracing::debug!("boot pass timed out listing {}'s Containers", peer.name);
                None
            }
        }
    }

    async fn ensure(&self, want: ContainerWant) -> Result<EnsuredContainer, LocalMachineError> {
        self.machine.ensure_container(want).await
    }
}

#[derive(Debug, thiserror::Error)]
enum PeerError {
    #[error("could not reach the peer")]
    Connect(#[from] tonic::transport::Error),
    #[error("the peer refused ListMachines")]
    Rpc(#[from] tonic::Status),
    #[error("the peer's answer did not decode")]
    Codec(#[from] CodecError),
    #[error("the peer's answer carried no store versions")]
    NoEnrollment,
}

/// A request with no iroh connection skips the Machine API's credential check.
async fn target_versions(address: SocketAddr) -> Result<BTreeMap<String, i64>, PeerError> {
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

/// Like [`target_versions`], over the mesh with no iroh connection.
async fn full_list(address: SocketAddr) -> Result<Vec<ContainerObservation>, PeerError> {
    let channel = Endpoint::from_shared(format!("http://{address}"))?
        .connect()
        .await?;
    let request = ListContainersRequest {
        environment: EnvironmentValues::Included,
    };
    Ok(MachineRpcClient::new(channel)
        .list_containers(op::ListContainers::into_request(request).encode()?)
        .await?
        .into_inner()
        .decode_response()?
        .decode::<op::ListContainers>()?
        .containers)
}

#[cfg(test)]
#[path = "boot_pass_tests.rs"]
mod tests;
