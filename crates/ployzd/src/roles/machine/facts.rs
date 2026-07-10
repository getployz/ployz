use super::current_unix_ms;
use super::response::{failure_message, machine_domain_error, machine_success};
use crate::roles::machine::endpoints::{observe_interface_endpoints, observe_machine_endpoints};
use crate::roles::machine::protocol::{
    MachineFactsGetDomainError, MachineFactsGetRpcOk, MachineFactsGetRpcRequest,
    MachineFactsGetRpcResponse, MachineFactsRefreshDomainError, MachineFactsRefreshRpcOk,
    MachineFactsRefreshRpcRequest, MachineFactsRefreshRpcResponse,
};
use crate::roles::machine::runner::{
    ExistingManagedContainerState, MachineContainerRunner, MachineContainerRunnerError,
};
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    MachineContainerObservationSnapshotError, MachineDiskSpace, MachineFactsRefreshConfirmation,
    MachineFactsSnapshot, MachineFactsSnapshotError, ManagedContainerObservation,
};
use ployz_core::state::MachineEndpointObservation;
use ployz_core::subjects::machine_facts;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MACHINE_DATA_PATH: &str = "/var/lib/ployz";
pub(crate) const MACHINE_FACTS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct MachineFactsState<R> {
    pub(crate) runner: R,
    pub(crate) endpoint_cache: MachineEndpointCache,
    pub(crate) client: async_nats::Client,
}

pub(crate) async fn handle_facts_refresh<R>(
    machine_id: MachineId,
    state: MachineFactsState<R>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    if let Err(response) = decode_json_request::<MachineFactsRefreshRpcRequest>(&request) {
        return response;
    }
    let endpoints = match state.endpoint_cache.latest() {
        Some(observation) => Some(observation),
        None => observe_interface_endpoints(&machine_id, state.endpoint_cache.wg_ifname()).await,
    };
    let refreshed = tokio::time::timeout(
        MACHINE_FACTS_REFRESH_TIMEOUT,
        publish_machine_facts_snapshot(&state.client, &machine_id, &state.runner, endpoints),
    )
    .await;
    match refreshed {
        Ok(Ok(facts)) => machine_success(MachineFactsRefreshRpcResponse::Ok(
            MachineFactsRefreshRpcOk {
                refresh: MachineFactsRefreshConfirmation::from(&facts),
            },
        )),
        Ok(Err(error)) => machine_domain_error(MachineFactsRefreshRpcResponse::DomainError {
            machine_id,
            error: MachineFactsRefreshDomainError::RefreshFailed {
                message: failure_message(error.to_string()),
            },
        }),
        Err(_) => machine_domain_error(MachineFactsRefreshRpcResponse::DomainError {
            machine_id,
            error: MachineFactsRefreshDomainError::RefreshFailed {
                message: failure_message(format!(
                    "machine facts refresh timed out after {}s",
                    MACHINE_FACTS_REFRESH_TIMEOUT.as_secs()
                )),
            },
        }),
    }
}

pub(crate) async fn publish_machine_facts<R>(
    client: &async_nats::Client,
    machine_id: &MachineId,
    runner: &R,
    endpoint_cache: &MachineEndpointCache,
) -> Result<MachineFactsSnapshot, MachineFactsPublishError>
where
    R: MachineContainerRunner,
{
    let endpoints = refresh_machine_endpoints(machine_id, endpoint_cache).await;
    publish_machine_facts_snapshot(client, machine_id, runner, endpoints).await
}

