use bollard::Docker;
use bollard::models::{
    EndpointIpamConfig, EndpointSettings, Ipam, IpamConfig, NetworkConnectRequest,
    NetworkCreateRequest,
};
use ipnet::Ipv4Net;
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::process::Command;
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::network::ipam::{container_ip, machine_ip};

#[cfg(target_os = "linux")]
fn iptables_idempotent(args: &[&str]) {
    // Check if rule exists by replacing -I/-A with -C
    let mut check_args: Vec<&str> = args.to_vec();
    for arg in &mut check_args {
        if *arg == "-I" || *arg == "-A" {
            *arg = "-C";
            break;
        }
    }
    let check = Command::new("iptables").args(&check_args).output();
    if check.map(|o| o.status.success()).unwrap_or(false) {
        return; // rule already exists
    }
    let _ = Command::new("iptables").args(args).output();
}

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

    /// Remove the network and clean up iptables rules, ignoring 404 (already removed).
    pub async fn remove(&self, wg_ifname: Option<&str>) -> Result<()> {
        if let Some(wg) = wg_ifname {
            self.remove_forwarding_rules(wg).await;
        }
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

    #[cfg(target_os = "linux")]
    async fn remove_forwarding_rules(&self, wg_ifname: &str) {
        let subnet = self.subnet_v4.to_string();
        if let Ok(bridge_ifname) = self.resolve_bridge_ifname().await {
            let _ = Command::new("iptables")
                .args(["-t", "raw", "-D", "PREROUTING", "-i", wg_ifname, "-d", &subnet, "-j", "ACCEPT"])
                .output();
            let _ = Command::new("iptables")
                .args(["-D", "DOCKER-USER", "-i", wg_ifname, "-o", &bridge_ifname, "-j", "ACCEPT"])
                .output();
            let _ = Command::new("iptables")
                .args(["-D", "DOCKER-USER", "-i", &bridge_ifname, "-o", wg_ifname, "-j", "ACCEPT"])
                .output();
            let _ = Command::new("iptables")
                .args(["-t", "nat", "-D", "POSTROUTING", "-s", &subnet, "-o", wg_ifname, "-j", "RETURN"])
                .output();
            info!(wg = wg_ifname, bridge = %bridge_ifname, "removed iptables forwarding rules");
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn remove_forwarding_rules(&self, _wg_ifname: &str) {}

    pub fn gateway_v4(&self) -> Ipv4Addr {
        self.gateway_v4
    }

    /// The IPv4 address for the WG container on this bridge (.2).
    /// Distinct from gateway (.1) which Docker assigns to the bridge interface.
    pub fn container_v4(&self) -> Ipv4Addr {
        self.container_v4
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Allow forwarding between a WG interface and this bridge network.
    /// Docker's default iptables rules block traffic from non-bridge interfaces
    /// to bridge containers (raw PREROUTING DROP + FORWARD policy DROP).
    /// This adds the necessary ACCEPT rules for overlay traffic.
    #[cfg(target_os = "linux")]
    pub async fn allow_forwarding_from(&self, wg_ifname: &str) -> Result<()> {
        let bridge_ifname = self.resolve_bridge_ifname().await?;
        let subnet = self.subnet_v4.to_string();

        // raw PREROUTING: accept WG traffic to bridge subnet before Docker's per-container DROP
        iptables_idempotent(&[
            "-t", "raw", "-I", "PREROUTING",
            "-i", wg_ifname, "-d", &subnet, "-j", "ACCEPT",
        ]);

        // DOCKER-USER: allow forwarding in both directions
        iptables_idempotent(&[
            "-I", "DOCKER-USER",
            "-i", wg_ifname, "-o", &bridge_ifname, "-j", "ACCEPT",
        ]);
        iptables_idempotent(&[
            "-I", "DOCKER-USER",
            "-i", &bridge_ifname, "-o", wg_ifname, "-j", "ACCEPT",
        ]);

        // NAT: skip Docker's MASQUERADE for bridge→WG traffic so containers keep
        // their bridge source IP when reaching remote overlay subnets.
        // Must be inserted before Docker's "-s <subnet> ! -o <bridge> -j MASQUERADE".
        iptables_idempotent(&[
            "-t", "nat", "-I", "POSTROUTING",
            "-s", &subnet, "-o", wg_ifname, "-j", "RETURN",
        ]);

        info!(
            wg = wg_ifname, bridge = %bridge_ifname,
            "allowed iptables forwarding between WG and bridge"
        );
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn allow_forwarding_from(&self, _wg_ifname: &str) -> Result<()> {
        Ok(())
    }

    /// Resolve the Linux bridge interface name (br-xxxx) from the Docker network ID.
    #[cfg(target_os = "linux")]
    async fn resolve_bridge_ifname(&self) -> Result<String> {
        let info = self.docker
            .inspect_network(&self.name, None)
            .await
            .map_err(|e| Error::operation("inspect network", e.to_string()))?;
        let id = info.id
            .ok_or_else(|| Error::operation("resolve bridge", "network has no ID"))?;
        Ok(format!("br-{}", &id[..12]))
    }
}
