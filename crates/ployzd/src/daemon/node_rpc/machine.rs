use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{
    MachineLifecycleRpcOperation, MachineLifecycleRpcTransport, MachineOperationRpcOperation,
    MachineOperationRpcTransport, MachineStorageRpcOperation, MachineStorageRpcTransport,
    MachineUpdateRpcOperation, MachineUpdateRpcTransport, NodeRpcError, NodeRpcPolicy,
};

#[derive(Clone)]
pub(crate) struct NatsMachineOperationRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineOperationRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineOperationRpcTransport for NatsMachineOperationRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_operation_request(
        &self,
        machine_id: &MachineId,
        operation: MachineOperationRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_operation_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_operation_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMachineLifecycleRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineLifecycleRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineLifecycleRpcTransport for NatsMachineLifecycleRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_lifecycle_request(
        &self,
        machine_id: &MachineId,
        operation: MachineLifecycleRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_lifecycle_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_lifecycle_node_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMachineUpdateRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineUpdateRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineUpdateRpcTransport for NatsMachineUpdateRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_update_request(
        &self,
        machine_id: &MachineId,
        operation: MachineUpdateRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_update_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_update_node_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMachineStorageRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineStorageRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineStorageRpcTransport for NatsMachineStorageRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_request(
        &self,
        machine_id: &MachineId,
        operation: MachineStorageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_storage_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_storage_node_rpc_error(operation, error))
    }
}

fn machine_operation_subject(
    machine_id: &MachineId,
    operation: MachineOperationRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineOperationRpcOperation::Get => NodeCommandSubject::machine_operation_get(machine_id),
    }
}

fn machine_storage_subject(
    machine_id: &MachineId,
    operation: MachineStorageRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineStorageRpcOperation::StoragePromoteSelf => {
            NodeCommandSubject::machine_storage_promote_self(machine_id)
        }
        MachineStorageRpcOperation::StorageRestoreSelf => {
            NodeCommandSubject::machine_storage_restore_self(machine_id)
        }
    }
}

fn machine_lifecycle_subject(
    machine_id: &MachineId,
    operation: MachineLifecycleRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineLifecycleRpcOperation::TransitionSelf => {
            NodeCommandSubject::machine_transition_self(machine_id)
        }
    }
}

fn machine_update_subject(
    machine_id: &MachineId,
    operation: MachineUpdateRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineUpdateRpcOperation::PrepareUpdate => {
            NodeCommandSubject::machine_update_prepare(machine_id)
        }
        MachineUpdateRpcOperation::ExecuteUpdate => {
            NodeCommandSubject::machine_update_execute(machine_id)
        }
    }
}

fn machine_operation_rpc_error(
    operation: MachineOperationRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn machine_lifecycle_node_rpc_error(
    operation: MachineLifecycleRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn machine_update_node_rpc_error(
    operation: MachineUpdateRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn machine_storage_node_rpc_error(
    operation: MachineStorageRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

#[cfg(test)]
mod tests {
    use ployz_nats::RpcFailureKind;

    use super::*;

    #[test]
    fn machine_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [(
            MachineOperationRpcOperation::Get,
            NodeCommandSubject::machine_operation_get(&machine_id),
        )] {
            assert_eq!(machine_operation_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_operation_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_operation_rpc_error(
            MachineOperationRpcOperation::Get,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_operation_get");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn machine_lifecycle_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [(
            MachineLifecycleRpcOperation::TransitionSelf,
            NodeCommandSubject::machine_transition_self(&machine_id),
        )] {
            assert_eq!(machine_lifecycle_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_update_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MachineUpdateRpcOperation::PrepareUpdate,
                NodeCommandSubject::machine_update_prepare(&machine_id),
            ),
            (
                MachineUpdateRpcOperation::ExecuteUpdate,
                NodeCommandSubject::machine_update_execute(&machine_id),
            ),
        ] {
            assert_eq!(machine_update_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_lifecycle_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_lifecycle_node_rpc_error(
            MachineLifecycleRpcOperation::TransitionSelf,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_transition_self");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn machine_update_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_update_node_rpc_error(
            MachineUpdateRpcOperation::ExecuteUpdate,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_update_execute");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn machine_storage_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MachineStorageRpcOperation::StoragePromoteSelf,
                NodeCommandSubject::machine_storage_promote_self(&machine_id),
            ),
            (
                MachineStorageRpcOperation::StorageRestoreSelf,
                NodeCommandSubject::machine_storage_restore_self(&machine_id),
            ),
        ] {
            assert_eq!(machine_storage_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_storage_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_storage_node_rpc_error(
            MachineStorageRpcOperation::StoragePromoteSelf,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_storage_promote_self");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }
}
