use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_node_api::{NodeRequest, NodeResponse};

use super::{NodeRpcError, NodeRpcPolicy, ensure_success};

#[cfg(test)]
use super::NodeRpcErrorKind;
#[cfg(test)]
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeProbeRpcOperation {
    Ping,
    Status,
}

impl NodeProbeRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::Ping => "node_ping",
            Self::Status => "node_status",
        }
    }
}

#[async_trait]
pub trait NodeProbeRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn node_probe_request(
        &self,
        machine_id: &MachineId,
        operation: NodeProbeRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct NodeProbeNodeClient<T> {
    transport: T,
}

impl<T> NodeProbeNodeClient<T>
where
    T: NodeProbeRpcTransport,
{
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    #[must_use]
    pub fn with_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            transport: self.transport.with_node_rpc_policy(policy),
        }
    }

    pub async fn ping(&self, machine_id: &MachineId) -> Result<(), NodeRpcError> {
        self.request_expect_ok(machine_id, NodeProbeRpcOperation::Ping, &NodeRequest::Ping)
            .await
    }

    pub async fn status(&self, machine_id: &MachineId) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            NodeProbeRpcOperation::Status,
            &NodeRequest::Status,
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: NodeProbeRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .node_probe_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }

    async fn request_expect_response(
        &self,
        machine_id: &MachineId,
        operation: NodeProbeRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        let response = self
            .transport
            .node_probe_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)?;
        Ok(response)
    }
}

#[cfg(test)]
mod node_probe_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeNodeProbeTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, NodeProbeRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeNodeProbeTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl NodeProbeRpcTransport for FakeNodeProbeTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn node_probe_request(
            &self,
            machine_id: &MachineId,
            operation: NodeProbeRpcOperation,
            request: &NodeRequest,
        ) -> Result<NodeResponse, NodeRpcError> {
            self.requests.lock().expect("requests").push((
                machine_id.clone(),
                operation,
                request.clone(),
            ));
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn node_probe_client_builds_ping_and_status_requests_and_applies_policy() {
        let transport = FakeNodeProbeTransport::default();
        let client = NodeProbeNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");

        client.ping(&machine_id).await.expect("ping");
        client.status(&machine_id).await.expect("status");

        let requests = transport.requests.lock().expect("requests");
        let [ping, status] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(ping.0, machine_id);
        assert_eq!(ping.1, NodeProbeRpcOperation::Ping);
        assert!(matches!(&ping.2, NodeRequest::Ping));
        assert_eq!(status.0, machine_id);
        assert_eq!(status.1, NodeProbeRpcOperation::Status);
        assert!(matches!(&status.2, NodeRequest::Status));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn node_probe_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeNodeProbeTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = NodeProbeNodeClient::new(transport)
            .ping(&machine_id)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "node_ping");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeNodeProbeTransport::with_responses(vec![Err(NodeRpcError::transport(
            "node_status",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = NodeProbeNodeClient::new(transport)
            .status(&machine_id)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "node_status");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}
