//! Request-side NATS adapters for node-local services.

use crate::deploy_worker::{
    NodeContainerRuntime, NodeContainerRuntimeError, NodeRuntimeUnavailableReason,
    WireGuardEbpfPreparer,
};
use crate::node::protocol::{
    NodeContainerRemoveDomainError, NodeContainerRemoveRpcRequest, NodeContainerRpcOk,
    NodeContainerRunDomainError, NodeContainerRunRpcOk, NodeContainerRunRpcRequest,
    NodeContainerStopDomainError, NodeContainerStopRpcRequest,
    NodeEnsureEndpointNetworkDomainError, NodeEnsureEndpointNetworkRpcOk,
    NodeEnsureEndpointNetworkRpcRequest, NodeLogsTailDomainError, NodeLogsTailResult,
    NodeLogsTailRpcOk, NodeLogsTailRpcRequest, NodeRpcResponder, NodeRpcResponse,
    NodeRunContainerOutcome, NodeWireGuardEbpfPrepareDomainError, NodeWireGuardEbpfPreparePhase,
    NodeWireGuardEbpfPrepareRpcRequest, NodeWireGuardEbpfPrepareRpcResponse,
};
use futures_util::future::try_join_all;
use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfNodeReady, WireGuardEbpfPrepareError,
    WireGuardEbpfPrepareReport, WireGuardEbpfPrepareReportError, WireGuardEbpfPrepareRequest,
    WireGuardPeer, WireGuardPublicKey,
};
use ployz_core::ids::NodeId;
use ployz_core::subjects::{NodeServiceEndpoint, node_service};
use ployz_nats::service_protocol::{NatsServiceError, NatsServiceErrorCode};
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
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

/// Outcome of one node RPC round trip: either the node answered with a typed
/// domain error, or the call never produced a usable answer.
enum NodeCallError<E> {
    Unavailable(NodeRuntimeUnavailableReason),
    Domain(E),
}

/// One node RPC round trip: encode the request, map transport failures, and
/// reject answers from the wrong node — exactly once for every endpoint.
async fn call_node<T, E>(
    client: &async_nats::Client,
    request_timeout: Duration,
    node_id: &NodeId,
    endpoint: NodeServiceEndpoint,
    request: &impl Serialize,
) -> Result<T, NodeCallError<E>>
where
    T: DeserializeOwned + NodeRpcResponder,
    E: DeserializeOwned,
{
    let subject = node_service(node_id, endpoint);
    let response =
        request_json::<_, NodeRpcResponse<T, E>>(client, subject, request, request_timeout)
            .await
            .map_err(|error| NodeCallError::Unavailable(unavailable_reason(error)))?;

    match response {
        NodeRpcResponse::Ok(value) => {
            match wrong_response_node(node_id, value.responder_node_id().clone()) {
                Some(reason) => Err(NodeCallError::Unavailable(reason)),
                None => Ok(value),
            }
        }
        NodeRpcResponse::DomainError {
            node_id: actual_node_id,
            error,
        } => match wrong_response_node(node_id, actual_node_id) {
            Some(reason) => Err(NodeCallError::Unavailable(reason)),
            None => Err(NodeCallError::Domain(error)),
        },
    }
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
        node_id: &NodeId,
        request: NodeLogsTailRpcRequest,
    ) -> Result<NodeLogsTailResult, NodeLogsTailRuntimeError> {
        call_node::<NodeLogsTailRpcOk, NodeLogsTailDomainError>(
            &self.client,
            self.request_timeout,
            node_id,
            NodeServiceEndpoint::LogsTail,
            &request,
        )
        .await
        .map(|ok| ok.value)
        .map_err(|error| match error {
            NodeCallError::Unavailable(reason) => NodeLogsTailRuntimeError::Unavailable {
                node_id: node_id.clone(),
                reason,
            },
            NodeCallError::Domain(error) => error.into_runtime_error(node_id.clone()),
        })
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
    async fn ensure_endpoint_network(
        &mut self,
        node_id: &NodeId,
        request: NodeEnsureEndpointNetworkRpcRequest,
    ) -> Result<(), NodeContainerRuntimeError> {
        call_node::<NodeEnsureEndpointNetworkRpcOk, NodeEnsureEndpointNetworkDomainError>(
            &self.client,
            self.request_timeout,
            node_id,
            NodeServiceEndpoint::ContainerEnsureEndpointNetwork,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            NodeCallError::Unavailable(reason) => container_runtime_unavailable(node_id, reason),
            NodeCallError::Domain(error) => error.into_runtime_error(node_id.clone()),
        })
    }

    async fn run_container(
        &mut self,
        node_id: &NodeId,
        request: NodeContainerRunRpcRequest,
    ) -> Result<NodeRunContainerOutcome, NodeContainerRuntimeError> {
        call_node::<NodeContainerRunRpcOk, NodeContainerRunDomainError>(
            &self.client,
            self.request_timeout,
            node_id,
            NodeServiceEndpoint::ContainerRun,
            &request,
        )
        .await
        .map(|ok| ok.outcome)
        .map_err(|error| match error {
            NodeCallError::Unavailable(reason) => container_runtime_unavailable(node_id, reason),
            NodeCallError::Domain(error) => error.into_runtime_error(node_id.clone()),
        })
    }

    async fn remove_container(
        &mut self,
        node_id: &NodeId,
        request: NodeContainerRemoveRpcRequest,
    ) -> Result<(), NodeContainerRuntimeError> {
        call_node::<NodeContainerRpcOk, NodeContainerRemoveDomainError>(
            &self.client,
            self.request_timeout,
            node_id,
            NodeServiceEndpoint::ContainerRemove,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            NodeCallError::Unavailable(reason) => container_runtime_unavailable(node_id, reason),
            NodeCallError::Domain(error) => error.into_runtime_error(node_id.clone()),
        })
    }

    async fn stop_container(
        &mut self,
        node_id: &NodeId,
        request: NodeContainerStopRpcRequest,
    ) -> Result<(), NodeContainerRuntimeError> {
        call_node::<NodeContainerRpcOk, NodeContainerStopDomainError>(
            &self.client,
            self.request_timeout,
            node_id,
            NodeServiceEndpoint::ContainerStop,
            &request,
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            NodeCallError::Unavailable(reason) => container_runtime_unavailable(node_id, reason),
            NodeCallError::Domain(error) => error.into_runtime_error(node_id.clone()),
        })
    }
}

