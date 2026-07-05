//! NATS Service API runtime wiring for machine-local commands.

use crate::machine_runtime::protocol::{
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerRemoveRpcResponse, MachineContainerRpcOk, MachineContainerRunDomainError,
    MachineContainerRunRpcOk, MachineContainerRunRpcRequest, MachineContainerRunRpcResponse,
    MachineContainerStopDomainError, MachineContainerStopRpcRequest,
    MachineContainerStopRpcResponse, MachineDataplanePrepareRpcRequest,
    MachineDataplanePrepareRpcResponse, MachineEnsureEndpointNetworkDomainError,
    MachineEnsureEndpointNetworkRpcOk, MachineEnsureEndpointNetworkRpcRequest,
    MachineEnsureEndpointNetworkRpcResponse, MachineFactsGetDomainError, MachineFactsGetRpcOk,
    MachineFactsGetRpcRequest, MachineFactsGetRpcResponse, MachineLogsTailDomainError,
    MachineLogsTailResult, MachineLogsTailRpcOk, MachineLogsTailRpcRequest,
    MachineLogsTailRpcResponse, MachinePlacementBidRpcOk, MachinePlacementBidRpcRequest,
    MachinePlacementBidRpcResponse, MachinePloyzNativeMeshPrepareDomainError,
    MachinePloyzNativeMeshPrepareRpcOk, MachinePloyzNativeMeshPrepareRpcRequest,
    MachineRunContainerOutcome, MachineSubstrateReportRpcOk, MachineSubstrateReportRpcRequest,
    MachineSubstrateReportRpcResponse, MachineSubstrateUpdateDomainError,
    MachineSubstrateUpdateRpcOk, MachineSubstrateUpdateRpcRequest,
    MachineSubstrateUpdateRpcResponse,
};
use crate::machine_runtime::runner::{
    CreateManagedContainer, ExistingManagedContainerState, MachineContainerRunDecision,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail, decide_container_run,
};
use crate::services::{machine_endpoint_spec, machine_runtime_service_base};
use ployz_core::dataplane::{
    PloyzNativeMeshMachineReady, PloyzNativeMeshReady, WireGuardEbpfEndpointRoute,
    WireGuardEbpfPrepareError, WireGuardPeer, WireGuardPublicKey,
};
use ployz_core::ids::{ContainerId, MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    MachineContainerObservationSnapshotError, MachineFactsSnapshot, MachineFactsSnapshotError,
    ManagedContainerObservation,
};
use ployz_core::ops::{FailureMessage, MachineSubstrateVersions, OperatorHint};
use ployz_core::state::MachinePublicIpObservation;
use ployz_core::subjects::MachineServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, decode_json_request, start_nats_service,
};
use serde::Deserialize;
use std::future::Future;
use std::net::IpAddr;
use std::path::Path;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

const SUBSTRATE_VERSION_FILE: &str = "/var/lib/ployz/substrate-version.json";

pub async fn start_machine_runtime_service<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
) -> Result<RunningNatsService, MachineServiceRuntimeError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    start_machine_runtime_service_with_public_ip(
        client, machine_id, runner, preparer, log_reader, None,
    )
    .await
}

