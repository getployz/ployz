use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{
    NodeProbeRpcOperation, NodeProbeRpcTransport, NodeRpcError, NodeRpcPolicy,
};

#[derive(Clone)]
pub(crate) struct NatsNodeProbeRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsNodeProbeRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl NodeProbeRpcTransport for NatsNodeProbeRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn node_probe_request(
        &self,
        machine_id: &MachineId,
        operation: NodeProbeRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(node_probe_subject(machine_id, operation), request)
            .await
            .map_err(|error| node_probe_rpc_error(operation, error))
    }
}

fn node_probe_subject(
    machine_id: &MachineId,
    operation: NodeProbeRpcOperation,
) -> NodeCommandSubject {
    match operation {
        NodeProbeRpcOperation::Ping => NodeCommandSubject::ping(machine_id),
        NodeProbeRpcOperation::Status => NodeCommandSubject::status(machine_id),
    }
}

fn node_probe_rpc_error(operation: NodeProbeRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

#[cfg(test)]
mod tests {
    use ployz_nats::RpcFailureKind;

    use super::*;

    #[test]
    fn node_probe_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                NodeProbeRpcOperation::Ping,
                NodeCommandSubject::ping(&machine_id),
            ),
            (
                NodeProbeRpcOperation::Status,
                NodeCommandSubject::status(&machine_id),
            ),
        ] {
            assert_eq!(node_probe_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn node_probe_rpc_transport_error_maps_operation_and_failure_context() {
        let error = node_probe_rpc_error(
            NodeProbeRpcOperation::Status,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "node_status");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }
}
