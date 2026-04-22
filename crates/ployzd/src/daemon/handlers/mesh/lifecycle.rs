use crate::daemon::setup::MeshStartOptions;
use crate::mesh_state::network::NetworkConfig;
use ployz_orchestrator::ipam::pick_candidate_subnet;
use ployz_types::model::NetworkName;
use tracing::warn;

use super::DaemonState;

impl DaemonState {
    pub(crate) fn handle_mesh_create(&self, network: &str) -> ployz_api::DaemonResponse {
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

    pub(crate) async fn handle_mesh_init(&mut self, network: &str) -> ployz_api::DaemonResponse {
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
    ) -> ployz_api::DaemonResponse {
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

    pub(crate) async fn handle_mesh_down(&mut self) -> ployz_api::DaemonResponse {
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

    pub(crate) async fn handle_mesh_destroy(&mut self, network: &str) -> ployz_api::DaemonResponse {
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
}