pub async fn start_machine_runtime_service_with_public_ip<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    public_ip: Option<IpAddr>,
) -> Result<RunningNatsService, MachineServiceRuntimeError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let spec = machine_runtime_service_base(&machine_id);
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(MachineServiceRuntimeError::Nats)?;

    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::FactsGet,
        MachineFactsState {
            runner: runner.clone(),
            public_ip,
        },
        handle_facts_get,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::PlacementBid,
        (),
        handle_placement_bid,
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
        MachineServiceEndpoint::ContainerRun,
        runner.clone(),
        handle_container_run,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerStop,
        runner.clone(),
        handle_container_stop,
    )
    .await?;
    bind_machine_endpoint(
        &mut runtime,
        &machine_id,
        MachineServiceEndpoint::ContainerRemove,
        runner.clone(),
        handle_container_remove,
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

#[derive(Clone)]
struct MachineFactsState<R> {
    runner: R,
    public_ip: Option<IpAddr>,
}

/// Bind one machine-scoped endpoint, handing every request the machine id and a
/// clone of the handler's state.
async fn bind_machine_endpoint<S, H, Fut>(
    runtime: &mut RunningNatsService,
    machine_id: &MachineId,
    endpoint: MachineServiceEndpoint,
    state: S,
    handler: H,
) -> Result<(), MachineServiceRuntimeError>
where
    S: Clone + Send + Sync + 'static,
    H: Fn(MachineId, S, NatsServiceRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = NatsServiceResponse> + Send + 'static,
{
    let spec = machine_endpoint_spec(machine_id, endpoint);
    let machine_id = machine_id.clone();
    runtime
        .bind_endpoint(&spec, move |request| {
            handler(machine_id.clone(), state.clone(), request)
        })
        .await
        .map_err(MachineServiceRuntimeError::Nats)
}

async fn handle_placement_bid(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    if let Err(response) = decode_json_request::<MachinePlacementBidRpcRequest>(&request) {
        return response;
    }

    machine_success(MachinePlacementBidRpcResponse::Ok(
        MachinePlacementBidRpcOk { machine_id },
    ))
}

async fn handle_substrate_update(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineSubstrateUpdateRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let update = tokio::task::spawn_blocking(move || {
        std::process::Command::new("ployz-keeper")
            .arg("substrate-update")
            .arg("--operation-id")
            .arg(request.operation_id.as_str())
            .arg("--version")
            .arg(request.target_version.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    })
    .await;

    match update {
        Ok(Ok(_child)) => machine_success(MachineSubstrateUpdateRpcResponse::Ok(
            MachineSubstrateUpdateRpcOk { machine_id },
        )),
        Ok(Err(error)) => machine_domain_error(MachineSubstrateUpdateRpcResponse::DomainError {
            machine_id,
            error: MachineSubstrateUpdateDomainError::UpdateFailed {
                message: FailureMessage::try_new(format!("failed to run ployz-keeper: {error}"))
                    .expect("process failure message is non-empty"),
            },
        }),
        Err(error) => machine_domain_error(MachineSubstrateUpdateRpcResponse::DomainError {
            machine_id,
            error: MachineSubstrateUpdateDomainError::UpdateFailed {
                message: FailureMessage::try_new(format!("substrate update task failed: {error}"))
                    .expect("task failure message is non-empty"),
            },
        }),
    }
}

async fn handle_substrate_report(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineSubstrateReportRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let reported = match read_substrate_update_evidence(&request.operation_id) {
        Ok(reported) => reported,
        Err(message) => {
            return machine_domain_error(MachineSubstrateReportRpcResponse::DomainError {
                machine_id,
                error: MachineSubstrateUpdateDomainError::UpdateFailed { message },
            });
        }
    };
    machine_success(MachineSubstrateReportRpcResponse::Ok(
        MachineSubstrateReportRpcOk {
            machine_id,
            reported,
        },
    ))
}

async fn handle_facts_get<R>(
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

    match read_machine_facts_snapshot(
        &machine_id,
        &state.runner,
        state.public_ip,
        current_unix_ms(),
    )
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

pub(crate) async fn read_machine_facts_snapshot<R>(
    machine_id: &MachineId,
    runner: &R,
    public_ip: Option<IpAddr>,
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
    let public_ip = public_ip.map(|public_ip| MachinePublicIpObservation {
        machine_id: machine_id.clone(),
        public_ip,
    });

    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        containers,
        public_ip,
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

#[derive(Deserialize)]
struct SubstrateUpdateEvidence {
    operation_id: OperationId,
    ployzd: InstallArtifactVersion,
}

fn read_substrate_update_evidence(
    operation_id: &OperationId,
) -> Result<MachineSubstrateVersions, FailureMessage> {
    let path = Path::new(SUBSTRATE_VERSION_FILE);
    if !path.exists() {
        return Ok(MachineSubstrateVersions::default());
    }
    let bytes = std::fs::read(path).map_err(|error| {
        FailureMessage::try_new(format!(
            "failed to read substrate update evidence {}: {error}",
            path.display()
        ))
        .expect("substrate update evidence read message is non-empty")
    })?;
    let evidence: SubstrateUpdateEvidence = serde_json::from_slice(&bytes).map_err(|error| {
        FailureMessage::try_new(format!(
            "failed to decode substrate update evidence {}: {error}",
            path.display()
        ))
        .expect("substrate update evidence decode message is non-empty")
    })?;
    if &evidence.operation_id != operation_id {
        return Ok(MachineSubstrateVersions::default());
    }
    Ok(MachineSubstrateVersions {
        ployzd: Some(evidence.ployzd),
        keeper: None,
    })
}

async fn handle_ensure_endpoint_network<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    if let Err(response) = decode_json_request::<MachineEnsureEndpointNetworkRpcRequest>(&request) {
        return response;
    }

    match runner.ensure_endpoint_network().await {
        Ok(()) => machine_success(MachineEnsureEndpointNetworkRpcResponse::Ok(
            MachineEnsureEndpointNetworkRpcOk { machine_id },
        )),
        Err(MachineContainerRunnerError::EnsureEndpointNetwork { message }) => {
            machine_domain_error(MachineEnsureEndpointNetworkRpcResponse::DomainError {
                machine_id,
                error: MachineEnsureEndpointNetworkDomainError::EnsureFailed {
                    message: failure_message(format!("endpoint network ensure failed: {message}")),
                },
            })
        }
        Err(error) => runner_error(error),
    }
}

async fn handle_container_run<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerRunRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let existing = match runner.existing_managed_containers().await {
        Ok(existing) => existing,
        Err(error) => return runner_error(error),
    };
    match decide_container_run(&request.container, existing) {
        MachineContainerRunDecision::Create { identity } => {
            match runner
                .create_managed_container(CreateManagedContainer {
                    image: request.image,
                    identity,
                })
                .await
            {
                Ok(container_id) => match runner.start_managed_container(&container_id).await {
                    Ok(()) => machine_success(container_run_ok(
                        machine_id,
                        MachineRunContainerOutcome::Created { container_id },
                    )),
                    Err(error) => container_start_error(
                        machine_id,
                        container_id,
                        error,
                        |container_id, message, inspect_hint| {
                            MachineContainerRunDomainError::CreatedContainerStartFailed {
                                container_id,
                                message,
                                inspect_hint,
                            }
                        },
                    ),
                },
                Err(error) => runner_error(error),
            }
        }
        MachineContainerRunDecision::ReuseRunning { container_id } => {
            machine_success(container_run_ok(
                machine_id,
                MachineRunContainerOutcome::ReusedRunning { container_id },
            ))
        }
        MachineContainerRunDecision::StartExisting { container_id } => {
            match runner.start_managed_container(&container_id).await {
                Ok(()) => machine_success(container_run_ok(
                    machine_id,
                    MachineRunContainerOutcome::StartedExisting { container_id },
                )),
                Err(error) => container_start_error(
                    machine_id,
                    container_id,
                    error,
                    |container_id, message, inspect_hint| {
                        MachineContainerRunDomainError::ExistingContainerStartFailed {
                            container_id,
                            message,
                            inspect_hint,
                        }
                    },
                ),
            }
        }
        MachineContainerRunDecision::NotStartable {
            container_id,
            state,
        } => machine_domain_error(MachineContainerRunRpcResponse::DomainError {
            machine_id,
            error: MachineContainerRunDomainError::OperationStepContainerNotStartable {
                container_id: container_id.clone(),
                message: failure_message(format!(
                    "operation step container is not startable: {state:?}"
                )),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        MachineContainerRunDecision::Conflict(conflict) => {
            machine_domain_error(MachineContainerRunRpcResponse::DomainError {
                machine_id,
                error: MachineContainerRunDomainError::OperationStepConflict {
                    container_id: conflict.container_id,
                    expected: conflict.expected,
                    actual: conflict.actual,
                },
            })
        }
        MachineContainerRunDecision::Ambiguous {
            operation_id,
            step_id,
            container_ids,
        } => machine_domain_error(MachineContainerRunRpcResponse::DomainError {
            machine_id,
            error: MachineContainerRunDomainError::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            },
        }),
    }
}

async fn handle_container_remove<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerRemoveRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match runner
        .remove_managed_container(&request.container_id, &request.expected_identity)
        .await
    {
        Ok(()) => machine_success(MachineContainerRemoveRpcResponse::Ok(
            MachineContainerRpcOk {
                machine_id,
                container_id: request.container_id,
            },
        )),
        Err(MachineContainerRunnerError::Remove {
            container_id,
            message,
        }) => machine_domain_error(MachineContainerRemoveRpcResponse::DomainError {
            machine_id,
            error: MachineContainerRemoveDomainError::RemoveFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("container remove failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        Err(error) => runner_error(error),
    }
}

async fn handle_container_stop<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerStopRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match runner
        .stop_managed_container(&request.container_id, &request.expected_identity)
        .await
    {
        Ok(()) => machine_success(MachineContainerStopRpcResponse::Ok(MachineContainerRpcOk {
            machine_id,
            container_id: request.container_id,
        })),
        Err(MachineContainerRunnerError::Stop {
            container_id,
            message,
        }) => machine_domain_error(MachineContainerStopRpcResponse::DomainError {
            machine_id,
            error: MachineContainerStopDomainError::StopFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("container stop failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        Err(error) => runner_error(error),
    }
}

async fn handle_logs_tail<R, L>(
    machine_id: MachineId,
    ports: (R, L),
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
    L: MachineLogReader,
{
    let (runner, log_reader) = ports;
    let request = match decode_json_request::<MachineLogsTailRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    let existing = match runner.existing_managed_containers().await {
        Ok(existing) => existing,
        Err(error) => return runner_error(error),
    };
    if !existing
        .iter()
        .any(|container| container.container_id == request.container_id)
    {
        return machine_domain_error(MachineLogsTailRpcResponse::DomainError {
            machine_id,
            error: MachineLogsTailDomainError::NotFound {
                container_id: request.container_id,
            },
        });
    }

    match log_reader
        .tail_container_logs(&request.container_id, request.tail_lines)
        .await
    {
        Ok(MachineLogTail { text, truncated }) => {
            machine_success(MachineLogsTailRpcResponse::Ok(MachineLogsTailRpcOk {
                value: MachineLogsTailResult {
                    machine_id,
                    container_id: request.container_id,
                    text,
                    truncated,
                },
            }))
        }
        Err(MachineLogReaderError::NotFound { container_id }) => {
            machine_domain_error(MachineLogsTailRpcResponse::DomainError {
                machine_id,
                error: MachineLogsTailDomainError::NotFound { container_id },
            })
        }
        Err(MachineLogReaderError::ReadFailed {
            container_id,
            message,
        }) => machine_domain_error(MachineLogsTailRpcResponse::DomainError {
            machine_id,
            error: MachineLogsTailDomainError::ReadFailed {
                container_id,
                message: failure_message(message),
            },
        }),
    }
}

fn container_run_ok(
    machine_id: MachineId,
    outcome: MachineRunContainerOutcome,
) -> MachineContainerRunRpcResponse {
    MachineContainerRunRpcResponse::Ok(MachineContainerRunRpcOk {
        machine_id,
        outcome,
    })
}

fn machine_success(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_ok(&response)
}

fn machine_domain_error(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_domain_error(&response)
}

fn runner_error(error: MachineContainerRunnerError) -> NatsServiceResponse {
    match error {
        MachineContainerRunnerError::ListExisting { message } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "container list failed: {message}"
            )))
        }
        MachineContainerRunnerError::EnsureEndpointNetwork { message } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "endpoint network ensure failed: {message}"
            )))
        }
        MachineContainerRunnerError::Create { message } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container create failed: {message}")),
        ),
        MachineContainerRunnerError::Start { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container start failed: {message}")),
        ),
        MachineContainerRunnerError::Stop { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container stop failed: {message}")),
        ),
        MachineContainerRunnerError::Remove { message, .. } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "container remove failed: {message}"
            )))
        }
    }
}

