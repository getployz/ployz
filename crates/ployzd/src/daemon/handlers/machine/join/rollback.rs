use ployz_api::DaemonRequest;
use tokio::time::{Duration, timeout};

use crate::daemon::ssh::SshOptions;

use super::super::types::{MachineAddContext, MachineAddStage};
use super::remote::remote_rpc;
use ployz_orchestrator::mesh::tasks::PeerSyncCommand;
use ployz_types::model::MachineId;

const REMOTE_CLEANUP_RPC_TIMEOUT: Duration = Duration::from_secs(10);

pub(in super::super) async fn remove_transient_peer(
    peer_sync_tx: &tokio::sync::mpsc::Sender<PeerSyncCommand>,
    machine_id: &MachineId,
) -> Result<(), String> {
    peer_sync_tx
        .send(PeerSyncCommand::RemoveTransient(machine_id.clone()))
        .await
        .map_err(|err| format!("failed to clear founder-local transient peer: {err}"))
}

pub(super) async fn rollback_machine_add_target(
    context: &MachineAddContext,
    target: &str,
    stage: MachineAddStage,
    joiner_id: Option<&MachineId>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    if matches!(
        stage,
        MachineAddStage::TransientPeerInstalled
            | MachineAddStage::PreAdmitted
            | MachineAddStage::Ready
            | MachineAddStage::Enabled
            | MachineAddStage::Finalized
    ) && let Some(joiner_id) = joiner_id
        && let Err(err) = remove_transient_peer(&context.peer_sync_tx, joiner_id).await
    {
        errors.push(err);
    }
    if matches!(
        stage,
        MachineAddStage::Joined
            | MachineAddStage::SelfRecorded
            | MachineAddStage::TransientPeerInstalled
            | MachineAddStage::Ready
            | MachineAddStage::Enabled
            | MachineAddStage::Finalized
    ) && let Err(err) =
        best_effort_remote_cleanup(target, &context.network_name, &context.ssh_options).await
    {
        errors.push(err);
    }

    if errors.is_empty() {
        return Ok(());
    }
    Err(errors.join("; "))
}

pub(in super::super) async fn best_effort_remote_cleanup(
    target: &str,
    network_name: &str,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    tracing::debug!(%target, %network_name, "machine add cleanup: mesh down starting");
    let down_error = match timeout(
        REMOTE_CLEANUP_RPC_TIMEOUT,
        remote_rpc(target, DaemonRequest::MeshStop { force: true }, ssh_options),
    )
    .await
    {
        Ok(Ok(response)) if response.ok => None,
        Ok(Ok(response)) => Some(super::remote::remote_response_error(&response)),
        Ok(Err(err)) => Some(err),
        Err(_) => Some(format!(
            "mesh down rpc exceeded {:?}",
            REMOTE_CLEANUP_RPC_TIMEOUT
        )),
    };
    tracing::debug!(
        %target,
        %network_name,
        had_error = down_error.is_some(),
        "machine add cleanup: mesh down complete"
    );
    tracing::debug!(%target, %network_name, "machine add cleanup: mesh destroy starting");
    let destroy_error = match timeout(
        REMOTE_CLEANUP_RPC_TIMEOUT,
        remote_rpc(
            target,
            DaemonRequest::MeshDestroy {
                network: network_name.to_string(),
            },
            ssh_options,
        ),
    )
    .await
    {
        Ok(Ok(response)) if response.ok => None,
        Ok(Ok(response)) => Some(super::remote::remote_response_error(&response)),
        Ok(Err(err)) => Some(err),
        Err(_) => Some(format!(
            "mesh destroy rpc exceeded {:?}",
            REMOTE_CLEANUP_RPC_TIMEOUT
        )),
    };
    tracing::debug!(
        %target,
        %network_name,
        had_error = destroy_error.is_some(),
        "machine add cleanup: mesh destroy complete"
    );

    let mut errors = Vec::new();
    if let Some(err) = down_error {
        errors.push(format!("mesh down: {err}"));
    }
    if let Some(err) = destroy_error {
        errors.push(format!("mesh destroy: {err}"));
    }

    if errors.is_empty() {
        return Ok(());
    }

    Err(errors.join("; "))
}
