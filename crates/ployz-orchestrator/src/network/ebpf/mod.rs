mod container;
#[cfg(all(target_os = "linux", feature = "ebpf-native"))]
mod native;

use crate::error::Result;
use ipnet::Ipv4Net;

pub enum EbpfDataplane {
    #[cfg(feature = "ebpf-native")]
    Native(native::NativeDataplane),
    Container(container::ContainerDataplane),
}

impl EbpfDataplane {
    #[cfg(feature = "ebpf-native")]
    pub fn attach_native(bridge_ifname: &str) -> Result<Self> {
        Ok(Self::Native(native::NativeDataplane::attach(
            bridge_ifname,
        )?))
    }

    pub async fn attach_container(
        wg_container_name: &str,
        bridge_ifname: &str,
        wg_ifname: &str,
    ) -> Result<Self> {
        Ok(Self::Container(
            container::ContainerDataplane::attach(wg_container_name, bridge_ifname, wg_ifname)
                .await?,
        ))
    }

    pub async fn set_observe(&self, enabled: bool) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(dataplane) => dataplane.set_observe(enabled),
            Self::Container(dataplane) => dataplane.set_observe(enabled).await,
        }
    }

    pub async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(dataplane) => dataplane.upsert_route(subnet, ifindex),
            Self::Container(dataplane) => dataplane.upsert_route(subnet, ifindex).await,
        }
    }

    pub async fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(dataplane) => dataplane.remove_route(subnet),
            Self::Container(dataplane) => dataplane.remove_route(subnet).await,
        }
    }

    pub async fn detach(self) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(dataplane) => {
                dataplane.detach();
                Ok(())
            }
            Self::Container(dataplane) => dataplane.detach().await,
        }
    }

    pub async fn detach_ref(&self) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(dataplane) => {
                dataplane.detach_ref();
                Ok(())
            }
            Self::Container(dataplane) => dataplane.detach_ref().await,
        }
    }
}