/// Map a start failure to the endpoint's domain error, parameterized by which
/// start-failed variant (created vs existing) the caller is reporting.
fn container_start_error(
    machine_id: MachineId,
    container_id: ContainerId,
    error: MachineContainerRunnerError,
    start_failed: impl FnOnce(
        ContainerId,
        FailureMessage,
        OperatorHint,
    ) -> MachineContainerRunDomainError,
) -> NatsServiceResponse {
    match error {
        MachineContainerRunnerError::Start { message, .. } => {
            machine_domain_error(MachineContainerRunRpcResponse::DomainError {
                machine_id,
                error: start_failed(
                    container_id.clone(),
                    failure_message(format!("container start failed: {message}")),
                    inspect_hint(&container_id),
                ),
            })
        }
        error @ (MachineContainerRunnerError::ListExisting { .. }
        | MachineContainerRunnerError::EnsureEndpointNetwork { .. }
        | MachineContainerRunnerError::Create { .. }
        | MachineContainerRunnerError::Stop { .. }
        | MachineContainerRunnerError::Remove { .. }) => runner_error(error),
    }
}

fn failure_message(value: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(value).expect("generated failure message is non-empty")
}

fn inspect_hint(container_id: &ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {}", container_id.as_str()))
        .expect("generated inspect hint is non-empty")
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
}

