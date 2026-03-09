use std::net::Ipv6Addr;
use std::path::Path;

use crate::config::Mode;
use crate::corrosion_config;
use crate::deploy::remote::start_remote_control_listener;
use crate::dns::{DnsConfig, start_managed_dns};
use crate::drivers::{StoreDriver, WireguardDriver};
use crate::gateway::{GatewayConfig, start_managed_gateway};
use crate::mesh::orchestrator::Mesh;
use crate::model::{MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey};
use crate::network::endpoints::detect_endpoints;
use crate::store::network::NetworkConfig;

use super::{ActiveMesh, DaemonState};

/// Read peer config from corrosion's sqlite DB (bypassing the API).
/// Only fetches the columns needed for WG peer setup — avoids breaking on
/// schema migrations (new columns won't exist until corrosion starts).
fn peer_records_from_db(network_dir: &Path) -> Result<Vec<MachineRecord>, String> {
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
                "prepare peer_records_from_db query '{}': {e}",
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
        .map_err(|e| format!("query peer_records_from_db '{}': {e}", db_path.display()))?;

    let mut records = Vec::new();
    for row in rows {
        let (id, public_key, overlay_ip, subnet, bridge_ip, endpoints) =
            row.map_err(|e| format!("read machine row from '{}': {e}", db_path.display()))?;

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
        let subnet_parsed: Option<ipnet::Ipv4Net> = if subnet.is_empty() {
            None
        } else {
            subnet.parse().ok()
        };
        let bridge_parsed: Option<OverlayIp> = if bridge_ip.is_empty() {
            None
        } else {
            bridge_ip.parse::<std::net::Ipv6Addr>().ok().map(OverlayIp)
        };
        let endpoints_parsed: Vec<String> = serde_json::from_str(&endpoints).unwrap_or_default();

        records.push(MachineRecord {
            id: MachineId(id),
            public_key: PublicKey(key),
            overlay_ip: OverlayIp(overlay),
            subnet: subnet_parsed,
            bridge_ip: bridge_parsed,
            endpoints: endpoints_parsed,
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
        });
    }

    Ok(records)
}

