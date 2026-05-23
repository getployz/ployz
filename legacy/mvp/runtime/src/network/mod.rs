#[cfg(feature = "docker")]
mod docker_bridge;

use mvp_mesh::{ContainerIp, ContainerSubnet};

use crate::RuntimeResult;

#[cfg(feature = "docker")]
pub use docker_bridge::{DockerBridgeNetwork, DockerBridgeNetworkConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerNetworkState {
    pub name: String,
    pub subnet: ContainerSubnet,
    pub gateway_ip: ContainerIp,
    pub first_service_ip: ContainerIp,
    pub bridge_ifname: Option<String>,
}

impl ContainerNetworkState {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        subnet: ContainerSubnet,
        bridge_ifname: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            subnet,
            gateway_ip: subnet.docker_gateway_ip(),
            first_service_ip: subnet.first_service_ip(),
            bridge_ifname,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerNetworkAttachment {
    pub network_name: String,
    pub container: String,
    pub ipv4: Option<ContainerIp>,
}

pub trait ContainerNetworkBackend: Send + Sync {
    fn ensure(&self) -> RuntimeResult<ContainerNetworkState>;
    fn connect(
        &self,
        container: &str,
        ipv4: Option<ContainerIp>,
    ) -> RuntimeResult<ContainerNetworkAttachment>;
    fn disconnect(&self, container: &str, force: bool) -> RuntimeResult<()>;
    fn remove(&self) -> RuntimeResult<()>;
    fn resolve_bridge_ifname(&self) -> RuntimeResult<String>;
    fn state(&self) -> RuntimeResult<ContainerNetworkState>;
}

#[must_use]
pub fn stable_network_name(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use mvp_mesh::ContainerSubnet;

    use super::{ContainerNetworkState, stable_network_name};

    #[test]
    fn network_names_are_docker_safe() {
        assert_eq!(stable_network_name("ployz mvp/node-a"), "ployz-mvp-node-a");
    }

    #[test]
    fn state_exposes_reserved_subnet_addresses() {
        let subnet = ContainerSubnet::new("10.210.42.0/24".parse().expect("valid subnet"))
            .expect("container subnet");
        let state = ContainerNetworkState::new("ployz-mvp-node-a", subnet, Some("br-abcd".into()));

        assert_eq!(state.gateway_ip.to_string(), "10.210.42.1");
        assert_eq!(state.first_service_ip.to_string(), "10.210.42.2");
        assert_eq!(state.bridge_ifname.as_deref(), Some("br-abcd"));
    }
}
