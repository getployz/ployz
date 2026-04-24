use crate::daemon::setup::MeshStartOptions;
use crate::mesh_state::network::NetworkConfig;
use ployz_api::MachineTransitionGoal;
use ployz_orchestrator::ipam::pick_candidate_subnet;
use ployz_store_api::MachineStore;
use ployz_types::model::MachineRecord;
use ployz_types::model::{MachineLifecycle, NetworkLifecycle, NetworkName};
use tracing::warn;

use super::DaemonState;
use crate::daemon::ActiveMesh;

impl DaemonState {
    pub(crate) fn handle_mesh_create(&self, network: &str) -> ployz_api::DaemonResponse {
        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        self.ok(format!(
            "created network '{}'\n  overlay: {}\n  lifecycle: {}",
            net_config.name, net_config.overlay_ip, net_config.lifecycle
        ))
    }

    pub(crate) async fn handle_mesh_init(&mut self, network: &str) -> ployz_api::DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh stop` first",
                    active.config.name
                ),
            );
        }

        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        match self.start_network_transition(net_config, false, true).await {
            Ok(message) => self.ok(message),
            Err(error) => self.err("NETWORK_START_FAILED", error),
        }
    }

    pub(crate) fn create_network_config(&self, network: &str) -> Result<NetworkConfig, String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return Err(format!(
                "network '{network}' already exists -- use `mesh start {network}` or `mesh destroy {network}`"
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

        net_config
            .save(&config_path)
            .map_err(|e| format!("failed to save network config: {e}"))?;

        Ok(net_config)
    }

    pub(crate) async fn handle_mesh_start(
        &mut self,
        network: &str,
        allow_disconnected_bootstrap: bool,
    ) -> ployz_api::DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh stop` first",
                    active.config.name
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
        if net_config.lifecycle != NetworkLifecycle::Stopped {
            return self.err(
                "INVALID_TRANSITION",
                format!(
                    "network '{}' is {} -- expected stopped before start",
                    net_config.name, net_config.lifecycle
                ),
            );
        }

        match self
            .start_network_transition(net_config, allow_disconnected_bootstrap, false)
            .await
        {
            Ok(message) => self.ok(message),
            Err(error) => self.err("NETWORK_START_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_stop(&mut self, force: bool) -> ployz_api::DaemonResponse {
        let (network_name, overlay_ip, cached_subnet, current_lifecycle, previous_self_record) = {
            let Some(active) = self.active.as_ref() else {
                return self.err("NO_RUNNING_NETWORK", "no mesh running");
            };
            let Some(self_record) = active.mesh.authoritative_self_record().await else {
                return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
            };
            (
                active.config.name.0.clone(),
                active.config.overlay_ip,
                active.config.subnet.or(active.cached_subnet),
                self_record.lifecycle,
                self_record,
            )
        };

        if !force && current_lifecycle != MachineLifecycle::Draining {
            return self.err(
                "INVALID_TRANSITION",
                "mesh stop requires the local machine to be draining; rerun with --force to bypass",
            );
        }

        let Some(mut active) = self.active.take() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        if let Err(error) = active.mesh.destroy().await {
            self.active = Some(active);
            return self.err("NETWORK_STOP_FAILED", format!("mesh stop failed: {error}"));
        }
        if let Err(error) = persist_stopped_self_record(&mut active, &previous_self_record).await {
            warn!(%error, "failed to persist standby self record after mesh stop");
        }
        let _ = active.peer_control.shutdown().await;
        let _ = active.remote_control.shutdown().await;
        if let Err(error) = active.dns.shutdown().await {
            warn!(?error, "dns stop failed during mesh stop");
        }
        if let Err(error) = active.gateway.shutdown().await {
            return self.err(
                "NETWORK_STOP_FAILED",
                format!("gateway stop failed: {error}"),
            );
        }

        let mut persisted = active.config.clone();
        persisted.lifecycle = NetworkLifecycle::Stopped;
        persisted.subnet = cached_subnet;
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        if let Err(error) = persisted.save(&config_path) {
            return self.err(
                "IO_ERROR",
                format!("failed to persist stopped network config: {error}"),
            );
        }

        self.ok(format!(
            "mesh '{}' stopped\n  overlay: {}\n  lifecycle: stopped",
            network_name, overlay_ip
        ))
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
            let response = self.handle_mesh_stop(true).await;
            if !response.ok {
                return response;
            }
        }

        if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
            return self.err("IO_ERROR", format!("failed to delete network config: {e}"));
        }

        self.ok(format!("mesh '{network}' destroyed"))
    }

    async fn start_network_transition(
        &mut self,
        net_config: NetworkConfig,
        allow_disconnected_bootstrap: bool,
        initialized: bool,
    ) -> Result<String, String> {
        let Some(assigned_subnet) = net_config.subnet else {
            return Err(format!(
                "network '{}' has no local subnet assignment to activate",
                net_config.name
            ));
        };

        let mut running_config = net_config.clone();
        running_config.lifecycle = NetworkLifecycle::Running;
        let network_name = running_config.name.clone();
        let overlay_ip = running_config.overlay_ip;
        self.start_mesh(
            running_config.clone(),
            None,
            MeshStartOptions {
                allow_disconnected_bootstrap,
            },
        )
        .await
        .map_err(|error| error.to_string())?;

        if let Err(error) = self
            .transition_local_machine(
                MachineTransitionGoal::Activate,
                Some(assigned_subnet),
                false,
            )
            .await
        {
            self.stop_started_mesh_after_transition_failure().await;
            return Err(error.message);
        }

        let config_path = NetworkConfig::path(&self.data_dir, &network_name.0);
        if let Some(active) = self.active.as_mut() {
            active.config.lifecycle = NetworkLifecycle::Running;
            if let Err(error) = active.config.save(&config_path) {
                self.stop_started_mesh_after_transition_failure().await;
                return Err(format!("save network config: {error}"));
            }
            active.cached_subnet = active.config.subnet;
        }

        let verb = if initialized {
            "initialized and started"
        } else {
            "started"
        };
        Ok(format!(
            "mesh '{}' {}\n  overlay: {}\n  lifecycle: running",
            network_name, verb, overlay_ip
        ))
    }

    pub(super) async fn stop_started_mesh_after_transition_failure(&mut self) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        if let Err(error) = active.mesh.destroy().await {
            warn!(?error, "failed to stop mesh after transition error");
        }
        let _ = active.peer_control.shutdown().await;
        let _ = active.remote_control.shutdown().await;
        if let Err(error) = active.dns.shutdown().await {
            warn!(?error, "failed to stop dns after transition error");
        }
        if let Err(error) = active.gateway.shutdown().await {
            warn!(?error, "failed to stop gateway after transition error");
        }
    }
}

async fn persist_stopped_self_record(
    active: &mut ActiveMesh,
    previous_self_record: &MachineRecord,
) -> Result<(), String> {
    let mut standby = previous_self_record.clone();
    standby.lifecycle = MachineLifecycle::Standby;
    standby.subnet = None;
    standby.updated_at = ployz_types::time::now_unix_secs();

    if let Err(error) = active.mesh.store.upsert_self_machine(&standby).await {
        return Err(format!("persist standby self record in store: {error}"));
    }

    if active
        .mesh
        .update_authoritative_self_record(|record| {
            *record = standby.clone();
        })
        .await
        .is_none()
    {
        return Err("persist standby self record in authoritative cache".to_string());
    }

    Ok(())
}
