use std::time::Duration;

use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
use ployz_model::{DeployId, MachineId};
use ployz_node_api::{NodeVolumeZfsTransferPayload, NodeVolumeZfsTransferState};
use ployz_node_runtime::{
    DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY, NodeRpcError, NodeRpcErrorKind, NodeRpcPolicy,
    VolumeZfsNodeClient, VolumeZfsRpcTransport,
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
    let MoveVolumeRequest {
        volume,
        from_machine,
        to_machine,
        snapshot,
    } = request;
    if *machine_id != from_machine {
        return Err(PloyzError::operation(
            "deploy_node_move_volume",
            format!(
                "move volume '{volume}' was sent to '{machine_id}' but request source was '{from_machine}'"
            ),
        ));
    }
    let client = VolumeZfsNodeClient::new(transport.clone());
    let move_client = client.with_policy(NodeRpcPolicy {
        timeout: start_timeout,
    });
    let response = move_client
        .send(
            machine_id,
            namespace.as_str(),
            &volume,
            &snapshot,
            &to_machine,
            None,
        )
        .await
        .map_err(volume_move_rpc_error)?;
    wait_for_volume_transfer(
        &client,
        machine_id,
        response.transfer.id,
        wait_timeout,
        DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY.timeout,
        poll_interval,
    )
    .await
}

async fn wait_for_volume_transfer<R: VolumeZfsRpcTransport>(
    client: &VolumeZfsNodeClient<R>,
    machine_id: &MachineId,
    transfer_id: String,
    timeout: Duration,
    poll_rpc_timeout: Duration,
    poll_interval: Duration,
) -> ployz_error::Result<MoveVolumeResult> {
    let started = tokio::time::Instant::now();
    let mut retry_delay = poll_interval;
    loop {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(PloyzError::operation(
                "volume_zfs_transfer",
                format!("timed out waiting for zfs transfer '{transfer_id}'"),
            ));
        };
        let poll_client = client.with_policy(NodeRpcPolicy {
            timeout: std::cmp::min(poll_rpc_timeout, remaining),
        });
        let response = match tokio::time::timeout(
            remaining,
            poll_client.transfer_get(machine_id, &transfer_id),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                if error.kind != NodeRpcErrorKind::Transport {
                    return Err(volume_move_rpc_error(error));
                }
                if started.elapsed() >= timeout {
                    return Err(volume_move_rpc_error(error));
                }
                tracing::warn!(
                    %error,
                    transfer_id,
                    machine_id = %machine_id,
                    "retrying zfs transfer status read after transient RPC failure"
                );
                tokio::time::sleep(retry_delay + retry_jitter(retry_delay)).await;
                retry_delay = std::cmp::min(retry_delay + retry_delay, Duration::from_secs(30));
                continue;
            }
            Err(error) => {
                return Err(PloyzError::operation(
                    "volume_zfs_transfer",
                    format!("timed out waiting for zfs transfer '{transfer_id}': {error}"),
                ));
            }
        };
        retry_delay = poll_interval;
        if let Some(result) = volume_move_result_from_transfer(response)? {
            return Ok(result);
        }
        if started.elapsed() >= timeout {
            return Err(PloyzError::operation(
                "volume_zfs_transfer",
                format!("timed out waiting for zfs transfer '{transfer_id}'"),
            ));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

fn retry_jitter(delay: Duration) -> Duration {
    let max_millis = (delay.as_millis() as u64 / 2).min(1_000);
    if max_millis == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(rand::random::<u64>() % (max_millis + 1))
    }
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

pub(super) fn volume_move_result_from_transfer(
    payload: NodeVolumeZfsTransferPayload,
) -> ployz_error::Result<Option<MoveVolumeResult>> {
    match payload.transfer.state {
        NodeVolumeZfsTransferState::Succeeded {
            snapshot_guid,
            bytes_transferred,
            ..
        } => Ok(Some(MoveVolumeResult {
            snapshot: payload.transfer.snapshot_name,
            snapshot_guid,
            bytes_transferred,
        })),
        NodeVolumeZfsTransferState::Failed { last_error, .. } => {
            Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer",
                code: "failed".into(),
                message: last_error,
            }))
        }
        NodeVolumeZfsTransferState::Interrupted { last_error, .. } => {
            Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer",
                code: "interrupted".into(),
                message: last_error.unwrap_or_else(|| {
                    format!("zfs transfer '{}' did not succeed", payload.transfer.id)
                }),
            }))
        }
        NodeVolumeZfsTransferState::Running { .. } => Ok(None),
    }
}
