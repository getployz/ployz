pub mod container_network;
pub mod driver;
pub mod orchestrator;
pub(crate) mod peer_state;
pub mod phase;
pub mod tasks;
pub mod wireguard;

use crate::error::Result;
use async_trait::async_trait;
use ipnet::Ipv4Net;

pub use ployz_runtime_api::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};

#[async_trait]
pub trait MeshDataplane: Send + Sync {
    async fn set_observe(&self, enabled: bool) -> Result<()>;
    async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()>;
    async fn remove_route(&self, subnet: Ipv4Net) -> Result<()>;
    async fn detach(&self) -> Result<()>;
}

#[async_trait]
impl MeshDataplane for crate::network::ebpf::EbpfDataplane {
    async fn set_observe(&self, enabled: bool) -> Result<()> {
        self.set_observe(enabled).await
    }

    async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
        self.upsert_route(subnet, ifindex).await
    }

    async fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
        self.remove_route(subnet).await
    }

    async fn detach(&self) -> Result<()> {
        self.detach_ref().await
    }
}
