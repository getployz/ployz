use aya::Ebpf;
use aya::maps::HashMap;
use aya::programs::tc::{NlOptions, TcAttachOptions};
use aya::programs::{SchedClassifier, TcAttachType};
use ipnet::Ipv4Net;
use ployz_ebpf_common::{RouteEntry, RouteKey};
use std::net::Ipv4Addr;
use std::sync::Mutex;
use tracing::{info, warn};

use crate::error::{Error, Result};

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
        let bytecode = include_bytes_aligned!(concat!(env!("OUT_DIR"), "/ployz-ebpf-tc"));
        let mut bpf =
            Ebpf::load(bytecode).map_err(|e| Error::operation("ebpf load", e.to_string()))?;

        let _ = aya::programs::tc::qdisc_add_clsact(bridge_ifname);

        let egress: &mut SchedClassifier = bpf
            .program_mut("ployz_egress")
            .ok_or_else(|| Error::operation("ebpf", "ployz_egress program not found"))?
            .try_into()
            .map_err(|e: aya::programs::ProgramError| {
                Error::operation("ebpf egress cast", e.to_string())
            })?;
        egress
            .load()
            .map_err(|e| Error::operation("ebpf egress load", e.to_string()))?;
        egress
            .attach_with_options(
                bridge_ifname,
                TcAttachType::Egress,
                TcAttachOptions::Netlink(NlOptions::default()),
            )
            .map_err(|e| Error::operation("ebpf egress attach", e.to_string()))?;

        let ingress: &mut SchedClassifier = bpf
            .program_mut("ployz_ingress")
            .ok_or_else(|| Error::operation("ebpf", "ployz_ingress program not found"))?
            .try_into()
            .map_err(|e: aya::programs::ProgramError| {
                Error::operation("ebpf ingress cast", e.to_string())
            })?;
        ingress
            .load()
            .map_err(|e| Error::operation("ebpf ingress load", e.to_string()))?;
        ingress
            .attach_with_options(
                bridge_ifname,
                TcAttachType::Ingress,
                TcAttachOptions::Netlink(NlOptions::default()),
            )
            .map_err(|e| Error::operation("ebpf ingress attach", e.to_string()))?;

        info!(
            bridge = bridge_ifname,
            "eBPF TC classifiers attached (native)"
        );

        Ok(Self {
            bpf: Mutex::new(bpf),
            bridge_ifname: bridge_ifname.to_string(),
        })
    }

    pub fn set_observe(&self, enabled: bool) -> Result<()> {
        let value: u32 = if enabled { 1 } else { 0 };
        let mut bpf = self.bpf.lock().unwrap();

        let mut arr: aya::maps::Array<_, u32> = aya::maps::Array::try_from(
            bpf.map_mut("OBSERVE_FLAG")
                .ok_or_else(|| Error::operation("ebpf", "OBSERVE_FLAG map not found"))?,
        )
        .map_err(|e| Error::operation("ebpf map", e.to_string()))?;

        arr.set(0, value, 0)
            .map_err(|e| Error::operation("ebpf observe set", e.to_string()))?;

        info!(enabled, "eBPF observation toggled (native)");
        Ok(())
    }

    pub fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()> {
        let key = PodRouteKey(subnet_to_key(subnet));
        let entry = PodRouteEntry(RouteEntry { ifindex });
        let mut bpf = self.bpf.lock().unwrap();

        let mut routes: HashMap<_, PodRouteKey, PodRouteEntry> = HashMap::try_from(
            bpf.map_mut("ROUTES")
                .ok_or_else(|| Error::operation("ebpf", "ROUTES map not found"))?,
        )
        .map_err(|e| Error::operation("ebpf map", e.to_string()))?;

        routes
            .insert(key, entry, 0)
            .map_err(|e| Error::operation("ebpf route insert", e.to_string()))?;

        info!(%subnet, ifindex, "eBPF route upserted (native)");
        Ok(())
    }

    pub fn remove_route(&self, subnet: Ipv4Net) -> Result<()> {
        let key = PodRouteKey(subnet_to_key(subnet));
        let mut bpf = self.bpf.lock().unwrap();

        let mut routes: HashMap<_, PodRouteKey, PodRouteEntry> = HashMap::try_from(
            bpf.map_mut("ROUTES")
                .ok_or_else(|| Error::operation("ebpf", "ROUTES map not found"))?,
        )
        .map_err(|e| Error::operation("ebpf map", e.to_string()))?;

        match routes.remove(&key) {
            Ok(()) => info!(%subnet, "eBPF route removed (native)"),
            Err(e) => warn!(%subnet, %e, "eBPF route remove failed"),
        }
        Ok(())
    }

    pub fn detach(self) {
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
