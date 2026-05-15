use std::time::Duration;

use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
use ployz_model::{DeployId, MachineId};
use ployz_node_runtime::{
    DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY, NodeRpcError, NodeRpcErrorKind, VolumeZfsMoveError,
    VolumeZfsMoveRequest, VolumeZfsNodeClient, VolumeZfsRpcTransport,
};
use ployz_orchestrator::deploy::participant::{MoveVolumeRequest, MoveVolumeResult};
use ployz_spec::Namespace;

pub(super) async fn run_volume_move_rpc<R: VolumeZfsRpcTransport>(
    transport: &R,
    machine_id: &MachineId,
    namespace: &Namespace,
    _deploy_id: &DeployId,
    request: MoveVolumeRequest,
    start_timeout: Duration,
    wait_timeout: Duration,
    poll_interval: Duration,
) -> ployz_error::Result<MoveVolumeResult> {
    let client = VolumeZfsNodeClient::new(transport.clone());
    let result = client
        .move_volume_and_wait(
            machine_id,
            namespace.as_str(),
            VolumeZfsMoveRequest {
                volume: request.volume,
                from_machine: request.from_machine,
                to_machine: request.to_machine,
                snapshot: request.snapshot,
            },
            start_timeout,
            wait_timeout,
            DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY.timeout,
            poll_interval,
        )
        .await
        .map_err(volume_move_error)?;
    Ok(MoveVolumeResult {
        snapshot: result.snapshot,
        snapshot_guid: result.snapshot_guid,
        bytes_transferred: result.bytes_transferred,
    })
}

fn volume_move_rpc_error(error: NodeRpcError) -> PloyzError {
    if matches!(
        error.kind,
        NodeRpcErrorKind::MissingPayload | NodeRpcErrorKind::Decode
    ) {
        return PloyzError::Deploy(DeployError::MissingNodePayload {
            payload: "volume zfs transfer",
        });
    }
    PloyzError::Deploy(DeployError::RemoteNodeError {
        operation: error.operation,
        code: error.code,
        message: error.message,
    })
}

fn volume_move_error(error: VolumeZfsMoveError) -> PloyzError {
    match error {
        VolumeZfsMoveError::SourceMismatch {
            volume,
            machine_id,
            from_machine,
        } => PloyzError::operation(
            "deploy_node_move_volume",
            format!(
                "move volume '{volume}' was sent to '{machine_id}' but request source was '{from_machine}'"
            ),
        ),
        VolumeZfsMoveError::Rpc(error) => volume_move_rpc_error(error),
        VolumeZfsMoveError::Timeout {
            transfer_id,
            detail,
        } => {
            let mut message = format!("timed out waiting for zfs transfer '{transfer_id}'");
            if let Some(detail) = detail {
                message.push_str(": ");
                message.push_str(&detail);
            }
            PloyzError::operation("volume_zfs_transfer", message)
        }
        VolumeZfsMoveError::TransferFailed { code, message } => {
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer",
                code: code.into(),
                message,
            })
        }
    }
}
