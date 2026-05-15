use ployz_api::{
    DaemonPayload, DaemonResponse, MachineStoragePromoteRequest, MachineStoragePromotionFailure,
    MachineStoragePromotionFailureCause, MachineStoragePromotionPayload,
};
use ployz_model::{
    MachineMembership, MachineStorageRole, StorageParticipation, StorageReplicaPolicy,
};
use ployz_node_runtime::{
    MACHINE_STORAGE_RPC_POLICY, MachineStorageNodeClient, NODE_STATUS_RPC_POLICY,
    NodeProbeNodeClient,
};
use ployz_store_api::MachineMembershipStore;
use tokio::sync::oneshot;

use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use crate::daemon::node_rpc::{NatsMachineStorageRpcTransport, NatsNodeProbeRpcTransport};
use crate::daemon::{DaemonState, RuntimeRestartMode};
use crate::mesh_state::bootstrap::{
    BootstrapPeerRecord, load_bootstrap_peer_records, write_bootstrap_peer_records,
};

mod local;
mod plan;
mod promotion;
use plan::StoragePromotionPlan;
use promotion::*;

impl DaemonState {
    pub(crate) async fn handle_machine_storage_promote(
        &mut self,
        request: &MachineStoragePromoteRequest,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        let _ = response_flushed;
        if request.replicas == StorageReplicaPolicy::Single {
            return self.err(
                "INVALID_ARGUMENT",
                "machine storage promote requires replicas r3 or r5",
            );
        }

        let (network_name, store, local_record) = {
            let Some(active) = self.active.as_ref() else {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "machine storage promote requires a running network",
                );
            };
            let Some(local_record) = active.mesh.authoritative_self_record().await else {
                return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
            };
            (
                active.config.name.0.clone(),
                active.mesh.store.clone(),
                local_record,
            )
        };

        if !matches!(
            local_record.storage_participation(),
            StorageParticipation::Authority { .. }
        ) {
            return self.err(
                "NOT_AUTHORITY_STORAGE",
                "machine storage promote must run from current authority storage",
            );
        }

        let operation_store = self.machine_operation_store();
        let mut operation = match operation_store.begin(
            MachineOperationKind::StoragePromote,
            Some(network_name.clone()),
            request.targets.clone(),
            "preflight",
            MachineOperationArtifacts::default(),
        ) {
            Ok(operation) => operation,
            Err(err) => return self.err("MACHINE_OPERATION_START_FAILED", err),
        };

        let outcome = self
            .promote_storage_members(
                request,
                &network_name,
                &store,
                &local_record,
                &mut operation,
            )
            .await;

