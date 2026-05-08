use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineStoragePromoteRequest,
    MachineStoragePromotionFailure, MachineStoragePromotionPayload, StatusPayload,
};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcPolicy};
use ployz_store_api::MachineMembershipStore;
use ployz_types::model::{
    AuthorityId, MachineLifecycle, MachineMembership, StorageParticipation, StorageReplicaPolicy,
};
use std::collections::BTreeSet;
use tokio::sync::oneshot;

use crate::mesh_state::bootstrap::{
    BootstrapPeerRecord, load_bootstrap_peer_records, write_bootstrap_peer_records,
};
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

        let mut seen_targets = BTreeSet::new();
        for target in &request.targets {
            if !seen_targets.insert(target.clone()) {
                failed.push(MachineStoragePromotionFailure {
                    machine_id: target.clone(),
                    message: "duplicate promotion target".into(),
                });
            }
        }
        if !failed.is_empty() {
            return Err(StoragePromotionError::Preflight { failed });
        }

        let default_authority = AuthorityId::default_authority();
        let mut authorities = machines
            .iter()
            .filter(|machine| {
                machine.lifecycle == MachineLifecycle::Active
                    && machine.storage
                    && matches!(
                        &machine.storage_participation,
                        StorageParticipation::Authority { authority_id }
                            if authority_id == &default_authority
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        if !authorities
            .iter()
            .any(|machine| machine.id == local_record.id)
        {
            if local_record.lifecycle != MachineLifecycle::Active
                || !local_record.storage
                || !matches!(
                    &local_record.storage_participation,
                    StorageParticipation::Authority { authority_id }
                        if authority_id == &default_authority
                )
            {
                return Err(StoragePromotionError::InvalidLocalAuthority {
                    message: format!(
                        "local authority must be active storage for {} (lifecycle={}, storage={}, participation={:?})",
                        default_authority,
                        local_record.lifecycle,
                        local_record.storage,
                        local_record.storage_participation
                    ),
                });
            }
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
            return Err(StoragePromotionError::Preflight { failed });
        }

        let final_authority_ids = authorities
            .iter()
            .chain(targets.iter())
            .map(|machine| machine.id.clone())
            .collect::<BTreeSet<_>>();
        let final_authority_count = final_authority_ids.len();
        if final_authority_count != request.replicas.replicas() {
            return Err(StoragePromotionError::ReplicaCount {
                message: format!(
                    "storage promotion to {} replicas requires exactly {} active authority participants after promotion; current authorities={}, targets={}",
                    request.replicas,
                    request.replicas.replicas(),
                    authorities.len(),
                    targets.len()
                ),
            });
        }

        let _ = self
            .machine_operation_store()
            .update_stage(operation, "promoting-authority-members");

        let mut authority_peers = authorities.clone();
        authority_peers.extend(targets.clone().into_iter().map(|mut target| {
            target.storage_participation = StorageParticipation::default_authority();
            target
        }));

        let mut promoted = Vec::new();
        if self.runtime_is_memory_test() {
            for target in &targets {
                let mut promoted_record = target.clone();
                promoted_record.storage_participation = StorageParticipation::default_authority();
                promoted_record.updated_at = ployz_types::time::now_unix_secs();
                if let Err(error) = store.upsert_self_machine(&promoted_record).await {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.0.clone(),
                        message: format!("publish promoted membership: {error}"),
                    });
                } else {
                    promoted.push(target.id.0.clone());
                }
            }
        } else {
            let client = match self.nats_node_rpc_client().await {
                Ok(client) => client.with_policy(RpcPolicy::default()),
                Err(error) => {
                    return Err(StoragePromotionError::RpcUnavailable {
                        error: error.to_string(),
                    });
                }
            };

            let remote_authorities = authority_peers
                .iter()
                .filter(|machine| machine.id != local_record.id)
                .collect::<Vec<_>>();
            if let Err(failed) =
                preflight_remote_storage_promotion(&client, &remote_authorities).await
            {
                return Err(StoragePromotionError::Preflight { failed });
            }

            for target in remote_authorities {
                if let Err(error) =
                    promote_remote_storage(&client, target, request.replicas, &authority_peers)
                        .await
                {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.0.clone(),
                        message: error,
                    });
                } else if targets.iter().any(|candidate| candidate.id == target.id) {
                    promoted.push(target.id.0.clone());
                }
            }
        }

        if !failed.is_empty() {
            return Err(StoragePromotionError::PromoteTargets { promoted, failed });
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
        let peer_records = authority_peers
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
                let intent_rollback_error = self
                    .restore_storage_replica_intent(network_name, previous_replicas)
                    .err();
                let peers_rollback_error =
                    write_bootstrap_peer_records(&network_dir, &previous_bootstrap_peers).err();
                return Err(StoragePromotionError::AuthorityRestart {
                    error,
                    intent_rollback_error,
                    peers_rollback_error,
                    promoted,
                });
            }
        }

        Ok(promotion_payload(
            &operation.id,
            request.replicas,
            promoted,
            failed,
        ))
    }

    fn record_storage_replica_intent(
        &mut self,
        network_name: &str,
        replicas: StorageReplicaPolicy,
    ) -> Result<StorageReplicaPolicy, String> {
        let config_path = NetworkConfig::path(&self.data_dir, network_name);
        let mut config =
            NetworkConfig::load(&config_path).map_err(|error| format!("load config: {error}"))?;
        let previous = config.storage_replicas;
        config.storage_replicas = replicas;
        config
            .save(&config_path)
            .map_err(|error| format!("save config: {error}"))?;
        if let Some(active) = self.active.as_mut() {
            active.config.storage_replicas = replicas;
        }
        Ok(previous)
    }

    fn restore_storage_replica_intent(
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

        let previous_peer_records = match load_bootstrap_peer_records(&network_dir) {
            Ok(records) => records,
            Err(error) => return self.err("IO_ERROR", format!("load bootstrap peers: {error}")),
        };
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
            let peer_rollback_error =
                write_bootstrap_peer_records(&network_dir, &previous_peer_records).err();
            return self.err(
                "NETWORK_RESTART_FAILED",
                promote_self_rollback_message(error, rollback_error, peer_rollback_error),
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

enum StoragePromotionError {
    StoreList {
        error: ployz_types::error::Error,
    },
    InvalidLocalAuthority {
        message: String,
    },
    Preflight {
        failed: Vec<MachineStoragePromotionFailure>,
    },
    ReplicaCount {
        message: String,
    },
    RpcUnavailable {
        error: String,
    },
    PromoteTargets {
        promoted: Vec<String>,
        failed: Vec<MachineStoragePromotionFailure>,
    },
    RecordReplicaIntent {
        error: String,
        promoted: Vec<String>,
    },
    BootstrapPeerRead {
        error: String,
        rollback_error: Option<String>,
        promoted: Vec<String>,
    },
    BootstrapPeerWrite {
        error: String,
        rollback_error: Option<String>,
        promoted: Vec<String>,
    },
    AuthorityRestart {
        error: String,
        intent_rollback_error: Option<String>,
        peers_rollback_error: Option<String>,
        promoted: Vec<String>,
    },
}

impl StoragePromotionError {
    fn message(&self) -> String {
        match self {
            Self::StoreList { error } => format!("list machines for storage promotion: {error}"),
            Self::InvalidLocalAuthority { message } => message.clone(),
            Self::Preflight { .. } => "storage promotion preflight failed".into(),
            Self::ReplicaCount { message } => message.clone(),
            Self::RpcUnavailable { error } => {
                format!("NATS RPC unavailable for storage promotion: {error}")
            }
            Self::PromoteTargets { .. } => {
                "storage promotion failed while promoting authority members".into()
            }
            Self::RecordReplicaIntent { error, .. } => {
                format!("record storage replica intent: {error}")
            }
            Self::BootstrapPeerRead {
                error,
                rollback_error,
                ..
            } => append_rollback_error(
                format!("load bootstrap peers before storage promotion: {error}"),
                rollback_error.as_deref(),
            ),
            Self::BootstrapPeerWrite {
                error,
                rollback_error,
                ..
            } => append_rollback_error(
                format!("write authority bootstrap peers: {error}"),
                rollback_error.as_deref(),
            ),
            Self::AuthorityRestart {
                error,
                intent_rollback_error,
                peers_rollback_error,
                ..
            } => {
                let message = append_rollback_error(
                    format!("restart authority storage: {error}"),
                    intent_rollback_error.as_deref(),
                );
                append_rollback_error(message, peers_rollback_error.as_deref())
            }
        }
    }

    fn into_payload(
        self,
        operation_id: &str,
        replicas: StorageReplicaPolicy,
    ) -> MachineStoragePromotionPayload {
        match self {
            Self::Preflight { failed } => {
                promotion_payload(operation_id, replicas, Vec::new(), failed)
            }
            Self::PromoteTargets { promoted, failed } => {
                promotion_payload(operation_id, replicas, promoted, failed)
            }
            Self::RecordReplicaIntent { promoted, .. }
            | Self::BootstrapPeerRead { promoted, .. }
            | Self::BootstrapPeerWrite { promoted, .. }
            | Self::AuthorityRestart { promoted, .. } => {
                promotion_payload(operation_id, replicas, promoted, Vec::new())
            }
            Self::StoreList { .. }
            | Self::InvalidLocalAuthority { .. }
            | Self::ReplicaCount { .. }
            | Self::RpcUnavailable { .. } => {
                promotion_payload(operation_id, replicas, Vec::new(), Vec::new())
            }
        }
    }
}

fn append_rollback_error(mut message: String, rollback_error: Option<&str>) -> String {
    if let Some(rollback_error) = rollback_error {
        message.push_str("; rollback failed: ");
        message.push_str(rollback_error);
    }
    message
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

async fn preflight_remote_storage_promotion(
    client: &NatsNodeRpcClient,
    targets: &[&MachineMembership],
) -> Result<(), Vec<MachineStoragePromotionFailure>> {
    let mut failed = Vec::new();
    for target in targets {
        match remote_status(client, target).await {
            Ok(status) => {
                if status.version != env!("CARGO_PKG_VERSION") {
                    failed.push(MachineStoragePromotionFailure {
                        machine_id: target.id.0.clone(),
                        message: format!(
                            "remote daemon version {} does not match required version {} for storage promotion",
                            status.version,
                            env!("CARGO_PKG_VERSION")
                        ),
                    });
                }
            }
            Err(message) => failed.push(MachineStoragePromotionFailure {
                machine_id: target.id.0.clone(),
                message,
            }),
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed)
    }
}

async fn remote_status(
    client: &NatsNodeRpcClient,
    target: &MachineMembership,
) -> Result<StatusPayload, String> {
    let response = client
        .request(
            NodeCommandSubject::status(&target.id),
            &DaemonRequest::Status,
        )
        .await
        .map_err(|error| error.to_string())?;
    if !response.ok {
        return Err(format!("{}: {}", response.code, response.message));
    }
    match response.payload {
        Some(DaemonPayload::Status(status)) if status.machine_id == target.id.0 => Ok(status),
        Some(DaemonPayload::Status(status)) => Err(format!(
            "remote status reported machine {} instead of {}",
            status.machine_id, target.id
        )),
        Some(payload) => Err(format!(
            "remote status returned unexpected payload: {payload:?}"
        )),
        None => Err("remote status returned no payload".into()),
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

fn promote_self_rollback_message(
    error: String,
    config_rollback_error: Option<String>,
    peer_rollback_error: Option<String>,
) -> String {
    let message = append_rollback_error(
        format!("failed to promote storage authority: {error}"),
        config_rollback_error.as_deref(),
    );
    append_rollback_error(message, peer_rollback_error.as_deref())
}
