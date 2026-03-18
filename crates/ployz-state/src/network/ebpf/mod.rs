mod container;
#[cfg(all(target_os = "linux", feature = "ebpf-native"))]
mod native;

use ipnet::Ipv4Net;

use crate::error::Result;

/// eBPF dataplane for WG↔Docker bridge forwarding.
///
/// Two backends:
/// - **Native** (Linux only): loads BPF directly via aya in the daemon process.
/// - **Container**: execs `ployz-bpfctl` in a privileged sidecar container.
///   Works on macOS Docker Desktop / OrbStack where TC hooks live in the VM.
pub enum EbpfDataplane {
    #[cfg(feature = "ebpf-native")]
    Native(native::NativeDataplane),
    Container(container::ContainerDataplane),
}

impl EbpfDataplane {
    /// Attach using the in-process aya loader (Linux native only).
    #[cfg(feature = "ebpf-native")]
    pub fn attach_native(bridge_ifname: &str) -> Result<Self> {
        Ok(Self::Native(native::NativeDataplane::attach(
            bridge_ifname,
        )?))
    }

    /// Attach by execing `ployz-bpfctl` inside the WG container.
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
            Self::Native(n) => n.set_observe(enabled),
            Self::Container(c) => c.set_observe(enabled).await,
        }
    }

    pub async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(n) => n.upsert_route(subnet, ifindex),
            Self::Container(c) => c.upsert_route(subnet, ifindex).await,
        }
    }

    pub async fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(n) => n.remove_route(subnet),
            Self::Container(c) => c.remove_route(subnet).await,
        }
    }

    pub async fn detach(self) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(n) => {
                n.detach();
                Ok(())
            }
            Self::Container(c) => c.detach().await,
        }
    }

    pub async fn detach_ref(&self) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(n) => {
                n.detach_ref();
                Ok(())
            }
            Self::Container(c) => c.detach_ref().await,
        }
    }
}
