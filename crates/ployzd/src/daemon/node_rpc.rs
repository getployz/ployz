use async_trait::async_trait;
use ployz_model::{
    ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest, MachineId,
};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{
    DeployRpcOperation, DeployRpcTransport, ImageNodeClient, ImageNodeResponse, ImageRpcOperation,
    ImageRpcTransport, NodeRpcError, NodeRpcPolicy, VolumeZfsRpcOperation, VolumeZfsRpcTransport,
};

use super::DaemonState;

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

#[derive(Clone)]
pub(crate) struct NatsVolumeZfsRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsVolumeZfsRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl VolumeZfsRpcTransport for NatsVolumeZfsRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn volume_zfs_request(
        &self,
        machine_id: &MachineId,
        operation: VolumeZfsRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(volume_zfs_subject(machine_id, operation), request)
            .await
            .map_err(|error| node_rpc_error(operation, error))
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

fn image_subject(machine_id: &MachineId, operation: ImageRpcOperation) -> NodeCommandSubject {
    match operation {
        ImageRpcOperation::ReceiveSession => NodeCommandSubject::image_receive_session(machine_id),
        ImageRpcOperation::Distribute => NodeCommandSubject::image_distribute(machine_id),
        ImageRpcOperation::ReceivedImport => NodeCommandSubject::image_received_import(machine_id),
    }
}

fn volume_zfs_subject(
    machine_id: &MachineId,
    operation: VolumeZfsRpcOperation,
) -> NodeCommandSubject {
    match operation {
        VolumeZfsRpcOperation::Send => NodeCommandSubject::volume_zfs_send(machine_id),
        VolumeZfsRpcOperation::TransferGet => {
            NodeCommandSubject::volume_zfs_transfer_get(machine_id)
        }
        VolumeZfsRpcOperation::PeerSnapshot => NodeCommandSubject::volume_zfs_snapshot(machine_id),
        VolumeZfsRpcOperation::PeerSnapshotGuid => {
            NodeCommandSubject::volume_zfs_snapshot_guid(machine_id)
        }
        VolumeZfsRpcOperation::PeerStartSend => {
            NodeCommandSubject::volume_zfs_start_send(machine_id)
        }
    }
}

fn deploy_node_rpc_error(operation: DeployRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn image_node_rpc_error(operation: ImageRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn node_rpc_error(operation: VolumeZfsRpcOperation, error: RpcFailure) -> NodeRpcError {
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

    #[test]
    fn volume_zfs_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                VolumeZfsRpcOperation::Send,
                NodeCommandSubject::volume_zfs_send(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::TransferGet,
                NodeCommandSubject::volume_zfs_transfer_get(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::PeerSnapshot,
                NodeCommandSubject::volume_zfs_snapshot(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::PeerSnapshotGuid,
                NodeCommandSubject::volume_zfs_snapshot_guid(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::PeerStartSend,
                NodeCommandSubject::volume_zfs_start_send(&machine_id),
            ),
        ] {
            assert_eq!(volume_zfs_subject(&machine_id, operation), expected);
        }
    }
}
