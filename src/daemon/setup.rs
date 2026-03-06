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
use crate::workload::manager::DockerWorkloadManager;
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

/// Read machine records directly from corrosion's sqlite DB (bypassing the API).
/// Used to pre-configure WG peers before corrosion starts, so the tunnel is ready
/// when corrosion's gossip immediately tries to reach bootstrap peers.
fn machines_from_db(network_dir: &Path) -> Result<Vec<MachineRecord>, String> {
    let db_path = corrosion_config::Paths::new(network_dir).db;
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = rusqlite::Connection::open(&db_path)
        .map_err(|e| format!("open corrosion db '{}': {e}", db_path.display()))?;
    let mut stmt = match conn.prepare(
        "SELECT id, public_key, overlay_ip, subnet, bridge_ip, endpoints FROM machines ORDER BY id",
    ) {
        Ok(stmt) => stmt,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table: machines") =>
        {
            return Ok(Vec::new());
        }
        Err(e) => {
            return Err(format!(
                "prepare machines_from_db query '{}': {e}",
                db_path.display()
            ));
        }
    };

    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get("id")?;
            let public_key: Vec<u8> = row.get("public_key")?;
            let overlay_ip: String = row.get("overlay_ip")?;
            let subnet: String = row.get("subnet")?;
            let bridge_ip: String = row.get("bridge_ip")?;
            let endpoints: String = row.get("endpoints")?;
            Ok((id, public_key, overlay_ip, subnet, bridge_ip, endpoints))
        })
        .map_err(|e| format!("query machines_from_db '{}': {e}", db_path.display()))?;

    let mut records = Vec::new();
    for row in rows {
        let (id, public_key, overlay_ip, subnet, bridge_ip, endpoints) = row
            .map_err(|e| format!("read machine row from '{}': {e}", db_path.display()))?;

        if overlay_ip.is_empty() {
            continue;
        }

        let key: [u8; 32] = match public_key.try_into() {
            Ok(k) => k,
            Err(_) => continue,
        };
        let overlay: std::net::Ipv6Addr = match overlay_ip.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };
        let subnet_parsed: Option<ipnet::Ipv4Net> =
            if subnet.is_empty() { None } else { subnet.parse().ok() };
        let bridge_parsed: Option<OverlayIp> = if bridge_ip.is_empty() {
            None
        } else {
            bridge_ip.parse::<std::net::Ipv6Addr>().ok().map(OverlayIp)
        };
        let endpoints_parsed: Vec<String> =
            serde_json::from_str(&endpoints).unwrap_or_default();

        records.push(MachineRecord {
            id: MachineId(id),
            public_key: PublicKey(key),
            overlay_ip: OverlayIp(overlay),
            subnet: subnet_parsed,
            bridge_ip: bridge_parsed,
            endpoints: endpoints_parsed,
        });
    }

    Ok(records)
}

fn corrosion_bootstrap_from_db(
    network_dir: &Path,
    local_machine_id: &MachineId,
) -> Result<Vec<String>, String> {
    let records = machines_from_db(network_dir)?;
    Ok(records
        .into_iter()
        .filter(|m| m.id.0 != local_machine_id.0)
        .map(|m| format!("[{}]:{}", m.overlay_ip.0, corrosion_config::DEFAULT_GOSSIP_PORT))
        .collect())
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

        // Save backbone reference for workload manager before mesh takes ownership
        let backbone_ref = match &network {
            WireguardDriver::Docker(backbone) => Some(backbone.clone()),
            _ => None,
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

        // Pre-load machine records from corrosion's sqlite DB so WG peers are
        // configured before corrosion starts. Without this, corrosion's gossip
        // tries to reach bootstrap peers over overlay IPs that WG doesn't know
        // about yet, causing a 503 storm until set_peers runs post-store-init.
        let mut seed_records = machines_from_db(&network_dir).unwrap_or_else(|e| {
            tracing::warn!(?e, "failed to pre-load machines from db, starting fresh");
            Vec::new()
        });

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

        let listen_port = crate::adapters::wireguard::DEFAULT_LISTEN_PORT;
        let endpoints = detect_endpoints(listen_port).await;
        let self_record = MachineRecord {
            id: self.identity.machine_id.clone(),
            public_key: self.identity.public_key.clone(),
            overlay_ip: net_config.overlay_ip,
            subnet: Some(net_config.subnet),
            bridge_ip: None,
            endpoints,
        };
        if let Some(existing) = seed_records
            .iter_mut()
            .find(|m| m.id == self_record.id)
        {
            *existing = self_record;
        } else {
            seed_records.push(self_record);
        }

        let mut mesh = Mesh::new(
            network,
            store,
            container_network,
            self.identity.machine_id.clone(),
            listen_port,
        )
        .with_seed_records(seed_records)
        .with_disconnected_bootstrap_allowed(options.allow_disconnected_bootstrap);

        mesh.up()
            .await
            .map_err(|e| format!("failed to start network: {e}"))?;

        // Build workload manager for Docker mode (after mesh is up so bridge + backbone are ready)
        let workload_manager = match backbone_ref {
            Some(backbone) => {
                let bridge = Arc::new(
                    crate::adapters::docker_network::DockerBridgeNetwork::new(
                        &net_config.name.0,
                        net_config.subnet,
                    )
                    .await
                    .map_err(|e| format!("workload bridge: {e}"))?,
                );
                Some(
                    DockerWorkloadManager::new(
                        self.identity.machine_id.clone(),
                        net_config.subnet,
                        self.cluster_cidr.clone(),
                        backbone,
                        bridge,
                    )
                    .map_err(|e| format!("workload manager: {e}"))?,
                )
            }
            None => None,
        };

        let network_name = net_config.name.0.clone();
        self.active = Some(ActiveMesh {
            config: net_config,
            mesh,
            workload_manager,
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
