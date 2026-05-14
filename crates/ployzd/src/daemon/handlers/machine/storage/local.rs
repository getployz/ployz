use ployz_api::{DaemonResponse, MachineStorageAuthorityPeer};
use ployz_model::{
    AuthorityId, MachineMembership, MachineStorageRole, StorageParticipation, StorageReplicaPolicy,
};
use ployz_store_api::MachineMembershipStore;
use std::collections::BTreeSet;
use std::path::Path;

use crate::daemon::{DaemonState, RuntimeRestartMode};
use crate::mesh_state::bootstrap::{
    BootstrapPeerRecord, load_bootstrap_peer_records, write_bootstrap_peer_records,
};
use crate::mesh_state::network::NetworkConfig;

use super::promotion::append_rollback_error;

impl DaemonState {
    pub(super) fn record_storage_replica_intent(
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

    pub(super) fn restore_storage_replica_intent(
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
        authority_peers: &[MachineStorageAuthorityPeer],
    ) -> DaemonResponse {
        let (network_name, config_path, network_dir, store) = {
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
                active.mesh.store.clone(),
            )
        };

        let mut config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(error) => return self.err("IO_ERROR", format!("load network config: {error}")),
        };
        let previous_peer_records = match load_bootstrap_peer_records(&network_dir) {
            Ok(records) => records,
            Err(error) => return self.err("IO_ERROR", format!("load bootstrap peers: {error}")),
        };
        if let Err(message) =
            validate_authority_peer_payload(replicas, authority_peers, &self.identity.machine_id)
        {
            return self.err("INVALID_AUTHORITY_PEERS", message);
        }
        let machines = match store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => return self.err("STORE_ERROR", format!("list machines: {error}")),
        };
        if let Err(message) = validate_authority_peers_match_membership(
            authority_peers,
            &machines,
            &self.identity.machine_id,
        ) {
            return self.err("INVALID_AUTHORITY_PEERS", message);
        }
        let previous_storage = config.storage;
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
            .map(BootstrapPeerRecord::from)
            .collect::<Vec<_>>();
        if let Err(error) = write_bootstrap_peer_records(&network_dir, &peer_records) {
            let _ = restore_storage_config(
                &config_path,
                &mut config,
                previous_storage,
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
                previous_storage,
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
                record.storage_role = MachineStorageRole::default_authority();
            })
            .await;
        if update_result.is_none() {
            let config_rollback_error = restore_storage_config(
                &config_path,
                &mut config,
                previous_storage,
                previous_participation,
                previous_replicas,
            )
            .err();
            let peer_rollback_error =
                write_bootstrap_peer_records(&network_dir, &previous_peer_records).err();
            let restart_rollback_error =
                if config_rollback_error.is_none() && peer_rollback_error.is_none() {
                    self.restart_active_runtime_from_config_with_mode(
                        &network_name,
                        RuntimeRestartMode::NetworkAndStore,
                    )
                    .await
                    .err()
                } else {
                    None
                };
            return self.err(
                "SELF_RECORD_MISSING",
                promote_self_record_rollback_message(
                    config_rollback_error,
                    peer_rollback_error,
                    restart_rollback_error,
                ),
            );
        }

        self.ok(format!(
            "machine promoted to authority storage\n  replicas: {}",
            replicas.replicas()
        ))
    }

    pub(crate) async fn handle_machine_storage_restore_self(
        &mut self,
        participation: StorageParticipation,
        replicas: StorageReplicaPolicy,
        authority_peers: &[MachineStorageAuthorityPeer],
    ) -> DaemonResponse {
        let (network_name, config_path, network_dir) = {
            let Some(active) = self.active.as_ref() else {
                return self.err(
                    "NO_RUNNING_NETWORK",
                    "storage rollback requires a running network",
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
        config.storage = true;
        config.storage_participation = participation.clone();
        config.storage_replicas = replicas;
        if let Err(error) = config.save(&config_path) {
            return self.err("IO_ERROR", format!("save network config: {error}"));
        }
        if let Some(active) = self.active.as_mut() {
            active.config.storage = true;
            active.config.storage_participation = participation.clone();
            active.config.storage_replicas = replicas;
        }

        let peer_records = authority_peers
            .iter()
            .map(BootstrapPeerRecord::from)
            .collect::<Vec<_>>();
        if let Err(error) = write_bootstrap_peer_records(&network_dir, &peer_records) {
            return self.err("IO_ERROR", format!("write bootstrap peers: {error}"));
        }

        if let Err(error) = self
            .restart_active_runtime_from_config_with_mode(
                &network_name,
                RuntimeRestartMode::NetworkAndStore,
            )
            .await
        {
            return self.err(
                "NETWORK_RESTART_FAILED",
                format!("restart storage rollback: {error}"),
            );
        }

        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let update_result = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.storage_role = participation.clone().into();
            })
            .await;
        if update_result.is_none() {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        }

        self.ok(format!(
            "machine storage config restored\n  replicas: {}",
            replicas.replicas()
        ))
    }
}

