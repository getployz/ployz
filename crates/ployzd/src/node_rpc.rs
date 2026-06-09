//! Request-side NATS adapters for node-local services.

use crate::deploy_worker::{
    NodeContainerRuntime, NodeContainerRuntimeError, NodeRuntimeUnavailableReason,
    WireGuardEbpfPreparer,
};
use crate::node_protocol::{
    NodeContainerRemoveDomainError, NodeContainerRemoveRpcRequest, NodeContainerRemoveRpcResponse,
    NodeContainerRunDomainError, NodeContainerRunRpcRequest, NodeContainerRunRpcResponse,
    NodeContainerStopDomainError, NodeContainerStopRpcRequest, NodeContainerStopRpcResponse,
    NodeLogsTailDomainError, NodeLogsTailRpcRequest, NodeLogsTailRpcResponse,
    NodeWireGuardEbpfPrepareDomainError, NodeWireGuardEbpfPreparePhase,
    NodeWireGuardEbpfPrepareRpcRequest, NodeWireGuardEbpfPrepareRpcResponse,
};
use crate::node_runtime_types::{
    NodeLogsTailRequest, NodeLogsTailResult, NodeRemoveContainerRequest, NodeRunContainerOutcome,
    NodeRunContainerRequest, NodeStopContainerRequest,
};
use crate::services::node_endpoint_subject;
use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfNodeReady, WireGuardEbpfPrepareError,
    WireGuardEbpfPrepareReport, WireGuardEbpfPrepareReportError, WireGuardEbpfPrepareRequest,
    WireGuardPeer, WireGuardPublicKey,
};
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

#[derive(Debug, Clone)]
pub struct NatsNodeWireGuardEbpfPreparer {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct NatsNodeLogsTailer {
    client: async_nats::Client,
    request_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeLogsTailRuntimeError {
    NotFound {
        node_id: NodeId,
        container_id: ployz_core::ids::ContainerId,
    },
    ReadFailed {
        node_id: NodeId,
        container_id: ployz_core::ids::ContainerId,
        message: ployz_core::ops::FailureMessage,
    },
    Unavailable {
        node_id: NodeId,
        reason: NodeRuntimeUnavailableReason,
    },
}

impl NatsNodeLogsTailer {
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

    pub async fn tail_logs(
        &self,
        request: NodeLogsTailRequest,
    ) -> Result<NodeLogsTailResult, NodeLogsTailRuntimeError> {
        let node_id = request.node_id.clone();
        let subject = node_endpoint_subject(&node_id, NodeServiceEndpoint::LogsTail);
        let response = request_json::<_, NodeLogsTailRpcResponse>(
            &self.client,
            subject,
            &NodeLogsTailRpcRequest::from(request),
            self.request_timeout,
        )
        .await
        .map_err(|error| logs_request_error(&node_id, error))?;

        match response {
            NodeLogsTailRpcResponse::Ok { value } => {
                if let Some(reason) = wrong_response_node(&node_id, value.node_id.clone()) {
                    return Err(NodeLogsTailRuntimeError::Unavailable { node_id, reason });
                }
                Ok(value)
            }
            NodeLogsTailRpcResponse::DomainError {
                node_id: actual_node_id,
                error,
            } => {
                if let Some(reason) = wrong_response_node(&node_id, actual_node_id) {
                    return Err(NodeLogsTailRuntimeError::Unavailable { node_id, reason });
                }
                Err(error.into_runtime_error(node_id))
            }
        }
    }
}

impl NatsNodeWireGuardEbpfPreparer {
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

