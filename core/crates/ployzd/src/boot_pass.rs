//! Once per daemon start, set Out each In copy on this Machine whose Service has
//! a newer In copy every live proxy routes. A Machine powered off during a Deploy
//! comes back with its old copy In, and proxies would route it again. The pass
//! decides only after this Machine has caught up with a peer's store and its own
//! ingress has published, and it never stops a Container.

use std::{
    collections::BTreeMap,
    future::Future,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use ployz_core::{
    CodecError, ContainerId, ContainerKind, ContainerLocator, ContainerObservation, ContainerWant,
    CopyRotation, EnsuredContainer, Gen, ListMachinesRequest, MACHINE_API_PORT, MachineId,
    MachineRpcClient, QualifiedService, op,
};
use tokio::sync::watch;
use tonic::transport::Endpoint;

use crate::{
    corrosion::{
        AdminClient, Error as CorrosionError, IngressRouteRow, MachineView, ReplicatedStore,
        wait_for_catch_up,
    },
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

/// A local In copy, and the gen of the newest In sibling every live proxy routes.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Superseded {
    pub(crate) service: QualifiedService,
    pub(crate) container: ContainerId,
    pub(crate) by: Gen,
}

/// The local In copies to set Out. A Machine list that is only partly replicated,
/// or never read, passes nothing, and a copy on a Down Machine serves no one.
pub(crate) fn superseded(
    local: &MachineId,
    Observed {
        containers,
        routes,
        membership,
    }: &Observed,
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
            let routes = routed(routes.clone(), &service);
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

/// Waits at most [`BOOT_PASS_WAIT`] for a peer's store versions, catch-up to them,
/// and a route publish from this Machine's ingress that read the store after
/// catch-up, then decides once.
pub(crate) async fn run<H: BootHost>(host: H, mut route_publishes: watch::Receiver<u64>) {
    let ready = async {
        let target = loop {
            if let Some(target) = host.peer_targets().await {
                break target;
            }
            tokio::time::sleep(PEER_RETRY).await;
        };
        host.catch_up(&target).await?;
        tracing::info!("boot pass caught up to the peer's store versions");
        // The publish in flight may have read the store before catch-up, so the one
        // after it is the first that surely read it after.
        let after = *route_publishes.borrow_and_update() + 2;
        if route_publishes
            .wait_for(|publishes| *publishes >= after)
            .await
            .is_err()
        {
            std::future::pending::<()>().await;
        }
        tracing::info!("boot pass saw this Machine's routes published after catch-up");
        Ok::<_, CorrosionError>(())
    };
    match tokio::time::timeout(BOOT_PASS_WAIT, ready).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "boot pass skipped, catch-up failed");
            return;
        }
        Err(_) => {
            tracing::info!(
                "boot pass skipped, not caught up after {}s",
                BOOT_PASS_WAIT.as_secs()
            );
            return;
        }
    }
    let observed = match host.observe().await {
        Ok(observed) => observed,
        Err(error) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "boot pass skipped, could not read the store");
            return;
        }
    };
    for Superseded {
        service,
        container,
        by,
    } in superseded(&host.machine_id(), &observed)
    {
        let want = ContainerWant::Out {
            container: ContainerLocator::Id(container),
            allow_empty: false,
            expect: Some(CopyRotation::In),
        };
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
