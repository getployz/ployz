//! NATS Service API wiring for machine-local commands.

use super::containers::{
    MachineContainerState, handle_container_inspect, handle_container_remove,
    handle_container_resolve_image, handle_container_restart, handle_container_run,
    handle_container_run_hook, handle_container_stop, handle_volume_remove,
};
use super::dataplane::{
    MachineDataplaneStatusState, handle_dataplane_public_key, handle_dataplane_status,
};
use super::facts::{
    MachineEndpointCache, MachineFactsState, handle_facts_get, handle_facts_refresh,
};
use super::images::{
    AvailableImageService, handle_image_blob_check, handle_image_blob_push, handle_image_ensure,
    handle_image_manifest_push,
};
use super::logs::handle_logs_tail;
use super::substrate::{handle_substrate_report, handle_substrate_update};
use crate::roles::machine::execution::host_dataplane::dataplane_status_budget;
use crate::roles::machine::projection::{
    MachineProjectionState, RunningProjectionTask, start_projection_task,
};
use crate::roles::machine::runner::{MachineContainerRunner, MachineLogReader};
use crate::service_catalog::{machine_endpoint_spec, machine_role_service_base};
use ployz_core::dataplane::{
    PloyzNativeMeshReady, WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError, WireGuardPeer,
    WireGuardPublicKey, WireGuardReady,
};
use ployz_core::ids::MachineId;
use ployz_core::state::MachineEndpointObservation;
use ployz_core::subjects::MachineServiceEndpoint;
use ployz_nats::service_runtime::{
    EndpointExecutionPolicy, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    NatsServiceShutdownError, RunningNatsService, start_nats_service,
};
use std::future::Future;
use std::num::NonZeroUsize;
use std::time::Duration;

// The request carries the operation-owned execution timeout. This outer bound
// limits malformed or future callers that fail to supply a useful inner bound.
const PRE_START_HOOK_ENDPOINT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub use super::facts::MachineFactsReadError;

const DATAPLANE_STATUS_ENDPOINT_TIMEOUT: Duration =
    dataplane_status_budget(ployz_core::dataplane::NetworkStatusMode::ProbePathMtu)
        .saturating_add(Duration::from_secs(10));

pub struct RunningMachineRoleRuntime {
    service: RunningNatsService,
    projection: RunningProjectionTask,
}

impl RunningMachineRoleRuntime {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.projection.shutdown().await;
        self.service.shutdown().await
    }
}

pub async fn start_machine_role_runtime<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
) -> Result<RunningMachineRoleRuntime, MachineServiceError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    start_machine_role_runtime_with_endpoint_cache(
        client,
        machine_id,
        runner,
        preparer,
        log_reader,
        MachineEndpointCache::default(),
    )
    .await
}

pub async fn start_machine_role_runtime_with_endpoint_observation<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    endpoint_observation: MachineEndpointObservation,
) -> Result<RunningMachineRoleRuntime, MachineServiceError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    start_machine_role_runtime_with_endpoint_cache(
        client,
        machine_id,
        runner,
        preparer,
        log_reader,
        MachineEndpointCache::with_observation(endpoint_observation),
    )
    .await
}

async fn start_machine_role_runtime_with_endpoint_cache<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    endpoint_cache: MachineEndpointCache,
) -> Result<RunningMachineRoleRuntime, MachineServiceError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let projection_state = MachineProjectionState::new();
    let service = start_machine_role_service_with_endpoint_cache_and_image(
        client.clone(),
        machine_id.clone(),
        runner.clone(),
        preparer.clone(),
        log_reader,
        endpoint_cache,
        MachineRoleProjectionServices {
            image_state: None,
            projection_state: projection_state.clone(),
        },
    )
    .await?;
    let projection = start_projection_task(client, machine_id, runner, preparer, projection_state);
    Ok(RunningMachineRoleRuntime {
        service,
        projection,
    })
}

pub async fn start_machine_role_service<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
) -> Result<RunningNatsService, MachineServiceError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    start_machine_role_service_with_endpoint_cache(
        client,
        machine_id,
        runner,
        preparer,
        log_reader,
        MachineEndpointCache::default(),
    )
    .await
}

pub(crate) async fn start_machine_role_service_with_endpoint_cache<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    endpoint_cache: MachineEndpointCache,
) -> Result<RunningNatsService, MachineServiceError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    start_machine_role_service_with_endpoint_cache_and_image(
        client,
        machine_id,
        runner,
        preparer,
        log_reader,
        endpoint_cache,
        MachineRoleProjectionServices {
            image_state: None,
            projection_state: MachineProjectionState::new(),
        },
    )
    .await
}