    async fn remove_container(
        &mut self,
        request: NodeRemoveContainerRequest,
    ) -> Result<(), NodeContainerRuntimeError> {
        let node_id = request.node_id.clone();
        let subject = node_endpoint_subject(&node_id, NodeServiceEndpoint::ContainerRemove);
        let response = request_json::<_, NodeContainerRemoveRpcResponse>(
            &self.client,
            subject,
            &NodeContainerRemoveRpcRequest::from(request),
            self.request_timeout,
        )
        .await
        .map_err(|error| node_request_error(&node_id, error))?;

        match response {
            NodeContainerRemoveRpcResponse::Ok {
                node_id: actual_node_id,
                ..
            } => {
                if let Some(reason) = wrong_response_node(&node_id, actual_node_id) {
                    return Err(NodeContainerRuntimeError::Unavailable { node_id, reason });
                }
                Ok(())
            }
            NodeContainerRemoveRpcResponse::DomainError {
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

    async fn stop_container(
        &mut self,
        request: NodeStopContainerRequest,
    ) -> Result<(), NodeContainerRuntimeError> {
        let node_id = request.node_id.clone();
        let subject = node_endpoint_subject(&node_id, NodeServiceEndpoint::ContainerStop);
        let response = request_json::<_, NodeContainerStopRpcResponse>(
            &self.client,
            subject,
            &NodeContainerStopRpcRequest::from(request),
            self.request_timeout,
        )
        .await
        .map_err(|error| node_request_error(&node_id, error))?;

        match response {
            NodeContainerStopRpcResponse::Ok {
                node_id: actual_node_id,
                ..
            } => {
                if let Some(reason) = wrong_response_node(&node_id, actual_node_id) {
                    return Err(NodeContainerRuntimeError::Unavailable { node_id, reason });
                }
                Ok(())
            }
            NodeContainerStopRpcResponse::DomainError {
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

impl NodeLogsTailDomainError {
    fn into_runtime_error(self, node_id: NodeId) -> NodeLogsTailRuntimeError {
        match self {
            Self::NotFound { container_id } => NodeLogsTailRuntimeError::NotFound {
                node_id,
                container_id,
            },
            Self::ReadFailed {
                container_id,
                message,
            } => NodeLogsTailRuntimeError::ReadFailed {
                node_id,
                container_id,
                message,
            },
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
            Self::CreatedContainerStartFailed {
                container_id,
                message,
                inspect_hint,
            } => NodeContainerRuntimeError::CreatedContainerStartFailed {
                node_id,
                container_id,
                message,
                inspect_hint,
            },
            Self::ExistingContainerStartFailed {
                container_id,
                message,
                inspect_hint,
            } => NodeContainerRuntimeError::ExistingContainerStartFailed {
                node_id,
                container_id,
                message,
                inspect_hint,
            },
            Self::OperationStepContainerNotStartable {
                container_id,
                message,
                inspect_hint,
            } => NodeContainerRuntimeError::OperationStepContainerNotStartable {
                node_id,
                container_id,
                message,
                inspect_hint,
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

impl NodeContainerRemoveDomainError {
    fn into_runtime_error(self, node_id: NodeId) -> NodeContainerRuntimeError {
        match self {
            Self::RemoveFailed {
                container_id,
                message,
                inspect_hint,
            } => NodeContainerRuntimeError::RemoveContainerFailed {
                node_id,
                container_id,
                message,
                inspect_hint,
            },
        }
    }
}

impl NodeContainerStopDomainError {
    fn into_runtime_error(self, node_id: NodeId) -> NodeContainerRuntimeError {
        match self {
            Self::StopFailed {
                container_id,
                message,
                inspect_hint,
            } => NodeContainerRuntimeError::StopContainerFailed {
                node_id,
                container_id,
                message,
                inspect_hint,
            },
        }
    }
}

impl WireGuardEbpfPreparer for NatsNodeWireGuardEbpfPreparer {
    async fn prepare_wireguard_ebpf(
        &mut self,
        request: WireGuardEbpfPrepareRequest,
    ) -> Result<WireGuardEbpfPrepareReport, WireGuardEbpfPrepareError> {
        let peers = if request.peers.is_empty() && request.nodes.len() > 1 {
            let rpc_request = read_public_key_request(&request);
            let mut public_keys = Vec::new();
            for node_id in &request.nodes {
                public_keys
                    .push(read_node_wireguard_public_key(self, node_id, &rpc_request).await?);
            }
            wireguard_peers_from_public_keys(&request, &public_keys)?
        } else {
            request.peers.clone()
        };

        let final_request = request.with_peers(peers);
        let rpc_request = NodeWireGuardEbpfPrepareRpcRequest::from(final_request.clone());
        let mut nodes = Vec::new();
        for node_id in &final_request.nodes {
            nodes.push(prepare_node_wireguard_ebpf(self, node_id, &rpc_request).await?);
        }

        WireGuardEbpfPrepareReport::for_request(&final_request, nodes)
            .map_err(wireguard_ebpf_report_error)
    }
}

fn read_public_key_request(
    request: &WireGuardEbpfPrepareRequest,
) -> NodeWireGuardEbpfPrepareRpcRequest {
    NodeWireGuardEbpfPrepareRpcRequest {
        phase: NodeWireGuardEbpfPreparePhase::ReadPublicKey,
        operation_id: request.operation_id.clone(),
        nodes: request.nodes.clone(),
        endpoint_routes: Vec::new(),
        peer_endpoints: Vec::new(),
        peers: Vec::new(),
    }
}

fn wireguard_peers_from_public_keys(
    request: &WireGuardEbpfPrepareRequest,
    public_keys: &[(NodeId, WireGuardPublicKey)],
) -> Result<Vec<WireGuardPeer>, WireGuardEbpfPrepareError> {
    if request.nodes.len() < 2 {
        return Ok(Vec::new());
    }

    let mut peers = Vec::new();
    for node_id in &request.nodes {
        let Some(endpoint) = request
            .peer_endpoints
            .iter()
            .find(|endpoint| endpoint.node_id == *node_id)
            .cloned()
        else {
            return Err(invalid_wireguard_report(format!(
                "wireguard endpoint is missing for {}",
                node_id.as_str()
            )));
        };
        let Some((_, public_key)) = public_keys
            .iter()
            .find(|(ready_node_id, _)| ready_node_id == node_id)
        else {
            return Err(invalid_wireguard_report(format!(
                "wireguard public key is missing for {}",
                node_id.as_str()
            )));
        };
        peers.push(WireGuardPeer::from_endpoint(endpoint, public_key.clone()));
    }

    Ok(peers)
}

async fn read_node_wireguard_public_key(
    preparer: &NatsNodeWireGuardEbpfPreparer,
    node_id: &NodeId,
    request: &NodeWireGuardEbpfPrepareRpcRequest,
) -> Result<(NodeId, WireGuardPublicKey), WireGuardEbpfPrepareError> {
    let subject = node_endpoint_subject(node_id, NodeServiceEndpoint::WireGuardEbpfPrepare);
    let response = request_json::<_, NodeWireGuardEbpfPrepareRpcResponse>(
        &preparer.client,
        subject,
        request,
        preparer.request_timeout,
    )
    .await
    .map_err(|error| wireguard_ebpf_request_error(node_id, error))?;

    match response {
        NodeWireGuardEbpfPrepareRpcResponse::PublicKey {
            node_id: actual_node_id,
            public_key,
        } => match wrong_response_node(node_id, actual_node_id.clone()) {
            Some(reason) => Err(wireguard_ebpf_unavailable(
                node_id,
                reason.failure_message(),
            )),
            None => Ok((actual_node_id, public_key)),
        },
        NodeWireGuardEbpfPrepareRpcResponse::Ok { .. } => Err(invalid_wireguard_report(format!(
            "node {} returned dataplane readiness for public key request",
            node_id.as_str()
        ))),
        NodeWireGuardEbpfPrepareRpcResponse::DomainError {
            node_id: actual_node_id,
            error,
        } => match wrong_response_node(node_id, actual_node_id) {
            Some(reason) => Err(wireguard_ebpf_unavailable(
                node_id,
                reason.failure_message(),
            )),
            None => Err(error.into_prepare_error(node_id.clone())),
        },
    }
}

async fn prepare_node_wireguard_ebpf(
    preparer: &NatsNodeWireGuardEbpfPreparer,
    node_id: &NodeId,
    request: &NodeWireGuardEbpfPrepareRpcRequest,
) -> Result<WireGuardEbpfNodeReady, WireGuardEbpfPrepareError> {
    let subject = node_endpoint_subject(node_id, NodeServiceEndpoint::WireGuardEbpfPrepare);
    let response = request_json::<_, NodeWireGuardEbpfPrepareRpcResponse>(
        &preparer.client,
        subject,
        request,
        preparer.request_timeout,
    )
    .await
    .map_err(|error| wireguard_ebpf_request_error(node_id, error))?;

    match response {
        NodeWireGuardEbpfPrepareRpcResponse::Ok { readiness } => {
            match wrong_response_node(node_id, readiness.node_id().clone()) {
                Some(reason) => Err(wireguard_ebpf_unavailable(
                    node_id,
                    reason.failure_message(),
                )),
                None => Ok(readiness),
            }
        }
        NodeWireGuardEbpfPrepareRpcResponse::PublicKey { .. } => {
            Err(invalid_wireguard_report(format!(
                "node {} returned public key for dataplane prepare request",
                node_id.as_str()
            )))
        }
        NodeWireGuardEbpfPrepareRpcResponse::DomainError {
            node_id: actual_node_id,
            error,
        } => match wrong_response_node(node_id, actual_node_id) {
            Some(reason) => Err(wireguard_ebpf_unavailable(
                node_id,
                reason.failure_message(),
            )),
            None => Err(error.into_prepare_error(node_id.clone())),
        },
    }
}

impl NodeWireGuardEbpfPrepareDomainError {
    fn into_prepare_error(self, node_id: NodeId) -> WireGuardEbpfPrepareError {
        match self {
            Self::Unavailable { component, message } => WireGuardEbpfPrepareError::Unavailable {
                node_id,
                component,
                message,
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

fn logs_request_error(
    node_id: &NodeId,
    error: NatsJsonServiceRequestError,
) -> NodeLogsTailRuntimeError {
    NodeLogsTailRuntimeError::Unavailable {
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

fn wireguard_ebpf_request_error(
    node_id: &NodeId,
    error: NatsJsonServiceRequestError,
) -> WireGuardEbpfPrepareError {
    let reason = match error {
        NatsJsonServiceRequestError::EncodeRequest { message } => {
            NodeRuntimeUnavailableReason::EncodeRequest { message }
        }
        NatsJsonServiceRequestError::Request { failure } => node_request_failure_reason(failure),
        NatsJsonServiceRequestError::Service { failure } => node_service_failure_reason(failure),
        NatsJsonServiceRequestError::ServiceProtocol { error } => {
            NodeRuntimeUnavailableReason::MalformedServiceError {
                message: error.to_string(),
            }
        }
        NatsJsonServiceRequestError::DecodeResponse { message } => {
            NodeRuntimeUnavailableReason::DecodeResponse { message }
        }
    };

    wireguard_ebpf_unavailable(node_id, reason.failure_message())
}

fn wireguard_ebpf_unavailable(
    node_id: &NodeId,
    message: ployz_core::ops::FailureMessage,
) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::Unavailable {
        node_id: node_id.clone(),
        component: WireGuardEbpfComponent::WireGuard,
        message,
    }
}

fn wireguard_ebpf_report_error(
    error: WireGuardEbpfPrepareReportError,
) -> WireGuardEbpfPrepareError {
    let message = match error {
        WireGuardEbpfPrepareReportError::Empty => "wireguard/eBPF report had no nodes",
        WireGuardEbpfPrepareReportError::DuplicateNode => {
            "wireguard/eBPF report contained duplicate nodes"
        }
        WireGuardEbpfPrepareReportError::NodeSetMismatch => {
            "wireguard/eBPF report did not match requested nodes"
        }
    };
    WireGuardEbpfPrepareError::InvalidReport {
        message: ployz_core::ops::FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}

fn invalid_wireguard_report(message: String) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::InvalidReport {
        message: ployz_core::ops::FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}
