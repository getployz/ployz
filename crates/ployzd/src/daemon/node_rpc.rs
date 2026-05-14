use async_trait::async_trait;
use ployz_model::MachineId;
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{
    NodeRpcError, NodeRpcPolicy, VolumeZfsRpcOperation, VolumeZfsRpcTransport,
};

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

fn node_rpc_error(operation: VolumeZfsRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

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
