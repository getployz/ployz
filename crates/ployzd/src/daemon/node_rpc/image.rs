use async_trait::async_trait;
use ployz_model::{
    ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest, MachineId,
};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{
    ImageNodeClient, ImageNodeResponse, ImageRpcOperation, ImageRpcTransport, NodeRpcError,
    NodeRpcPolicy,
};

use crate::daemon::DaemonState;

#[derive(Clone)]
pub(crate) struct NatsImageRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsImageRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ImageRpcTransport for NatsImageRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn image_request(
        &self,
        machine_id: &MachineId,
        operation: ImageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(image_subject(machine_id, operation), request)
            .await
            .map_err(|error| image_node_rpc_error(operation, error))
    }
}

impl DaemonState {
    pub(crate) async fn request_image_receive_session(
        &self,
        target_machine: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<ImageNodeResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image receive session: {error}"))?;
        ImageNodeClient::new(NatsImageRpcTransport::new(client))
            .receive_session(target_machine, request)
            .await
            .map_err(|error| {
                format!("request image receive session from {target_machine}: {error}")
            })
    }

    pub(crate) async fn request_image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageNodeResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image distribute: {error}"))?;
        ImageNodeClient::new(NatsImageRpcTransport::new(client))
            .distribute(source_machine, request)
            .await
            .map_err(|error| format!("request image distribute from {source_machine}: {error}"))
    }

    pub(crate) async fn request_image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageNodeResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image import: {error}"))?;
        ImageNodeClient::new(NatsImageRpcTransport::new(client))
            .received_import(target_machine, request)
            .await
            .map_err(|error| format!("request image import from {target_machine}: {error}"))
    }
}

fn image_subject(machine_id: &MachineId, operation: ImageRpcOperation) -> NodeCommandSubject {
    match operation {
        ImageRpcOperation::ReceiveSession => NodeCommandSubject::image_receive_session(machine_id),
        ImageRpcOperation::Distribute => NodeCommandSubject::image_distribute(machine_id),
        ImageRpcOperation::ReceivedImport => NodeCommandSubject::image_received_import(machine_id),
    }
}

fn image_node_rpc_error(operation: ImageRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                ImageRpcOperation::ReceiveSession,
                NodeCommandSubject::image_receive_session(&machine_id),
            ),
            (
                ImageRpcOperation::Distribute,
                NodeCommandSubject::image_distribute(&machine_id),
            ),
            (
                ImageRpcOperation::ReceivedImport,
                NodeCommandSubject::image_received_import(&machine_id),
            ),
        ] {
            assert_eq!(image_subject(&machine_id, operation), expected);
        }
    }
}
