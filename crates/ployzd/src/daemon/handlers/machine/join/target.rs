use ipnet::Ipv4Net;
use ployz_api::{DaemonRequest, MachineTransitionGoal, MeshBootstrapRequest};
use ployz_store_api::MachineRegistry;
use ployz_types::model::{MachineLifecycle, MachineMembership, PublicKey};

use super::super::operations::{
    MachineOperationRecord, MachineOperationStatus, MachineOperationStore,
};
use super::super::types::{
    MachineAddContext, MachineAddFailure, MachineAddStage, MachineAddTargetResult,
};
use super::bootstrap::bootstrap_remote_machine;
use super::coordination::{
    BootstrapSubnetClaim, assert_subnet_unique, consume_invite, persist_machine_control_target,
    release_reserved_subnet,
};
use super::remote::{
    ExpectedSubnetState, overlay_rpc_expect_ok_with_read_timeout, remote_rpc_expect_ok,
    remote_self_record, wait_for_machine_projection, wait_for_overlay_ready, wait_for_remote_ready,
};
use super::rollback::rollback_machine_add_target;
use crate::daemon::handlers::peer_rpc::PEER_RPC_DESTRUCTIVE_READ_TIMEOUT;
use crate::mesh_state::bootstrap::refresh_bootstrap_peer_records_from_store;

pub(super) async fn run_machine_add_target(
    context: MachineAddContext,
    operation_store: MachineOperationStore,
    mut operation: MachineOperationRecord,
    target: String,
    invite_id: String,
    subnet_claim: BootstrapSubnetClaim,
) -> MachineAddTargetResult {
    let mut stage;
    let mut joiner_id = None;

    tracing::info!(%target, "machine add target: bootstrap starting");
    if let Err(err) =
        bootstrap_remote_machine(&target, &context.install, &context.ssh_options).await
    {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Preflight { reason: err },
        };
    }
    stage = MachineAddStage::Bootstrapped;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: bootstrap complete");

    tracing::info!(%target, "machine add target: remote join starting");
    match remote_rpc_expect_ok(
        &target,
        DaemonRequest::MeshBootstrap {
            request: match build_mesh_bootstrap_request(
                &context,
                subnet_claim.subnet,
                Some(target.clone()),
            )
            .await
            {
                Ok(request) => request,
                Err(err) => {
                    let _ = release_reserved_subnet(&context, &subnet_claim).await;
                    let _ = operation_store.update_status(
                        &mut operation,
                        MachineOperationStatus::Failed,
                        Some(err.clone()),
                    );
                    return MachineAddTargetResult::Failed {
                        target,
                        failure: MachineAddFailure::Join { reason: err },
                    };
                }
            },
        },
        &context.ssh_options,
    )
    .await
    {
        Ok(()) => {}
        Err(err) => {
            let _ = release_reserved_subnet(&context, &subnet_claim).await;
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return MachineAddTargetResult::Failed {
                target,
                failure: MachineAddFailure::Join { reason: err },
            };
        }
    }
    stage = MachineAddStage::Joined;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: remote join complete");

    tracing::info!(%target, "machine add target: self-record starting");
    let mut record = match remote_self_record(&target, &context.ssh_options).await {
        Ok(record) => record,
        Err(err) => {
            let _ = release_reserved_subnet(&context, &subnet_claim).await;
            let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
            let _ = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Failed,
                Some(err.clone()),
            );
            return MachineAddTargetResult::Failed {
                target,
                failure: MachineAddFailure::SelfRecord { reason: err },
            };
        }
    };
    if let Err(err) = validate_joined_machine_subnet(&record, subnet_claim.subnet) {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::SelfRecord { reason: err },
        };
    }
    record.control_target = Some(target.clone());
    stage = MachineAddStage::SelfRecorded;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: self-record complete");

    let machine_id = record.id.clone();
    let joiner_overlay_ip = record.overlay_ip;
    if let Err(err) = persist_machine_control_target(&context, &machine_id, &target).await {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::SelfRecord { reason: err },
        };
    }
    operation.artifacts.machine_id = Some(machine_id.clone());
    let _ = operation_store.save(&operation);
    joiner_id = Some(machine_id.clone());
    if let Err(err) = consume_invite(&context, &invite_id, &machine_id).await {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Join { reason: err },
        };
    }
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: transient peer install starting");
    if let Err(err) = upsert_transient_peer(&context.peer_sync_tx, record).await {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Preflight { reason: err },
        };
    }
    stage = MachineAddStage::TransientPeerInstalled;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: transient peer installed");

    tracing::info!(%target, joiner_id = %machine_id, "machine add target: waiting for remote ready");
    if let Err(err) = wait_for_remote_ready(&target, &context.ssh_options).await {
        let _ = release_reserved_subnet(&context, &subnet_claim).await;
        tracing::warn!(
            %target,
            joiner_id = %machine_id,
            error = %err,
            "machine add target: remote ready failed"
        );
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Ready { reason: err },
        };
    }
    stage = MachineAddStage::Ready;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: remote ready");
    let _ = release_reserved_subnet(&context, &subnet_claim).await;

    tracing::info!(%target, joiner_id = %machine_id, "machine add target: waiting for overlay ready");
    let joiner_ref = MachineMembership::seed(
        machine_id.clone(),
        PublicKey([0; 32]),
        joiner_overlay_ip,
        Some(subnet_claim.subnet),
        vec![],
    );
    if let Err(err) = wait_for_overlay_ready(&joiner_ref, subnet_claim.peer_rpc_port).await {
        tracing::warn!(%target, joiner_id = %machine_id, error = %err, "machine add target: overlay ready failed");
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Enable { reason: err },
        };
    }

    tracing::info!(%target, joiner_id = %machine_id, "machine add target: activating lifecycle");
    if let Err(err) = overlay_rpc_expect_ok_with_read_timeout(
        joiner_overlay_ip,
        subnet_claim.peer_rpc_port,
        DaemonRequest::MachineTransitionSelf {
            goal: MachineTransitionGoal::Activate,
            assigned_subnet: Some(subnet_claim.subnet()),
            force: false,
        },
        PEER_RPC_DESTRUCTIVE_READ_TIMEOUT,
    )
    .await
    {
        tracing::warn!(%target, joiner_id = %machine_id, error = %err, "machine add target: activate lifecycle failed");
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Enable { reason: err },
        };
    }

    tracing::info!(%target, joiner_id = %machine_id, "machine add target: waiting for lifecycle projection");
    if let Err(err) = wait_for_machine_projection(
        &context.store,
        &machine_id,
        MachineLifecycle::Active,
        ExpectedSubnetState::Present,
    )
    .await
    {
        tracing::warn!(%target, joiner_id = %machine_id, error = %err, "machine add target: lifecycle projection failed");
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Enable { reason: err },
        };
    }
    stage = MachineAddStage::Enabled;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: lifecycle active");

    if let Err(err) = assert_subnet_unique(&context.store, &machine_id, subnet_claim.subnet()).await
    {
        let quorum_peer_ids = subnet_claim
            .quorum_peer_ids()
            .into_iter()
            .map(|machine_id| machine_id.0)
            .collect::<Vec<_>>();
        tracing::error!(
            %target,
            joiner_id = %machine_id,
            subnet = %subnet_claim.subnet(),
            error = %err,
            quorum_peer_ids = ?quorum_peer_ids,
            "machine add target: subnet uniqueness invariant violated"
        );
        let _ = rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref()).await;
        let _ = operation_store.update_status(
            &mut operation,
            MachineOperationStatus::Failed,
            Some(err.clone()),
        );
        return MachineAddTargetResult::Failed {
            target,
            failure: MachineAddFailure::Enable { reason: err },
        };
    }

    let _ = operation_store.update_stage(&mut operation, MachineAddStage::Finalized.to_string());
    if let Err(err) = refresh_bootstrap_peer_records_from_store(
        &context.network_dir,
        &context.store,
        &context.local_machine_id,
    )
    .await
    {
        tracing::warn!(%target, error = %err, "failed to refresh bootstrap seed cache after machine add");
    }
    let _ = operation_store.update_status(&mut operation, MachineOperationStatus::Succeeded, None);
    tracing::info!(
        %target,
        joiner_id = %machine_id,
        "machine add target: awaiting self-publication"
    );
    MachineAddTargetResult::AwaitingSelfPublication {
        target,
        joiner_id: machine_id,
    }
}

