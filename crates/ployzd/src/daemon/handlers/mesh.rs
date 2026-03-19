use crate::mesh_state::bootstrap::{
    BootstrapInfo, BootstrapPeerRecord, write_bootstrap_peer_record,
};
use crate::mesh_state::invite::{InviteClaims, parse_and_verify_invite_token};
use crate::mesh_state::network::NetworkConfig;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_orchestrator::ipam::Ipam;
use ployz_orchestrator::mesh::orchestrator::MeshReadyStatus;
use ployz_orchestrator::mesh::tasks::PeerSyncCommand;
use ployz_types::model::{JoinResponse, NetworkName};
use ployz_types::time::now_unix_secs;
use tracing::warn;

use crate::daemon::setup::MeshStartOptions;
use ployz_api::{DaemonPayload, DaemonResponse, MeshReadyPayload, MeshSelfRecordPayload};

#[cfg(test)]
use super::super::DaemonRuntimeConfig;
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

    pub(crate) async fn handle_mesh_ready(
        &self,
        output: ployz_api::MeshReadyOutput,
    ) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let status = mesh_ready_payload(active.mesh.ready_status().await);
        if output == ployz_api::MeshReadyOutput::Json {
            return match serde_json::to_string(&status) {
                Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::MeshReady(status))),
                Err(err) => self.err(
                    "ENCODE_FAILED",
                    format!("failed to encode readiness payload: {err}"),
                ),
            };
        }

        self.ok_with_payload(format!(
            "ready:            {}\nphase:            {}\nstore healthy:    {}\nsync connected:   {}\nheartbeat ready:  {}",
            status.ready,
            status.phase,
            status.store_healthy,
            status.sync_connected,
            status.heartbeat_started,
        ), Some(DaemonPayload::MeshReady(status)))
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

        if let Some(bootstrap_peer) = BootstrapPeerRecord::from_invite(&invite)
            && let Err(error) = write_bootstrap_peer_record(
                &NetworkConfig::dir(&self.data_dir, network),
                &bootstrap_peer,
            )
        {
            return self.err(
                "IO_ERROR",
                format!("failed to persist bootstrap founder peer: {error}"),
            );
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
        bootstrap_wait: ployz_api::BootstrapWaitMode,
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
            allow_disconnected_bootstrap: bootstrap_wait == ployz_api::BootstrapWaitMode::Skip,
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

        let endpoints = match active.mesh.detect_endpoints().await {
            Ok(endpoints) => endpoints,
            Err(error) => {
                return self.err(
                    "ENDPOINT_DISCOVERY_FAILED",
                    format!("failed to detect mesh endpoints: {error}"),
                );
            }
        };
        let resp = JoinResponse {
            machine_id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: active.config.overlay_ip,
            subnet: Some(active.config.subnet),
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

    fn extract_bootstrap(invite: &InviteClaims) -> Option<BootstrapInfo> {
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

fn mesh_ready_payload(value: MeshReadyStatus) -> MeshReadyPayload {
    MeshReadyPayload {
        ready: value.ready,
        phase: value.phase.to_string(),
        store_healthy: value.store_healthy,
        sync_connected: value.sync_connected,
        heartbeat_started: value.heartbeat_started,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ActiveMesh;
    use crate::mesh_state::invite::{InviteTokenContext, issue_invite_token};
    use crate::mesh_state::network::NetworkConfig;
    use ployz_orchestrator::Mesh;
    use ployz_runtime_api::Identity;
    use ployz_runtime_api::{
        MemoryServiceRuntime, MemoryWireGuard, StaticEndpointDiscovery, WireguardDriver,
    };
    use ployz_store_api::internal::StoreDriver;
    use ployz_store_api::MachineStore;
    use ployz_store_api::memory::MemoryStore;
    use ployz_types::model::{MachineId, OverlayIp, PublicKey};
    use ployz_types::time::now_unix_secs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn mesh_join_uses_founder_allocated_subnet_exactly() {
        let founder_identity =
            Identity::generate(ployz_types::model::MachineId("founder".into()), [7; 32]);
        let joiner_identity =
            Identity::generate(ployz_types::model::MachineId("joiner".into()), [8; 32]);
        let founder_subnet: ipnet::Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let allocated_subnet = "10.210.42.0/24";
        let network = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &founder_identity.public_key,
            "10.210.0.0/16",
            founder_subnet,
        );

        let (token, _) = issue_invite_token(
            &founder_identity,
            &network,
            600,
            now_unix_secs(),
            InviteTokenContext {
                issuer_endpoints: Vec::new(),
                issuer_overlay_ip: Some(network.overlay_ip.0.to_string()),
                issuer_wg_public_key: Some("wg-public".into()),
                issuer_subnet: Some(network.subnet.to_string()),
                allocated_subnet: allocated_subnet.into(),
            },
        )
        .expect("issue invite");

        let data_dir = unique_temp_dir("ployz-mesh-join");
        let mut state = DaemonState::new_for_tests(
            &data_dir,
            joiner_identity,
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                gateway_listen_addr: "127.0.0.1:0".into(),
                gateway_threads: 1,
            },
        );

        let response = state.handle_mesh_join(&token).await;
        assert!(response.ok, "{}", response.message);

        let active = state.active.as_ref().expect("mesh active");
        assert_eq!(active.config.subnet.to_string(), allocated_subnet);

        if let Some(active) = state.active.as_mut() {
            active.mesh.destroy().await.expect("destroy mesh");
        }
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
                subnet: Some(config.subnet),
                bridge_ip: None,
                endpoints: vec!["127.0.0.1:51820".into()],
                status: ployz_types::model::MachineStatus::Unknown,
                participation: ployz_types::model::Participation::Disabled,
                last_heartbeat: 0,
                created_at: 0,
                updated_at: 0,
                labels: std::collections::BTreeMap::new(),
            })
            .await
            .expect("upsert founder");
        let network = Arc::new(MemoryWireGuard::new());
        let service = Arc::new(MemoryServiceRuntime::new());
        let mut mesh = Mesh::new(
            WireguardDriver::memory_with(network.clone()),
            StoreDriver::memory_with(store.clone()),
            service,
            None,
            Arc::new(StaticEndpointDiscovery::empty()),
            None,
            identity.machine_id.clone(),
            51820,
        );
        mesh.up().await.expect("mesh up");

        let mut state = DaemonState::new_for_tests(
            &unique_temp_dir("ployz-mesh-accept"),
            identity,
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                gateway_listen_addr: "127.0.0.1:0".into(),
                gateway_threads: 1,
            },
        );
        state.active = Some(ActiveMesh {
            config,
            mesh,
            remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
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
