use crate::error::Result;
use async_trait::async_trait;
use std::net::Ipv4Addr;
use std::sync::Arc;

#[async_trait]
pub trait ContainerNetworkBackend: Send + Sync {
    async fn ensure(&self) -> Result<()>;
    async fn connect(&self, container: &str, ipv4: Option<Ipv4Addr>) -> Result<()>;
    async fn disconnect(&self, container: &str, force: bool) -> Result<()>;
    async fn remove(&self) -> Result<()>;
    async fn resolve_bridge_ifname(&self) -> Result<String>;
    fn container_v4(&self) -> Ipv4Addr;
}

#[derive(Clone)]
pub struct ContainerNetwork {
    backend: Arc<dyn ContainerNetworkBackend>,
}

impl ContainerNetwork {
    #[doc(hidden)]
    #[must_use]
    pub fn from_backend(backend: Arc<dyn ContainerNetworkBackend>) -> Self {
        Self { backend }
    }

    pub async fn ensure(&self) -> Result<()> {
        self.backend.ensure().await
    }

    pub async fn connect(&self, container: &str, ipv4: Option<Ipv4Addr>) -> Result<()> {
        self.backend.connect(container, ipv4).await
    }

    pub async fn disconnect(&self, container: &str, force: bool) -> Result<()> {
        self.backend.disconnect(container, force).await
    }

    pub async fn remove(&self) -> Result<()> {
        self.backend.remove().await
    }

    pub async fn resolve_bridge_ifname(&self) -> Result<String> {
        self.backend.resolve_bridge_ifname().await
    }

    #[must_use]
    pub fn container_v4(&self) -> Ipv4Addr {
        self.backend.container_v4()
    }
}
