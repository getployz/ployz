use crate::mesh_state::bootstrap::{
    BootstrapInfo, BootstrapPeerRecord, write_bootstrap_peer_record,
};
use crate::mesh_state::network::NetworkConfig;
use ployz_orchestrator::ipam::pick_candidate_subnet;
use ployz_orchestrator::mesh::orchestrator::MeshReadyStatus;
use ployz_orchestrator::mesh::tasks::PeerSyncCommand;
use ployz_orchestrator::network::endpoints::detect_endpoints;
use ployz_types::model::{JoinResponse, MachineRecord, NetworkName};
use std::path::Path;
use tracing::warn;

use crate::daemon::setup::MeshStartOptions;
use ployz_api::{
    DaemonPayload, DaemonResponse, MeshBootstrapRequest, MeshReadyPayload, MeshSelfRecordPayload,
};

use super::super::DaemonState;

impl DaemonState {
    pub(crate) fn handle_mesh_list(&self) -> DaemonResponse {
        let networks_dir = self.data_dir.join("networks");
        let entries = match std::fs::read_dir(&networks_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return self.ok("no networks found");
            }
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to read networks dir: {err}"));
            }
        };

        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().to_str().map(ToOwned::to_owned))
            .collect();
        names.sort();

        if names.is_empty() {
            return self.ok("no networks found");
        }

        let running = self.active.as_ref().map(|a| a.config.name.0.as_str());
        let lines: Vec<String> = names
            .iter()
            .map(|name| {
                let state = if running == Some(name.as_str()) {
                    "running"
                } else {
                    "created"
                };
                format!("{name}: {state}")
            })
            .collect();

        self.ok(lines.join("\n"))
    }

    pub(crate) fn handle_mesh_status(&self, network: &str) -> DaemonResponse {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!("network '{network}' does not exist"),
            );
        }

        let config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to load network config: {err}"));
            }
        };

        let running = self
            .active
            .as_ref()
            .is_some_and(|a| a.config.name.0 == network);
        let state = if running { "running" } else { "created" };
        self.ok(format!(
            "network: {}\noverlay: {}\nstate:   {}",
            config.name, config.overlay_ip, state
        ))
    }

    pub(crate) async fn handle_mesh_ready(&self, json: bool) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let Some(self_record) = active.mesh.authoritative_self_record().await else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        let status = mesh_ready_payload(active.mesh.ready_status().await, &self_record);
        if json {
            return match serde_json::to_string(&status) {
                Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::MeshReady(status))),
                Err(err) => self.err(
                    "ENCODE_FAILED",
                    format!("failed to encode readiness payload: {err}"),
                ),
            };
        }

        self.ok_with_payload(format!(
            "ready:                   {}\nphase:                   {}\nstore healthy:           {}\nsync connected:          {}\nparticipation:           {}\nworkload subnet present: {}",
            status.ready,
            status.phase,
            status.store_healthy,
            status.sync_connected,
            status.participation,
            status.workload_subnet_present,
        ), Some(DaemonPayload::MeshReady(status)))
    }

    pub(crate) async fn handle_mesh_join(&mut self, token: &str) -> DaemonResponse {
        let _ = token;
        self.err(
            "UNSUPPORTED",
            "standalone `mesh join` is not supported in founder-mediated mode",
        )
    }

    pub(crate) async fn handle_mesh_bootstrap(
        &mut self,
        request: &MeshBootstrapRequest,
    ) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name
                ),
            );
        }

        let network = request.network_name.trim();
        if network.is_empty() {
            return self.err("INVALID_ARGUMENT", "bootstrap network name is empty");
        }

        let mut net_config = NetworkConfig::new(
            NetworkName(network.to_string()),
            &self.identity.public_key,
            &request.cluster_cidr,
            request.assigned_subnet,
        );
        net_config.id = request.network_id.clone();

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return self.err(
                "NETWORK_ALREADY_EXISTS",
                format!(
                    "network '{network}' already exists -- run `mesh up {network}` or `mesh destroy {network}`"
                ),
            );
        }
        if let Err(error) = net_config.save(&config_path) {
            return self.err(
                "IO_ERROR",
                format!("failed to save network config: {error}"),
            );
        }

        let network_dir = NetworkConfig::dir(&self.data_dir, network);
        for peer in &request.bootstrap_peers {
            let peer_record = BootstrapPeerRecord {
                machine_id: peer.id.clone(),
                public_key: peer.public_key.clone(),
                overlay_ip: peer.overlay_ip,
                endpoints: peer.endpoints.clone(),
            };
            if let Err(error) = write_bootstrap_peer_record(&network_dir, &peer_record) {
                return self.err(
                    "IO_ERROR",
                    format!("failed to persist bootstrap peer '{}': {error}", peer.id),
                );
            }
        }

        let bootstrap = request
            .bootstrap_peers
            .first()
            .map(bootstrap_info_from_record);
        let options = MeshStartOptions {
            allow_disconnected_bootstrap: bootstrap.is_some(),
        };
        match self.start_mesh(net_config, bootstrap, options).await {
            Ok(_) => {
                if let Some(active) = self.active.as_ref()
                    && let Some(control_target) = request.self_control_target.clone()
                {
                    let _ = active
                        .mesh
                        .update_authoritative_self_record(|record| {
                            record.control_target = Some(control_target);
                        })
                        .await;
                }
                self.ok(format!(
                    "bootstrapped and started network '{}'",
                    request.network_name
                ))
            }
            Err(error) => self.err(
                "NETWORK_START_FAILED",
                format!("bootstrap failed to start mesh: {error}"),
            ),
        }
    }

    pub(crate) fn handle_mesh_create(&self, network: &str) -> DaemonResponse {
        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        self.ok(format!(
            "created network '{}'\n  overlay: {}\n  state:   created",
            net_config.name, net_config.overlay_ip,
        ))
    }

    pub(crate) async fn handle_mesh_init(&mut self, network: &str) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name,
                ),
            );
        }

        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        let network_name = net_config.name.clone();
        let overlay_ip = net_config.overlay_ip;
        match self
            .start_mesh(net_config, None, MeshStartOptions::default())
            .await
        {
            Ok(_) => {}
            Err(e) => {
                return self.err(
                    "NETWORK_START_FAILED",
                    format!(
                        "initialized network '{}' but failed to start: {e}\n  state:   created",
                        network_name,
                    ),
                );
            }
        }

        self.ok(format!(
            "initialized and started network '{}'\n  overlay: {}\n  state:   running",
            network_name, overlay_ip,
        ))
    }

    pub(crate) fn create_network_config(&self, network: &str) -> Result<NetworkConfig, String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return Err(format!(
                "network '{network}' already exists -- use `mesh up {network}` or `mesh destroy {network}`"
            ));
        }

        let cluster: ipnet::Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|e| format!("invalid cluster CIDR '{}': {e}", self.cluster_cidr))?;
        let subnet = pick_candidate_subnet(
            cluster,
            self.subnet_prefix_len,
            &std::collections::HashSet::new(),
            0,
        )
        .ok_or_else(|| "no available subnets in cluster CIDR".to_string())?;

        let net_config = NetworkConfig::new(
            NetworkName(network.into()),
            &self.identity.public_key,
            &self.cluster_cidr,
            subnet,
        );

        if let Err(e) = net_config.save(&config_path) {
            return Err(format!("failed to save network config: {e}"));
        }

        Ok(net_config)
    }

    pub(crate) async fn handle_mesh_up(
        &mut self,
        network: &str,
        skip_bootstrap_wait: bool,
    ) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name,
                ),
            );
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!(
                    "network '{network}' does not exist -- run `mesh create {network}` or `mesh init {network}`"
                ),
            );
        }

        let net_config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(e) => {
                return self.err("IO_ERROR", format!("failed to load network config: {e}"));
            }
        };

        let network_name = net_config.name.clone();
        let options = MeshStartOptions {
            allow_disconnected_bootstrap: skip_bootstrap_wait,
        };
        match self.start_mesh(net_config, None, options).await {
            Ok(_) => {}
            Err(e) => {
                return self.err("NETWORK_START_FAILED", e.to_string());
            }
        }

        self.ok(format!("mesh '{}' started", network_name))
    }

    pub(crate) async fn handle_mesh_down(&mut self) -> DaemonResponse {
        let Some(mut active) = self.active.take() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };

        if let Err(e) = active.mesh.destroy().await {
            self.active = Some(active);
            return self.err("NETWORK_STOP_FAILED", format!("mesh down failed: {e}"));
        }
        let _ = active.peer_control.shutdown().await;
        let _ = active.remote_control.shutdown().await;
        if let Err(e) = active.dns.shutdown().await {
            warn!(?e, "dns stop failed during mesh down");
        }
        if let Err(e) = active.gateway.shutdown().await {
            return self.err("NETWORK_STOP_FAILED", format!("gateway stop failed: {e}"));
        }

        self.clear_active_marker();
        self.ok("mesh stopped (config kept)")
    }

    pub(crate) async fn handle_mesh_destroy(&mut self, network: &str) -> DaemonResponse {
        let running_target = self
            .active
            .as_ref()
            .is_some_and(|a| a.config.name.0 == network);

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !running_target && !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!("network '{network}' does not exist"),
            );
        }

        if running_target {
            let Some(mut active) = self.active.take() else {
                return self.err("NO_RUNNING_NETWORK", "no mesh running");
            };
            if let Err(e) = active.mesh.destroy().await {
                self.active = Some(active);
                return self.err("NETWORK_DESTROY_FAILED", format!("destroy failed: {e}"));
            }
            let _ = active.peer_control.shutdown().await;
            let _ = active.remote_control.shutdown().await;
            if let Err(e) = active.dns.shutdown().await {
                warn!(?e, "dns stop failed during mesh destroy");
            }
            if let Err(e) = active.gateway.shutdown().await {
                return self.err(
                    "NETWORK_DESTROY_FAILED",
                    format!("gateway stop failed: {e}"),
                );
            }
        }

        if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
            return self.err("IO_ERROR", format!("failed to delete network config: {e}"));
        }

        self.clear_active_marker();
        self.ok(format!("mesh '{network}' destroyed"))
    }

    pub(crate) async fn handle_mesh_self_record(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(a) => a,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let endpoints = detect_endpoints(51820).await;
        let Some(self_record) = active.mesh.authoritative_self_record().await else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        let resp = JoinResponse {
            machine_id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: active.config.overlay_ip,
            subnet: self_record.subnet,
            endpoints,
        };

        match resp.encode() {
            Ok(encoded) => self.ok_with_payload(
                encoded.clone(),
                Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
                    encoded,
                    record: resp.into_seed_machine_record(),
                })),
            ),
            Err(e) => self.err(
                "ENCODE_FAILED",
                format!("failed to encode self-record: {e}"),
            ),
        }
    }

    pub(crate) async fn handle_mesh_accept(&self, response: &str) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(a) => a,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let join_resp = match JoinResponse::decode(response) {
            Ok(r) => r,
            Err(e) => return self.err("INVALID_JOIN_RESPONSE", format!("decode failed: {e}")),
        };

        let Some(peer_sync_tx) = active.mesh.peer_sync_sender() else {
            return self.err("PEER_SYNC_UNAVAILABLE", "peer sync task is not running");
        };

        let record = join_resp.into_seed_machine_record();
        let machine_id = record.id.clone();
        match peer_sync_tx
            .send(PeerSyncCommand::UpsertTransient(record))
            .await
        {
            Ok(()) => self.ok(format!(
                "accepted transient peer '{}' (awaiting self-publication)",
                machine_id
            )),
            Err(e) => self.err(
                "PEER_SYNC_UNAVAILABLE",
                format!("failed to install transient peer: {e}"),
            ),
        }
    }

    pub(crate) async fn handle_mesh_set_participation(
        &mut self,
        participation: ployz_types::model::Participation,
    ) -> DaemonResponse {
        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let now = ployz_types::time::now_unix_secs();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.participation = participation;
                record.updated_at = now;
            })
            .await
        else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        self.ok(format!("participation set to {}", record.participation))
    }

    pub(crate) async fn handle_mesh_standby(&mut self, force: bool) -> DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let network_name = active.config.name.0.clone();
        let Some(self_record) = active.mesh.authoritative_self_record().await else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        if !force && self_record.participation != ployz_types::model::Participation::Draining {
            let has_local_workloads = match self
                .runtime_has_local_workloads(&self.identity.machine_id)
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    return self.err(
                        "WORKLOAD_INSPECTION_FAILED",
                        format!("failed to inspect local workloads before standby: {error}"),
                    );
                }
            };
            if has_local_workloads {
                return self.err(
                    "MACHINE_NOT_DRAINED",
                    "machine must be draining before standby; rerun with --force to bypass",
                );
            }
        }
        let now = ployz_types::time::now_unix_secs();
        {
            let Some(active) = self.active.as_ref() else {
                return self.err("NO_RUNNING_NETWORK", "no mesh running");
            };
            let Some(record) = active
                .mesh
                .update_authoritative_self_record(|record| {
                    record.participation = ployz_types::model::Participation::Disabled;
                    record.updated_at = now;
                })
                .await
            else {
                return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
            };
            let _ = record;
        }
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        let mut config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(error) => {
                return self.err("IO_ERROR", format!("load network config: {error}"));
            }
        };
        let previous_subnet = config.subnet;
        config.subnet = None;
        if let Err(error) = config.save(&config_path) {
            return self.err("IO_ERROR", format!("save network config: {error}"));
        }
        if let Err(error) = self.restart_active_runtime_from_config(&network_name).await {
            let rollback_error =
                restore_network_config_subnet(&config_path, &mut config, previous_subnet).err();
            return self.err(
                "NETWORK_RESTART_FAILED",
                match rollback_error {
                    Some(rollback_error) => {
                        format!(
                            "failed to enter standby: {error}; failed to restore config: {rollback_error}"
                        )
                    }
                    None => format!("failed to enter standby: {error}"),
                },
            );
        }
        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.participation = ployz_types::model::Participation::Disabled;
                record.subnet = None;
                record.status = ployz_types::model::MachineStatus::Up;
                record.updated_at = now;
            })
            .await
        else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        active.config.subnet = None;
        let _ = record;
        self.ok("machine entered standby")
    }

    pub(crate) async fn handle_mesh_promote(
        &mut self,
        assigned_subnet: ipnet::Ipv4Net,
    ) -> DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let network_name = active.config.name.0.clone();
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        let mut config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(error) => {
                return self.err("IO_ERROR", format!("load network config: {error}"));
            }
        };
        let previous_subnet = config.subnet;
        config.subnet = Some(assigned_subnet);
        if let Err(error) = config.save(&config_path) {
            return self.err("IO_ERROR", format!("save network config: {error}"));
        }
        if let Err(error) = self.restart_active_runtime_from_config(&network_name).await {
            let rollback_error =
                restore_network_config_subnet(&config_path, &mut config, previous_subnet).err();
            return self.err(
                "NETWORK_RESTART_FAILED",
                match rollback_error {
                    Some(rollback_error) => {
                        format!(
                            "failed to promote machine: {error}; failed to restore config: {rollback_error}"
                        )
                    }
                    None => format!("failed to promote machine: {error}"),
                },
            );
        }
        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let now = ployz_types::time::now_unix_secs();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.participation = ployz_types::model::Participation::Disabled;
                record.subnet = Some(assigned_subnet);
                record.status = ployz_types::model::MachineStatus::Up;
                record.updated_at = now;
            })
            .await
        else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        active.config.subnet = Some(assigned_subnet);
        let _ = record;
        self.ok(format!("machine promoted with subnet {}", assigned_subnet))
    }
}

