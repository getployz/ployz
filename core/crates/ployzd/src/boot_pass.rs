//! Once per daemon start, set Out each In copy on this Machine whose Service has
//! a newer In copy of another shape that every live proxy routes. A Machine
//! powered off during a Deploy comes back with its old copy In, and proxies would
//! route it again. A copy a Deploy kept keeps its gen beside the newer copies it
//! started, and shares their shape. The pass decides only after this Machine has
//! caught up with a peer's store and its own ingress has published since, and it
//! never stops a Container.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use ployz_core::{
    CodecError, ContainerId, ContainerKind, ContainerLocator, ContainerObservation, ContainerWant,
    CopyRotation, EnsuredContainer, Gen, ListMachinesRequest, MACHINE_API_PORT, MachineId,
    MachineRpcClient, QualifiedService, op,
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

/// A local In copy, and the gen of the newest In sibling of another shape every
/// live proxy routes.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Superseded {
    pub(crate) service: QualifiedService,
    pub(crate) container: ContainerId,
    pub(crate) by: Gen,
}

/// The local In copies to set Out. A Machine list that is only partly replicated,
/// or never read, passes nothing, and a copy on a Down Machine serves no one. This
/// Machine's own acks come from `own`, what its ingress published since it started,
/// because its row in the store may predate a power-off.
pub(crate) fn superseded(
    local: &MachineId,
    Observed {
        containers,
        routes,
        membership,
    }: &Observed,
    own: &BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
) -> Vec<Superseded> {
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
            let shape = copy.resolved_spec.published_serving_shape();
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
            let by = containers
                .iter()
                .filter(serving)
                .filter(|sibling| {
                    sibling.container_id != copy.container_id
                        && sibling.identity() == service
                        && sibling.copy_gen() > copy.copy_gen()
                        && sibling.resolved_spec.published_serving_shape() != shape
                        && acked(&sibling.container_id)
                })
                .map(|sibling| sibling.copy_gen())
                .max()?;
            Some(Superseded {
                service,
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
    let superseded = superseded(&host.machine_id(), &observed, &own);
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

#[cfg(test)]
#[path = "boot_pass_tests.rs"]
mod tests;
