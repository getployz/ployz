use crate::error::{Error, Result};
use async_trait::async_trait;
use ipnet::Ipv4Net;
use ployz_runtime_api::{
    AttachedDataplane, ContainerNetwork, DataplaneFactory, MeshDataplane, ObserveMode,
    WireguardDriver,
};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tracing::{info, warn};

const CTL_BIN: &str = "/usr/local/bin/ployz-bpfctl";

pub struct DefaultDataplaneFactory {
    container_name: String,
}

impl DefaultDataplaneFactory {
    #[must_use]
    pub fn new(container_name: impl Into<String>) -> Self {
        Self {
            container_name: container_name.into(),
        }
    }
}

#[async_trait]
impl DataplaneFactory for DefaultDataplaneFactory {
    async fn attach(
        &self,
        network: &WireguardDriver,
        container_network: &ContainerNetwork,
    ) -> Result<AttachedDataplane> {
        let bridge_ifname = container_network.resolve_bridge_ifname().await?;
        let wg_ifname = network.ebpf_attachment_ifname(&bridge_ifname);

        #[cfg(feature = "ebpf-native")]
        {
            let wg_ifindex = resolve_ifindex(&wg_ifname)?;
            let dataplane = Arc::new(EbpfDataplane::attach_native(&bridge_ifname)?);
            return Ok(AttachedDataplane {
                dataplane,
                wg_ifindex,
            });
        }

        #[cfg(not(feature = "ebpf-native"))]
        {
            let dataplane = Arc::new(
                EbpfDataplane::attach_container(
                    &self.container_name,
                    &bridge_ifname,
                    &wg_ifname,
                )
                .await?,
            );
            Ok(AttachedDataplane {
                dataplane,
                wg_ifindex: 0,
            })
        }
    }
}

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

    pub async fn set_observe(&self, mode: ObserveMode) -> Result<()> {
        match self {
            #[cfg(feature = "ebpf-native")]
            Self::Native(dataplane) => dataplane.set_observe(mode),
            Self::Container(dataplane) => dataplane.set_observe(mode).await,
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

#[async_trait]
impl MeshDataplane for EbpfDataplane {
    async fn set_observe(&self, mode: ObserveMode) -> Result<()> {
        Self::set_observe(self, mode).await
    }

    async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
        Self::upsert_route(self, subnet, ifindex).await
    }

    async fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
        Self::remove_route(self, subnet).await
    }

    async fn detach(&self) -> Result<()> {
        self.detach_ref().await
    }
}

