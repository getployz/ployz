use std::future::Future;
use std::sync::Arc;
use std::thread;

use bollard::Docker;
use bollard::models::{
    EndpointIpamConfig, EndpointSettings, Ipam, IpamConfig, NetworkConnectRequest,
    NetworkCreateRequest, NetworkDisconnectRequest,
};
use mvp_mesh::{ContainerIp, ContainerSubnet};

use crate::{
    ContainerNetworkAttachment, ContainerNetworkBackend, ContainerNetworkState, RuntimeError,
    RuntimeResult, stable_network_name,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerBridgeNetworkConfig {
    pub name: String,
    pub subnet: ContainerSubnet,
    pub mtu: u16,
}

impl DockerBridgeNetworkConfig {
    #[must_use]
    pub fn new(name: impl AsRef<str>, subnet: ContainerSubnet) -> Self {
        Self {
            name: stable_network_name(name),
            subnet,
            mtu: 1420,
        }
    }

    #[must_use]
    pub fn with_mtu(mut self, mtu: u16) -> Self {
        self.mtu = mtu;
        self
    }
}

#[derive(Clone)]
pub struct DockerBridgeNetwork {
    config: DockerBridgeNetworkConfig,
    docker: Docker,
    executor: Arc<tokio::runtime::Runtime>,
}

impl DockerBridgeNetwork {
    pub fn connect(config: DockerBridgeNetworkConfig) -> RuntimeResult<Self> {
        let docker = Docker::connect_with_socket_defaults().map_err(|source| {
            RuntimeError::DockerOperation {
                operation: "docker connect",
                message: source.to_string(),
            }
        })?;
        let executor = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|source| RuntimeError::DockerOperation {
                    operation: "docker network runtime",
                    message: source.to_string(),
                })?,
        );
        let network = Self {
            config,
            docker,
            executor,
        };
        network.run(async {
            network
                .docker
                .ping()
                .await
                .map_err(|source| RuntimeError::DockerOperation {
                    operation: "docker ping",
                    message: source.to_string(),
                })
        })?;
        Ok(network)
    }

    fn run<F, T>(&self, future: F) -> RuntimeResult<T>
    where
        F: Future<Output = RuntimeResult<T>> + Send,
        T: Send,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            thread::scope(|scope| {
                scope
                    .spawn(|| self.executor.block_on(future))
                    .join()
                    .expect("docker bridge worker thread panicked")
            })
        } else {
            self.executor.block_on(future)
        }
    }

    async fn ensure_network(&self) -> RuntimeResult<ContainerNetworkState> {
        match self.docker.inspect_network(&self.config.name, None).await {
            Ok(_) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => self.create_network().await?,
            Err(source) => {
                return Err(RuntimeError::DockerOperation {
                    operation: "docker inspect network",
                    message: source.to_string(),
                });
            }
        }
        self.inspect_state().await
    }

    async fn create_network(&self) -> RuntimeResult<()> {
        let ipam = Ipam {
            driver: Some("default".to_string()),
            config: Some(vec![IpamConfig {
                subnet: Some(self.config.subnet.docker_ipam_subnet()),
                gateway: Some(self.config.subnet.docker_gateway_ip().to_string()),
                ..Default::default()
            }]),
            options: None,
        };
        let options = [(
            "com.docker.network.driver.mtu".to_string(),
            self.config.mtu.to_string(),
        )]
        .into_iter()
        .collect();
        let request = NetworkCreateRequest {
            name: self.config.name.clone(),
            driver: Some("bridge".to_string()),
            ipam: Some(ipam),
            options: Some(options),
            ..Default::default()
        };
        self.docker
            .create_network(request)
            .await
            .map_err(|source| RuntimeError::DockerOperation {
                operation: "docker create network",
                message: source.to_string(),
            })?;
        Ok(())
    }

    async fn inspect_state(&self) -> RuntimeResult<ContainerNetworkState> {
        let bridge_ifname = self.resolve_bridge_ifname_async().await.ok();
        Ok(ContainerNetworkState::new(
            self.config.name.clone(),
            self.config.subnet,
            bridge_ifname,
        ))
    }

    async fn connect_container(
        &self,
        container: &str,
        ipv4: Option<ContainerIp>,
    ) -> RuntimeResult<ContainerNetworkAttachment> {
        self.ensure_network().await?;
        if let Some(existing) = self.inspect_attachment(container).await? {
            if ipv4.is_none() || existing.ipv4 == ipv4 {
                return Ok(existing);
            }
            return Ok(existing);
        }
        let endpoint_config = EndpointSettings {
            ipam_config: ipv4.map(|ip| EndpointIpamConfig {
                ipv4_address: Some(ip.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let request = NetworkConnectRequest {
            container: container.to_string(),
            endpoint_config: Some(endpoint_config),
        };
        match self
            .docker
            .connect_network(&self.config.name, request)
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 403,
                message,
            }) if message.contains("already exists in network") => {}
            Err(source) => {
                return Err(RuntimeError::DockerOperation {
                    operation: "docker connect network",
                    message: source.to_string(),
                });
            }
        }
        Ok(self
            .inspect_attachment(container)
            .await?
            .unwrap_or_else(|| ContainerNetworkAttachment {
                network_name: self.config.name.clone(),
                container: container.to_string(),
                ipv4,
            }))
    }

    async fn inspect_attachment(
        &self,
        container: &str,
    ) -> RuntimeResult<Option<ContainerNetworkAttachment>> {
        let info = match self.docker.inspect_container(container, None).await {
            Ok(info) => info,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(None),
            Err(source) => {
                return Err(RuntimeError::DockerOperation {
                    operation: "docker inspect container network",
                    message: source.to_string(),
                });
            }
        };
        let Some(networks) = info.network_settings.and_then(|settings| settings.networks) else {
            return Ok(None);
        };
        let Some(endpoint) = networks.get(&self.config.name) else {
            return Ok(None);
        };
        let ipv4 = endpoint
            .ip_address
            .as_deref()
            .filter(|ip| !ip.is_empty())
            .and_then(|ip| ip.parse().ok())
            .map(ContainerIp::new);
        Ok(Some(ContainerNetworkAttachment {
            network_name: self.config.name.clone(),
            container: container.to_string(),
            ipv4,
        }))
    }

    async fn disconnect_container(&self, container: &str, force: bool) -> RuntimeResult<()> {
        let request = NetworkDisconnectRequest {
            container: container.to_string(),
            force: Some(force),
        };
        match self
            .docker
            .disconnect_network(&self.config.name, request)
            .await
        {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(source) => Err(RuntimeError::DockerOperation {
                operation: "docker disconnect network",
                message: source.to_string(),
            }),
        }
    }

    async fn remove_network(&self) -> RuntimeResult<()> {
        match self.docker.remove_network(&self.config.name).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(source) => Err(RuntimeError::DockerOperation {
                operation: "docker remove network",
                message: source.to_string(),
            }),
        }
    }

    async fn resolve_bridge_ifname_async(&self) -> RuntimeResult<String> {
        let info = self
            .docker
            .inspect_network(&self.config.name, None)
            .await
            .map_err(|source| RuntimeError::DockerOperation {
                operation: "docker inspect network",
                message: source.to_string(),
            })?;
        let id = info.id.ok_or_else(|| RuntimeError::DockerOperation {
            operation: "docker resolve bridge",
            message: "network has no id".to_string(),
        })?;
        Ok(bridge_ifname_from_network_id(&id))
    }
}

