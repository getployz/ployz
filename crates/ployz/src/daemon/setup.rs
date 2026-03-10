use std::net::{IpAddr, SocketAddr};

use crate::config::Mode;
use crate::deploy::remote::start_remote_control_listener;
use crate::mesh::driver::WireguardDriver;
use crate::mesh::orchestrator::Mesh;
use crate::services::dns::{DnsConfig, start_managed_dns};
use crate::services::gateway::{GatewayConfig, start_managed_gateway};
use crate::store::bootstrap::{BootstrapInfo, build_seed_records, resolve_bootstrap_addrs};
use crate::store::driver::StoreDriver;
use crate::store::network::NetworkConfig;

use super::{ActiveMesh, DaemonState};

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

        let gateway_port = self
            .gateway_listen_addr
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .unwrap_or(80);
        let exposed_tcp_ports = [gateway_port];

        let network = WireguardDriver::from_mode(
            self.mode,
            &self.identity,
            net_config.overlay_ip,
            &network_dir,
            &net_config.name.0,
            net_config.subnet,
            &exposed_tcp_ports,
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
                crate::network::docker_bridge::DockerBridgeNetwork::new(
                    &net_config.name.0,
                    net_config.subnet,
                )
                .await
                .map_err(|e| format!("docker bridge network: {e}"))?,
            ),
        };

        let listen_port = crate::mesh::wireguard::DEFAULT_LISTEN_PORT;
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
            Mode::Docker => {
                // In Docker mode, the overlay IP lives inside the WG container.
                // The daemon binds on localhost; the bridge relays overlay traffic.
                let bind_addr = SocketAddr::from(([127, 0, 0, 1], self.remote_control_port));
                match start_remote_control_listener(
                    bind_addr,
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
            Mode::HostExec | Mode::HostService => {
                let bind_addr =
                    SocketAddr::new(IpAddr::V6(net_config.overlay_ip.0), self.remote_control_port);
                match start_remote_control_listener(
                    bind_addr,
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
                    crate::services::dns::DnsHandle::noop()
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
