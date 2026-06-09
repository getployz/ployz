//! NATS Service API runtime wiring for node-local commands.

use crate::node_agent::runtime::{
    CreateManagedContainer, NodeContainerRunConflict, NodeContainerRunDecision,
    NodeContainerRunner, NodeContainerRunnerError, NodeLogReader, NodeLogReaderError, NodeLogTail,
    decide_container_run, managed_container_labels,
};
use crate::node_protocol::{
    NodeContainerRemoveDomainError, NodeContainerRemoveRpcRequest, NodeContainerRemoveRpcResponse,
    NodeContainerRunDomainError, NodeContainerRunRpcRequest, NodeContainerRunRpcResponse,
    NodeLogsTailDomainError, NodeLogsTailRpcRequest, NodeLogsTailRpcResponse,
    NodeWireGuardEbpfPrepareDomainError, NodeWireGuardEbpfPrepareRpcRequest,
    NodeWireGuardEbpfPrepareRpcResponse,
};
use crate::node_runtime_types::{NodeLogsTailResult, NodeRunContainerOutcome};
use crate::services::{node_endpoint_spec, node_runtime_service_base};
use ployz_core::dataplane::{
    WireGuardEbpfEndpointRoute, WireGuardEbpfNodeReady, WireGuardEbpfPrepareError,
    WireGuardEbpfReady,
};
use ployz_core::ids::{ContainerId, NodeId, OperationId, StepId};
use ployz_core::subjects::NodeServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, decode_json_request, start_nats_service,
};

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
    let container_endpoint = node_endpoint_spec(&node_id, NodeServiceEndpoint::ContainerRun);
    let container_remove_endpoint =
        node_endpoint_spec(&node_id, NodeServiceEndpoint::ContainerRemove);
    let wireguard_ebpf_endpoint =
        node_endpoint_spec(&node_id, NodeServiceEndpoint::WireGuardEbpfPrepare);
    let logs_tail_endpoint = node_endpoint_spec(&node_id, NodeServiceEndpoint::LogsTail);
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(NodeServiceRuntimeError::Nats)?;

    runtime
        .bind_endpoint(&container_endpoint, {
            let node_id = node_id.clone();
            let runner = runner.clone();
            move |request| {
                let node_id = node_id.clone();
                let runner = runner.clone();
                async move { handle_container_run(node_id, runner, request).await }
            }
        })
        .await
        .map_err(NodeServiceRuntimeError::Nats)?;

    runtime
        .bind_endpoint(&container_remove_endpoint, {
            let node_id = node_id.clone();
            let runner = runner.clone();
            move |request| {
                let node_id = node_id.clone();
                let runner = runner.clone();
                async move { handle_container_remove(node_id, runner, request).await }
            }
        })
        .await
        .map_err(NodeServiceRuntimeError::Nats)?;

    let wireguard_node_id = node_id.clone();
    runtime
        .bind_endpoint(&wireguard_ebpf_endpoint, move |request| {
            let node_id = wireguard_node_id.clone();
            let preparer = preparer.clone();
            async move { handle_wireguard_ebpf_prepare(node_id, preparer, request).await }
        })
        .await
        .map_err(NodeServiceRuntimeError::Nats)?;

    runtime
        .bind_endpoint(&logs_tail_endpoint, move |request| {
            let node_id = node_id.clone();
            let runner = runner.clone();
            let log_reader = log_reader.clone();
            async move { handle_logs_tail(node_id, runner, log_reader, request).await }
        })
        .await
        .map_err(NodeServiceRuntimeError::Nats)?;

    Ok(runtime)
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
                    Ok(()) => node_success(container_created_response(node_id, container_id)),
                    Err(error) => created_container_start_error(node_id, container_id, error),
                },
                Err(error) => runner_error(error),
            }
        }
        NodeContainerRunDecision::ReuseRunning { container_id } => {
            node_success(container_reused_running_response(node_id, container_id))
        }
        NodeContainerRunDecision::StartExisting { container_id } => {
            match runner.start_managed_container(&container_id).await {
                Ok(()) => node_success(container_started_existing_response(node_id, container_id)),
                Err(error) => existing_container_start_error(node_id, container_id, error),
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
            node_domain_error(container_conflict_response(node_id, conflict))
        }
        NodeContainerRunDecision::Ambiguous {
            operation_id,
            step_id,
            container_ids,
        } => node_domain_error(container_ambiguous_response(
            node_id,
            operation_id,
            step_id,
            container_ids,
        )),
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
        Ok(()) => node_success(NodeContainerRemoveRpcResponse::Ok {
            node_id,
            container_id: request.container_id,
        }),
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

async fn handle_logs_tail<R, L>(
    node_id: NodeId,
    runner: R,
    log_reader: L,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: NodeContainerRunner,
    L: NodeLogReader,
{
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
        Ok(NodeLogTail { text, truncated }) => node_success(NodeLogsTailRpcResponse::Ok {
            value: NodeLogsTailResult {
                node_id,
                container_id: request.container_id,
                text,
                truncated,
            },
        }),
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

fn container_created_response(
    node_id: NodeId,
    container_id: ContainerId,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::Ok {
        node_id,
        outcome: NodeRunContainerOutcome::Created { container_id },
    }
}

fn container_reused_running_response(
    node_id: NodeId,
    container_id: ContainerId,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::Ok {
        node_id,
        outcome: NodeRunContainerOutcome::ReusedRunning { container_id },
    }
}

fn container_started_existing_response(
    node_id: NodeId,
    container_id: ContainerId,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::Ok {
        node_id,
        outcome: NodeRunContainerOutcome::StartedExisting { container_id },
    }
}

fn container_conflict_response(
    node_id: NodeId,
    conflict: NodeContainerRunConflict,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::DomainError {
        node_id,
        error: NodeContainerRunDomainError::OperationStepConflict {
            container_id: conflict.container_id,
            expected: conflict.expected,
            actual: conflict.actual,
        },
    }
}

fn container_ambiguous_response(
    node_id: NodeId,
    operation_id: OperationId,
    step_id: StepId,
    container_ids: Vec<ContainerId>,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::DomainError {
        node_id,
        error: NodeContainerRunDomainError::OperationStepAmbiguous {
            operation_id,
            step_id,
            container_ids,
        },
    }
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
        NodeContainerRunnerError::Create { message } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container create failed: {message}")),
        ),
        NodeContainerRunnerError::Start { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container start failed: {message}")),
        ),
        NodeContainerRunnerError::Remove { message, .. } => NatsServiceResponse::transport_error(
            NatsServiceError::internal(format!("container remove failed: {message}")),
        ),
    }
}

fn created_container_start_error(
    node_id: NodeId,
    container_id: ContainerId,
    error: NodeContainerRunnerError,
) -> NatsServiceResponse {
    match error {
        NodeContainerRunnerError::Start { message, .. } => {
            node_domain_error(NodeContainerRunRpcResponse::DomainError {
                node_id,
                error: NodeContainerRunDomainError::CreatedContainerStartFailed {
                    container_id: container_id.clone(),
                    message: failure_message(format!("container start failed: {message}")),
                    inspect_hint: inspect_hint(&container_id),
                },
            })
        }
        NodeContainerRunnerError::ListExisting { message } => {
            runner_error(NodeContainerRunnerError::ListExisting { message })
        }
        NodeContainerRunnerError::Create { message } => {
            runner_error(NodeContainerRunnerError::Create { message })
        }
        NodeContainerRunnerError::Remove {
            container_id,
            message,
        } => runner_error(NodeContainerRunnerError::Remove {
            container_id,
            message,
        }),
    }
}

fn existing_container_start_error(
    node_id: NodeId,
    container_id: ContainerId,
    error: NodeContainerRunnerError,
) -> NatsServiceResponse {
    match error {
        NodeContainerRunnerError::Start { message, .. } => {
            node_domain_error(NodeContainerRunRpcResponse::DomainError {
                node_id,
                error: NodeContainerRunDomainError::ExistingContainerStartFailed {
                    container_id: container_id.clone(),
                    message: failure_message(format!("container start failed: {message}")),
                    inspect_hint: inspect_hint(&container_id),
                },
            })
        }
        NodeContainerRunnerError::ListExisting { message } => {
            runner_error(NodeContainerRunnerError::ListExisting { message })
        }
        NodeContainerRunnerError::Create { message } => {
            runner_error(NodeContainerRunnerError::Create { message })
        }
        NodeContainerRunnerError::Remove {
            container_id,
            message,
        } => runner_error(NodeContainerRunnerError::Remove {
            container_id,
            message,
        }),
    }
}

fn failure_message(value: impl Into<String>) -> ployz_core::ops::FailureMessage {
    ployz_core::ops::FailureMessage::try_new(value).expect("generated failure message is non-empty")
}

fn inspect_hint(container_id: &ContainerId) -> ployz_core::ops::OperatorHint {
    ployz_core::ops::OperatorHint::try_new(format!(
        "ployz container inspect {}",
        container_id.as_str()
    ))
    .expect("generated inspect hint is non-empty")
}

pub trait NodeWireGuardEbpfPreparer {
    fn prepare_wireguard_ebpf(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
    ) -> impl std::future::Future<Output = Result<WireGuardEbpfReady, WireGuardEbpfPrepareError>> + Send;
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

    match preparer
        .prepare_wireguard_ebpf(&request.endpoint_routes)
        .await
    {
        Ok(ready) => node_success(NodeWireGuardEbpfPrepareRpcResponse::Ok {
            readiness: WireGuardEbpfNodeReady::new(node_id, ready),
        }),
        Err(error) => node_domain_error(NodeWireGuardEbpfPrepareRpcResponse::DomainError {
            node_id,
            error: error.into(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeServiceRuntimeError {
    Nats(NatsServiceRuntimeError),
}
