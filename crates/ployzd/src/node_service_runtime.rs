//! NATS Service API runtime wiring for node-local commands.

use crate::node_agent::runtime::{
    CreateManagedContainer, NodeContainerRunConflict, NodeContainerRunDecision,
    NodeContainerRunner, NodeContainerRunnerError, decide_container_run,
};
use crate::node_protocol::{
    NodeContainerRunDomainError, NodeContainerRunRpcRequest, NodeContainerRunRpcResponse,
};
use crate::node_runtime_types::NodeRunContainerOutcome;
use crate::services::{node_endpoint_spec, node_runtime_service_base};
use ployz_core::ids::{ContainerId, NodeId, OperationId, StepId};
use ployz_core::subjects::NodeServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, decode_json_request, start_nats_service,
};

pub async fn start_node_runtime_service<R>(
    client: ployz_nats::service_runtime::NatsClient,
    node_id: NodeId,
    runner: R,
) -> Result<RunningNatsService, NodeServiceRuntimeError>
where
    R: Clone + NodeContainerRunner + Send + Sync + 'static,
{
    let spec = node_runtime_service_base(&node_id);
    let endpoint = node_endpoint_spec(&node_id, NodeServiceEndpoint::ContainerRun);
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(NodeServiceRuntimeError::Nats)?;

    runtime
        .bind_endpoint(&endpoint, move |request| {
            let node_id = node_id.clone();
            let runner = runner.clone();
            async move { handle_container_run(node_id, runner, request).await }
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

    match decide_container_run(&request.labels, existing) {
        NodeContainerRunDecision::Create { labels } => {
            match runner
                .create_managed_container(CreateManagedContainer {
                    image: request.image,
                    labels,
                })
                .await
            {
                Ok(container_id) => node_success(container_created_response(node_id, container_id)),
                Err(error) => runner_error(error),
            }
        }
        NodeContainerRunDecision::Reuse { container_id } => {
            node_success(container_reused_response(node_id, container_id))
        }
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

fn container_created_response(
    node_id: NodeId,
    container_id: ContainerId,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::Ok {
        node_id,
        outcome: NodeRunContainerOutcome::Created { container_id },
    }
}

fn container_reused_response(
    node_id: NodeId,
    container_id: ContainerId,
) -> NodeContainerRunRpcResponse {
    NodeContainerRunRpcResponse::Ok {
        node_id,
        outcome: NodeRunContainerOutcome::Reused { container_id },
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

fn node_success(response: NodeContainerRunRpcResponse) -> NatsServiceResponse {
    NatsServiceResponse::json_ok(&response)
}

fn node_domain_error(response: NodeContainerRunRpcResponse) -> NatsServiceResponse {
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
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeServiceRuntimeError {
    Nats(NatsServiceRuntimeError),
}
