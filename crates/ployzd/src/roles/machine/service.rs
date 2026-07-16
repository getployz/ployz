//! NATS Service API wiring for machine-local commands.

use super::build::{MachineBuildRuntime, handle_build_cancel, handle_build_start};
use super::containers::{
    MachineContainerState, handle_container_inspect, handle_container_remove,
    handle_container_resolve_image, handle_container_restart, handle_container_run,
    handle_container_run_hook, handle_container_stop, handle_volume_ensure, handle_volume_remove,
};
use super::dataplane::{
    MachineDataplaneStatusState, handle_dataplane_public_key, handle_dataplane_status,
};
use super::facts::{
    MachineEndpointCache, MachineFactsGetState, MachineFactsState, handle_facts_get,
    handle_facts_refresh,
};
use super::images::{
    AvailableImageService, handle_image_blob_check, handle_image_blob_push, handle_image_ensure,
    handle_image_manifest_push, handle_image_remove,
};
use super::logs::handle_logs_tail;
use super::substrate::{
    handle_storage_prepare, handle_storage_prepare_report, handle_substrate_report,
    handle_substrate_update,
};
use super::volume::DATASET_ENSURE_HOST_COMMAND_TIMEOUT;
use crate::roles::machine::execution::host_dataplane::dataplane_status_budget;
use crate::roles::machine::projection::MachineProjectionState;
#[cfg(test)]
use crate::roles::machine::projection::{RunningProjectionTask, start_projection_task};
use crate::roles::machine::runner::{
    MachineContainerRunner, MachineImageRemovalRunner, MachineLogReader,
};
use crate::service_catalog::{machine_endpoint_spec, machine_role_service_base};
use ployz_core::build::BUILD_START_ENDPOINT_TIMEOUT;
use ployz_core::ids::MachineId;
#[cfg(test)]
use ployz_core::machine::MachineEndpointObservation;
use ployz_core::network::{
    PloyzNativeMeshReady, WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError, WireGuardPeer,
    WireGuardPublicKey, WireGuardReady,
};
#[cfg(test)]
use ployz_nats::service_runtime::NatsServiceShutdownError;
use ployz_nats::service_runtime::{
    EndpointExecutionPolicy, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, start_nats_service,
};
use ployz_nats::subjects::MachineServiceEndpoint;
use std::future::Future;
use std::num::NonZeroUsize;
use std::time::Duration;

// The request carries the operation-owned execution timeout. This outer bound
// limits malformed or future callers that fail to supply a useful inner bound.
const PRE_START_HOOK_ENDPOINT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub use super::facts::MachineFactsReadError;

const DATAPLANE_STATUS_ENDPOINT_TIMEOUT: Duration =
    dataplane_status_budget(ployz_core::network::NetworkStatusMode::ProbePathMtu)
        .saturating_add(Duration::from_secs(10));
const VOLUME_ENSURE_ENDPOINT_TIMEOUT: Duration =
    DATASET_ENSURE_HOST_COMMAND_TIMEOUT.saturating_add(Duration::from_secs(30));

#[cfg(test)]
pub struct RunningMachineRoleRuntime {
    service: RunningNatsService,
    projection: RunningProjectionTask,
}

#[cfg(test)]
impl RunningMachineRoleRuntime {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.projection.shutdown().await;
        self.service.shutdown().await
    }
}

#[cfg(test)]
pub async fn start_machine_role_runtime<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
) -> Result<RunningMachineRoleRuntime, MachineServiceError>
where
    R: Clone + MachineContainerRunner + MachineImageRemovalRunner + Send + Sync + 'static,
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

#[cfg(test)]
pub async fn start_machine_role_runtime_with_endpoint_observation<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    endpoint_observation: MachineEndpointObservation,
) -> Result<RunningMachineRoleRuntime, MachineServiceError>
where
    R: Clone + MachineContainerRunner + MachineImageRemovalRunner + Send + Sync + 'static,
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

#[cfg(test)]
async fn start_machine_role_runtime_with_endpoint_cache<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    endpoint_cache: MachineEndpointCache,
) -> Result<RunningMachineRoleRuntime, MachineServiceError>
where
    R: Clone + MachineContainerRunner + MachineImageRemovalRunner + Send + Sync + 'static,
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
            build_state: None,
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

#[cfg(test)]
pub async fn start_machine_role_service<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
) -> Result<RunningNatsService, MachineServiceError>
where
    R: Clone + MachineContainerRunner + MachineImageRemovalRunner + Send + Sync + 'static,
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

