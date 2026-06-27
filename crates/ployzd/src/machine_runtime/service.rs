//! NATS Service API runtime wiring for machine-local commands.

use crate::machine_runtime::protocol::{
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerRemoveRpcResponse, MachineContainerRpcOk, MachineContainerRunDomainError,
    MachineContainerRunRpcOk, MachineContainerRunRpcRequest, MachineContainerRunRpcResponse,
    MachineContainerStopDomainError, MachineContainerStopRpcRequest,
    MachineContainerStopRpcResponse, MachineEnsureEndpointNetworkDomainError,
    MachineEnsureEndpointNetworkRpcOk, MachineEnsureEndpointNetworkRpcRequest,
    MachineEnsureEndpointNetworkRpcResponse, MachineLogsTailDomainError, MachineLogsTailResult,
    MachineLogsTailRpcOk, MachineLogsTailRpcRequest, MachineLogsTailRpcResponse,
    MachineRunContainerOutcome, MachineWireGuardEbpfPrepareDomainError,
    MachineWireGuardEbpfPreparePhase, MachineWireGuardEbpfPrepareRpcRequest,
    MachineWireGuardEbpfPrepareRpcResponse,
};
use crate::machine_runtime::runner::{
    CreateManagedContainer, MachineContainerRunDecision, MachineContainerRunner,
    MachineContainerRunnerError, MachineLogReader, MachineLogReaderError, MachineLogTail,
    decide_container_run, managed_container_labels,
};
use crate::services::{machine_endpoint_spec, machine_runtime_service_base};
use ployz_core::dataplane::{
    WireGuardEbpfEndpointRoute, WireGuardEbpfMachineReady, WireGuardEbpfPrepareError,
    WireGuardEbpfReady, WireGuardPeer, WireGuardPublicKey,
};
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::ops::{FailureMessage, OperatorHint};
use ployz_core::subjects::MachineServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, decode_json_request, start_nats_service,
};
use std::future::Future;

pub async fn start_machine_runtime_service<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
) -> Result<RunningNatsService, MachineServiceRuntimeError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone + MachineWireGuardEbpfPreparer + Send + Sync + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let spec = machine_runtime_service_base(&machine_id);
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(MachineServiceRuntimeError::Nats)?;

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
        MachineServiceEndpoint::WireGuardEbpfPrepare,
        preparer,
        handle_wireguard_ebpf_prepare,
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
    let labels = managed_container_labels(&request.container, request.endpoint.as_ref());

    match decide_container_run(&labels, existing) {
        MachineContainerRunDecision::Create { labels } => {
            match runner
                .create_managed_container(CreateManagedContainer {
                    image: request.image,
                    endpoint: request.endpoint,
                    labels,
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

pub trait MachineWireGuardEbpfPreparer {
    fn read_wireguard_public_key(
        &self,
    ) -> impl Future<Output = Result<WireGuardPublicKey, WireGuardEbpfPrepareError>> + Send;

    fn prepare_wireguard_ebpf(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> impl Future<Output = Result<WireGuardEbpfReady, WireGuardEbpfPrepareError>> + Send;
}

async fn handle_wireguard_ebpf_prepare<P>(
    machine_id: MachineId,
    preparer: P,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    P: MachineWireGuardEbpfPreparer,
{
    let request = match decode_json_request::<MachineWireGuardEbpfPrepareRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    if !request
        .machines
        .iter()
        .any(|candidate| candidate == &machine_id)
    {
        return machine_domain_error(MachineWireGuardEbpfPrepareRpcResponse::DomainError {
            machine_id: machine_id.clone(),
            error: MachineWireGuardEbpfPrepareDomainError::Unavailable {
                component: ployz_core::dataplane::WireGuardEbpfComponent::WireGuard,
                message: failure_message(
                    "wireguard/eBPF prepare request did not target this machine",
                ),
            },
        });
    }

    match request.phase {
        MachineWireGuardEbpfPreparePhase::ReadPublicKey => {
            match preparer.read_wireguard_public_key().await {
                Ok(public_key) => {
                    machine_success(MachineWireGuardEbpfPrepareRpcResponse::PublicKey {
                        machine_id,
                        public_key,
                    })
                }
                Err(error) => {
                    machine_domain_error(MachineWireGuardEbpfPrepareRpcResponse::DomainError {
                        machine_id,
                        error: error.into(),
                    })
                }
            }
        }
        MachineWireGuardEbpfPreparePhase::PrepareDataplane => {
            match preparer
                .prepare_wireguard_ebpf(&request.endpoint_routes, &request.peers)
                .await
            {
                Ok(ready) => machine_success(MachineWireGuardEbpfPrepareRpcResponse::Ok {
                    readiness: WireGuardEbpfMachineReady { machine_id, ready },
                }),
                Err(error) => {
                    machine_domain_error(MachineWireGuardEbpfPrepareRpcResponse::DomainError {
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
