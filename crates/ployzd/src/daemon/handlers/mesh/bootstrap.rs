use crate::mesh_state::bootstrap::{
    BootstrapPeerRecord, refresh_bootstrap_peer_records_from_store, write_bootstrap_peer_records,
};
use crate::mesh_state::network::NetworkConfig;
use ployz_api::{
    DaemonPayload, DaemonResponse, MachineSelfTransition, MeshBootstrapRequest,
    MeshSelfRecordPayload,
};
use ployz_host_backends::network::endpoints::detect_advertised_endpoints;
use ployz_types::model::{
    NetworkLifecycleGoal, NetworkLifecycleTransition, NetworkName, NetworkTransitionEvidence,
    RegionRole, StorageParticipation,
};

use super::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_mesh_join(&mut self, token: &str) -> DaemonResponse {
        let _ = token;
        self.err(
            "UNSUPPORTED",
            "standalone `mesh join` is not supported; use `machine add` from an active mesh",
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
        net_config.storage = true;
        net_config.storage_participation = StorageParticipation::Candidate;
        net_config.region_role = RegionRole::Compute;

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

        if let Err(error) = net_config
            .lifecycle
            .apply_transition(NetworkLifecycleTransition {
                goal: NetworkLifecycleGoal::Start,
                evidence: NetworkTransitionEvidence::BootstrapJoin {
                    network: net_config.name.clone(),
                },
                at_unix_secs: ployz_types::time::now_unix_secs(),
            })
        {
            return self.err("INVALID_TRANSITION", error.message().to_string());
        }
        match self.start_mesh(net_config.clone()).await {
            Ok(_) => {
                if let Err(error) = self
                    .transition_local_machine(MachineSelfTransition::Activate {
                        assigned_subnet: request.assigned_subnet,
                    })
                    .await
                {
                    self.stop_started_mesh_after_transition_failure().await;
                    return self.err("NETWORK_START_FAILED", error.message);
                }
                let config_path = NetworkConfig::path(&self.data_dir, network);
                if let Some(active) = self.active.as_mut() {
                    let active_network_name = active.config.name.clone();
                    if let Err(error) =
                        active
                            .config
                            .lifecycle
                            .apply_transition(NetworkLifecycleTransition {
                                goal: NetworkLifecycleGoal::Start,
                                evidence: NetworkTransitionEvidence::BootstrapJoin {
                                    network: active_network_name,
                                },
                                at_unix_secs: ployz_types::time::now_unix_secs(),
                            })
                    {
                        self.stop_started_mesh_after_transition_failure().await;
                        return self.err("INVALID_TRANSITION", error.message().to_string());
                    }
                    if let Err(error) = active.config.save(&config_path) {
                        self.stop_started_mesh_after_transition_failure().await;
                        return self.err(
                            "NETWORK_START_FAILED",
                            format!("failed to persist running network config: {error}"),
                        );
                    }
                }
                if let Some(active) = self.active.as_ref()
                    && let Err(error) = refresh_bootstrap_peer_records_from_store(
                        &network_dir,
                        &active.mesh.store,
                        &self.identity.machine_id,
                    )
                    .await
                {
                    tracing::warn!(%error, "failed to refresh bootstrap peer seed after mesh bootstrap");
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
        let mut record = self_record;
        record.endpoints = endpoints;
        record.overlay_ip = active.config.overlay_ip;
        self.ok_with_payload(
            format!("machine self record '{}'", record.id),
            Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
                record,
            })),
        )
    }
}