fn container_runtime_unavailable(
    node_id: &NodeId,
    reason: NodeRuntimeUnavailableReason,
) -> NodeContainerRuntimeError {
    NodeContainerRuntimeError::Unavailable {
        node_id: node_id.clone(),
        reason,
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
        }
    }
}

impl NodeEnsureEndpointNetworkDomainError {
    fn into_runtime_error(self, node_id: NodeId) -> NodeContainerRuntimeError {
        match self {
            Self::EnsureFailed { message } => NodeContainerRuntimeError::Unavailable {
                node_id,
                reason: NodeRuntimeUnavailableReason::ServiceUnavailable {
                    message: message.as_str().to_owned(),
                },
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
        let peers =
            if request.peers.is_empty() && request.nodes.len() > 1 {
                let rpc_request = read_public_key_request(&request);
                let public_keys =
                    try_join_all(request.nodes.iter().map(|node_id| {
                        read_node_wireguard_public_key(self, node_id, &rpc_request)
                    }))
                    .await?;
                wireguard_peers_from_public_keys(&request, &public_keys)?
            } else {
                request.peers.clone()
            };

        let final_request = request.with_peers(peers);
        let rpc_request = NodeWireGuardEbpfPrepareRpcRequest::from(final_request.clone());
        let nodes = try_join_all(
            final_request
                .nodes
                .iter()
                .map(|node_id| prepare_node_wireguard_ebpf(self, node_id, &rpc_request)),
        )
        .await?;

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
    let subject = node_service(node_id, NodeServiceEndpoint::WireGuardEbpfPrepare);
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
    let subject = node_service(node_id, NodeServiceEndpoint::WireGuardEbpfPrepare);
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
            match wrong_response_node(node_id, readiness.node_id.clone()) {
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

fn unavailable_reason(error: NatsJsonServiceRequestError) -> NodeRuntimeUnavailableReason {
    match error {
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
    wireguard_ebpf_unavailable(node_id, unavailable_reason(error).failure_message())
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
