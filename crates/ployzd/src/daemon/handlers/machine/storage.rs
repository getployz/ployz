use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineStoragePromoteRequest,
    MachineStoragePromotionFailure, MachineStoragePromotionPayload,
};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcPolicy};
use ployz_store_api::MachineMembershipStore;
use ployz_types::model::{
    AuthorityId, MachineLifecycle, MachineMembership, StorageParticipation, StorageReplicaPolicy,
};
use tokio::sync::oneshot;

use crate::mesh_state::bootstrap::{BootstrapPeerRecord, write_bootstrap_peer_records};
use crate::mesh_state::network::NetworkConfig;

use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use crate::daemon::{DaemonState, RuntimeRestartMode};

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
            local_record.storage_participation,
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
            Err((message, mut payload)) => {
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
    ) -> Result<MachineStoragePromotionPayload, (String, MachineStoragePromotionPayload)> {
        let mut failed = Vec::new();
        let machines = match store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return Err((
                    format!("list machines for storage promotion: {error}"),
                    promotion_payload(&operation.id, request.replicas, Vec::new(), failed),
                ));
            }
        };

        let mut authorities = machines
            .iter()
            .filter(|machine| {
                machine.lifecycle == MachineLifecycle::Active
                    && matches!(
                        machine.storage_participation,
                        StorageParticipation::Authority { .. }
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        if !authorities
            .iter()
            .any(|machine| machine.id == local_record.id)
        {
            authorities.push(local_record.clone());
        }

        let mut targets = Vec::new();
        for target in &request.targets {
            match machines.iter().find(|machine| machine.id.0 == *target) {
                Some(machine)
                    if machine.lifecycle == MachineLifecycle::Active
                        && machine.storage
                        && machine.storage_participation == StorageParticipation::Candidate =>
                {
                    targets.push(machine.clone());
                }
                Some(machine) => failed.push(MachineStoragePromotionFailure {
                    machine_id: target.clone(),
                    message: format!(
                        "machine must be an active storage candidate (lifecycle={}, storage={}, participation={:?})",
                        machine.lifecycle, machine.storage, machine.storage_participation
                    ),
                }),
                None => failed.push(MachineStoragePromotionFailure {
                    machine_id: target.clone(),
                    message: "machine not found".into(),
                }),
            }
        }
        if !failed.is_empty() {
            return Err((
                "storage promotion preflight failed".into(),
                promotion_payload(&operation.id, request.replicas, Vec::new(), failed),
            ));
        }

        let final_authority_count = authorities.len() + targets.len();
        if final_authority_count != request.replicas.replicas() {
            return Err((
                format!(
                    "storage promotion to {} replicas requires exactly {} active authority participants after promotion; current authorities={}, targets={}",
                    request.replicas,
                    request.replicas.replicas(),
                    authorities.len(),
                    targets.len()
                ),
                promotion_payload(&operation.id, request.replicas, Vec::new(), failed),
            ));
        }

        let _ = self
            .machine_operation_store()
            .update_stage(operation, "promoting-authority-members");

        let mut authority_peers = authorities.clone();
        authority_peers.extend(targets.clone().into_iter().map(|mut target| {
            target.storage_participation = StorageParticipation::default_authority();
            target
        }));

        if self.runtime_is_memory_test() {
            for target in &targets {
                let mut promoted = target.clone();
                promoted.storage_participation = StorageParticipation::default_authority();
                promoted.updated_at = ployz_types::time::now_unix_secs();
                if let Err(error) = store.upsert_self_machine(&promoted).await {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.0.clone(),
                        message: format!("publish promoted membership: {error}"),
                    });
                }
            }
        } else {
            let client = match self.nats_node_rpc_client().await {
                Ok(client) => client.with_policy(RpcPolicy::default()),
                Err(error) => {
                    return Err((
                        format!("NATS RPC unavailable for storage promotion: {error}"),
                        promotion_payload(&operation.id, request.replicas, Vec::new(), failed),
                    ));
                }
            };
            for target in &targets {
                if let Err(error) =
                    promote_remote_storage(&client, target, request.replicas, &authority_peers)
                        .await
                {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.0.clone(),
                        message: error,
                    });
                }
            }
        }

        if !failed.is_empty() {
            return Err((
                "storage promotion failed while promoting targets".into(),
                promotion_payload(&operation.id, request.replicas, Vec::new(), failed),
            ));
        }

        let _ = self
            .machine_operation_store()
            .update_stage(operation, "recording-replica-intent");
        if let Err(error) = self.record_storage_replica_intent(network_name, request.replicas) {
            return Err((
                format!("record storage replica intent: {error}"),
                promotion_payload(&operation.id, request.replicas, Vec::new(), failed),
            ));
        }

        if !self.runtime_is_memory_test() {
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
                return Err((
                    format!("restart authority storage with replica intent: {error}"),
                    promotion_payload(&operation.id, request.replicas, Vec::new(), failed),
                ));
            }
        }

        Ok(promotion_payload(
            &operation.id,
            request.replicas,
            targets.into_iter().map(|target| target.id.0).collect(),
            failed,
        ))
    }

    fn record_storage_replica_intent(
        &mut self,
        network_name: &str,
        replicas: StorageReplicaPolicy,
    ) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network_name);
        let mut config =
            NetworkConfig::load(&config_path).map_err(|error| format!("load config: {error}"))?;
        config.storage_replicas = replicas;
        config
            .save(&config_path)
            .map_err(|error| format!("save config: {error}"))?;
        if let Some(active) = self.active.as_mut() {
            active.config.storage_replicas = replicas;
        }
        Ok(())
    }

    pub(crate) async fn handle_machine_storage_promote_self(
        &mut self,
        replicas: StorageReplicaPolicy,
        authority_peers: &[MachineMembership],
    ) -> DaemonResponse {
        let (network_name, config_path, network_dir) = {
            let Some(active) = self.active.as_ref() else {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "storage promotion requires a running network",
                );
            };
            let network_name = active.config.name.0.clone();
            (
                network_name.clone(),
                NetworkConfig::path(&self.data_dir, &network_name),
                self.network_dir(&network_name),
            )
        };

        let mut config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(error) => return self.err("IO_ERROR", format!("load network config: {error}")),
        };
        let previous_participation = config.storage_participation.clone();
        let previous_replicas = config.storage_replicas;
        config.storage = true;
        config.storage_participation = StorageParticipation::Authority {
            authority_id: AuthorityId::default_authority(),
        };
        config.storage_replicas = replicas;
        if let Err(error) = config.save(&config_path) {
            return self.err("IO_ERROR", format!("save network config: {error}"));
        }

        let peer_records = authority_peers
            .iter()
            .map(BootstrapPeerRecord::from_machine_record)
            .collect::<Vec<_>>();
        if let Err(error) = write_bootstrap_peer_records(&network_dir, &peer_records) {
            let _ = restore_storage_config(
                &config_path,
                &mut config,
                previous_participation,
                previous_replicas,
            );
            return self.err("IO_ERROR", format!("write bootstrap peers: {error}"));
        }

        if let Err(error) = self
            .restart_active_runtime_from_config_with_mode(
                &network_name,
                RuntimeRestartMode::NetworkAndStore,
            )
            .await
        {
            let rollback_error = restore_storage_config(
                &config_path,
                &mut config,
                previous_participation,
                previous_replicas,
            )
            .err();
            return self.err(
                "NETWORK_RESTART_FAILED",
                match rollback_error {
                    Some(rollback_error) => format!(
                        "failed to promote storage authority: {error}; failed to restore config: {rollback_error}"
                    ),
                    None => format!("failed to promote storage authority: {error}"),
                },
            );
        }

        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let update_result = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.storage = true;
                record.storage_participation = StorageParticipation::default_authority();
            })
            .await;
        if update_result.is_none() {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        }

        self.ok(format!(
            "machine promoted to authority storage\n  replicas: {}",
            replicas.replicas()
        ))
    }
}

fn promotion_payload(
    operation_id: &str,
    replicas: StorageReplicaPolicy,
    promoted: Vec<String>,
    failed: Vec<MachineStoragePromotionFailure>,
) -> MachineStoragePromotionPayload {
    MachineStoragePromotionPayload {
        operation_id: operation_id.to_string(),
        replicas,
        promoted,
        failed,
    }
}

async fn promote_remote_storage(
    client: &NatsNodeRpcClient,
    target: &MachineMembership,
    replicas: StorageReplicaPolicy,
    authority_peers: &[MachineMembership],
) -> Result<(), String> {
    let response = client
        .request(
            NodeCommandSubject::machine_storage_promote_self(&target.id),
            &DaemonRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers: authority_peers.to_vec(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if response.ok {
        Ok(())
    } else {
        Err(format!("{}: {}", response.code, response.message))
    }
}

fn restore_storage_config(
    path: &std::path::Path,
    config: &mut NetworkConfig,
    participation: StorageParticipation,
    replicas: StorageReplicaPolicy,
) -> Result<(), String> {
    config.storage_participation = participation;
    config.storage_replicas = replicas;
    config
        .save(path)
        .map_err(|error| format!("restore network config: {error}"))
}
