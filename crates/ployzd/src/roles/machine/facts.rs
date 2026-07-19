use super::current_unix_ms;
use super::response::{failure_message, machine_domain_error, machine_success};
use crate::roles::machine::endpoints::{observe_interface_endpoints, observe_machine_endpoints};
use crate::roles::machine::protocol::{
    MachineBuildCapability, MachineFactsGetDomainError, MachineFactsGetRpcOk,
    MachineFactsGetRpcRequest, MachineFactsGetRpcResponse, MachineFactsRefreshDomainError,
    MachineFactsRefreshRpcOk, MachineFactsRefreshRpcRequest, MachineFactsRefreshRpcResponse,
};
use crate::roles::machine::runner::{
    ExistingManagedContainer, ExistingManagedContainerState, MachineContainerListError,
    MachineContainerRunner,
};
use crate::roles::machine::volume::STORAGE_CAPABILITY_HOST_COMMAND_TIMEOUT;
use crate::roles::machine::volume::observe_storage_capability;
use ployz_core::ids::MachineId;
use ployz_core::machine::MachineEndpointObservation;
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    MachineContainerObservationSnapshotError, MachineContainerTestimony,
    MachineContainerUnavailableReason, MachineDiskSpace, MachineFactsCompletionError,
    MachineFactsRefreshConfirmation, MachineFactsSnapshot, MachineFactsSnapshotError,
    MachineFactsTestimony, ManagedContainerObservation,
};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use ployz_nats::subjects::machine_facts;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MACHINE_DATA_PATH: &str = "/var/lib/ployz";
const NON_STORAGE_FACTS_REFRESH_ALLOWANCE: Duration = Duration::from_secs(5);
pub(crate) const MACHINE_FACTS_REFRESH_TIMEOUT: Duration =
    STORAGE_CAPABILITY_HOST_COMMAND_TIMEOUT.saturating_add(NON_STORAGE_FACTS_REFRESH_ALLOWANCE);

#[derive(Clone)]
pub(crate) struct MachineFactsState<R> {
    pub(crate) runner: R,
    pub(crate) endpoint_cache: MachineEndpointCache,
    pub(crate) client: async_nats::Client,
}

#[derive(Clone)]
pub(crate) struct MachineFactsGetState<R> {
    pub(crate) facts: MachineFactsState<R>,
    pub(crate) build_runtime_available: bool,
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
    let storage = observe_storage_capability().await;
    let facts =
        read_machine_facts_snapshot(machine_id, runner, endpoints, storage, current_unix_ms())
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
    state: MachineFactsGetState<R>,
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
    let endpoints = match state.facts.endpoint_cache.latest() {
        Some(observation) => Some(observation),
        None => {
            observe_interface_endpoints(&machine_id, state.facts.endpoint_cache.wg_ifname()).await
        }
    };
    let storage = observe_storage_capability().await;
    match read_machine_facts_testimony(
        &machine_id,
        &state.facts.runner,
        endpoints,
        storage,
        current_unix_ms(),
    )
    .await
    {
        Ok(facts) => {
            let build =
                build_capability_for_facts(state.build_runtime_available, facts.platform()).await;
            machine_success(MachineFactsGetRpcResponse::Ok(MachineFactsGetRpcOk {
                facts,
                build,
            }))
        }
        Err(error) => machine_domain_error(MachineFactsGetRpcResponse::DomainError {
            machine_id,
            error: MachineFactsGetDomainError::GatherFailed {
                message: failure_message(error.to_string()),
            },
        }),
    }
}

async fn build_capability_for_facts(
    build_runtime_available: bool,
    platform: &ployz_core::image::OciPlatform,
) -> MachineBuildCapability {
    if !build_runtime_available {
        return MachineBuildCapability::Unavailable;
    }
    map_build_capability(
        build_runtime_available,
        ployz_build_executor::railpack_helper_is_ready(platform).await,
    )
}

