use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use crate::config::Mode;
use crate::drivers::{StoreDriver, WireguardDriver};
use crate::mesh::orchestrator::Mesh;
use crate::model::MachineRecord;
use crate::store::MachineStore;
use crate::store::network::NetworkConfig;
use crate::{
    CorrosionStore, DockerCorrosion, DockerWireGuard, HostWireGuard, MemoryService, MemoryStore,
    MemoryWireGuard, SCHEMA_SQL, corrosion_config,
};

use super::{ActiveMesh, DaemonState};

impl DaemonState {
    pub async fn start_mesh_by_name(&mut self, network: &str) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        let net_config = NetworkConfig::load(&config_path)
            .map_err(|e| format!("load network config: {e}"))?;
        self.start_mesh(net_config).await
    }

    pub async fn start_mesh(&mut self, net_config: NetworkConfig) -> Result<(), String> {
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
                    .build_corrosion_store_bridged(net_config.overlay_ip, local_api)
                    .await?;
                (network, store)
            }
            Mode::HostExec | Mode::HostService => {
                let wg = HostWireGuard::userspace(
                    "utun8",
                    self.identity.private_key.clone(),
                    net_config.overlay_ip,
                )
                .map_err(|e| format!("host wireguard: {e}"))?;
                let network = WireguardDriver::Host(Arc::new(wg));
                let store = self.build_corrosion_store_direct(net_config.overlay_ip).await?;
                (network, store)
            }
        };

        tracing::info!(mode = ?self.mode, "starting mesh");

        let overlay_ip = net_config.overlay_ip;
        let mut mesh = Mesh::new(network, store);

        mesh.up()
            .await
            .map_err(|e| format!("failed to start network: {e}"))?;

        let self_record = MachineRecord {
            id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip,
            subnet: Some(net_config.subnet),
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
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
    ) -> Result<StoreDriver, String> {
        let paths = corrosion_config::Paths::new(&self.data_dir);
        let gossip_addr = corrosion_config::default_gossip_addr();
        // Corrosion API binds to overlay IP inside the container namespace
        let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

        corrosion_config::write_config(&paths, SCHEMA_SQL, gossip_addr, api_addr, &[])
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

    /// Build Corrosion store for Host modes with direct overlay access.
    async fn build_corrosion_store_direct(
        &self,
        overlay_ip: crate::model::OverlayIp,
    ) -> Result<StoreDriver, String> {
        let paths = corrosion_config::Paths::new(&self.data_dir);
        let gossip_addr = corrosion_config::default_gossip_addr();
        let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

        corrosion_config::write_config(&paths, SCHEMA_SQL, gossip_addr, api_addr, &[])
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

        // Host connects directly to overlay IP
        let endpoint = api_addr.to_string();
        let corrosion = CorrosionStore::new(&endpoint, &paths.db)
            .map_err(|e| format!("corrosion store: {e}"))?;

        tracing::info!(%endpoint, "store backend: corrosion (direct)");

        Ok(StoreDriver::Corrosion {
            store: corrosion,
            service: Arc::new(service),
        })
    }
}
