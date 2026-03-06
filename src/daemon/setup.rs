use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Mode;
use crate::drivers::{StoreDriver, WireguardDriver};
use crate::mesh::orchestrator::Mesh;
use crate::model::{MachineId, MachineRecord, OverlayIp, PublicKey};
use crate::network::endpoints::detect_endpoints;
use crate::store::MachineStore;
use crate::store::network::NetworkConfig;
use crate::{
    CorrosionStore, DockerCorrosion, DockerWireGuard, HostCorrosion, HostWireGuard, MemoryService,
    MemoryStore, MemoryWireGuard, SCHEMA_SQL, corrosion_config,
};

use super::{ActiveMesh, DaemonState};

fn which_corrosion() -> Result<PathBuf, String> {
    let candidates = ["/usr/local/bin/corrosion", "/usr/bin/corrosion"];
    for path in &candidates {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }
    Err("corrosion binary not found (expected at /usr/local/bin/corrosion)".into())
}

pub struct BootstrapInfo {
    pub peer_id: String,
    pub peer_wg_public_key: [u8; 32],
    pub peer_overlay_ip: Ipv6Addr,
    pub peer_endpoints: Vec<String>,
}

impl DaemonState {
    pub async fn start_mesh_by_name(&mut self, network: &str) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        let net_config = NetworkConfig::load(&config_path)
            .map_err(|e| format!("load network config: {e}"))?;
        self.start_mesh(net_config, None).await
    }

    pub async fn start_mesh(
        &mut self,
        net_config: NetworkConfig,
        bootstrap: Option<BootstrapInfo>,
    ) -> Result<(), String> {
        let corrosion_bootstrap: Vec<String> = bootstrap
            .as_ref()
            .and_then(|bs| {
                Some(vec![format!("[{}]:{}", bs.peer_overlay_ip, corrosion_config::DEFAULT_GOSSIP_PORT)])
            })
            .unwrap_or_default();

        let (network, store) = match self.mode {
            Mode::Memory => {
                let network = WireguardDriver::Memory(Arc::new(MemoryWireGuard::new()));
                let store = StoreDriver::Memory {
                    store: Arc::new(MemoryStore::new()),
                    service: Arc::new(MemoryService::new()),
                };
                (network, store)
            }
            Mode::Docker => {
                // In Docker mode, the bridge forwards localhost → overlay for host access.
                // Corrosion binds to overlay_ip inside the container namespace,
                // but the host connects to localhost via the bridge.
                let api_port = corrosion_config::DEFAULT_API_PORT;
                let overlay_api = SocketAddr::new(
                    IpAddr::V6(net_config.overlay_ip.0),
                    api_port,
                );
                let local_api = SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    api_port,
                );

                let wg = DockerWireGuard::new(
                    "ployz-wireguard",
                    &self.data_dir,
                    self.identity.private_key.clone(),
                    net_config.overlay_ip,
                )
                .with_bridge_forward(local_api, overlay_api)
                .build()
                .await
                .map_err(|e| format!("docker wireguard: {e}"))?;
                let network = WireguardDriver::Docker(Arc::new(wg));
                let store = self
                    .build_corrosion_store_bridged(net_config.overlay_ip, local_api, &corrosion_bootstrap, &net_config.id.0)
                    .await?;
                (network, store)
            }
            Mode::HostExec | Mode::HostService => {
                let ifname = format!("plz-{}", net_config.name.0);
                #[cfg(target_os = "linux")]
                let wg = HostWireGuard::kernel(
                    &ifname,
                    self.identity.private_key.clone(),
                    net_config.overlay_ip,
                )
                .map_err(|e| format!("host wireguard: {e}"))?;
                #[cfg(not(target_os = "linux"))]
                let wg = HostWireGuard::userspace(
                    &ifname,
                    self.identity.private_key.clone(),
                    net_config.overlay_ip,
                )
                .map_err(|e| format!("host wireguard: {e}"))?;
                let network = WireguardDriver::Host(Arc::new(wg));
                let store = self.build_corrosion_store_host(net_config.overlay_ip, &corrosion_bootstrap, &net_config.id.0)?;
                (network, store)
            }
        };

        tracing::info!(mode = ?self.mode, "starting mesh");

        let container_network = match self.mode {
            Mode::Docker => Some(
                crate::adapters::docker_network::DockerBridgeNetwork::new(
                    &net_config.name.0,
                    net_config.subnet,
                )
                .await
                .map_err(|e| format!("docker bridge network: {e}"))?,
            ),
            _ => None,
        };

        let overlay_ip = net_config.overlay_ip;
        let mut mesh = Mesh::new(
            network,
            store,
            container_network,
            self.identity.machine_id.clone(),
            51820,
        );

        mesh.up()
            .await
            .map_err(|e| format!("failed to start network: {e}"))?;

        // Inject bootstrap peer into WG before Corrosion gossip starts syncing
        if let Some(ref bs) = bootstrap {
            let bootstrap_record = MachineRecord {
                id: MachineId(bs.peer_id.clone()),
                public_key: PublicKey(bs.peer_wg_public_key),
                overlay_ip: OverlayIp(bs.peer_overlay_ip),
                subnet: None,
                bridge_ip: None,
                endpoints: bs.peer_endpoints.clone(),
            };
            mesh.store()
                .upsert_machine(&bootstrap_record)
                .await
                .map_err(|e| format!("failed to seed bootstrap peer: {e}"))?;
        }

        let endpoints = detect_endpoints(51820).await;
        let self_record = MachineRecord {
            id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip,
            subnet: Some(net_config.subnet),
            bridge_ip: None,
            endpoints,
        };
        mesh.store()
            .upsert_machine(&self_record)
            .await
            .map_err(|e| format!("failed to seed store: {e}"))?;

        let network_name = net_config.name.0.clone();
        self.active = Some(ActiveMesh {
            config: net_config,
            mesh,
        });
        self.write_active_marker(&network_name);
        Ok(())
    }

    /// Build Corrosion store for Docker mode with bridge forwarding.
    /// Corrosion config uses overlay_ip (container-side), client uses local_endpoint (host-side via bridge).
    async fn build_corrosion_store_bridged(
        &self,
        overlay_ip: crate::model::OverlayIp,
        local_endpoint: SocketAddr,
        bootstrap: &[String],
        network_id: &str,
    ) -> Result<StoreDriver, String> {
        let paths = corrosion_config::Paths::new(&self.data_dir);
        let gossip_addr = corrosion_config::default_gossip_addr();
        // Corrosion API binds to overlay IP inside the container namespace
        let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

        corrosion_config::write_config(&paths, SCHEMA_SQL, gossip_addr, api_addr, bootstrap, Some(network_id))
            .map_err(|e| format!("write corrosion config: {e}"))?;

        let config_path = paths.config.to_string_lossy().into_owned();
        let dir_mount = paths.dir.to_string_lossy().into_owned();

        let service = DockerCorrosion::new("ployz-corrosion", "ghcr.io/getployz/corrosion")
            .cmd(vec!["agent".into(), "-c".into(), config_path])
            .volume(&dir_mount, &dir_mount)
            .network_mode("container:ployz-wireguard")
            .build()
            .await
            .map_err(|e| format!("docker service: {e}"))?;

        // Host connects to localhost via bridge
        let endpoint = local_endpoint.to_string();
        let corrosion = CorrosionStore::new(&endpoint, &paths.db)
            .map_err(|e| format!("corrosion store: {e}"))?;

        tracing::info!(%endpoint, "store backend: corrosion (bridged)");

        Ok(StoreDriver::Corrosion {
            store: corrosion,
            service: Arc::new(service),
        })
    }

    /// Build Corrosion store for Host modes using the native corrosion binary.
    fn build_corrosion_store_host(
        &self,
        overlay_ip: crate::model::OverlayIp,
        bootstrap: &[String],
        network_id: &str,
    ) -> Result<StoreDriver, String> {
        let paths = corrosion_config::Paths::new(&self.data_dir);
        let gossip_addr = corrosion_config::default_gossip_addr();
        let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

        corrosion_config::write_config(&paths, SCHEMA_SQL, gossip_addr, api_addr, bootstrap, Some(network_id))
            .map_err(|e| format!("write corrosion config: {e}"))?;

        let binary = which_corrosion().map_err(|e| format!("corrosion binary: {e}"))?;
        let service = HostCorrosion::new(binary, &paths.config);

        let endpoint = api_addr.to_string();
        let corrosion = CorrosionStore::new(&endpoint, &paths.db)
            .map_err(|e| format!("corrosion store: {e}"))?;

        tracing::info!(%endpoint, "store backend: corrosion (host)");

        Ok(StoreDriver::CorrosionHost {
            store: corrosion,
            service: Arc::new(service),
        })
    }
}
