use ipnet::Ipv4Net;
use ployz_api::{DaemonRequest, MachineTransitionGoal, MeshBootstrapRequest};
use ployz_nats::NodeCommandSubject;
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_store_api::MachineMembershipStore;
use ployz_types::model::{
    MachineLifecycle, MachineMembership, RegionRole, StorageParticipation, management_ip_from_key,
};

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
    ExpectedMachineRecord, ExpectedSubnetState, nats_rpc_expect_ok, nats_self_record,
    remote_daemon_identity, remote_rpc_expect_ok, remote_self_record, wait_for_machine_record,
    wait_for_nats_command_responder, wait_for_nats_ready, wait_for_remote_ready,
};
use super::rollback::rollback_machine_add_target;
use crate::mesh_state::bootstrap::refresh_bootstrap_peer_records_from_store;

pub(super) async fn run_machine_add_target(
    context: MachineAddContext,
    operation_store: MachineOperationStore,
    mut operation: MachineOperationRecord,
    target: String,
    invite_id: String,
    mut subnet_claim: BootstrapSubnetClaim,
) -> MachineAddTargetResult {
    let mut stage;
    let mut joiner_id: Option<ployz_types::model::MachineId>;

    tracing::info!(%target, "machine add target: bootstrap starting");
    if let Err(err) =
        bootstrap_remote_machine(&target, &context.install, &context.ssh_options).await
    {
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
            let _ = release_reserved_subnet(&mut subnet_claim).await;
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
    let bootstrap_overlay_ip = management_ip_from_key(&remote_identity.public_key);
    let mut bootstrap_record = MachineMembership::seed(
        remote_identity.machine_id.clone(),
        remote_identity.public_key.clone(),
        bootstrap_overlay_ip,
        Some(subnet_claim.subnet),
        bootstrap_wireguard_endpoints(&target),
    );
    bootstrap_record.storage = true;
    bootstrap_record.storage_participation = StorageParticipation::Candidate;
    bootstrap_record.region_role = RegionRole::Compute;
    bootstrap_record.created_at = ployz_types::time::now_unix_secs();
    bootstrap_record.updated_at = bootstrap_record.created_at;
    joiner_id = Some(remote_identity.machine_id.clone());
    operation.artifacts.machine_id = Some(remote_identity.machine_id.clone());
    let _ = operation_store.save(&operation);
    tracing::info!(
        %target,
        joiner_id = %remote_identity.machine_id,
        "machine add target: publishing bootstrap membership seed"
    );
    if let Err(err) = publish_bootstrap_membership_seed(&context, &bootstrap_record).await {
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
                    let _ = release_reserved_subnet(&mut subnet_claim).await;
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
            let _ = release_reserved_subnet(&mut subnet_claim).await;
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
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
            let _ = release_reserved_subnet(&mut subnet_claim).await;
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
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
            let _ = release_reserved_subnet(&mut subnet_claim).await;
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
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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
        let _ = release_reserved_subnet(&mut subnet_claim).await;
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

    let _ = release_reserved_subnet(&mut subnet_claim).await;
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

fn bootstrap_wireguard_endpoints(target: &str) -> Vec<String> {
    let host = target
        .rsplit_once('@')
        .map_or(target, |(_, host)| host)
        .trim();
    let Some(host) = host.split_whitespace().next() else {
        return Vec::new();
    };
    if host.is_empty() {
        return Vec::new();
    }

    if host.starts_with('[') {
        let Some((address, _rest)) = host[1..].split_once(']') else {
            return Vec::new();
        };
        return vec![format!("[{address}]:{DEFAULT_LISTEN_PORT}")];
    }

    if host.contains(':') {
        return vec![format!("[{host}]:{DEFAULT_LISTEN_PORT}")];
    }

    vec![format!("{host}:{DEFAULT_LISTEN_PORT}")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_wireguard_endpoint_uses_ssh_target_host() {
        assert_eq!(
            bootstrap_wireguard_endpoints("root@192.168.227.3"),
            vec!["192.168.227.3:51820"]
        );
    }

    #[test]
    fn bootstrap_wireguard_endpoint_brackets_ipv6_hosts() {
        assert_eq!(
            bootstrap_wireguard_endpoints("root@fd00::12"),
            vec!["[fd00::12]:51820"]
        );
        assert_eq!(
            bootstrap_wireguard_endpoints("root@[fd00::12]"),
            vec!["[fd00::12]:51820"]
        );
    }
}

async fn publish_bootstrap_membership_seed(
    context: &MachineAddContext,
    record: &MachineMembership,
) -> Result<(), String> {
    if let Some(existing) =
        super::super::list::find_machine_record(&context.store, &record.id).await?
    {
        return Err(format!(
            "machine '{}' already exists with lifecycle '{}'",
            existing.id, existing.lifecycle
        ));
    }
    context
        .store
        .upsert_self_machine(record)
        .await
        .map_err(|err| format!("publish bootstrap membership seed: {err}"))
}

async fn wait_for_joiner_command_responder(
    context: &MachineAddContext,
    machine: &MachineMembership,
) -> Result<(), String> {
    if let Some(client) = &context.nats_rpc {
        return wait_for_nats_command_responder(client, machine).await;
    }

    #[cfg(test)]
    {
        let _ = machine;
        return Ok(());
    }

    #[cfg(not(test))]
    {
        let _ = machine;
        Err("NATS RPC client unavailable".into())
    }
}

async fn joiner_self_record(
    context: &MachineAddContext,
    target: &str,
    machine: &MachineMembership,
) -> Result<MachineMembership, String> {
    if let Some(client) = &context.nats_rpc {
        return nats_self_record(client, machine).await;
    }
    remote_self_record(target, &context.ssh_options).await
}

async fn wait_for_joiner_ready(
    context: &MachineAddContext,
    target: &str,
    machine: &MachineMembership,
) -> Result<(), String> {
    if let Some(client) = &context.nats_rpc {
        return wait_for_nats_ready(client, machine).await;
    }
    wait_for_remote_ready(target, &context.ssh_options).await
}

async fn activate_joiner_lifecycle(
    context: &MachineAddContext,
    record: &MachineMembership,
    assigned_subnet: Ipv4Net,
) -> Result<(), String> {
    let request = DaemonRequest::MachineTransitionSelf {
        goal: MachineTransitionGoal::Activate,
        assigned_subnet: Some(assigned_subnet),
        force: false,
    };
    if let Some(client) = &context.nats_rpc {
        return nats_rpc_expect_ok(
            client,
            NodeCommandSubject::machine_transition_self(&record.id),
            request,
        )
        .await;
    }

    #[cfg(test)]
    {
        let mut active_record = record.clone();
        active_record
            .apply_lifecycle_transition(ployz_types::model::MachineLifecycleTransition {
                goal: ployz_types::model::MachineLifecycleGoal::Activate { assigned_subnet },
                evidence: ployz_types::model::MachineTransitionEvidence::BootstrapActivation {
                    operation_id: None,
                },
                at_unix_secs: ployz_types::time::now_unix_secs(),
            })
            .map_err(|err| format!("activate joined machine: {err}"))?;
        return context
            .store
            .upsert_self_machine(&active_record)
            .await
            .map_err(|err| format!("persist active self-record: {err}"));
    }

    #[cfg(not(test))]
    {
        Err("NATS RPC client unavailable".into())
    }
}

async fn build_mesh_bootstrap_request(
    context: &MachineAddContext,
    assigned_subnet: Ipv4Net,
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

fn validate_joined_machine_authority_posture(record: &MachineMembership) -> Result<(), String> {
    match &record.storage_participation {
        StorageParticipation::Candidate => Ok(()),
        StorageParticipation::Authority { authority_id } => Err(format!(
            "remote machine '{}' reported authority storage for '{}' during machine add; use explicit storage promotion",
            record.id, authority_id
        )),
    }
}
