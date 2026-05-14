use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{DeployRpcOperation, DeployRpcTransport, NodeRpcError, NodeRpcPolicy};

#[derive(Clone)]
pub(crate) struct NatsDeployRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsDeployRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DeployRpcTransport for NatsDeployRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn deploy_request(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(deploy_subject(machine_id, operation), request)
            .await
            .map_err(|error| deploy_node_rpc_error(operation, error))
    }
}

fn deploy_subject(machine_id: &MachineId, operation: DeployRpcOperation) -> NodeCommandSubject {
    match operation {
        DeployRpcOperation::InspectNamespace => {
            NodeCommandSubject::deploy_inspect_namespace(machine_id)
        }
        DeployRpcOperation::StartCandidate => {
            NodeCommandSubject::deploy_start_candidate(machine_id)
        }
        DeployRpcOperation::CloneVolume | DeployRpcOperation::CleanupVolumeClone => {
            NodeCommandSubject::deploy_clone_volume(machine_id)
        }
        DeployRpcOperation::DrainInstance => NodeCommandSubject::deploy_drain_instance(machine_id),
        DeployRpcOperation::RemoveInstance => {
            NodeCommandSubject::deploy_remove_instance(machine_id)
        }
    }
}

fn deploy_node_rpc_error(operation: DeployRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                DeployRpcOperation::InspectNamespace,
                NodeCommandSubject::deploy_inspect_namespace(&machine_id),
            ),
            (
                DeployRpcOperation::StartCandidate,
                NodeCommandSubject::deploy_start_candidate(&machine_id),
            ),
            (
                DeployRpcOperation::CloneVolume,
                NodeCommandSubject::deploy_clone_volume(&machine_id),
            ),
            (
                DeployRpcOperation::CleanupVolumeClone,
                NodeCommandSubject::deploy_clone_volume(&machine_id),
            ),
            (
                DeployRpcOperation::DrainInstance,
                NodeCommandSubject::deploy_drain_instance(&machine_id),
            ),
            (
                DeployRpcOperation::RemoveInstance,
                NodeCommandSubject::deploy_remove_instance(&machine_id),
            ),
        ] {
            assert_eq!(deploy_subject(&machine_id, operation), expected);
        }
    }
}