#[cfg(feature = "ebpf-native")]
fn resolve_ifindex(ifname: &str) -> Result<u32> {
    use std::ffi::CString;

    let c_ifname = CString::new(ifname)
        .map_err(|error| Error::operation("if_nametoindex", error.to_string()))?;
    let index = unsafe { libc::if_nametoindex(c_ifname.as_ptr()) };
    if index == 0 {
        return Err(Error::operation(
            "if_nametoindex",
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(index)
}

mod container {
    use super::*;

    pub struct ContainerDataplane {
        container_name: String,
        bridge_ifname: String,
    }

    impl ContainerDataplane {
        pub async fn attach(
            wg_container_name: &str,
            bridge_ifname: &str,
            wg_ifname: &str,
        ) -> Result<Self> {
            let dataplane = Self {
                container_name: wg_container_name.to_string(),
                bridge_ifname: bridge_ifname.to_string(),
            };

            dataplane
                .exec(&[CTL_BIN, "attach", bridge_ifname, wg_ifname])
                .await?;

            info!(
                bridge = bridge_ifname,
                container = wg_container_name,
                "eBPF TC classifiers attached (via WG container)"
            );
            Ok(dataplane)
        }

        pub async fn set_observe(&self, mode: ObserveMode) -> Result<()> {
            let state = match mode {
                ObserveMode::Enabled => "on",
                ObserveMode::Disabled => "off",
            };
            self.exec(&[CTL_BIN, "observe", state]).await?;
            info!(?mode, "eBPF observation toggled (container)");
            Ok(())
        }

        pub async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
            let subnet_str = subnet.to_string();
            let ifindex_str = ifindex.to_string();
            self.exec(&[CTL_BIN, "route", "add", &subnet_str, &ifindex_str])
                .await?;
            info!(%subnet, ifindex, "eBPF route upserted (container)");
            Ok(())
        }

        pub async fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
            let subnet_str = subnet.to_string();
            match self.exec(&[CTL_BIN, "route", "del", &subnet_str]).await {
                Ok(()) => info!(%subnet, "eBPF route removed (container)"),
                Err(error) => warn!(%subnet, ?error, "eBPF route remove failed"),
            }
            Ok(())
        }

        pub async fn detach_ref(&self) -> Result<()> {
            let _ = self.exec(&[CTL_BIN, "detach", &self.bridge_ifname]).await;
            info!(bridge = %self.bridge_ifname, "eBPF TC classifiers detached (container)");
            Ok(())
        }

        async fn exec(&self, cmd: &[&str]) -> Result<()> {
            let mut full_cmd: Vec<&str> = vec![
                "exec",
                "--privileged",
                &self.container_name,
                "nsenter",
                "--net=/proc/1/ns/net",
                "--",
            ];
            full_cmd.extend_from_slice(cmd);

            let output = Command::new("docker")
                .args(&full_cmd)
                .stdin(Stdio::null())
                .output()
                .await
                .map_err(|error| Error::operation("ebpf exec", error.to_string()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let detail = if stderr.is_empty() {
                    format!("exit code {}", output.status)
                } else {
                    stderr
                };
                return Err(Error::operation("ebpf exec", detail));
            }

            Ok(())
        }
    }
}

#[cfg(all(target_os = "linux", feature = "ebpf-native"))]
mod native {
    use super::*;
    use aya::Ebpf;
    use aya::maps::HashMap;
    use aya::programs::tc::{NlOptions, TcAttachOptions};
    use aya::programs::{SchedClassifier, TcAttachType};
    use ployz_ebpf_common::{RouteEntry, RouteKey};
    use std::net::Ipv4Addr;
    use std::sync::Mutex;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PodRouteKey(RouteKey);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PodRouteEntry(RouteEntry);

    unsafe impl aya::Pod for PodRouteKey {}
    unsafe impl aya::Pod for PodRouteEntry {}

    pub struct NativeDataplane {
        bpf: Mutex<Ebpf>,
        bridge_ifname: String,
    }

    impl NativeDataplane {
        pub fn attach(bridge_ifname: &str) -> Result<Self> {
            let bytecode = include_bytes_aligned!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../ebpf/target/bpfel-unknown-none/release/ployz-ebpf-tc"
            ));
            let mut bpf = Ebpf::load(bytecode)
                .map_err(|error| Error::operation("ebpf load", error.to_string()))?;

            let _ = aya::programs::tc::qdisc_add_clsact(bridge_ifname);

            attach_tc_classifier(
                &mut bpf,
                "ployz_egress",
                bridge_ifname,
                TcAttachType::Egress,
            )?;
            attach_tc_classifier(
                &mut bpf,
                "ployz_ingress",
                bridge_ifname,
                TcAttachType::Ingress,
            )?;

            info!(
                bridge = bridge_ifname,
                "eBPF TC classifiers attached (native)"
            );

            Ok(Self {
                bpf: Mutex::new(bpf),
                bridge_ifname: bridge_ifname.to_string(),
            })
        }

        pub fn set_observe(&self, mode: ObserveMode) -> Result<()> {
            let value: u32 = match mode {
                ObserveMode::Disabled => 0,
                ObserveMode::Enabled => 1,
            };
            let mut bpf = self
                .bpf
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            let mut observe_flag: aya::maps::Array<_, u32> = aya::maps::Array::try_from(
                bpf.map_mut("OBSERVE_FLAG")
                    .ok_or_else(|| Error::operation("ebpf", "OBSERVE_FLAG map not found"))?,
            )
            .map_err(|error| Error::operation("ebpf map", error.to_string()))?;

            observe_flag
                .set(0, value, 0)
                .map_err(|error| Error::operation("ebpf observe set", error.to_string()))?;

            info!(?mode, "eBPF observation toggled (native)");
            Ok(())
        }

        pub fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
            let key = PodRouteKey(subnet_to_key(subnet));
            let entry = PodRouteEntry(RouteEntry { ifindex });
            let mut bpf = self
                .bpf
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            let mut routes: HashMap<_, PodRouteKey, PodRouteEntry> = HashMap::try_from(
                bpf.map_mut("ROUTES")
                    .ok_or_else(|| Error::operation("ebpf", "ROUTES map not found"))?,
            )
            .map_err(|error| Error::operation("ebpf map", error.to_string()))?;

            routes
                .insert(key, entry, 0)
                .map_err(|error| Error::operation("ebpf route insert", error.to_string()))?;

            info!(%subnet, ifindex, "eBPF route upserted (native)");
            Ok(())
        }

        pub fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
            let key = PodRouteKey(subnet_to_key(subnet));
            let mut bpf = self
                .bpf
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            let mut routes: HashMap<_, PodRouteKey, PodRouteEntry> = HashMap::try_from(
                bpf.map_mut("ROUTES")
                    .ok_or_else(|| Error::operation("ebpf", "ROUTES map not found"))?,
            )
            .map_err(|error| Error::operation("ebpf map", error.to_string()))?;

            match routes.remove(&key) {
                Ok(()) => info!(%subnet, "eBPF route removed (native)"),
                Err(error) => warn!(%subnet, %error, "eBPF route remove failed"),
            }
            Ok(())
        }

        pub fn detach_ref(&self) {
            let _ = aya::programs::tc::qdisc_detach_program(
                &self.bridge_ifname,
                TcAttachType::Egress,
                "ployz_egress",
            );
            let _ = aya::programs::tc::qdisc_detach_program(
                &self.bridge_ifname,
                TcAttachType::Ingress,
                "ployz_ingress",
            );
            info!(bridge = %self.bridge_ifname, "eBPF TC classifiers detached (native)");
        }
    }

    fn attach_tc_classifier(
        bpf: &mut Ebpf,
        program_name: &str,
        ifname: &str,
        attach_type: TcAttachType,
    ) -> Result<()> {
        let classifier: &mut SchedClassifier = bpf
            .program_mut(program_name)
            .ok_or_else(|| Error::operation("ebpf", format!("{program_name} program not found")))?
            .try_into()
            .map_err(|error: aya::programs::ProgramError| {
                Error::operation("ebpf program cast", format!("{program_name}: {error}"))
            })?;
        classifier.load().map_err(|error| {
            Error::operation("ebpf program load", format!("{program_name}: {error}"))
        })?;
        classifier
            .attach_with_options(
                ifname,
                attach_type,
                TcAttachOptions::Netlink(NlOptions::default()),
            )
            .map_err(|error| {
                Error::operation("ebpf program attach", format!("{program_name}: {error}"))
            })?;
        Ok(())
    }

    fn subnet_to_key(subnet: Ipv4Net) -> RouteKey {
        let network_addr: Ipv4Addr = subnet.network();
        RouteKey {
            network: u32::from(network_addr).to_be(),
            prefix_len: subnet.prefix_len() as u32,
        }
    }

    macro_rules! include_bytes_aligned {
        ($path:expr) => {{
            #[repr(C, align(8))]
            struct Aligned<Bytes: ?Sized>(Bytes);
            static ALIGNED: &Aligned<[u8]> = &Aligned(*include_bytes!($path));
            &ALIGNED.0
        }};
    }
    use include_bytes_aligned;
}
