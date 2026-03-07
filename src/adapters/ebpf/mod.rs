mod container;
#[cfg(target_os = "linux")]
mod native;

use ipnet::Ipv4Net;

use crate::error::Result;

/// eBPF dataplane for WG↔Docker bridge forwarding.
///
/// Two backends:
/// - **Native** (Linux only): loads BPF directly via aya in the daemon process.
/// - **Container**: execs `ployz-ebpf-ctl` in a privileged sidecar container.
///   Works on macOS Docker Desktop / OrbStack where TC hooks live in the VM.
pub enum EbpfDataplane {
    #[cfg(target_os = "linux")]
    Native(native::NativeDataplane),
    Container(container::ContainerDataplane),
}

impl EbpfDataplane {
    /// Attach using the in-process aya loader (Linux native only).
    #[cfg(target_os = "linux")]
    pub fn attach_native(bridge_ifname: &str, wg_ifindex: u32) -> Result<Self> {
        Ok(Self::Native(native::NativeDataplane::attach(bridge_ifname, wg_ifindex)?))
    }

    /// Attach via a sidecar container running `ployz-ebpf-ctl`.
    pub async fn attach_container(
        container_name: &str,
        image: &str,
        bridge_ifname: &str,
    ) -> Result<Self> {
        Ok(Self::Container(
            container::ContainerDataplane::attach(container_name, image, bridge_ifname).await?,
        ))
    }

    pub async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Native(n) => n.upsert_route(subnet, ifindex),
            Self::Container(c) => c.upsert_route(subnet, ifindex).await,
        }
    }

    pub async fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Native(n) => n.remove_route(subnet),
            Self::Container(c) => c.remove_route(subnet).await,
        }
    }

    pub async fn detach(self) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Native(n) => { n.detach(); Ok(()) }
            Self::Container(c) => c.detach().await,
        }
    }
}
