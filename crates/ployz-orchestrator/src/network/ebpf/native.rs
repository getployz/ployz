use crate::error::{Error, Result};
use aya::Ebpf;
use aya::maps::HashMap;
use aya::programs::tc::{NlOptions, TcAttachOptions};
use aya::programs::{SchedClassifier, TcAttachType};
use ipnet::Ipv4Net;
use ployz_ebpf_common::{RouteEntry, RouteKey};
use std::net::Ipv4Addr;
use std::sync::Mutex;
use tracing::{info, warn};

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

    pub fn set_observe(&self, enabled: bool) -> Result<()> {
        let value: u32 = u32::from(enabled);
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

        info!(enabled, "eBPF observation toggled (native)");
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
    classifier
        .load()
        .map_err(|error| Error::operation("ebpf program load", format!("{program_name}: {error}")))?;
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