pub(crate) async fn start_machine_role_service_with_endpoint_cache_and_image<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    endpoint_cache: MachineEndpointCache,
    projection_services: MachineRoleProjectionServices,
) -> Result<RunningNatsService, MachineServiceError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let MachineRoleProjectionServices {
        image_state,
        projection_state,
    } = projection_services;
    let spec = machine_role_service_base(&machine_id);
    let mutation_state = MachineContainerState {
        runner: runner.clone(),
        client: client.clone(),
    };
    let mut runtime = start_nats_service(client.clone(), &spec)
        .await
        .map_err(MachineServiceError::Nats)?;

    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::FactsGet,
        MachineFactsState {
            runner: runner.clone(),
            endpoint_cache: endpoint_cache.clone(),
            client: client.clone(),
        },
        handle_facts_get,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::FactsRefresh,
        MachineFactsState {
            runner: runner.clone(),
            endpoint_cache,
            client: client.clone(),
        },
        handle_facts_refresh,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerInspect,
        runner.clone(),
        handle_container_inspect,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerResolveImage,
        runner.clone(),
        handle_container_resolve_image,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerRun,
        mutation_state.clone(),
        handle_container_run,
    )
    .await?;
    let hook_spec = machine_endpoint_spec(&machine_id, MachineServiceEndpoint::ContainerRunHook);
    let hook_machine_id = machine_id.clone();
    let hook_state = mutation_state.clone();
    runtime
        .bind_endpoint_with_policy(
            &hook_spec,
            EndpointExecutionPolicy::new(NonZeroUsize::MIN, PRE_START_HOOK_ENDPOINT_TIMEOUT),
            move |request| {
                handle_container_run_hook(hook_machine_id.clone(), hook_state.clone(), request)
            },
        )
        .await
        .map_err(MachineServiceError::Nats)?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerStop,
        mutation_state.clone(),
        handle_container_stop,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerRestart,
        mutation_state.clone(),
        handle_container_restart,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerRemove,
        mutation_state,
        handle_container_remove,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::VolumeRemove,
        runner.clone(),
        handle_volume_remove,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::DataplanePublicKey,
        preparer.clone(),
        handle_dataplane_public_key,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::DataplaneStatus,
        MachineDataplaneStatusState {
            runner: runner.clone(),
            preparer,
            projection: projection_state,
        },
        handle_dataplane_status,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::SubstrateUpdate,
        (),
        handle_substrate_update,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::SubstrateReport,
        (),
        handle_substrate_report,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::LogsTail,
        (runner, log_reader),
        handle_logs_tail,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ImageBlobCheck,
        image_state.clone(),
        handle_image_blob_check,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ImageBlobPush,
        image_state.clone(),
        handle_image_blob_push,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ImageManifestPush,
        image_state.clone(),
        handle_image_manifest_push,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ImageEnsure,
        image_state,
        handle_image_ensure,
    )
    .await?;

    Ok(runtime)
}

/// Bind one machine-scoped endpoint, handing every request the machine id and a
/// clone of the handler's state.
async fn bind_machine_endpoint<S, H, Fut>(
    runtime: &mut RunningNatsService,
    machine_id: &MachineId,
    endpoint: MachineServiceEndpoint,
    state: S,
    handler: H,
) -> Result<(), MachineServiceError>
where
    S: Clone + Send + Sync + 'static,
    H: Fn(MachineId, S, NatsServiceRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = NatsServiceResponse> + Send + 'static,
{
    let spec = machine_endpoint_spec(machine_id, endpoint);
    let policy = machine_endpoint_policy(endpoint);
    let machine_id = machine_id.clone();
    runtime
        .bind_endpoint_with_policy(&spec, policy, move |request| {
            handler(machine_id.clone(), state.clone(), request)
        })
        .await
        .map_err(MachineServiceError::Nats)
}

fn machine_endpoint_policy(endpoint: MachineServiceEndpoint) -> EndpointExecutionPolicy {
    let mut policy = EndpointExecutionPolicy::default();
    if endpoint == MachineServiceEndpoint::DataplaneStatus {
        policy.request_timeout = DATAPLANE_STATUS_ENDPOINT_TIMEOUT;
    }
    policy
}

pub trait MachinePloyzNativeMeshPreparer {
    fn read_wireguard_public_key(
        &self,
    ) -> impl Future<Output = Result<WireGuardPublicKey, WireGuardEbpfPrepareError>> + Send;

    fn prepare_ployz_native_mesh(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> impl Future<Output = Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError>> + Send;

    fn read_ployz_native_mesh_status(
        &self,
        mode: ployz_core::dataplane::NetworkStatusMode,
    ) -> impl Future<Output = Result<ployz_core::dataplane::MachineDataplaneStatus, String>> + Send;

    fn prepare_wireguard(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> impl Future<Output = Result<WireGuardReady, WireGuardEbpfPrepareError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineServiceError {
    #[error("failed to start machine service: {0:?}")]
    Nats(NatsServiceRuntimeError),
}

pub(crate) struct MachineRoleProjectionServices {
    pub image_state: Option<AvailableImageService>,
    pub projection_state: MachineProjectionState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataplane_status_endpoint_timeout_covers_snapshot_and_probe_budgets() {
        let policy = machine_endpoint_policy(MachineServiceEndpoint::DataplaneStatus);

        assert!(
            policy.request_timeout
                > dataplane_status_budget(ployz_core::dataplane::NetworkStatusMode::Snapshot)
                && policy.request_timeout
                    > dataplane_status_budget(
                        ployz_core::dataplane::NetworkStatusMode::ProbePathMtu,
                    )
        );
    }
}