fn restore_network_config_subnet(
    config_path: &Path,
    config: &mut NetworkConfig,
    subnet: Option<ipnet::Ipv4Net>,
) -> Result<(), String> {
    config.subnet = subnet;
    config
        .save(config_path)
        .map_err(|error| format!("restore network config: {error}"))
}

fn bootstrap_info_from_record(record: &MachineRecord) -> BootstrapInfo {
    BootstrapInfo {
        peer_id: record.id.0.clone(),
        peer_wg_public_key: record.public_key.0,
        peer_overlay_ip: record.overlay_ip.0,
        peer_endpoints: record.endpoints.clone(),
    }
}

fn mesh_ready_payload(value: MeshReadyStatus, self_record: &MachineRecord) -> MeshReadyPayload {
    MeshReadyPayload {
        ready: value.ready,
        phase: value.phase.to_string(),
        store_healthy: value.store_healthy,
        sync_connected: value.sync_connected,
        workload_subnet_present: self_record.subnet.is_some(),
        participation: self_record.participation.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ActiveMesh;
    use crate::mesh_state::invite::issue_invite_token;
    use crate::mesh_state::network::NetworkConfig;
    use ployz_api::MeshBootstrapRequest;
    use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
    use ployz_orchestrator::{Mesh, WireguardDriver};
    use ployz_runtime_api::Identity;
    use ployz_store_api::MachineStore;
    use ployz_store_api::StoreDriver;
    use ployz_store_api::memory::{MemoryService, MemoryStore};
    use ployz_types::model::{MachineId, OverlayIp, PublicKey};
    use ployz_types::time::now_unix_secs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn mesh_join_is_unsupported_in_founder_mediated_mode() {
        let founder_identity =
            Identity::generate(ployz_types::model::MachineId("founder".into()), [7; 32]);
        let joiner_identity =
            Identity::generate(ployz_types::model::MachineId("joiner".into()), [8; 32]);
        let founder_subnet: ipnet::Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let network = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &founder_identity.public_key,
            "10.210.0.0/16",
            founder_subnet,
        );

        let (token, _) = issue_invite_token(
            &founder_identity,
            &network,
            "invite-1".into(),
            600,
            now_unix_secs(),
            Vec::new(),
            Some(network.overlay_ip.0.to_string()),
            Some("wg-public".into()),
            Vec::new(),
        )
        .expect("issue invite");

        let data_dir = unique_temp_dir("ployz-mesh-join");
        let mut state = DaemonState::new_for_tests(
            &data_dir,
            joiner_identity,
            "10.210.0.0/16".into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            1,
        );

        let response = state.handle_mesh_join(&token).await;
        assert!(!response.ok);
        assert_eq!(response.code, "UNSUPPORTED");
    }

    #[tokio::test]
    async fn mesh_accept_installs_transient_peer_without_store_write() {
        let (mut state, store, network) = make_active_state().await;
        let response = JoinResponse {
            machine_id: MachineId("joiner".into()),
            public_key: PublicKey([2; 32]),
            overlay_ip: "fd00::2".parse().map(OverlayIp).expect("valid overlay"),
            subnet: Some("10.210.1.0/24".parse().expect("valid subnet")),
            endpoints: vec!["203.0.113.10:51820".into()],
        }
        .encode()
        .expect("encode join response");

        let result = state.handle_mesh_accept(&response).await;
        assert!(result.ok, "{}", result.message);
        assert!(result.message.contains("awaiting self-publication"));

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let machines = store.list_machines().await.expect("list machines");
        assert!(!machines.into_iter().any(|machine| machine.id.0 == "joiner"));
        assert!(
            network
                .current_peers()
                .into_iter()
                .any(|machine| machine.id.0 == "joiner")
        );

        if let Some(active) = state.active.as_mut() {
            active.mesh.destroy().await.expect("destroy mesh");
        }
    }

    #[tokio::test]
    async fn mesh_bootstrap_refuses_to_overwrite_existing_network_config() {
        let identity = Identity::generate(MachineId("joiner".into()), [9; 32]);
        let data_dir = unique_temp_dir("ployz-bootstrap-guard");
        let existing = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.1.0/24".parse().expect("valid subnet"),
        );
        let config_path = NetworkConfig::path(&data_dir, "alpha");
        existing.save(&config_path).expect("save existing config");

        let mut state = DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            1,
        );

        let response = state
            .handle_mesh_bootstrap(&MeshBootstrapRequest {
                network_id: ployz_types::model::NetworkId("net-new".into()),
                network_name: "alpha".into(),
                cluster_cidr: "10.210.0.0/16".into(),
                assigned_subnet: "10.210.2.0/24".parse().expect("valid subnet"),
                self_control_target: None,
                bootstrap_peers: Vec::new(),
            })
            .await;
        assert!(!response.ok);
        assert_eq!(response.code, "NETWORK_ALREADY_EXISTS");

        let persisted = NetworkConfig::load(&config_path).expect("load existing config");
        assert_eq!(persisted.id, existing.id);
        assert_eq!(persisted.subnet, existing.subnet);
    }

    #[test]
    fn restore_network_config_subnet_restores_previous_value() {
        let identity = Identity::generate(MachineId("joiner".into()), [10; 32]);
        let data_dir = unique_temp_dir("ployz-promote-rollback");
        let config_path = NetworkConfig::path(&data_dir, "alpha");
        let previous_subnet = Some("10.210.1.0/24".parse().expect("valid subnet"));
        let mut config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            previous_subnet.expect("subnet present"),
        );
        config.save(&config_path).expect("save initial config");

        config.subnet = Some("10.210.2.0/24".parse().expect("valid subnet"));
        config.save(&config_path).expect("save promoted config");

        restore_network_config_subnet(&config_path, &mut config, previous_subnet)
            .expect("restore subnet");

        let persisted = NetworkConfig::load(&config_path).expect("load restored config");
        assert_eq!(persisted.subnet, previous_subnet);
    }

    async fn make_active_state() -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);
        let config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        let store = Arc::new(MemoryStore::new());
        store
            .upsert_self_machine(&ployz_types::model::MachineRecord {
                id: identity.machine_id.clone(),
                public_key: identity.public_key.clone(),
                overlay_ip: config.overlay_ip,
                subnet: config.subnet,
                control_target: None,
                bridge_ip: None,
                endpoints: vec!["127.0.0.1:51820".into()],
                status: ployz_types::model::MachineStatus::Unknown,
                participation: ployz_types::model::Participation::Disabled,
                created_at: 0,
                updated_at: 0,
                labels: std::collections::BTreeMap::new(),
            })
            .await
            .expect("upsert founder");
        let network = Arc::new(MemoryWireGuard::new());
        let mut mesh = Mesh::new(
            WireguardDriver::memory_with(network.clone()),
            StoreDriver::memory_with(store.clone(), Arc::new(MemoryService::new())),
            None,
            identity.machine_id.clone(),
            51820,
        );
        mesh.up().await.expect("mesh up");

        let mut state = DaemonState::new_for_tests(
            &unique_temp_dir("ployz-mesh-accept"),
            identity,
            "10.210.0.0/16".into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            1,
        );
        state.active = Some(ActiveMesh {
            config,
            mesh,
            remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            peer_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        });

        (state, store, network)
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
