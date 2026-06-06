//! Request-side NATS adapters for node-local services.

use crate::deploy_worker::{
    NodeContainerRuntime, NodeContainerRuntimeError, NodeRuntimeUnavailableReason,
};
use crate::node_protocol::{
    NodeContainerRunDomainError, NodeContainerRunRpcRequest, NodeContainerRunRpcResponse,
};
use crate::node_runtime_types::{NodeRunContainerOutcome, NodeRunContainerRequest};
use crate::services::node_endpoint_subject;
use ployz_core::ids::NodeId;
use ployz_core::subjects::NodeServiceEndpoint;
use ployz_nats::service_protocol::{NatsServiceError, NatsServiceErrorCode};
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use std::time::Duration;

pub const DEFAULT_NODE_RPC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct NatsNodeContainerRuntime {
    client: async_nats::Client,
    request_timeout: Duration,
}

impl NatsNodeContainerRuntime {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_NODE_RPC_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }
}

impl NodeContainerRuntime for NatsNodeContainerRuntime {
    async fn run_container(
        &mut self,
        request: NodeRunContainerRequest,
    ) -> Result<NodeRunContainerOutcome, NodeContainerRuntimeError> {
        let node_id = request.node_id.clone();
        let subject = node_endpoint_subject(&node_id, NodeServiceEndpoint::ContainerRun);
        let response = request_json::<_, NodeContainerRunRpcResponse>(
            &self.client,
            subject,
            &NodeContainerRunRpcRequest::from(request),
            self.request_timeout,
        )
        .await
        .map_err(|error| node_request_error(&node_id, error))?;

        match response {
            NodeContainerRunRpcResponse::Ok {
                node_id: actual_node_id,
                outcome,
            } => {
                if let Some(reason) = wrong_response_node(&node_id, actual_node_id) {
                    return Err(NodeContainerRuntimeError::Unavailable { node_id, reason });
                }
                Ok(outcome)
            }
            NodeContainerRunRpcResponse::DomainError {
                node_id: actual_node_id,
                error,
            } => {
                if let Some(reason) = wrong_response_node(&node_id, actual_node_id) {
                    return Err(NodeContainerRuntimeError::Unavailable { node_id, reason });
                }
                Err(error.into_runtime_error(node_id))
            }
        }
    }
}

impl NodeContainerRunDomainError {
    fn into_runtime_error(self, node_id: NodeId) -> NodeContainerRuntimeError {
        match self {
            Self::OperationStepConflict {
                container_id,
                expected,
                actual,
            } => NodeContainerRuntimeError::OperationStepConflict {
                node_id,
                container_id,
                expected,
                actual,
            },
            Self::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            } => NodeContainerRuntimeError::OperationStepAmbiguous {
                node_id,
                operation_id,
                step_id,
                container_ids,
            },
            Self::StartedContainerUnhealthy {
                container_id,
                message,
                log_hint,
            } => NodeContainerRuntimeError::StartedContainerUnhealthy {
                node_id,
                container_id,
                message,
                log_hint,
            },
        }
    }
}

fn wrong_response_node(
    requested_node_id: &NodeId,
    actual_node_id: NodeId,
) -> Option<NodeRuntimeUnavailableReason> {
    if actual_node_id == *requested_node_id {
        return None;
    }

    Some(NodeRuntimeUnavailableReason::WrongResponder { actual_node_id })
}

fn node_request_error(
    node_id: &NodeId,
    error: NatsJsonServiceRequestError,
) -> NodeContainerRuntimeError {
    NodeContainerRuntimeError::Unavailable {
        node_id: node_id.clone(),
        reason: match error {
            NatsJsonServiceRequestError::EncodeRequest { message } => {
                NodeRuntimeUnavailableReason::EncodeRequest { message }
            }
            NatsJsonServiceRequestError::Request { failure } => {
                node_request_failure_reason(failure)
            }
            NatsJsonServiceRequestError::Service { failure } => {
                node_service_failure_reason(failure)
            }
            NatsJsonServiceRequestError::ServiceProtocol { error } => {
                NodeRuntimeUnavailableReason::MalformedServiceError {
                    message: error.to_string(),
                }
            }
            NatsJsonServiceRequestError::DecodeResponse { message } => {
                NodeRuntimeUnavailableReason::DecodeResponse { message }
            }
        },
    }
}

fn node_request_failure_reason(failure: NatsServiceRequestFailure) -> NodeRuntimeUnavailableReason {
    match failure {
        NatsServiceRequestFailure::TimedOut => NodeRuntimeUnavailableReason::RequestTimedOut,
        NatsServiceRequestFailure::NoResponders => NodeRuntimeUnavailableReason::NoResponders,
        NatsServiceRequestFailure::InvalidSubject => NodeRuntimeUnavailableReason::InvalidSubject,
        NatsServiceRequestFailure::MaxPayloadExceeded => {
            NodeRuntimeUnavailableReason::MaxPayloadExceeded
        }
        NatsServiceRequestFailure::Other { message } => {
            NodeRuntimeUnavailableReason::RequestFailed { message }
        }
    }
}

fn node_service_failure_reason(error: NatsServiceError) -> NodeRuntimeUnavailableReason {
    match error.code {
        NatsServiceErrorCode::BadRequest => NodeRuntimeUnavailableReason::ServiceBadRequest {
            message: error.message,
        },
        NatsServiceErrorCode::Conflict => NodeRuntimeUnavailableReason::ServiceConflict {
            message: error.message,
        },
        NatsServiceErrorCode::Unavailable => NodeRuntimeUnavailableReason::ServiceUnavailable {
            message: error.message,
        },
        NatsServiceErrorCode::Timeout => NodeRuntimeUnavailableReason::ServiceTimedOut {
            message: error.message,
        },
        NatsServiceErrorCode::Internal => NodeRuntimeUnavailableReason::ServiceInternal {
            message: error.message,
        },
    }
}