#[cfg(test)]
pub(crate) async fn start_machine_role_service_with_endpoint_cache<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    endpoint_cache: MachineEndpointCache,
) -> Result<RunningNatsService, MachineServiceError>
where
    R: Clone + MachineContainerRunner + MachineImageRemovalRunner + Send + Sync + 'static,
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
            build_state: None,
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
    R: Clone + MachineContainerRunner + MachineImageRemovalRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let MachineRoleProjectionServices {
        build_state,
        image_state,
        projection_state,
    } = projection_services;
    let build_capability = if build_state.is_some() {
        crate::roles::machine::protocol::MachineBuildCapability::Available
    } else {
        crate::roles::machine::protocol::MachineBuildCapability::Unavailable
    };
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
        MachineServiceEndpoint::BuildStart,
        build_state.clone(),
        handle_build_start,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::BuildCancel,
        build_state,
        handle_build_cancel,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::FactsGet,
        MachineFactsGetState {
            facts: MachineFactsState {
                runner: runner.clone(),
                endpoint_cache: endpoint_cache.clone(),
                client: client.clone(),
            },
            build: build_capability,
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
        MachineServiceEndpoint::VolumeEnsure,
        runner.clone(),
        handle_volume_ensure,
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
        MachineServiceEndpoint::StoragePrepare,
        (),
        handle_storage_prepare,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::StoragePrepareReport,
        (),
        handle_storage_prepare_report,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::LogsTail,
        (runner.clone(), log_reader),
        handle_logs_tail,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ImageRemove,
        runner,
        handle_image_remove,
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
    match endpoint {
        MachineServiceEndpoint::DataplaneStatus => {
            policy.request_timeout = DATAPLANE_STATUS_ENDPOINT_TIMEOUT;
        }
        MachineServiceEndpoint::StoragePrepare => {
            policy.request_timeout = ployz_core::storage::MACHINE_STORAGE_PREPARE_RPC_TIMEOUT;
        }
        MachineServiceEndpoint::BuildStart => {
            policy.request_timeout = BUILD_START_ENDPOINT_TIMEOUT;
        }
        MachineServiceEndpoint::VolumeEnsure => {
            policy.request_timeout = VOLUME_ENSURE_ENDPOINT_TIMEOUT;
        }
        MachineServiceEndpoint::Inspect
        | MachineServiceEndpoint::FactsGet
        | MachineServiceEndpoint::FactsRefresh
        | MachineServiceEndpoint::DnsResolve
        | MachineServiceEndpoint::DnsStatus
        | MachineServiceEndpoint::ContainerInspect
        | MachineServiceEndpoint::ContainerResolveImage
        | MachineServiceEndpoint::ContainerRun
        | MachineServiceEndpoint::ContainerRunHook
        | MachineServiceEndpoint::ContainerRestart
        | MachineServiceEndpoint::ContainerStop
        | MachineServiceEndpoint::ContainerRemove
        | MachineServiceEndpoint::VolumeRemove
        | MachineServiceEndpoint::DataplanePublicKey
        | MachineServiceEndpoint::SubstrateUpdate
        | MachineServiceEndpoint::SubstrateReport
        | MachineServiceEndpoint::StoragePrepareReport
        | MachineServiceEndpoint::LogsTail
        | MachineServiceEndpoint::ImageBlobCheck
        | MachineServiceEndpoint::ImageBlobPush
        | MachineServiceEndpoint::ImageManifestPush
        | MachineServiceEndpoint::ImageEnsure
        | MachineServiceEndpoint::ImageRemove
        | MachineServiceEndpoint::BuildCancel
        | MachineServiceEndpoint::CertificateArtifactStatus
        | MachineServiceEndpoint::CertificateArtifactPush
        | MachineServiceEndpoint::CertificateArtifactRemove
        | MachineServiceEndpoint::CertificateChallengeApply
        | MachineServiceEndpoint::CertificateChallengeRemove
        | MachineServiceEndpoint::CertificateChallengeStatus
        | MachineServiceEndpoint::GatewayStatusGet => {}
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
        mode: ployz_core::network::NetworkStatusMode,
    ) -> impl Future<Output = Result<ployz_core::network::MachineDataplaneStatus, String>> + Send;

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
    pub build_state: Option<MachineBuildRuntime>,
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
                > dataplane_status_budget(ployz_core::network::NetworkStatusMode::Snapshot)
                && policy.request_timeout
                    > dataplane_status_budget(ployz_core::network::NetworkStatusMode::ProbePathMtu,)
        );
    }

    #[test]
    fn storage_prepare_endpoint_covers_the_supervised_child_budget() {
        let policy = machine_endpoint_policy(MachineServiceEndpoint::StoragePrepare);

        assert_eq!(
            policy.request_timeout,
            ployz_core::storage::MACHINE_STORAGE_PREPARE_RPC_TIMEOUT
        );
    }

    #[test]
    fn build_start_endpoint_covers_the_max_operation_and_cleanup_budget() {
        let policy = machine_endpoint_policy(MachineServiceEndpoint::BuildStart);

        assert_eq!(policy.request_timeout, BUILD_START_ENDPOINT_TIMEOUT);
        assert!(policy.request_timeout > ployz_core::build::BUILD_MAX_MACHINE_RESPONSE_LIFETIME);
    }

    #[test]
    fn volume_ensure_endpoint_sits_between_child_and_operation_budgets() {
        let policy = machine_endpoint_policy(MachineServiceEndpoint::VolumeEnsure);

        assert!(policy.request_timeout > super::super::volume::DATASET_ENSURE_HOST_COMMAND_TIMEOUT);
        assert!(policy.request_timeout < crate::config::DEFAULT_DEPLOY_STEP_TIMEOUT);
    }
}