async fn handle_dataplane_prepare<P>(
    machine_id: MachineId,
    preparer: P,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    P: MachinePloyzNativeMeshPreparer,
{
    let request = match decode_json_request::<MachineDataplanePrepareRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if !request
        .machines
        .iter()
        .any(|candidate| candidate == &machine_id)
    {
        return machine_domain_error(MachineDataplanePrepareRpcResponse::DomainError {
            machine_id: machine_id.clone(),
            error: MachinePloyzNativeMeshPrepareDomainError::Unavailable {
                component: ployz_core::dataplane::PloyzNativeMeshComponent::WireGuard,
                message: failure_message(
                    "ployz native mesh prepare request did not target this machine",
                ),
            },
        });
    }

    match request.request {
        MachinePloyzNativeMeshPrepareRpcRequest::ReadPublicKey => {
            match preparer.read_wireguard_public_key().await {
                Ok(public_key) => machine_success(MachineDataplanePrepareRpcResponse::Ok(
                    MachinePloyzNativeMeshPrepareRpcOk::PublicKey {
                        machine_id,
                        public_key,
                    },
                )),
                Err(error) => {
                    machine_domain_error(MachineDataplanePrepareRpcResponse::DomainError {
                        machine_id,
                        error: error.into(),
                    })
                }
            }
        }
        MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
            endpoint_routes,
            peers,
        } => {
            match preparer
                .prepare_ployz_native_mesh(&endpoint_routes, &peers)
                .await
            {
                Ok(ready) => machine_success(MachineDataplanePrepareRpcResponse::Ok(
                    MachinePloyzNativeMeshPrepareRpcOk::Ready {
                        readiness: PloyzNativeMeshMachineReady { machine_id, ready },
                    },
                )),
                Err(error) => {
                    machine_domain_error(MachineDataplanePrepareRpcResponse::DomainError {
                        machine_id,
                        error: error.into(),
                    })
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineServiceRuntimeError {
    Nats(NatsServiceRuntimeError),
}

impl std::fmt::Display for MachineServiceRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nats(error) => {
                write!(formatter, "failed to start machine service: {error:?}")
            }
        }
    }
}

impl std::error::Error for MachineServiceRuntimeError {}
