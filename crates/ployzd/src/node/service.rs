//! NATS Service API runtime wiring for node-local commands.

use crate::node::protocol::{
    NodeContainerRemoveDomainError, NodeContainerRemoveRpcRequest, NodeContainerRemoveRpcResponse,
    NodeContainerRpcOk, NodeContainerRunDomainError, NodeContainerRunRpcOk,
    NodeContainerRunRpcRequest, NodeContainerRunRpcResponse, NodeContainerStopDomainError,
    NodeContainerStopRpcRequest, NodeContainerStopRpcResponse,
    NodeEnsureEndpointNetworkDomainError, NodeEnsureEndpointNetworkRpcOk,
    NodeEnsureEndpointNetworkRpcRequest, NodeEnsureEndpointNetworkRpcResponse,
    NodeLogsTailDomainError, NodeLogsTailResult, NodeLogsTailRpcOk, NodeLogsTailRpcRequest,
    NodeLogsTailRpcResponse, NodeRunContainerOutcome, NodeWireGuardEbpfPrepareDomainError,
    NodeWireGuardEbpfPreparePhase, NodeWireGuardEbpfPrepareRpcRequest,
    NodeWireGuardEbpfPrepareRpcResponse,
};
use crate::node::runner::{
    CreateManagedContainer, NodeContainerRunDecision, NodeContainerRunner,
    NodeContainerRunnerError, NodeLogReader, NodeLogReaderError, NodeLogTail, decide_container_run,
    managed_container_labels,
};
use crate::services::{node_endpoint_spec, node_runtime_service_base};
use ployz_core::dataplane::{
    WireGuardEbpfEndpointRoute, WireGuardEbpfNodeReady, WireGuardEbpfPrepareError,
    WireGuardEbpfReady, WireGuardPeer, WireGuardPublicKey,
};
use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::ops::{FailureMessage, OperatorHint};
use ployz_core::subjects::NodeServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, decode_json_request, start_nats_service,
};
use std::future::Future;

pub async fn start_node_runtime_service<R, P, L>(
    client: ployz_nats::service_runtime::NatsClient,
    node_id: NodeId,
    runner: R,
    preparer: P,
    log_reader: L,
) -> Result<RunningNatsService, NodeServiceRuntimeError>
where
    R: Clone + NodeContainerRunner + Send + Sync + 'static,
    P: Clone + NodeWireGuardEbpfPreparer + Send + Sync + 'static,
    L: Clone + NodeLogReader + Send + Sync + 'static,
{
    let spec = node_runtime_service_base(&node_id);
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(NodeServiceRuntimeError::Nats)?;

    bind_node_endpoint(
        &mut runtime,
        &node_id,
        NodeServiceEndpoint::ContainerEnsureEndpointNetwork,
        runner.clone(),
        handle_ensure_endpoint_network,
    )
    .await?;
    bind_node_endpoint(
        &mut runtime,
        &node_id,
        NodeServiceEndpoint::ContainerRun,
        runner.clone(),
        handle_container_run,
    )
    .await?;
    bind_node_endpoint(
        &mut runtime,
        &node_id,
        NodeServiceEndpoint::ContainerStop,
        runner.clone(),
        handle_container_stop,
    )
    .await?;
    bind_node_endpoint(
        &mut runtime,
        &node_id,
        NodeServiceEndpoint::ContainerRemove,
        runner.clone(),
        handle_container_remove,
    )
    .await?;
    bind_node_endpoint(
        &mut runtime,
        &node_id,
        NodeServiceEndpoint::WireGuardEbpfPrepare,
        preparer,
        handle_wireguard_ebpf_prepare,
    )
    .await?;
    bind_node_endpoint(
        &mut runtime,
        &node_id,
        NodeServiceEndpoint::LogsTail,
        (runner, log_reader),
        handle_logs_tail,
    )
    .await?;

    Ok(runtime)
}