async fn publish_machine_facts_snapshot<R>(
    client: &async_nats::Client,
    machine_id: &MachineId,
    runner: &R,
    endpoints: Option<MachineEndpointObservation>,
) -> Result<MachineFactsSnapshot, MachineFactsPublishError>
where
    R: MachineContainerRunner,
{
    let facts = read_machine_facts_snapshot(machine_id, runner, endpoints, current_unix_ms())
        .await
        .map_err(MachineFactsPublishError::Read)?;
    let payload = serde_json::to_vec(&facts).map_err(MachineFactsPublishError::Encode)?;
    client
        .publish(machine_facts(machine_id), payload.into())
        .await
        .map_err(|error| MachineFactsPublishError::Publish {
            message: error.to_string(),
        })?;
    client
        .flush()
        .await
        .map_err(|error| MachineFactsPublishError::Publish {
            message: error.to_string(),
        })?;
    Ok(facts)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MachineFactsPublishError {
    #[error("failed to read machine facts: {0}")]
    Read(MachineFactsReadError),
    #[error("failed to encode machine facts: {0}")]
    Encode(serde_json::Error),
    #[error("failed to publish machine facts: {message}")]
    Publish { message: String },
}

pub(crate) async fn handle_facts_get<R>(
    machine_id: MachineId,
    state: MachineFactsState<R>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    if let Err(response) = decode_json_request::<MachineFactsGetRpcRequest>(&request) {
        return response;
    }

    // Serve the observation task's cached endpoints. Until it has populated them
    // (process startup, or a test with no observer), fall back to interface-only
    // discovery — a syscall, no network — so mesh peer discovery never sees missing
    // endpoints and the public-IP echo stays off the per-RPC path.
    let endpoints = match state.endpoint_cache.latest() {
        Some(observation) => Some(observation),
        None => observe_interface_endpoints(&machine_id, state.endpoint_cache.wg_ifname()).await,
    };
    match read_machine_facts_snapshot(&machine_id, &state.runner, endpoints, current_unix_ms())
        .await
    {
        Ok(facts) => machine_success(MachineFactsGetRpcResponse::Ok(MachineFactsGetRpcOk {
            facts,
        })),
        Err(error) => machine_domain_error(MachineFactsGetRpcResponse::DomainError {
            machine_id,
            error: MachineFactsGetDomainError::GatherFailed {
                message: failure_message(error.to_string()),
            },
        }),
    }
}

/// The machine's last discovered endpoints, shared between the observation task
/// (which discovers them off the hot path and stores them here) and the `FactsGet`
/// RPC handler (which serves them without re-running discovery). Endpoints are a
/// slow-changing address property, so a periodically-refreshed snapshot is right;
/// re-probing external IP services on every RPC would put that I/O on the deploy
/// planning path.
#[derive(Clone, Default)]
pub(crate) struct MachineEndpointCache {
    latest: Arc<Mutex<Option<MachineEndpointObservation>>>,
    /// The configured WireGuard interface, excluded from discovery so its overlay
    /// tunnel address is never advertised as a mesh candidate.
    wg_ifname: String,
}

impl MachineEndpointCache {
    pub(crate) fn new(wg_ifname: String) -> Self {
        Self {
            latest: Arc::default(),
            wg_ifname,
        }
    }

    fn wg_ifname(&self) -> &str {
        &self.wg_ifname
    }

    fn store(&self, observation: Option<MachineEndpointObservation>) {
        *self
            .latest
            .lock()
            .expect("machine endpoint cache lock is not poisoned") = observation;
    }

    fn latest(&self) -> Option<MachineEndpointObservation> {
        self.latest
            .lock()
            .expect("machine endpoint cache lock is not poisoned")
            .clone()
    }
}

/// Discover the machine's endpoints and record them in the cache. Called from the
/// observation tick, never from an RPC handler.
pub(crate) async fn refresh_machine_endpoints(
    machine_id: &MachineId,
    cache: &MachineEndpointCache,
) -> Option<MachineEndpointObservation> {
    let observation = observe_machine_endpoints(machine_id, cache.wg_ifname()).await;
    cache.store(observation.clone());
    observation
}

pub(crate) async fn read_machine_facts_snapshot<R>(
    machine_id: &MachineId,
    runner: &R,
    endpoints: Option<MachineEndpointObservation>,
    observed_at_unix_ms: u64,
) -> Result<MachineFactsSnapshot, MachineFactsReadError>
where
    R: MachineContainerRunner,
{
    let existing = runner
        .existing_managed_containers()
        .await
        .map_err(MachineFactsReadError::ListContainers)?;
    let containers = existing
        .into_iter()
        .map(|container| ManagedContainerObservation {
            machine_id: machine_id.clone(),
            container_id: container.container_id,
            identity: container.identity,
            state: observation_state(container.state),
            health_status: container.health_status,
            resolved_image_identity: container.resolved_image_identity,
            created_at_unix_seconds: container.created_at_unix_seconds,
        });
    let containers = MachineContainerObservationSnapshot::try_new(machine_id.clone(), containers)
        .map_err(MachineFactsReadError::BuildContainerSnapshot)?;
    let disk_space =
        read_disk_space(Path::new(MACHINE_DATA_PATH)).map_err(MachineFactsReadError::DiskSpace)?;

    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        containers,
        endpoints,
        disk_space,
        ployz_core::image::OciPlatform::current(),
        observed_at_unix_ms,
    )
    .map_err(MachineFactsReadError::BuildFactsSnapshot)
}

#[derive(Debug, thiserror::Error)]
pub enum MachineFactsReadError {
    #[error("failed to list managed Docker containers: {0:?}")]
    ListContainers(MachineContainerRunnerError),
    #[error("failed to build container snapshot: {0}")]
    BuildContainerSnapshot(MachineContainerObservationSnapshotError),
    #[error("failed to read disk space: {0}")]
    DiskSpace(std::io::Error),
    #[error("failed to build machine facts: {0}")]
    BuildFactsSnapshot(MachineFactsSnapshotError),
}

fn read_disk_space(path: &Path) -> std::io::Result<MachineDiskSpace> {
    read_existing_path_disk_space(existing_filesystem_path(path))
}

fn existing_filesystem_path(path: &Path) -> &Path {
    let mut current = path;
    while !current.exists() {
        let Some(parent) = current.parent() else {
            return Path::new("/");
        };
        current = parent;
    }
    current
}

fn read_existing_path_disk_space(path: &Path) -> std::io::Result<MachineDiskSpace> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(MachineDiskSpace {
        available_bytes: bytes_from_blocks(stat.f_bavail, stat.f_frsize),
        total_bytes: bytes_from_blocks(stat.f_blocks, stat.f_frsize),
    })
}

fn bytes_from_blocks(blocks: u64, block_size: u64) -> u64 {
    u64::try_from(u128::from(blocks).saturating_mul(u128::from(block_size))).unwrap_or(u64::MAX)
}

pub(crate) fn observation_state(state: ExistingManagedContainerState) -> ContainerRuntimeState {
    match state {
        ExistingManagedContainerState::Running {
            ip,
            health,
            started_at_unix_ms,
        } => ContainerRuntimeState::Running {
            ip,
            health,
            started_at_unix_ms,
        },
        ExistingManagedContainerState::StartableStopped
        | ExistingManagedContainerState::NotStartable { .. } => ContainerRuntimeState::Exited,
    }
}
