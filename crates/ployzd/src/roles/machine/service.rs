//! NATS Service API wiring for machine-local commands.

use super::containers::{
    MachineContainerState, handle_container_inspect, handle_container_remove,
    handle_container_restart, handle_container_run, handle_container_run_hook,
    handle_container_stop, handle_ensure_endpoint_network, handle_volume_remove,
};
use super::dataplane::{handle_dataplane_mtu_probe, handle_dataplane_prepare};
use super::facts::{MachineEndpointCache, MachineFactsState, handle_facts_get};
use super::logs::handle_logs_tail;
use super::substrate::{handle_substrate_report, handle_substrate_update};
use crate::roles::machine::runner::{MachineContainerRunner, MachineLogReader};
use crate::service_catalog::{machine_endpoint_spec, machine_role_service_base};
use ployz_core::dataplane::{
    OVERLAY_CONNECTIVITY_PROOF_BUDGET, PloyzNativeMeshReady, WireGuardEbpfEndpointRoute,
    WireGuardEbpfPrepareError, WireGuardPeer, WireGuardPublicKey, WireGuardReady,
};
use ployz_core::ids::MachineId;
use ployz_core::subjects::MachineServiceEndpoint;
use ployz_nats::service_runtime::{
    EndpointExecutionPolicy, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, start_nats_service,
};
use std::future::Future;
use std::net::Ipv4Addr;
use std::num::NonZeroUsize;
use std::time::Duration;

// The request carries the operation-owned execution timeout. This outer bound
// limits malformed or future callers that fail to supply a useful inner bound.
const PRE_START_HOOK_ENDPOINT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub use super::facts::MachineFactsReadError;

const DATAPLANE_REQUEST_TIMEOUT: Duration =
    OVERLAY_CONNECTIVITY_PROOF_BUDGET.saturating_add(Duration::from_secs(15));

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
    let spec = machine_role_service_base(&machine_id);
    let mutation_state = MachineContainerState {
        runner: runner.clone(),
        client: client.clone(),
    };
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(MachineServiceError::Nats)?;

    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::FactsGet,
        MachineFactsState {
            runner: runner.clone(),
            endpoint_cache,
        },
        handle_facts_get,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerEnsureEndpointNetwork,
        runner.clone(),
        handle_ensure_endpoint_network,
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
        MachineServiceEndpoint::DataplaneProbeMtu,
        preparer.clone(),
        handle_dataplane_mtu_probe,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::DataplanePrepare,
        preparer,
        handle_dataplane_prepare,
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
    if endpoint == MachineServiceEndpoint::DataplanePrepare {
        policy.request_timeout = DATAPLANE_REQUEST_TIMEOUT;
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

    fn prepare_wireguard(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> impl Future<Output = Result<WireGuardReady, WireGuardEbpfPrepareError>> + Send;

    fn probe_overlay(
        &self,
        peers: &[WireGuardPublicKey],
    ) -> impl Future<Output = Result<Vec<WireGuardPublicKey>, WireGuardEbpfPrepareError>> + Send;

    fn probe_link_mtu(
        &self,
        peer_gateway: Ipv4Addr,
    ) -> impl Future<Output = Result<u32, WireGuardEbpfPrepareError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineServiceError {
    #[error("failed to start machine service: {0:?}")]
    Nats(NatsServiceRuntimeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataplane_endpoint_timeout_covers_the_overlay_probe_budget() {
        let policy = machine_endpoint_policy(MachineServiceEndpoint::DataplanePrepare);

        assert!(policy.request_timeout > OVERLAY_CONNECTIVITY_PROOF_BUDGET);
    }
}