/// Bind one node-scoped endpoint, handing every request the node id and a
/// clone of the handler's state.
async fn bind_node_endpoint<S, H, Fut>(
    runtime: &mut RunningNatsService,
    node_id: &NodeId,
    endpoint: NodeServiceEndpoint,
    state: S,
    handler: H,
) -> Result<(), NodeServiceRuntimeError>
where
    S: Clone + Send + Sync + 'static,
    H: Fn(NodeId, S, NatsServiceRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = NatsServiceResponse> + Send + 'static,
{
    let spec = node_endpoint_spec(node_id, endpoint);
    let node_id = node_id.clone();
    runtime
        .bind_endpoint(&spec, move |request| {
            handler(node_id.clone(), state.clone(), request)
        })
        .await
        .map_err(NodeServiceRuntimeError::Nats)
}

async fn handle_ensure_endpoint_network<R>(
    node_id: NodeId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: NodeContainerRunner,
{
    if let Err(response) = decode_json_request::<NodeEnsureEndpointNetworkRpcRequest>(&request) {
        return response;
    }

    match runner.ensure_endpoint_network().await {
        Ok(()) => node_success(NodeEnsureEndpointNetworkRpcResponse::Ok(
            NodeEnsureEndpointNetworkRpcOk { node_id },
        )),
        Err(NodeContainerRunnerError::EnsureEndpointNetwork { message }) => {
            node_domain_error(NodeEnsureEndpointNetworkRpcResponse::DomainError {
                node_id,
                error: NodeEnsureEndpointNetworkDomainError::EnsureFailed {
                    message: failure_message(format!("endpoint network ensure failed: {message}")),
                },
            })
        }
        Err(error) => runner_error(error),
    }
}

async fn handle_container_run<R>(
    node_id: NodeId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: NodeContainerRunner,
{
    let request = match decode_json_request::<NodeContainerRunRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let existing = match runner.existing_managed_containers().await {
        Ok(existing) => existing,
        Err(error) => return runner_error(error),
    };
    let labels = managed_container_labels(&request.container, request.endpoint.as_ref());

    match decide_container_run(&labels, existing) {
        NodeContainerRunDecision::Create { labels } => {
            match runner
                .create_managed_container(CreateManagedContainer {
                    image: request.image,
                    endpoint: request.endpoint,
                    labels,
                })
                .await
            {
                Ok(container_id) => match runner.start_managed_container(&container_id).await {
                    Ok(()) => node_success(container_run_ok(
                        node_id,
                        NodeRunContainerOutcome::Created { container_id },
                    )),
                    Err(error) => container_start_error(
                        node_id,
                        container_id,
                        error,
                        |container_id, message, inspect_hint| {
                            NodeContainerRunDomainError::CreatedContainerStartFailed {
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
        NodeContainerRunDecision::ReuseRunning { container_id } => node_success(container_run_ok(
            node_id,
            NodeRunContainerOutcome::ReusedRunning { container_id },
        )),
        NodeContainerRunDecision::StartExisting { container_id } => {
            match runner.start_managed_container(&container_id).await {
                Ok(()) => node_success(container_run_ok(
                    node_id,
                    NodeRunContainerOutcome::StartedExisting { container_id },
                )),
                Err(error) => container_start_error(
                    node_id,
                    container_id,
                    error,
                    |container_id, message, inspect_hint| {
                        NodeContainerRunDomainError::ExistingContainerStartFailed {
                            container_id,
                            message,
                            inspect_hint,
                        }
                    },
                ),
            }
        }
        NodeContainerRunDecision::NotStartable {
            container_id,
            state,
        } => node_domain_error(NodeContainerRunRpcResponse::DomainError {
            node_id,
            error: NodeContainerRunDomainError::OperationStepContainerNotStartable {
                container_id: container_id.clone(),
                message: failure_message(format!(
                    "operation step container is not startable: {state:?}"
                )),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        NodeContainerRunDecision::Conflict(conflict) => {
            node_domain_error(NodeContainerRunRpcResponse::DomainError {
                node_id,
                error: NodeContainerRunDomainError::OperationStepConflict {
                    container_id: conflict.container_id,
                    expected: conflict.expected,
                    actual: conflict.actual,
                },
            })
        }
        NodeContainerRunDecision::Ambiguous {
            operation_id,
            step_id,
            container_ids,
        } => node_domain_error(NodeContainerRunRpcResponse::DomainError {
            node_id,
            error: NodeContainerRunDomainError::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            },
        }),
    }
}

async fn handle_container_remove<R>(
    node_id: NodeId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: NodeContainerRunner,
{
    let request = match decode_json_request::<NodeContainerRemoveRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match runner
        .remove_managed_container(&request.container_id, &request.expected_identity)
        .await
    {
        Ok(()) => node_success(NodeContainerRemoveRpcResponse::Ok(NodeContainerRpcOk {
            node_id,
            container_id: request.container_id,
        })),
        Err(NodeContainerRunnerError::Remove {
            container_id,
            message,
        }) => node_domain_error(NodeContainerRemoveRpcResponse::DomainError {
            node_id,
            error: NodeContainerRemoveDomainError::RemoveFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("container remove failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        Err(error) => runner_error(error),
    }
}

async fn handle_container_stop<R>(
    node_id: NodeId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: NodeContainerRunner,
{
    let request = match decode_json_request::<NodeContainerStopRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match runner
        .stop_managed_container(&request.container_id, &request.expected_identity)
        .await
    {
        Ok(()) => node_success(NodeContainerStopRpcResponse::Ok(NodeContainerRpcOk {
            node_id,
            container_id: request.container_id,
        })),
        Err(NodeContainerRunnerError::Stop {
            container_id,
            message,
        }) => node_domain_error(NodeContainerStopRpcResponse::DomainError {
            node_id,
            error: NodeContainerStopDomainError::StopFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("container stop failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        Err(error) => runner_error(error),
    }
}

async fn handle_logs_tail<R, L>(
    node_id: NodeId,
    ports: (R, L),
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: NodeContainerRunner,
    L: NodeLogReader,
{
    let (runner, log_reader) = ports;
    let request = match decode_json_request::<NodeLogsTailRpcRequest>(&request) {
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
        return node_domain_error(NodeLogsTailRpcResponse::DomainError {
            node_id,
            error: NodeLogsTailDomainError::NotFound {
                container_id: request.container_id,
            },
        });
    }

    match log_reader
        .tail_container_logs(&request.container_id, request.tail_lines)
        .await
    {
        Ok(NodeLogTail { text, truncated }) => {
            node_success(NodeLogsTailRpcResponse::Ok(NodeLogsTailRpcOk {
                value: NodeLogsTailResult {
                    node_id,
                    container_id: request.container_id,
                    text,
                    truncated,
                },
            }))
        }
        Err(NodeLogReaderError::NotFound { container_id }) => {
            node_domain_error(NodeLogsTailRpcResponse::DomainError {
                node_id,
                error: NodeLogsTailDomainError::NotFound { container_id },
            })
        }
        Err(NodeLogReaderError::ReadFailed {
            container_id,
            message,
        }) => node_domain_error(NodeLogsTailRpcResponse::DomainError {
            node_id,
            error: NodeLogsTailDomainError::ReadFailed {
                container_id,
                message: failure_message(message),
            },
        }),
    }
}

fn container_run_ok(
    node_id: NodeId,
    outcome: NodeRunContainerOutcome,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::Ok(NodeContainerRunRpcOk { node_id, outcome })
}

fn node_success(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_ok(&response)
}

fn node_domain_error(response: impl serde::Serialize) -> NatsServiceResponse {
    NatsServiceResponse::json_domain_error(&response)
}

fn runner_error(error: NodeContainerRunnerError) -> NatsServiceResponse {
    match error {
        NodeContainerRunnerError::ListExisting { message } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container list failed: {message}")),
        ),
        NodeContainerRunnerError::EnsureEndpointNetwork { message } => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(format!(
                "endpoint network ensure failed: {message}"
            )))
        }
        NodeContainerRunnerError::Create { message } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container create failed: {message}")),
        ),
        NodeContainerRunnerError::Start { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container start failed: {message}")),
        ),
        NodeContainerRunnerError::Stop { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container stop failed: {message}")),
        ),
        NodeContainerRunnerError::Remove { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container remove failed: {message}")),
        ),
    }
}

/// Map a start failure to the endpoint's domain error, parameterized by which
/// start-failed variant (created vs existing) the caller is reporting.
fn container_start_error(
    node_id: NodeId,
    container_id: ContainerId,
    error: NodeContainerRunnerError,
    start_failed: impl FnOnce(ContainerId, FailureMessage, OperatorHint) -> NodeContainerRunDomainError,
) -> NatsServiceResponse {
    match error {
        NodeContainerRunnerError::Start { message, .. } => {
            node_domain_error(NodeContainerRunRpcResponse::DomainError {
                node_id,
                error: start_failed(
                    container_id.clone(),
                    failure_message(format!("container start failed: {message}")),
                    inspect_hint(&container_id),
                ),
            })
        }
        error @ (NodeContainerRunnerError::ListExisting { .. }
        | NodeContainerRunnerError::EnsureEndpointNetwork { .. }
        | NodeContainerRunnerError::Create { .. }
        | NodeContainerRunnerError::Stop { .. }
        | NodeContainerRunnerError::Remove { .. }) => runner_error(error),
    }
}

fn failure_message(value: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(value).expect("generated failure message is non-empty")
}

fn inspect_hint(container_id: &ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {}", container_id.as_str()))
        .expect("generated inspect hint is non-empty")
}

pub trait NodeWireGuardEbpfPreparer {
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
    node_id: NodeId,
    preparer: P,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    P: NodeWireGuardEbpfPreparer,
{
    let request = match decode_json_request::<NodeWireGuardEbpfPrepareRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    if !request.nodes.iter().any(|candidate| candidate == &node_id) {
        return node_domain_error(NodeWireGuardEbpfPrepareRpcResponse::DomainError {
            node_id: node_id.clone(),
            error: NodeWireGuardEbpfPrepareDomainError::Unavailable {
                component: ployz_core::dataplane::WireGuardEbpfComponent::WireGuard,
                message: failure_message("wireguard/eBPF prepare request did not target this node"),
            },
        });
    }

    match request.phase {
        NodeWireGuardEbpfPreparePhase::ReadPublicKey => {
            match preparer.read_wireguard_public_key().await {
                Ok(public_key) => node_success(NodeWireGuardEbpfPrepareRpcResponse::PublicKey {
                    node_id,
                    public_key,
                }),
                Err(error) => node_domain_error(NodeWireGuardEbpfPrepareRpcResponse::DomainError {
                    node_id,
                    error: error.into(),
                }),
            }
        }
        NodeWireGuardEbpfPreparePhase::PrepareDataplane => {
            match preparer
                .prepare_wireguard_ebpf(&request.endpoint_routes, &request.peers)
                .await
            {
                Ok(ready) => node_success(NodeWireGuardEbpfPrepareRpcResponse::Ok {
                    readiness: WireGuardEbpfNodeReady { node_id, ready },
                }),
                Err(error) => node_domain_error(NodeWireGuardEbpfPrepareRpcResponse::DomainError {
                    node_id,
                    error: error.into(),
                }),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeServiceRuntimeError {
    Nats(NatsServiceRuntimeError),
}

impl std::fmt::Display for NodeServiceRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nats(error) => {
                write!(formatter, "failed to start node service: {error:?}")
            }
        }
    }
}

impl std::error::Error for NodeServiceRuntimeError {}
