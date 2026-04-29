use crate::daemon::setup::MeshStartOptions;
use crate::mesh_state::bootstrap::{BootstrapPeerRecord, write_bootstrap_peer_records};
use crate::mesh_state::network::NetworkConfig;
use ployz_api::{
    DaemonPayload, DaemonResponse, MachineTransitionGoal, MeshBootstrapRequest,
    MeshSelfRecordPayload,
};
use ployz_orchestrator::mesh::tasks::PeerSyncCommand;
use ployz_orchestrator::network::endpoints::detect_advertised_endpoints;
use ployz_types::model::NetworkLifecycle;
use ployz_types::model::{JoinResponse, NetworkName};

use super::DaemonState;

impl DaemonState {
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
        let peer_records = request
            .bootstrap_peers
            .iter()
            .map(BootstrapPeerRecord::from_machine_record)
            .collect::<Vec<_>>();
        if let Err(error) = write_bootstrap_peer_records(&network_dir, &peer_records) {
            return self.err(
                "IO_ERROR",
                format!("failed to persist bootstrap peers: {error}"),
            );
        }

        let options = MeshStartOptions {
            allow_disconnected_bootstrap: !request.bootstrap_peers.is_empty(),
        };
        net_config.lifecycle = NetworkLifecycle::Running;
        match self.start_mesh(net_config.clone(), options).await {
            Ok(_) => {
                if let Err(error) = self
                    .transition_local_machine(
                        MachineTransitionGoal::Activate,
                        Some(request.assigned_subnet),
                        false,
                    )
                    .await
                {
                    self.stop_started_mesh_after_transition_failure().await;
                    return self.err("NETWORK_START_FAILED", error.message);
                }
                let config_path = NetworkConfig::path(&self.data_dir, network);
                if let Some(active) = self.active.as_mut() {
                    active.config.lifecycle = NetworkLifecycle::Running;
                    if let Err(error) = active.config.save(&config_path) {
                        self.stop_started_mesh_after_transition_failure().await;
                        return self.err(
                            "NETWORK_START_FAILED",
                            format!("failed to persist running network config: {error}"),
                        );
                    }
                }
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

    pub(crate) async fn handle_mesh_self_record(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(a) => a,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let endpoints = detect_advertised_endpoints(51820).await;
        let Some(self_record) = active.mesh.authoritative_self_record().await else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        let resp = JoinResponse {
            machine_id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: active.config.overlay_ip,
            topology: self_record.topology.clone(),
            subnet: self_record.subnet,
            endpoints,
        };

        match resp.encode() {
            Ok(encoded) => self.ok_with_payload(
                encoded.clone(),
                Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
                    encoded,
                    record: resp.into_seed_machine_membership(),
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

        let record = join_resp.into_seed_machine_membership();
        let machine_id = record.id.clone();
        let observation = record.observation();
        match peer_sync_tx
            .send(PeerSyncCommand::UpsertTransient(observation))
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
}