fn map_build_capability(
    build_runtime_available: bool,
    railpack_helper_ready: bool,
) -> MachineBuildCapability {
    match (build_runtime_available, railpack_helper_ready) {
        (false, _) => MachineBuildCapability::Unavailable,
        (true, false) => MachineBuildCapability::RailpackUnavailable,
        (true, true) => MachineBuildCapability::Available,
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

    #[cfg(test)]
    pub(crate) fn with_observation(observation: MachineEndpointObservation) -> Self {
        Self {
            latest: Arc::new(Mutex::new(Some(observation))),
            wg_ifname: String::new(),
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
    storage: Option<ployz_core::machine::StorageCapability>,
    observed_at_unix_ms: u64,
) -> Result<MachineFactsSnapshot, MachineFactsReadError>
where
    R: MachineContainerRunner,
{
    read_machine_facts_testimony(machine_id, runner, endpoints, storage, observed_at_unix_ms)
        .await?
        .try_into()
        .map_err(MachineFactsReadError::Complete)
}

pub(crate) async fn read_machine_facts_testimony<R>(
    machine_id: &MachineId,
    runner: &R,
    endpoints: Option<MachineEndpointObservation>,
    storage: Option<ployz_core::machine::StorageCapability>,
    observed_at_unix_ms: u64,
) -> Result<MachineFactsTestimony, MachineFactsReadError>
where
    R: MachineContainerRunner,
{
    let existing = runner.existing_managed_containers().await;
    let disk_space =
        read_disk_space(Path::new(MACHINE_DATA_PATH)).map_err(MachineFactsReadError::DiskSpace)?;

    assemble_machine_facts_testimony(
        machine_id,
        existing,
        endpoints,
        disk_space,
        storage,
        ployz_core::image::OciPlatform::current(),
        observed_at_unix_ms,
    )
}

fn assemble_machine_facts_testimony(
    machine_id: &MachineId,
    existing: Result<Vec<ExistingManagedContainer>, MachineContainerListError>,
    endpoints: Option<MachineEndpointObservation>,
    disk_space: MachineDiskSpace,
    storage: Option<ployz_core::machine::StorageCapability>,
    platform: ployz_core::image::OciPlatform,
    observed_at_unix_ms: u64,
) -> Result<MachineFactsTestimony, MachineFactsReadError> {
    let containers = match existing {
        Ok(existing) => MachineContainerTestimony::Answered {
            snapshot: container_snapshot(machine_id, existing)?,
        },
        Err(MachineContainerListError::ListExisting { message: _ }) => {
            MachineContainerTestimony::Unavailable {
                reason: MachineContainerUnavailableReason::DockerUnavailable,
            }
        }
    };
    MachineFactsTestimony::try_new(
        machine_id.clone(),
        containers,
        endpoints,
        disk_space,
        storage,
        platform,
        observed_at_unix_ms,
    )
    .map_err(MachineFactsReadError::BuildFactsSnapshot)
}

fn container_snapshot(
    machine_id: &MachineId,
    existing: Vec<ExistingManagedContainer>,
) -> Result<MachineContainerObservationSnapshot, MachineFactsReadError> {
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
    MachineContainerObservationSnapshot::try_new(machine_id.clone(), containers)
        .map_err(MachineFactsReadError::BuildContainerSnapshot)
}

#[derive(Debug, thiserror::Error)]
pub enum MachineFactsReadError {
    #[error("failed to build container snapshot: {0}")]
    BuildContainerSnapshot(MachineContainerObservationSnapshotError),
    #[error("failed to read disk space: {0}")]
    DiskSpace(std::io::Error),
    #[error("failed to build machine facts: {0}")]
    BuildFactsSnapshot(MachineFactsSnapshotError),
    #[error("machine facts are not complete: {0}")]
    Complete(MachineFactsCompletionError),
}

pub(super) fn read_disk_space(path: &Path) -> std::io::Result<MachineDiskSpace> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::machine::{StorageCapability, StorageUnavailableReason};

    #[test]
    fn facts_refresh_budget_includes_storage_and_non_storage_work() {
        assert_eq!(
            MACHINE_FACTS_REFRESH_TIMEOUT,
            STORAGE_CAPABILITY_HOST_COMMAND_TIMEOUT.saturating_add(Duration::from_secs(5))
        );
        assert!(MACHINE_FACTS_REFRESH_TIMEOUT > STORAGE_CAPABILITY_HOST_COMMAND_TIMEOUT);
    }

    #[test]
    fn facts_get_maps_build_and_railpack_readiness() {
        for (build_runtime_available, railpack_helper_ready, expected) in [
            (false, true, MachineBuildCapability::Unavailable),
            (true, false, MachineBuildCapability::RailpackUnavailable),
            (true, true, MachineBuildCapability::Available),
        ] {
            assert_eq!(
                map_build_capability(build_runtime_available, railpack_helper_ready),
                expected
            );
        }
    }

    #[test]
    fn direct_testimony_preserves_storage_when_docker_is_unavailable() {
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let endpoints = MachineEndpointObservation {
            machine_id: machine_id.clone(),
            control_endpoints: vec!["203.0.113.10".parse().expect("control endpoint")],
            mesh_endpoints: vec!["203.0.113.10:51820".parse().expect("mesh endpoint")],
        };
        let platform = ployz_core::image::OciPlatform::try_new("linux", "amd64").expect("platform");
        let testimony = assemble_machine_facts_testimony(
            &machine_id,
            Err(MachineContainerListError::ListExisting {
                message: "daemon unavailable".to_owned(),
            }),
            Some(endpoints.clone()),
            MachineDiskSpace {
                available_bytes: 40,
                total_bytes: 100,
            },
            Some(StorageCapability::Unavailable {
                reason: StorageUnavailableReason::ZfsModuleMissing,
            }),
            platform.clone(),
            123,
        )
        .expect("independent axes remain valid");

        assert_eq!(
            testimony.containers(),
            &MachineContainerTestimony::Unavailable {
                reason: MachineContainerUnavailableReason::DockerUnavailable,
            }
        );
        assert_eq!(
            testimony.storage(),
            Some(&StorageCapability::Unavailable {
                reason: StorageUnavailableReason::ZfsModuleMissing,
            })
        );
        assert_eq!(testimony.disk_space().available_bytes, 40);
        assert_eq!(testimony.disk_space().total_bytes, 100);
        assert_eq!(testimony.endpoints(), Some(&endpoints));
        assert_eq!(testimony.platform(), &platform);
        assert_eq!(testimony.observed_at_unix_ms(), 123);
        assert!(MachineFactsSnapshot::try_from(testimony).is_err());
    }
}
