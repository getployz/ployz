pub mod docker_bridge;

use async_trait::async_trait;
use docker_bridge::DockerBridgeNetwork;
use ipnet::Ipv4Net;
use ployz_runtime_api::{
    ContainerNetwork, ContainerNetworkBackend, DisconnectMode, Result as RuntimeResult,
    RuntimeError,
};
use std::net::Ipv4Addr;
use std::sync::Arc;

pub async fn docker_bridge_network(
    mesh_name: &str,
    subnet_v4: Ipv4Net,
) -> RuntimeResult<ContainerNetwork> {
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
    async fn ensure(&self) -> RuntimeResult<()> {
        self.inner.ensure().await.map_err(RuntimeError::from)
    }

    async fn connect(&self, container: &str, ipv4: Option<Ipv4Addr>) -> RuntimeResult<()> {
        self.inner
            .connect(container, ipv4)
            .await
            .map_err(RuntimeError::from)
    }

    async fn disconnect(&self, container: &str, mode: DisconnectMode) -> RuntimeResult<()> {
        self.inner
            .disconnect(container, mode)
            .await
            .map_err(RuntimeError::from)
    }

    async fn remove(&self) -> RuntimeResult<()> {
        self.inner.remove().await.map_err(RuntimeError::from)
    }

    async fn resolve_bridge_ifname(&self) -> RuntimeResult<String> {
        self.inner
            .resolve_bridge_ifname()
            .await
            .map_err(RuntimeError::from)
    }

    fn container_v4(&self) -> Ipv4Addr {
        self.inner.container_v4()
    }
}