impl From<&MachineStorageAuthorityPeer> for BootstrapPeerRecord {
    fn from(peer: &MachineStorageAuthorityPeer) -> Self {
        Self {
            machine_id: peer.machine_id.clone(),
            public_key: peer.public_key.clone(),
            overlay_ip: peer.overlay_ip,
            subnet: peer.subnet,
            bridge_ip: peer.bridge_ip,
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            region_role: peer.region_role,
            endpoints: peer.endpoints.clone(),
        }
    }
}

fn validate_authority_peer_payload(
    replicas: StorageReplicaPolicy,
    authority_peers: &[MachineStorageAuthorityPeer],
    local_machine_id: &ployz_model::MachineId,
) -> Result<(), String> {
    if authority_peers.len() != replicas.replicas() {
        return Err(format!(
            "storage promotion self payload for {replicas} must contain exactly {} authority peers, got {}",
            replicas.replicas(),
            authority_peers.len()
        ));
    }
    let mut seen = BTreeSet::new();
    let mut has_local = false;
    for peer in authority_peers {
        if !seen.insert(peer.machine_id.clone()) {
            return Err(format!(
                "storage promotion self payload contains duplicate authority peer '{}'",
                peer.machine_id
            ));
        }
        if peer.machine_id == *local_machine_id {
            has_local = true;
        }
        if peer.endpoints.is_empty() {
            return Err(format!(
                "storage promotion self payload authority peer '{}' has no endpoints",
                peer.machine_id
            ));
        }
    }
    if !has_local {
        return Err(format!(
            "storage promotion self payload must include local authority peer '{}'",
            local_machine_id
        ));
    }
    Ok(())
}

fn validate_authority_peers_match_membership(
    authority_peers: &[MachineStorageAuthorityPeer],
    machines: &[MachineMembership],
    local_machine_id: &ployz_model::MachineId,
) -> Result<(), String> {
    let Some(local_peer) = authority_peers
        .iter()
        .find(|peer| peer.machine_id == *local_machine_id)
    else {
        return Err(format!(
            "storage promotion self payload must include local authority peer '{}'",
            local_machine_id
        ));
    };
    let Some(machine) = machines
        .iter()
        .find(|machine| machine.id == local_peer.machine_id)
    else {
        return Err(format!(
            "storage promotion self payload local authority peer '{}' is not in machine membership",
            local_peer.machine_id
        ));
    };
    if machine.public_key != local_peer.public_key
        || machine.overlay_ip != local_peer.overlay_ip
        || machine.subnet != local_peer.subnet
        || machine.bridge_ip != local_peer.bridge_ip
        || machine.region_role != local_peer.region_role
        || machine.endpoints != local_peer.endpoints
    {
        return Err(format!(
            "storage promotion self payload local authority peer '{}' does not match machine membership",
            local_peer.machine_id
        ));
    }
    if machine.lifecycle != ployz_model::MachineLifecycle::Active || !machine.storage() {
        return Err(format!(
            "storage promotion self payload local authority peer '{}' is not active storage membership",
            local_peer.machine_id
        ));
    }
    Ok(())
}

fn restore_storage_config(
    path: &Path,
    config: &mut NetworkConfig,
    storage: bool,
    participation: StorageParticipation,
    replicas: StorageReplicaPolicy,
) -> Result<(), String> {
    config.storage = storage;
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

fn promote_self_record_rollback_message(
    config_rollback_error: Option<String>,
    peer_rollback_error: Option<String>,
    restart_rollback_error: Option<String>,
) -> String {
    let message = append_rollback_error(
        String::from("mesh self record unavailable after storage promotion restart"),
        config_rollback_error.as_deref(),
    );
    let message = append_rollback_error(message, peer_rollback_error.as_deref());
    append_rollback_error(message, restart_rollback_error.as_deref())
}