        match outcome {
            Ok(payload) => {
                let _ = operation_store.update_status(
                    &mut operation,
                    MachineOperationStatus::Succeeded,
                    None,
                );
                self.ok_with_payload(
                    format!(
                        "storage promotion complete\n  replicas: {}\n  promoted: {}",
                        request.replicas.replicas(),
                        payload.promoted.join(", ")
                    ),
                    Some(DaemonPayload::MachineStoragePromotion(payload)),
                )
            }
            Err(error) => {
                let message = error.message();
                let mut payload = error.into_payload(&operation.id, request.replicas);
                let _ = operation_store.update_status(
                    &mut operation,
                    MachineOperationStatus::Failed,
                    Some(message.clone()),
                );
                payload.operation_id = operation.id;
                self.err_with_payload(
                    "MACHINE_STORAGE_PROMOTION_FAILED",
                    message,
                    Some(DaemonPayload::MachineStoragePromotion(payload)),
                )
            }
        }
    }

    async fn promote_storage_members(
        &mut self,
        request: &MachineStoragePromoteRequest,
        network_name: &str,
        store: &ployz_store_api::StoreDriver,
        local_record: &MachineMembership,
        operation: &mut super::operations::MachineOperationRecord,
    ) -> Result<MachineStoragePromotionPayload, StoragePromotionError> {
        let mut failed = Vec::new();
        let machines = match store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return Err(StoragePromotionError::StoreList { error });
            }
        };

        let plan = StoragePromotionPlan::build(request, machines.as_slice(), local_record)?;
        let previous_remote_replicas = self
            .active
            .as_ref()
            .map(|active| active.config.storage_replicas)
            .unwrap_or(StorageReplicaPolicy::Single);

        let mut promoted = Vec::new();
        let mut remote_rollbacks = Vec::new();
        if self.runtime_is_memory_test() {
            let _ = self
                .machine_operation_store()
                .update_stage(operation, "promoting-authority-members");
            for target in &plan.targets {
                let mut promoted_record = target.clone();
                promoted_record.storage_role = MachineStorageRole::default_authority();
                promoted_record.updated_at = ployz_time::now_unix_secs();
                if let Err(error) = store.upsert_self_machine(&promoted_record).await {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.as_str().to_string(),
                        cause: MachineStoragePromotionFailureCause::PublishPromotedMembershipFailed,
                        message: format!("publish promoted membership: {error}"),
                    });
                } else {
                    promoted.push(target.id.as_str().to_string());
                }
            }
            if failed.is_empty() {
                let _ = self
                    .machine_operation_store()
                    .update_stage(operation, "recording-replica-intent");
                let previous_replicas = match self
                    .record_storage_replica_intent(network_name, request.replicas)
                {
                    Ok(previous) => previous,
                    Err(error) => {
                        return Err(StoragePromotionError::RecordReplicaIntent { error, promoted });
                    }
                };
                let network_dir = self.network_dir(network_name);
                let previous_bootstrap_peers = match load_bootstrap_peer_records(&network_dir) {
                    Ok(peers) => peers,
                    Err(error) => {
                        let rollback_error = self
                            .restore_storage_replica_intent(network_name, previous_replicas)
                            .err();
                        return Err(StoragePromotionError::BootstrapPeerRead {
                            error,
                            rollback_error,
                            promoted,
                        });
                    }
                };
                let peer_records = plan
                    .authority_peers
                    .iter()
                    .map(BootstrapPeerRecord::from_machine_record)
                    .collect::<Vec<_>>();
                if let Err(error) = write_bootstrap_peer_records(&network_dir, &peer_records) {
                    let rollback_error = self
                        .restore_storage_replica_intent(network_name, previous_replicas)
                        .err();
                    let _ = write_bootstrap_peer_records(&network_dir, &previous_bootstrap_peers);
                    return Err(StoragePromotionError::BootstrapPeerWrite {
                        error,
                        rollback_error,
                        promoted,
                    });
                }
            }
        } else {
            let _ = self
                .machine_operation_store()
                .update_stage(operation, "preflighting-authority-members");
            let client = match self.nats_node_rpc_client().await {
                Ok(client) => client,
                Err(error) => {
                    return Err(StoragePromotionError::RpcUnavailable {
                        error: error.to_string(),
                    });
                }
            };
            let machine_client =
                MachineStorageNodeClient::new(NatsMachineStorageRpcTransport::new(client.clone()))
                    .with_policy(MACHINE_STORAGE_RPC_POLICY);
            let probe_client =
                NodeProbeNodeClient::new(NatsNodeProbeRpcTransport::new(client.clone()))
                    .with_policy(NODE_STATUS_RPC_POLICY);

            let remote_authorities = plan
                .authority_peers
                .iter()
                .filter(|machine| machine.id != local_record.id)
                .collect::<Vec<_>>();
            if let Err(failed) =
                preflight_remote_storage_promotion(&probe_client, &remote_authorities).await
            {
                return Err(StoragePromotionError::Preflight { failed });
            }

            let _ = self
                .machine_operation_store()
                .update_stage(operation, "recording-replica-intent");
            let previous_replicas =
                match self.record_storage_replica_intent(network_name, request.replicas) {
                    Ok(previous) => previous,
                    Err(error) => {
                        return Err(StoragePromotionError::RecordReplicaIntent { error, promoted });
                    }
                };

            let network_dir = self.network_dir(network_name);
            let previous_bootstrap_peers = match load_bootstrap_peer_records(&network_dir) {
                Ok(peers) => peers,
                Err(error) => {
                    let rollback_error = self
                        .restore_storage_replica_intent(network_name, previous_replicas)
                        .err();
                    return Err(StoragePromotionError::BootstrapPeerRead {
                        error,
                        rollback_error,
                        promoted,
                    });
                }
            };
            let peer_records = plan
                .authority_peers
                .iter()
                .map(BootstrapPeerRecord::from_machine_record)
                .collect::<Vec<_>>();
            if let Err(error) = write_bootstrap_peer_records(&network_dir, &peer_records) {
                let rollback_error = self
                    .restore_storage_replica_intent(network_name, previous_replicas)
                    .err();
                return Err(StoragePromotionError::BootstrapPeerWrite {
                    error,
                    rollback_error,
                    promoted,
                });
            }

            let _ = self
                .machine_operation_store()
                .update_stage(operation, "promoting-authority-members");
            for target in remote_authorities {
                if let Err(error) = promote_remote_storage(
                    &machine_client,
                    target,
                    request.replicas,
                    &plan.authority_peer_payloads,
                )
                .await
                {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.as_str().to_string(),
                        cause: MachineStoragePromotionFailureCause::RpcUnavailable,
                        message: error,
                    });
                } else {
                    remote_rollbacks.push(RemoteStorageRollback {
                        machine_id: target.id.clone(),
                        participation: target.storage_participation().clone(),
                        replicas: previous_remote_replicas,
                        authority_peers: plan.previous_authority_peers.clone(),
                    });
                    if plan
                        .targets
                        .iter()
                        .any(|candidate| candidate.id == target.id)
                    {
                        promoted.push(target.id.as_str().to_string());
                    }
                }
            }
            if !failed.is_empty() {
                let rollback_failed =
                    rollback_remote_storage_promotions(&machine_client, &remote_rollbacks).await;
                if rollback_failed.is_empty() {
                    promoted.clear();
                } else {
                    failed.extend(rollback_failed);
                }
                let rollback_error = self
                    .restore_storage_replica_intent(network_name, previous_replicas)
                    .err();
                let peers_rollback_error =
                    write_bootstrap_peer_records(&network_dir, &previous_bootstrap_peers).err();
                return Err(StoragePromotionError::PromoteTargets {
                    promoted,
                    failed: append_rollback_failures(failed, rollback_error, peers_rollback_error),
                });
            }

            let _ = self
                .machine_operation_store()
                .update_stage(operation, "restarting-authority-storage");
            if let Err(error) = self
                .restart_active_runtime_from_config_with_mode(
                    network_name,
                    RuntimeRestartMode::NetworkAndStore,
                )
                .await
            {
                return Err(StoragePromotionError::AuthorityRestart { error, promoted });
            }
        }

        if !failed.is_empty() {
            return Err(StoragePromotionError::PromoteTargets { promoted, failed });
        }

        Ok(promotion_payload(
            &operation.id,
            request.replicas,
            promoted,
            failed,
        ))
    }
}
