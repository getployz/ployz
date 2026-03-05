pub mod handlers;
pub mod ssh;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::drivers::{StoreDriver, WireguardDriver};
use crate::config::Mode;
use crate::mesh::orchestrator::Mesh;
use crate::node::identity::Identity;
use crate::store::MachineStore;
use crate::model::MachineRecord;
use crate::store::network::NetworkConfig;
use crate::transport::DaemonResponse;
use crate::{
    CorrosionStore, DockerCorrosion, DockerWireGuard, HostWireGuard, MemoryService, MemoryStore,
    MemoryWireGuard, SCHEMA_SQL, corrosion_config,
};

pub struct ActiveMesh {
    pub config: NetworkConfig,
    pub mesh: Mesh,
}

pub struct DaemonState {
    pub data_dir: PathBuf,
    pub identity: Identity,
    pub mode: Mode,
    pub active: Option<ActiveMesh>,
}

impl DaemonState {
    pub fn new(data_dir: &Path, identity: Identity, mode: Mode) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            identity,
            mode,
            active: None,
        }
    }

    pub fn active_marker_path(&self) -> PathBuf {
        self.data_dir.join("active_network")
    }

    pub fn read_active_marker(&self) -> Option<String> {
        std::fs::read_to_string(self.active_marker_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn write_active_marker(&self, network: &str) {
        let _ = std::fs::write(self.active_marker_path(), network);
    }

    pub fn clear_active_marker(&self) {
        let _ = std::fs::remove_file(self.active_marker_path());
    }

    pub fn ok(&self, message: impl Into<String>) -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: message.into(),
        }
    }

    pub fn err(&self, code: &str, message: impl Into<String>) -> DaemonResponse {
        DaemonResponse {
            ok: false,
            code: code.into(),
            message: message.into(),
        }
    }

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
                let wg = DockerWireGuard::new(
                    "ployz-wireguard",
                    &self.data_dir,
                    self.identity.private_key.clone(),
                    net_config.overlay_ip,
                )
                .build()
                .await
                .map_err(|e| format!("docker wireguard: {e}"))?;
                let network = WireguardDriver::Docker(Arc::new(wg));
                let store = self.build_corrosion_store().await?;
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
                let store = self.build_corrosion_store().await?;
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

    async fn build_corrosion_store(&self) -> Result<StoreDriver, String> {
        let paths = corrosion_config::Paths::new(&self.data_dir);
        let gossip_addr = corrosion_config::default_gossip_addr();
        let api_addr = corrosion_config::default_api_addr();

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

        let endpoint = api_addr.to_string();
        let corrosion = CorrosionStore::new(&endpoint, &paths.db)
            .map_err(|e| format!("corrosion store: {e}"))?;

        tracing::info!(%endpoint, "store backend: corrosion");

        Ok(StoreDriver::Corrosion {
            store: corrosion,
            service: Arc::new(service),
        })
    }
}