impl ContainerNetworkBackend for DockerBridgeNetwork {
    fn ensure(&self) -> RuntimeResult<ContainerNetworkState> {
        self.run(self.ensure_network())
    }

    fn connect(
        &self,
        container: &str,
        ipv4: Option<ContainerIp>,
    ) -> RuntimeResult<ContainerNetworkAttachment> {
        self.run(self.connect_container(container, ipv4))
    }

    fn disconnect(&self, container: &str, force: bool) -> RuntimeResult<()> {
        self.run(self.disconnect_container(container, force))
    }

    fn remove(&self) -> RuntimeResult<()> {
        self.run(self.remove_network())
    }

    fn resolve_bridge_ifname(&self) -> RuntimeResult<String> {
        self.run(self.resolve_bridge_ifname_async())
    }

    fn state(&self) -> RuntimeResult<ContainerNetworkState> {
        self.run(self.inspect_state())
    }
}

fn bridge_ifname_from_network_id(id: &str) -> String {
    let prefix_len = id.len().min(12);
    format!("br-{}", &id[..prefix_len])
}

#[cfg(test)]
mod tests {
    use mvp_mesh::ContainerSubnet;

    use super::{DockerBridgeNetworkConfig, bridge_ifname_from_network_id};

    #[test]
    fn bridge_network_config_sanitizes_name_and_preserves_ipam_inputs() {
        let subnet = ContainerSubnet::new("10.210.8.0/24".parse().expect("valid subnet"))
            .expect("container subnet");
        let config = DockerBridgeNetworkConfig::new("ployz mvp/node-a", subnet).with_mtu(1300);

        assert_eq!(config.name, "ployz-mvp-node-a");
        assert_eq!(config.subnet.docker_ipam_subnet(), "10.210.8.0/24");
        assert_eq!(config.subnet.docker_gateway_ip().to_string(), "10.210.8.1");
        assert_eq!(config.mtu, 1300);
    }

    #[test]
    fn bridge_interface_name_uses_docker_network_id_prefix() {
        assert_eq!(
            bridge_ifname_from_network_id("abcdef0123456789"),
            "br-abcdef012345"
        );
        assert_eq!(bridge_ifname_from_network_id("abc"), "br-abc");
    }
}
