use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::adapters::corrosion::client::Transport;
use crate::config::Mode;
use crate::drivers::{StoreDriver, WireguardDriver};
use crate::mesh::orchestrator::Mesh;
use crate::model::{MachineId, MachineRecord, OverlayIp, PublicKey};
use crate::network::endpoints::detect_endpoints;
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

fn corrosion_bootstrap_from_db(
    network_dir: &Path,
    local_machine_id: &MachineId,
) -> Result<Vec<String>, String> {
    let db_path = corrosion_config::Paths::new(network_dir).db;
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("open corrosion db '{}': {e}", db_path.display()))?;
    let mut stmt = match conn.prepare("SELECT id, overlay_ip FROM machines ORDER BY id") {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table: machines") =>
        {
            return Ok(Vec::new());
        }
        Err(e) => {
            return Err(format!(
                "prepare corrosion bootstrap query '{}': {e}",
                db_path.display()
            ));
        }
    };

    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let overlay_ip: String = row.get(1)?;
            Ok((id, overlay_ip))
        })
        .map_err(|e| format!("query corrosion bootstrap peers '{}': {e}", db_path.display()))?;

    let mut bootstrap = Vec::new();
    for row in rows {
        let (id, overlay_ip) = row.map_err(|e| {
            format!(
                "read corrosion bootstrap peer row from '{}': {e}",
                db_path.display()
            )
        })?;
        if id == local_machine_id.0 || overlay_ip.is_empty() {
            continue;
        }
        bootstrap.push(format!("[{overlay_ip}]:{}", corrosion_config::DEFAULT_GOSSIP_PORT));
    }

    Ok(bootstrap)
}

pub struct BootstrapInfo {
    pub peer_id: String,
    pub peer_wg_public_key: [u8; 32],
    pub peer_overlay_ip: Ipv6Addr,
    pub peer_endpoints: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MeshStartOptions {
    pub allow_disconnected_bootstrap: bool,
}

impl DaemonState {
    pub async fn start_mesh_by_name(&mut self, network: &str) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        let net_config =
            NetworkConfig::load(&config_path).map_err(|e| format!("load network config: {e}"))?;
        self.start_mesh(net_config, None, MeshStartOptions::default())
            .await
    }

    pub async fn start_mesh(
        &mut self,
        net_config: NetworkConfig,
        bootstrap: Option<BootstrapInfo>,
        options: MeshStartOptions,
    ) -> Result<(), String> {
        let network_dir = self.network_dir(&net_config.name.0);
        let corrosion_bootstrap: Vec<String> = bootstrap
            .as_ref()
            .and_then(|bs| {
                Some(vec![format!(
                    "[{}]:{}",
                    bs.peer_overlay_ip,
                    corrosion_config::DEFAULT_GOSSIP_PORT
                )])
            })
            .unwrap_or(corrosion_bootstrap_from_db(
                &network_dir,
                &self.identity.machine_id,
            )?);

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
                let api_port = corrosion_config::DEFAULT_API_PORT;
                let overlay_api = SocketAddr::new(IpAddr::V6(net_config.overlay_ip.0), api_port);
                let local_api = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), api_port);

                let wg = DockerWireGuard::new(
                    "ployz-wireguard",
                    &network_dir,
                    self.identity.private_key.clone(),
                    net_config.overlay_ip,
                )
                .with_bridge_forward(local_api, overlay_api)
                .build()
                .await
                .map_err(|e| format!("docker wireguard: {e}"))?;
                let network = WireguardDriver::Docker(Arc::new(wg));

                let transport = Transport::Bridge { local_addr: local_api };
                let store = self
                    .build_corrosion_store_docker(
                        &network_dir,
                        net_config.overlay_ip,
                        transport,
                        &corrosion_bootstrap,
                        &net_config.id.0,
                    )
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

                let transport = Transport::Direct;
                let store = self.build_corrosion_store_host(
                    &network_dir,
                    net_config.overlay_ip,
                    transport,
                    &corrosion_bootstrap,
                    &net_config.id.0,
                )?;
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

        let mut seed_records = Vec::new();

        if let Some(ref bs) = bootstrap {
            seed_records.push(MachineRecord {
                id: MachineId(bs.peer_id.clone()),
                public_key: PublicKey(bs.peer_wg_public_key),
                overlay_ip: OverlayIp(bs.peer_overlay_ip),
                subnet: None,
                bridge_ip: None,
                endpoints: bs.peer_endpoints.clone(),
            });
        }

        let endpoints = detect_endpoints(51820).await;
        seed_records.push(MachineRecord {
            id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: net_config.overlay_ip,
            subnet: Some(net_config.subnet),
            bridge_ip: None,
            endpoints,
        });

        let mut mesh = Mesh::new(
            network,
            store,
            container_network,
            self.identity.machine_id.clone(),
            51820,
        )
        .with_seed_records(seed_records)
        .with_disconnected_bootstrap_allowed(options.allow_disconnected_bootstrap);

        mesh.up()
            .await
            .map_err(|e| format!("failed to start network: {e}"))?;

        let network_name = net_config.name.0.clone();
        self.active = Some(ActiveMesh {
            config: net_config,
            mesh,
        });
        self.write_active_marker(&network_name);
        Ok(())
    }

    /// Build Corrosion store for Docker mode.
    /// Corrosion config uses overlay_ip (container-side), client uses transport for actual connection.
    async fn build_corrosion_store_docker(
        &self,
        network_dir: &std::path::Path,
        overlay_ip: crate::model::OverlayIp,
        transport: Transport,
        bootstrap: &[String],
        network_id: &str,
    ) -> Result<StoreDriver, String> {
        let paths = corrosion_config::Paths::new(network_dir);
        let gossip_addr =
            SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_GOSSIP_PORT);
        let api_addr =
            SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

        corrosion_config::write_config(
            &paths,
            SCHEMA_SQL,
            gossip_addr,
            api_addr,
            bootstrap,
            Some(network_id),
        )
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

        let corrosion = CorrosionStore::new(api_addr, transport.clone());

        tracing::info!(endpoint = %api_addr, ?transport, "store backend: corrosion (docker)");

        Ok(StoreDriver::Corrosion {
            store: corrosion,
            service: Arc::new(service),
        })
    }

    /// Build Corrosion store for Host modes using the native corrosion binary.
    fn build_corrosion_store_host(
        &self,
        network_dir: &std::path::Path,
        overlay_ip: crate::model::OverlayIp,
        transport: Transport,
        bootstrap: &[String],
        network_id: &str,
    ) -> Result<StoreDriver, String> {
        let paths = corrosion_config::Paths::new(network_dir);
        let gossip_addr =
            SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_GOSSIP_PORT);
        let api_addr =
            SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

        corrosion_config::write_config(
            &paths,
            SCHEMA_SQL,
            gossip_addr,
            api_addr,
            bootstrap,
            Some(network_id),
        )
        .map_err(|e| format!("write corrosion config: {e}"))?;

        let binary = which_corrosion().map_err(|e| format!("corrosion binary: {e}"))?;
        let service = HostCorrosion::new(binary, &paths.config);

        let corrosion = CorrosionStore::new(api_addr, transport.clone());

        tracing::info!(endpoint = %api_addr, ?transport, "store backend: corrosion (host)");

        Ok(StoreDriver::CorrosionHost {
            store: corrosion,
            service: Arc::new(service),
        })
    }
}
