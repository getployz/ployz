mod bootstrap_seed;
mod joiner;
mod validation;

use ployz_api::DaemonRequest;
use ployz_model::MachineLifecycle;
use ployz_store_api::MachineMembershipStore;

use self::bootstrap_seed::{
    build_bootstrap_membership_seed, build_mesh_bootstrap_request,
    publish_bootstrap_membership_seed,
};
use self::joiner::{
    activate_joiner_lifecycle, joiner_self_record, wait_for_joiner_command_responder,
    wait_for_joiner_ready,
};
use self::validation::{validate_joined_machine_authority_posture, validate_joined_machine_subnet};
use super::super::operations::{
    MachineOperationRecord, MachineOperationStatus, MachineOperationStore,
};
use super::super::types::{
    MachineAddContext, MachineAddFailure, MachineAddStage, MachineAddTargetResult,
};
use super::bootstrap::bootstrap_remote_machine;
use super::coordination::{
    BootstrapSubnetClaim, assert_subnet_unique, consume_invite, release_reserved_subnet,
};
use super::remote::{
    ExpectedMachineRecord, ExpectedSubnetState, remote_daemon_identity, remote_rpc_expect_ok,
    wait_for_machine_record,
};
use super::rollback::rollback_machine_add_target;
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
    let mut joiner_id: Option<ployz_model::MachineId>;

    tracing::info!(%target, "machine add target: bootstrap starting");
    if let Err(err) =
        bootstrap_remote_machine(&target, &context.install, &context.ssh_options).await
    {
        let _ = release_reserved_subnet(subnet_claim).await;
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

    tracing::info!(%target, "machine add target: bootstrap identity starting");
    let remote_identity = match remote_daemon_identity(&target, &context.ssh_options).await {
        Ok(identity) => identity,
        Err(err) => {
            let _ = release_reserved_subnet(subnet_claim).await;
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
    };
    let bootstrap_record = build_bootstrap_membership_seed(&target, &remote_identity);
    joiner_id = Some(remote_identity.machine_id.clone());
    operation.artifacts.machine_id = Some(remote_identity.machine_id.clone());
    let _ = operation_store.save(&operation);
    tracing::info!(
        %target,
        joiner_id = %remote_identity.machine_id,
        "machine add target: publishing bootstrap membership seed"
    );
    if let Err(err) = publish_bootstrap_membership_seed(&context, &bootstrap_record).await {
        let _ = release_reserved_subnet(subnet_claim).await;
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
    stage = MachineAddStage::BootstrapPublished;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, joiner_id = %remote_identity.machine_id, "machine add target: bootstrap membership seed published");

    tracing::info!(%target, "machine add target: remote join starting");
    match remote_rpc_expect_ok(
        &target,
        DaemonRequest::MeshBootstrap {
            request: match build_mesh_bootstrap_request(&context, subnet_claim.subnet).await {
                Ok(request) => request,
                Err(err) => {
                    let _ = release_reserved_subnet(subnet_claim).await;
                    let _ =
                        rollback_machine_add_target(&context, &target, stage, joiner_id.as_ref())
                            .await;
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
            let _ = release_reserved_subnet(subnet_claim).await;
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
    }
    stage = MachineAddStage::Joined;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: remote join complete");

    tracing::info!(%target, joiner_id = %remote_identity.machine_id, "machine add target: waiting for NATS command responder");
    if let Err(err) = wait_for_joiner_command_responder(&context, &bootstrap_record).await {
        tracing::warn!(%target, joiner_id = %remote_identity.machine_id, error = %err, "machine add target: NATS command responder failed");
        let _ = release_reserved_subnet(subnet_claim).await;
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

    tracing::info!(%target, "machine add target: self-record starting");
    let record = match joiner_self_record(&context, &target, &bootstrap_record).await {
        Ok(record) => record,
        Err(err) => {
            let _ = release_reserved_subnet(subnet_claim).await;
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
        let _ = release_reserved_subnet(subnet_claim).await;
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
    if let Err(err) = validate_joined_machine_authority_posture(&record) {
        let _ = release_reserved_subnet(subnet_claim).await;
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
    if let Err(err) = context
        .store
        .upsert_self_machine(&record)
        .await
        .map_err(|err| format!("publish joined machine self-record: {err}"))
    {
        let _ = release_reserved_subnet(subnet_claim).await;
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
    stage = MachineAddStage::SelfRecorded;
    let _ = operation_store.update_stage(&mut operation, stage.to_string());
    tracing::info!(%target, "machine add target: self-record complete");

    let machine_id = record.id.clone();
    joiner_id = Some(machine_id.clone());
    if let Err(err) = consume_invite(&context, &invite_id, &machine_id).await {
        let _ = release_reserved_subnet(subnet_claim).await;
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
    tracing::info!(%target, joiner_id = %machine_id, "machine add target: waiting for remote ready");
    if let Err(err) = wait_for_joiner_ready(&context, &target, &record).await {
        let _ = release_reserved_subnet(subnet_claim).await;
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

    if record.lifecycle == MachineLifecycle::Active && record.subnet == Some(subnet_claim.subnet())
    {
        tracing::info!(
            %target,
            joiner_id = %machine_id,
            "machine add target: lifecycle already active"
        );
    } else {
        tracing::info!(%target, joiner_id = %machine_id, "machine add target: activating lifecycle");
        if let Err(err) = activate_joiner_lifecycle(&context, &record, subnet_claim.subnet()).await
        {
            tracing::warn!(%target, joiner_id = %machine_id, error = %err, "machine add target: activate lifecycle failed");
            let _ = release_reserved_subnet(subnet_claim).await;
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
    }

    tracing::info!(%target, joiner_id = %machine_id, "machine add target: waiting for observed machine record");
    if let Err(err) = wait_for_machine_record(
        &context.store,
        &machine_id,
        ExpectedMachineRecord::new(MachineLifecycle::Active, ExpectedSubnetState::Present),
    )
    .await
    {
        tracing::warn!(%target, joiner_id = %machine_id, error = %err, "machine add target: observed machine record failed");
        let _ = release_reserved_subnet(subnet_claim).await;
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
        tracing::error!(
            %target,
            joiner_id = %machine_id,
            subnet = %subnet_claim.subnet(),
            error = %err,
            "machine add target: subnet uniqueness invariant violated"
        );
        let _ = release_reserved_subnet(subnet_claim).await;
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

    let _ = release_reserved_subnet(subnet_claim).await;
    let _ = operation_store.update_stage(&mut operation, MachineAddStage::Finalized.to_string());
    if let Err(err) = refresh_bootstrap_peer_records_from_store(
        &context.network_dir,
        &context.store,
        &context.local_machine_id,
    )
    .await
    {
        tracing::warn!(
            %target,
            joiner_id = %machine_id,
            error = %err,
            "machine add target: failed to refresh bootstrap peer seed"
        );
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
