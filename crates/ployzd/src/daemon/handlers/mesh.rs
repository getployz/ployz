use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use tracing::warn;

use crate::daemon::setup::MeshStartOptions;
use crate::model::{JoinResponse, NetworkName};
use crate::network::endpoints::detect_endpoints;
use crate::network::ipam::Ipam;
use crate::node::invite::parse_and_verify_invite_token;
use crate::store::bootstrap::BootstrapInfo;
use crate::store::network::NetworkConfig;
use crate::store::{InviteStore, MachineStore};
use ployz_sdk::transport::DaemonResponse;

use super::super::DaemonState;
use crate::time::now_unix_secs;

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
            .map(|a| a.config.name.0 == network)
            .unwrap_or(false);
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

        let status = active.mesh.ready_status().await;
        if json {
            let payload = MeshReadyPayload::from(status);
            return match serde_json::to_string(&payload) {
                Ok(body) => self.ok(body),
                Err(err) => self.err(
                    "ENCODE_FAILED",
                    format!("failed to encode readiness payload: {err}"),
                ),
            };
        }

        self.ok(format!(
            "ready:            {}\nphase:            {}\nstore healthy:    {}\nsync connected:   {}\nheartbeat ready:  {}",
            status.ready,
            status.phase,
            status.store_healthy,
            status.sync_connected,
            status.heartbeat_started,
        ))
    }

    pub(crate) async fn handle_mesh_join(&mut self, token: &str) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name
                ),
            );
        }

        let invite = match parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };

        let now = now_unix_secs();
        if now > invite.expires_at {
            return self.err("INVITE_EXPIRED", "invite token has expired");
        }

        let network = invite.network_name.trim();
        if network.is_empty() {
            return self.err("INVALID_INVITE_TOKEN", "invite token network name is empty");
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return self.err(
                "NETWORK_ALREADY_EXISTS",
                format!("network '{network}' already exists -- destroy it first"),
            );
        }

        let subnet = match invite.allocated_subnet.parse() {
            Ok(subnet) => subnet,
            Err(err) => {
                return self.err(
                    "INVALID_INVITE_TOKEN",
                    format!("invite allocated subnet is invalid: {err}"),
                );
            }
        };

        let mut net_config = NetworkConfig::new(
            NetworkName(network.to_string()),
            &self.identity.public_key,
            &self.cluster_cidr,
            subnet,
        );
        net_config.id = invite.network_id.clone();

        if let Err(e) = net_config.save(&config_path) {
            return self.err("IO_ERROR", format!("failed to save network config: {e}"));
        }

        let bootstrap = Self::extract_bootstrap(&invite);

        let options = MeshStartOptions {
            allow_disconnected_bootstrap: bootstrap.is_some(),
        };
        match self.start_mesh(net_config, bootstrap, options).await {
            Ok(_) => {}
            Err(e) => {
                return self.err(
                    "NETWORK_START_FAILED",
                    format!("join failed to start mesh: {e}"),
                );
            }
        }

        if let Some(active) = self.active.as_ref()
            && let Err(e) = active
                .mesh
                .store
                .consume_invite(&invite.invite_id, now_unix_secs())
                .await
        {
            tracing::warn!(?e, "failed to consume invite (mesh already joined)");
        }

        self.ok(format!("joined and started network '{network}'"))
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
                return DaemonResponse {
                    ok: false,
                    code: "NETWORK_START_FAILED".into(),
                    message: format!(
                        "initialized network '{}' but failed to start: {e}\n  state:   created",
                        network_name,
                    ),
                };
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
        let mut ipam = Ipam::new(cluster, self.subnet_prefix_len);
        let subnet = ipam
            .allocate()
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
                return DaemonResponse {
                    ok: false,
                    code: "IO_ERROR".into(),
                    message: format!("failed to load network config: {e}"),
                };
            }
        };

        let network_name = net_config.name.clone();
        let options = MeshStartOptions {
            allow_disconnected_bootstrap: skip_bootstrap_wait,
        };
        match self.start_mesh(net_config, None, options).await {
            Ok(_) => {}
            Err(e) => {
                return DaemonResponse {
                    ok: false,
                    code: "NETWORK_START_FAILED".into(),
                    message: e.to_string(),
                };
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
        active.remote_control.shutdown().await;
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
            .map(|a| a.config.name.0 == network)
            .unwrap_or(false);

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
                return DaemonResponse {
                    ok: false,
                    code: "NETWORK_DESTROY_FAILED".into(),
                    message: format!("destroy failed: {e}"),
                };
            }
            active.remote_control.shutdown().await;
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
            return DaemonResponse {
                ok: false,
                code: "IO_ERROR".into(),
                message: format!("failed to delete network config: {e}"),
            };
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
        let resp = JoinResponse {
            machine_id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: active.config.overlay_ip,
            subnet: Some(active.config.subnet),
            endpoints,
        };

        match resp.encode() {
            Ok(encoded) => self.ok(encoded),
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

        let record = join_resp.into_machine_record();
        let machine_id = record.id.clone();
        match active.mesh.store.upsert_machine(&record).await {
            Ok(()) => self.ok(format!("accepted machine '{}'", machine_id)),
            Err(e) => self.err("UPSERT_FAILED", format!("failed to upsert machine: {e}")),
        }
    }

    fn extract_bootstrap(invite: &crate::node::invite::InviteClaims) -> Option<BootstrapInfo> {
        let wg_key_b64 = invite.issuer_wg_public_key.as_deref()?;
        let overlay_str = invite.issuer_overlay_ip.as_deref()?;

        let key_bytes = URL_SAFE_NO_PAD.decode(wg_key_b64).ok()?;
        let peer_wg_public_key: [u8; 32] = key_bytes.as_slice().try_into().ok()?;
        let peer_overlay_ip: std::net::Ipv6Addr = overlay_str.parse().ok()?;

        if invite.issuer_endpoints.is_empty() {
            return None;
        }

        Some(BootstrapInfo {
            peer_id: invite.issued_by.clone(),
            peer_wg_public_key,
            peer_overlay_ip,
            peer_endpoints: invite.issuer_endpoints.clone(),
        })
    }
}

