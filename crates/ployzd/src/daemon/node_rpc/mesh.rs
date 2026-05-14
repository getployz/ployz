use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{
    MeshReadinessRpcOperation, MeshReadinessRpcTransport, MeshRpcOperation, MeshRpcTransport,
    NodeRpcError, NodeRpcPolicy,
};

#[derive(Clone)]
pub(crate) struct NatsMeshReadinessRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMeshReadinessRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MeshReadinessRpcTransport for NatsMeshReadinessRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn mesh_readiness_request(
        &self,
        machine_id: &MachineId,
        operation: MeshReadinessRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(mesh_readiness_subject(machine_id, operation), request)
            .await
            .map_err(|error| mesh_readiness_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMeshRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMeshRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MeshRpcTransport for NatsMeshRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn mesh_request(
        &self,
        machine_id: &MachineId,
        operation: MeshRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(mesh_subject(machine_id, operation), request)
            .await
            .map_err(|error| mesh_node_rpc_error(operation, error))
    }
}

fn mesh_readiness_subject(
    machine_id: &MachineId,
    operation: MeshReadinessRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MeshReadinessRpcOperation::Ready => NodeCommandSubject::mesh_ready(machine_id),
        MeshReadinessRpcOperation::SelfRecord => NodeCommandSubject::mesh_self_record(machine_id),
    }
}

fn mesh_subject(machine_id: &MachineId, operation: MeshRpcOperation) -> NodeCommandSubject {
    match operation {
        MeshRpcOperation::PrepareDestroy => NodeCommandSubject::mesh_prepare_destroy(machine_id),
        MeshRpcOperation::CancelDestroy => NodeCommandSubject::mesh_cancel_destroy(machine_id),
        MeshRpcOperation::ExecuteDestroy => NodeCommandSubject::mesh_execute_destroy(machine_id),
        MeshRpcOperation::RemoveMachine => NodeCommandSubject::mesh_remove_machine(machine_id),
    }
}

fn mesh_readiness_rpc_error(
    operation: MeshReadinessRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn mesh_node_rpc_error(operation: MeshRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

#[cfg(test)]
mod tests {
    use ployz_nats::RpcFailureKind;

    use super::*;

    #[test]
    fn mesh_readiness_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MeshReadinessRpcOperation::Ready,
                NodeCommandSubject::mesh_ready(&machine_id),
            ),
            (
                MeshReadinessRpcOperation::SelfRecord,
                NodeCommandSubject::mesh_self_record(&machine_id),
            ),
        ] {
            assert_eq!(mesh_readiness_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn mesh_readiness_rpc_transport_error_maps_operation_and_failure_context() {
        let error = mesh_readiness_rpc_error(
            MeshReadinessRpcOperation::Ready,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "mesh_ready");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn mesh_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MeshRpcOperation::PrepareDestroy,
                NodeCommandSubject::mesh_prepare_destroy(&machine_id),
            ),
            (
                MeshRpcOperation::CancelDestroy,
                NodeCommandSubject::mesh_cancel_destroy(&machine_id),
            ),
            (
                MeshRpcOperation::ExecuteDestroy,
                NodeCommandSubject::mesh_execute_destroy(&machine_id),
            ),
            (
                MeshRpcOperation::RemoveMachine,
                NodeCommandSubject::mesh_remove_machine(&machine_id),
            ),
        ] {
            assert_eq!(mesh_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn mesh_rpc_transport_error_maps_operation_and_failure_context() {
        let error = mesh_node_rpc_error(
            MeshRpcOperation::ExecuteDestroy,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "mesh_peer_execute_destroy");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }
}
