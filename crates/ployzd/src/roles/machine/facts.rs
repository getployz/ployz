use super::response::{failure_message, machine_domain_error, machine_success};
use crate::roles::machine::endpoints::{observe_interface_endpoints, observe_machine_endpoints};
use crate::roles::machine::protocol::{
    MachineFactsGetDomainError, MachineFactsGetRpcOk, MachineFactsGetRpcRequest,
    MachineFactsGetRpcResponse,
};
use crate::roles::machine::runner::{
    ExistingManagedContainerState, MachineContainerRunner, MachineContainerRunnerError,
};
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    MachineContainerObservationSnapshotError, MachineFactsSnapshot, MachineFactsSnapshotError,
    ManagedContainerObservation,
};
use ployz_core::state::MachineEndpointObservation;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub(crate) struct MachineFactsState<R> {
    pub(crate) runner: R,
    pub(crate) endpoint_cache: MachineEndpointCache,
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
        });
    let containers = MachineContainerObservationSnapshot::try_new(machine_id.clone(), containers)
        .map_err(MachineFactsReadError::BuildContainerSnapshot)?;

    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        containers,
        endpoints,
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
    #[error("failed to build machine facts: {0}")]
    BuildFactsSnapshot(MachineFactsSnapshotError),
}

pub(crate) fn observation_state(state: ExistingManagedContainerState) -> ContainerRuntimeState {
    match state {
        ExistingManagedContainerState::Running { ip } => ContainerRuntimeState::Running { ip },
        ExistingManagedContainerState::StartableStopped
        | ExistingManagedContainerState::NotStartable { .. } => ContainerRuntimeState::Exited,
    }
}

pub(crate) fn current_unix_ms() -> u64 {
    let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
