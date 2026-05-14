use crate::daemon::{DaemonState, RetainedSubnet};
use crate::mesh_state::network::NetworkConfig;
use ployz_api::MachineSelfTransition;
use ployz_model::{
    NetworkLifecycle, NetworkLifecycleGoal, NetworkLifecycleTransition, NetworkName,
    NetworkTransitionEvidence,
};
use ployz_runtime_api::ipam::pick_candidate_subnet;

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

        match self.start_network_transition(net_config, true).await {
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

    pub(crate) async fn handle_mesh_start(&mut self, network: &str) -> ployz_api::DaemonResponse {
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

        match self.start_network_transition(net_config, false).await {
            Ok(message) => self.ok(message),
            Err(error) => self.err("NETWORK_START_FAILED", error),
        }
    }

    async fn start_network_transition(
        &mut self,
        net_config: NetworkConfig,
        initialized: bool,
    ) -> Result<String, String> {
        let Some(assigned_subnet) = net_config.subnet else {
            return Err(format!(
                "network '{}' has no local subnet assignment to activate",
                net_config.name
            ));
        };

        let mut running_config = net_config.clone();
        running_config
            .lifecycle
            .apply_transition(NetworkLifecycleTransition {
                goal: NetworkLifecycleGoal::Start,
                evidence: NetworkTransitionEvidence::OperatorCommand {
                    command: if initialized {
                        "mesh init".into()
                    } else {
                        "mesh start".into()
                    },
                },
                at_unix_secs: ployz_time::now_unix_secs(),
            })
            .map_err(|error| error.message().to_string())?;
        let network_name = running_config.name.clone();
        let overlay_ip = running_config.overlay_ip;
        self.start_mesh(running_config.clone())
            .await
            .map_err(|error| error.to_string())?;

        if let Err(error) = self
            .transition_local_machine(MachineSelfTransition::Activate { assigned_subnet })
            .await
        {
            self.stop_started_mesh_after_transition_failure().await;
            return Err(error.message);
        }

        let config_path = NetworkConfig::path(&self.data_dir, &network_name.0);
        if let Some(active) = self.active.as_mut() {
            let active_command = if initialized {
                "mesh init".into()
            } else {
                "mesh start".into()
            };
            active
                .config
                .lifecycle
                .apply_transition(NetworkLifecycleTransition {
                    goal: NetworkLifecycleGoal::Start,
                    evidence: NetworkTransitionEvidence::OperatorCommand {
                        command: active_command,
                    },
                    at_unix_secs: ployz_time::now_unix_secs(),
                })
                .map_err(|error| error.message().to_string())?;
            if let Err(error) = active.config.save(&config_path) {
                self.stop_started_mesh_after_transition_failure().await;
                return Err(format!("save network config: {error}"));
            }
            active.retained_subnet = RetainedSubnet::from_running_config(active.config.subnet);
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
}
