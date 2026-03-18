pub mod docker_bridge;

use async_trait::async_trait;
use docker_bridge::DockerBridgeNetwork;
use ipnet::Ipv4Net;
use ployz_runtime_api::{ContainerNetwork, ContainerNetworkBackend};
use ployz_types::Result;
use std::net::Ipv4Addr;
use std::sync::Arc;

pub async fn docker_bridge_network(
    mesh_name: &str,
    subnet_v4: Ipv4Net,
) -> Result<ContainerNetwork> {
    let network = DockerBridgeNetwork::new(mesh_name, subnet_v4).await?;
    Ok(ContainerNetwork::from_backend(Arc::new(
        DockerBridgeBackend { inner: network },
    )))
}

struct DockerBridgeBackend {
    inner: DockerBridgeNetwork,
}

#[async_trait]
impl ContainerNetworkBackend for DockerBridgeBackend {
    async fn ensure(&self) -> Result<()> {
        self.inner.ensure().await
    }

    async fn connect(&self, container: &str, ipv4: Option<Ipv4Addr>) -> Result<()> {
        self.inner.connect(container, ipv4).await
    }

    async fn disconnect(&self, container: &str, force: bool) -> Result<()> {
        self.inner.disconnect(container, force).await
    }

    async fn remove(&self) -> Result<()> {
        self.inner.remove().await
    }

    async fn resolve_bridge_ifname(&self) -> Result<String> {
        self.inner.resolve_bridge_ifname().await
    }

    fn container_v4(&self) -> Ipv4Addr {
        self.inner.container_v4()
    }
}
