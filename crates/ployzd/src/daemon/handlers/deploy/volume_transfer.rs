use std::time::Duration;

use ployz_api::{DaemonPayload, DaemonResponse, VolumeZfsTransferPayload, VolumeZfsTransferState};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_orchestrator::deploy::participant::{MoveVolumeRequest, MoveVolumeResult};
use ployz_types::Error as PloyzError;
use ployz_types::error::DeployError;
use ployz_types::model::{DeployId, MachineId};
use ployz_types::spec::Namespace;

#[async_trait::async_trait]
pub(super) trait DeployMoveRpcClient: Clone + Send + Sync {
    fn with_rpc_policy(&self, policy: RpcPolicy) -> Self;

    async fn request(
        &self,
        subject: NodeCommandSubject,
        request: &ployz_api::DaemonRequest,
    ) -> std::result::Result<DaemonResponse, RpcFailure>;
}

#[async_trait::async_trait]
impl DeployMoveRpcClient for NatsNodeRpcClient {
    fn with_rpc_policy(&self, policy: RpcPolicy) -> Self {
        self.clone().with_policy(policy)
    }

    async fn request(
        &self,
        subject: NodeCommandSubject,
        request: &ployz_api::DaemonRequest,
    ) -> std::result::Result<DaemonResponse, RpcFailure> {
        NatsNodeRpcClient::request(self, subject, request).await
    }
}
pub(super) async fn run_volume_move_rpc<R: DeployMoveRpcClient>(
    client: &R,
    machine_id: &MachineId,
    namespace: &Namespace,
    _deploy_id: &DeployId,
    request: MoveVolumeRequest,
    start_timeout: Duration,
    wait_timeout: Duration,
    poll_interval: Duration,
) -> ployz_types::Result<MoveVolumeResult> {
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
    let move_client = client.with_rpc_policy(RpcPolicy {
        timeout: start_timeout,
    });
    let response = move_client
        .request(
            NodeCommandSubject::volume_zfs_send(machine_id),
            &ployz_api::DaemonRequest::VolumeZfsSend {
                namespace: namespace.as_str().to_string(),
                volume,
                snapshot,
                target_machine: to_machine.as_str().to_string(),
                from_snapshot: None,
            },
        )
        .await
        .map_err(|error| volume_move_rpc_error("volume_zfs_send", error))?;
    if !response.ok {
        return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
            operation: "volume_zfs_send",
            code: response.code,
            message: response.message,
        }));
    }
    let Some(DaemonPayload::VolumeZfsTransfer(payload)) = response.payload else {
        return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
            payload: "volume zfs transfer",
        }));
    };
    wait_for_volume_transfer(
        client,
        machine_id,
        payload.transfer.id,
        wait_timeout,
        super::DEPLOY_VOLUME_MOVE_POLL_RPC_TIMEOUT,
        poll_interval,
    )
    .await
}

async fn wait_for_volume_transfer<R: DeployMoveRpcClient>(
    client: &R,
    machine_id: &MachineId,
    transfer_id: String,
    timeout: Duration,
    poll_rpc_timeout: Duration,
    poll_interval: Duration,
) -> ployz_types::Result<MoveVolumeResult> {
    let started = tokio::time::Instant::now();
    let mut retry_delay = poll_interval;
    loop {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(PloyzError::operation(
                "volume_zfs_transfer",
                format!("timed out waiting for zfs transfer '{transfer_id}'"),
            ));
        };
        let poll_client = client.with_rpc_policy(RpcPolicy {
            timeout: std::cmp::min(poll_rpc_timeout, remaining),
        });
        let response = match tokio::time::timeout(
            remaining,
            poll_client.request(
                NodeCommandSubject::volume_zfs_transfer_get(machine_id),
                &ployz_api::DaemonRequest::VolumeZfsTransferGet {
                    id: transfer_id.clone(),
                },
            ),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                if started.elapsed() >= timeout {
                    return Err(volume_move_rpc_error("volume_zfs_transfer_get", error));
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
        if !response.ok {
            return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer_get",
                code: response.code,
                message: response.message,
            }));
        }
        let Some(DaemonPayload::VolumeZfsTransfer(payload)) = response.payload else {
            return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs transfer",
            }));
        };
        if let Some(result) = volume_move_result_from_transfer(payload)? {
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

fn volume_move_rpc_error(operation: &'static str, error: RpcFailure) -> PloyzError {
    PloyzError::Deploy(DeployError::RemoteNodeError {
        operation,
        code: error.code().into(),
        message: error.message,
    })
}

pub(super) fn volume_move_result_from_transfer(
    payload: VolumeZfsTransferPayload,
) -> ployz_types::Result<Option<MoveVolumeResult>> {
    match payload.transfer.state {
        VolumeZfsTransferState::Succeeded {
            snapshot_guid,
            bytes_transferred,
            ..
        } => Ok(Some(MoveVolumeResult {
            snapshot: payload.transfer.snapshot_name,
            snapshot_guid,
            bytes_transferred,
        })),
        VolumeZfsTransferState::Failed { last_error, .. } => {
            Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer",
                code: "failed".into(),
                message: last_error,
            }))
        }
        VolumeZfsTransferState::Interrupted { last_error, .. } => {
            Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "volume_zfs_transfer",
                code: "interrupted".into(),
                message: last_error.unwrap_or_else(|| {
                    format!("zfs transfer '{}' did not succeed", payload.transfer.id)
                }),
            }))
        }
        VolumeZfsTransferState::Running { .. } => Ok(None),
    }
}