#[derive(Serialize)]
struct MeshReadyPayload {
    ready: bool,
    phase: String,
    store_healthy: bool,
    sync_connected: bool,
    heartbeat_started: bool,
}

impl From<crate::mesh::orchestrator::MeshReadyStatus> for MeshReadyPayload {
    fn from(value: crate::mesh::orchestrator::MeshReadyStatus) -> Self {
        Self {
            ready: value.ready,
            phase: value.phase.to_string(),
            store_healthy: value.store_healthy,
            sync_connected: value.sync_connected,
            heartbeat_started: value.heartbeat_started,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;
    use crate::node::identity::Identity;
    use crate::node::invite::issue_invite_token;
    use crate::store::network::NetworkConfig;
    use crate::time::now_unix_secs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn mesh_join_uses_founder_allocated_subnet_exactly() {
        let founder_identity =
            Identity::generate(crate::model::MachineId("founder".into()), [7; 32]);
        let joiner_identity = Identity::generate(crate::model::MachineId("joiner".into()), [8; 32]);
        let founder_subnet: ipnet::Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let allocated_subnet = "10.210.42.0/24";
        let network = NetworkConfig::new(
            crate::model::NetworkName("alpha".into()),
            &founder_identity.public_key,
            "10.210.0.0/16",
            founder_subnet,
        );

        let (token, _) = issue_invite_token(
            &founder_identity,
            &network,
            600,
            now_unix_secs(),
            Vec::new(),
            Some(network.overlay_ip.0.to_string()),
            Some("wg-public".into()),
            Some(network.subnet.to_string()),
            allocated_subnet.into(),
        )
        .expect("issue invite");

        let data_dir = unique_temp_dir("ployz-mesh-join");
        let mut state = DaemonState::new(
            &data_dir,
            joiner_identity,
            Mode::Memory,
            "10.210.0.0/16".into(),
            24,
            4317,
            "127.0.0.1:0".into(),
            1,
        );

        let response = state.handle_mesh_join(&token).await;
        assert!(response.ok, "{}", response.message);

        let active = state.active.as_ref().expect("mesh active");
        assert_eq!(active.config.subnet.to_string(), allocated_subnet);

        if let Some(active) = state.active.as_mut() {
            active.mesh.destroy().await.expect("destroy mesh");
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
