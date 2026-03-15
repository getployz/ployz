use bollard::Docker;
use bollard::models::{
    EndpointIpamConfig, EndpointSettings, Ipam, IpamConfig, NetworkConnectRequest,
    NetworkCreateRequest, NetworkDisconnectRequest,
};
use ipnet::Ipv4Net;
use std::net::Ipv4Addr;
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::network::ipam::{container_ip, machine_ip};

/// Manages an IPv4 Docker bridge network for container connectivity.
pub struct DockerBridgeNetwork {
    docker: Docker,
    name: String,
    subnet_v4: Ipv4Net,
    gateway_v4: Ipv4Addr,
    container_v4: Ipv4Addr,
}

impl DockerBridgeNetwork {
    pub async fn new(mesh_name: &str, subnet_v4: Ipv4Net) -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;

        let gateway_v4 = machine_ip(&subnet_v4);
        let container_v4 = container_ip(&subnet_v4);

        Ok(Self {
            docker,
            name: format!("ployz-{mesh_name}"),
            subnet_v4,
            gateway_v4,
            container_v4,
        })
    }

    /// Idempotent: create the bridge network if it doesn't exist.
    pub async fn ensure(&self) -> Result<()> {
        match self.docker.inspect_network(&self.name, None).await {
            Ok(_) => {
                info!(name = %self.name, "docker network already exists");
                return Ok(());
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => {
                return Err(Error::operation("inspect network", e.to_string()));
            }
        }

        let ipam = Ipam {
            driver: Some("default".to_string()),
            config: Some(vec![IpamConfig {
                subnet: Some(self.subnet_v4.to_string()),
                gateway: Some(self.gateway_v4.to_string()),
                ..Default::default()
            }]),
            options: None,
        };

        let options: std::collections::HashMap<String, String> =
            [("com.docker.network.driver.mtu".into(), "1420".into())]
                .into_iter()
                .collect();

        let config = NetworkCreateRequest {
            name: self.name.clone(),
            driver: Some("bridge".to_string()),
            ipam: Some(ipam),
            options: Some(options),
            ..Default::default()
        };

        self.docker
            .create_network(config)
            .await
            .map_err(|e| Error::operation("create network", e.to_string()))?;

        info!(
            name = %self.name,
            v4 = %self.subnet_v4,
            "created docker bridge network"
        );
        Ok(())
    }

    /// Connect a container to this network at a specific IPv4 address.
    pub async fn connect(&self, container: &str, ipv4: Option<Ipv4Addr>) -> Result<()> {
        match self.docker.inspect_container(container, None).await {
            Ok(details) => {
                if let Some(networks) = details.network_settings.and_then(|ns| ns.networks)
                    && let Some(endpoint) = networks.get(&self.name)
                {
                    let connected_ip = endpoint
                        .ip_address
                        .as_deref()
                        .and_then(|s| s.parse::<Ipv4Addr>().ok());

                    if ipv4.is_none() || connected_ip == ipv4 {
                        info!(
                            network = %self.name,
                            container,
                            connected_ipv4 = ?connected_ip,
                            requested_ipv4 = ?ipv4,
                            "container already connected to network"
                        );
                        return Ok(());
                    }

                    warn!(
                        network = %self.name,
                        container,
                        connected_ipv4 = ?connected_ip,
                        requested_ipv4 = ?ipv4,
                        "container already connected with different IPv4"
                    );
                    return Ok(());
                }
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => {
                return Err(Error::operation("inspect container", e.to_string()));
            }
        }

        let endpoint_config = EndpointSettings {
            ipam_config: ipv4.map(|ip| EndpointIpamConfig {
                ipv4_address: Some(ip.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let config = NetworkConnectRequest {
            container: container.to_string(),
            endpoint_config: Some(endpoint_config),
        };

        match self.docker.connect_network(&self.name, config).await {
            Ok(()) => {
                info!(
                    network = %self.name,
                    container,
                    ipv4 = ?ipv4,
                    "connected container to network"
                );
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 403,
                message,
            }) if message.contains("already exists in network") => {
                info!(
                    network = %self.name,
                    container,
                    ipv4 = ?ipv4,
                    %message,
                    "container already connected to network"
                );
                Ok(())
            }
            Err(e) => Err(Error::operation("connect network", e.to_string())),
        }
    }

    pub async fn disconnect(&self, container: &str, force: bool) -> Result<()> {
        let request = NetworkDisconnectRequest {
            container: container.to_string(),
            force: Some(force),
        };

        match self.docker.disconnect_network(&self.name, request).await {
            Ok(()) => {
                info!(network = %self.name, container, force, "disconnected container from network");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(Error::operation("disconnect network", e.to_string())),
        }
    }

    /// Remove the network, ignoring 404 (already removed).
    pub async fn remove(&self) -> Result<()> {
        match self.docker.remove_network(&self.name).await {
            Ok(_) => {
                info!(name = %self.name, "removed docker network");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(Error::operation("remove network", e.to_string())),
        }
    }

    #[must_use]
    pub fn gateway_v4(&self) -> Ipv4Addr {
        self.gateway_v4
    }

    /// The IPv4 address for the WG container on this bridge (.2).
    /// Distinct from gateway (.1) which Docker assigns to the bridge interface.
    #[must_use]
    pub fn container_v4(&self) -> Ipv4Addr {
        self.container_v4
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Resolve the Linux bridge interface name (br-xxxx) from the Docker network ID.
    /// Used as the TC attachment point for eBPF classifiers.
    pub async fn resolve_bridge_ifname(&self) -> Result<String> {
        let info = self
            .docker
            .inspect_network(&self.name, None)
            .await
            .map_err(|e| Error::operation("inspect network", e.to_string()))?;
        let id = info
            .id
            .ok_or_else(|| Error::operation("resolve bridge", "network has no ID"))?;
        Ok(format!("br-{}", &id[..12]))
    }
}