pub(super) async fn upsert_transient_peer(
    peer_sync_tx: &tokio::sync::mpsc::Sender<ployz_orchestrator::mesh::tasks::PeerSyncCommand>,
    record: MachineMembership,
) -> Result<(), String> {
    peer_sync_tx
        .send(
            ployz_orchestrator::mesh::tasks::PeerSyncCommand::UpsertTransient(record.observation()),
        )
        .await
        .map_err(|err| format!("failed to install founder-local transient peer: {err}"))
}

async fn build_mesh_bootstrap_request(
    context: &MachineAddContext,
    assigned_subnet: Ipv4Net,
    self_control_target: Option<String>,
) -> Result<MeshBootstrapRequest, String> {
    let bootstrap_peers = context
        .store
        .list_machines()
        .await
        .map_err(|err| format!("list machines for bootstrap: {err}"))?
        .into_iter()
        .filter(|machine| !machine.endpoints.is_empty())
        .collect::<Vec<_>>();

    Ok(MeshBootstrapRequest {
        network_id: context.network_id.clone(),
        network_name: context.network_name.clone(),
        cluster_cidr: context.cluster_cidr.clone(),
        assigned_subnet,
        self_control_target,
        bootstrap_peers,
    })
}

fn validate_joined_machine_subnet(
    record: &MachineMembership,
    expected_subnet: Ipv4Net,
) -> Result<(), String> {
    match record.subnet {
        Some(subnet) if subnet == expected_subnet => Ok(()),
        Some(subnet) => Err(format!(
            "remote machine '{}' reported subnet '{}' but founder reserved '{}'",
            record.id, subnet, expected_subnet
        )),
        None => Err(format!(
            "remote machine '{}' reported no subnet but founder reserved '{}'",
            record.id, expected_subnet
        )),
    }
}
