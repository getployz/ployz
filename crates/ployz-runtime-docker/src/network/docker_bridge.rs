use bollard::Docker;
use bollard::models::{
    EndpointIpamConfig, EndpointSettings, Ipam, IpamConfig, NetworkConnectRequest,
    NetworkCreateRequest, NetworkDisconnectRequest,
};
use ipnet::Ipv4Net;
use ployz_runtime_api::ipam::{container_ip, machine_ip};
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::process::Command;
use tracing::{info, warn};

use crate::error::{Error, Result};

/// Manages an IPv4 Docker bridge network for container connectivity.
pub struct DockerBridgeNetwork {
    docker: Docker,
    name: String,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    overlay_ifname: String,
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
            overlay_ifname: format!("plz-{mesh_name}"),
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
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                self.create_network().await?;
            }
            Err(e) => {
                return Err(Error::operation("inspect network", e.to_string()));
            }
        }

        self.ensure_overlay_firewall_rules().await?;
        Ok(())
    }

    async fn create_network(&self) -> Result<()> {
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

    async fn ensure_overlay_firewall_rules(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let bridge_ifname = self.resolve_bridge_ifname().await?;
            install_overlay_firewall_rules(self.subnet_v4, &bridge_ifname, &self.overlay_ifname)?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = self;
        }
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

#[cfg(target_os = "linux")]
fn install_overlay_firewall_rules(
    subnet: Ipv4Net,
    bridge_ifname: &str,
    overlay_ifname: &str,
) -> Result<()> {
    let subnet = subnet.to_string();

    // Docker's bridge driver installs broad raw-table drops and subnet
    // masquerade rules. Overlay traffic must keep its machine-subnet source
    // address so WireGuard cryptokey routing accepts it on the remote node.
    ensure_iptables_rule(
        "raw",
        "PREROUTING",
        &["-i", overlay_ifname, "-d", &subnet, "-j", "ACCEPT"],
    )?;
    ensure_iptables_rule(
        "filter",
        "FORWARD",
        &[
            "-i",
            overlay_ifname,
            "-o",
            bridge_ifname,
            "-d",
            &subnet,
            "-j",
            "ACCEPT",
        ],
    )?;
    ensure_iptables_rule(
        "filter",
        "FORWARD",
        &[
            "-i",
            bridge_ifname,
            "-o",
            overlay_ifname,
            "-s",
            &subnet,
            "-j",
            "ACCEPT",
        ],
    )?;
    ensure_iptables_rule(
        "nat",
        "POSTROUTING",
        &["-s", &subnet, "-o", overlay_ifname, "-j", "ACCEPT"],
    )?;

    info!(
        %subnet,
        bridge = bridge_ifname,
        overlay = overlay_ifname,
        "installed docker overlay firewall exemptions"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_iptables_rule(table: &str, chain: &str, rule: &[&str]) -> Result<()> {
    let mut check_args = vec!["-w", "-t", table, "-C", chain];
    check_args.extend_from_slice(rule);
    if run_iptables(&check_args)?.success() {
        return Ok(());
    }

    let mut insert_args = vec!["-w", "-t", table, "-I", chain, "1"];
    insert_args.extend_from_slice(rule);
    let status = run_iptables(&insert_args)?;
    if status.success() {
        return Ok(());
    }

    Err(Error::operation(
        "iptables insert",
        format!("{table} {chain} {}", rule.join(" ")),
    ))
}

#[cfg(target_os = "linux")]
fn run_iptables(args: &[&str]) -> Result<std::process::ExitStatus> {
    Command::new("iptables")
        .args(args)
        .status()
        .map_err(|error| Error::operation("iptables", error.to_string()))
}