fn corrosion_bootstrap_from_db(
    network_dir: &Path,
    local_machine_id: &MachineId,
) -> Result<Vec<String>, String> {
    let records = peer_records_from_db(network_dir)?;
    Ok(records
        .into_iter()
        .filter(|m| m.id.0 != local_machine_id.0)
        .map(|m| {
            format!(
                "[{}]:{}",
                m.overlay_ip.0,
                corrosion_config::DEFAULT_GOSSIP_PORT
            )
        })
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
        let bootstrap_addrs =
            resolve_bootstrap_addrs(&network_dir, &self.identity.machine_id, &bootstrap)?;

        let network = WireguardDriver::from_mode(
            self.mode,
            &self.identity,
            net_config.overlay_ip,
            &network_dir,
            &net_config.name.0,
            net_config.subnet,
        )
        .await?;

        let store = StoreDriver::from_mode(
            self.mode,
            net_config.overlay_ip,
            &network_dir,
            &bootstrap_addrs,
            &net_config.id.0,
        )
        .await?;

        tracing::info!(mode = ?self.mode, "starting mesh");

        let container_network = match self.mode {
            Mode::Memory => None,
            Mode::Docker | Mode::HostExec | Mode::HostService => Some(
                crate::adapters::docker_network::DockerBridgeNetwork::new(
                    &net_config.name.0,
                    net_config.subnet,
                )
                .await
                .map_err(|e| format!("docker bridge network: {e}"))?,
            ),
        };

        let listen_port = crate::adapters::wireguard::DEFAULT_LISTEN_PORT;
        let seed_records = build_seed_records(
            &network_dir,
            &self.identity,
            &net_config,
            bootstrap.as_ref(),
            listen_port,
        )
        .await;

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

        let remote_control = match self.mode {
            Mode::Memory => crate::deploy::remote::RemoteControlHandle::noop(),
            Mode::Docker | Mode::HostExec | Mode::HostService => {
                match start_remote_control_listener(
                    net_config.overlay_ip,
                    self.remote_control_port,
                    mesh.store.clone(),
                    self.namespace_locks.clone(),
                    self.identity.machine_id.clone(),
                    Some(format!("ployz-{}", net_config.name.0)),
                )
                .await
                {
                    Ok(handle) => handle,
                    Err(err) => {
                        let _ = mesh.destroy().await;
                        return Err(format!("failed to start remote deploy listener: {err}"));
                    }
                }
            }
        };

        let gateway_config = GatewayConfig::for_network(
            &self.data_dir,
            &net_config.name.0,
            self.gateway_listen_addr.clone(),
            self.gateway_threads,
        );
        let gateway = match start_managed_gateway(self.mode, mesh.store.clone(), gateway_config).await
        {
            Ok(handle) => handle,
            Err(err) => {
                remote_control.shutdown().await;
                let _ = mesh.destroy().await;
                return Err(format!("failed to start gateway: {err}"));
            }
        };

        let dns_config =
            DnsConfig::for_network(&self.data_dir, &net_config.name.0, net_config.overlay_ip);
        let dns =
            match start_managed_dns(self.mode, mesh.store.clone(), dns_config).await {
                Ok(handle) => handle,
                Err(err) => {
                    tracing::warn!(?err, "failed to start dns, continuing without it");
                    crate::dns::DnsHandle::noop()
                }
            };

        let network_name = net_config.name.0.clone();
        self.active = Some(ActiveMesh {
            config: net_config,
            mesh,
            remote_control,
            gateway,
            dns,
        });
        self.write_active_marker(&network_name);
        Ok(())
    }
}

fn resolve_bootstrap_addrs(
    network_dir: &Path,
    machine_id: &MachineId,
    bootstrap: &Option<BootstrapInfo>,
) -> Result<Vec<String>, String> {
    Ok(bootstrap
        .as_ref()
        .map(|bs| {
            vec![format!(
                "[{}]:{}",
                bs.peer_overlay_ip,
                corrosion_config::DEFAULT_GOSSIP_PORT
            )]
        })
        .unwrap_or(corrosion_bootstrap_from_db(network_dir, machine_id)?))
}

async fn build_seed_records(
    network_dir: &Path,
    identity: &crate::node::identity::Identity,
    net_config: &NetworkConfig,
    bootstrap: Option<&BootstrapInfo>,
    listen_port: u16,
) -> Vec<MachineRecord> {
    let mut seed_records = peer_records_from_db(network_dir).unwrap_or_else(|e| {
        tracing::warn!(?e, "failed to pre-load machines from db, starting fresh");
        Vec::new()
    });

    if let Some(bs) = bootstrap {
        seed_records.push(MachineRecord {
            id: MachineId(bs.peer_id.clone()),
            public_key: PublicKey(bs.peer_wg_public_key),
            overlay_ip: OverlayIp(bs.peer_overlay_ip),
            subnet: None,
            bridge_ip: None,
            endpoints: bs.peer_endpoints.clone(),
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
        });
    }

    let endpoints = detect_endpoints(listen_port).await;
    let self_record = MachineRecord {
        id: identity.machine_id.clone(),
        public_key: identity.public_key.clone(),
        overlay_ip: net_config.overlay_ip,
        subnet: Some(net_config.subnet),
        bridge_ip: None,
        endpoints,
        status: MachineStatus::Unknown,
        participation: Participation::Disabled,
        last_heartbeat: 0,
        created_at: 0,
        updated_at: 0,
    };
    if let Some(existing) = seed_records.iter_mut().find(|m| m.id == self_record.id) {
        *existing = self_record;
    } else {
        seed_records.push(self_record);
    }

    seed_records
}
