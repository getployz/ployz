#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_OK,
    macros::{cgroup_skb, classifier, map},
    maps::{Array, HashMap},
    programs::{SkBuffContext, TcContext},
};
use network_types::eth::{EthHdr, EtherType};
use network_types::ip::Ipv4Hdr;
use ployz_ebpf_common::{
    ContainerIp, IsolationConfig, MAX_ISOLATION_ENTRIES, MAX_ROUTES, NamespaceTag, RouteEntry,
    RouteKey, isolation_packet_allowed,
};

const CGROUP_ALLOW: i32 = 1;
const CGROUP_DROP: i32 = 0;
const AF_INET_FAMILY: u32 = 2;
const ETH_P_IP_NETWORK_ORDER: u32 = u16::to_be(0x0800) as u32;

#[map]
static ROUTES: HashMap<RouteKey, RouteEntry> = HashMap::with_max_entries(MAX_ROUTES, 0);

#[map]
static WG_IFINDEX: Array<u32> = Array::with_max_entries(1, 0);

#[map]
static ISOLATION_CONFIG: Array<IsolationConfig> = Array::with_max_entries(1, 0);

#[map]
static ISOLATION_NAMESPACES: HashMap<ContainerIp, NamespaceTag> =
    HashMap::with_max_entries(MAX_ISOLATION_ENTRIES, 0);

#[classifier]
pub fn ployz_egress(ctx: TcContext) -> i32 {
    let target = classify(&ctx);
    match target {
        Some(ifindex) => unsafe { aya_ebpf::helpers::bpf_redirect(ifindex, 0) as i32 },
        None => TC_ACT_OK,
    }
}

#[classifier]
pub fn ployz_ingress(_ctx: TcContext) -> i32 {
    TC_ACT_OK
}

#[cgroup_skb(egress)]
pub fn ployz_cgroup_egress(ctx: SkBuffContext) -> i32 {
    enforce_namespace_isolation(&ctx)
}

#[cgroup_skb(ingress)]
pub fn ployz_cgroup_ingress(ctx: SkBuffContext) -> i32 {
    enforce_namespace_isolation(&ctx)
}

#[inline(always)]
fn enforce_namespace_isolation(ctx: &SkBuffContext) -> i32 {
    if ctx.skb.family() != AF_INET_FAMILY || ctx.skb.protocol() != ETH_P_IP_NETWORK_ORDER {
        return CGROUP_ALLOW;
    }

    let Some(config) = ISOLATION_CONFIG.get(0) else {
        return CGROUP_ALLOW;
    };
    if !config.is_armed() {
        return CGROUP_ALLOW;
    }

    let Ok(ip_header) = ctx.load::<Ipv4Hdr>(0) else {
        return CGROUP_DROP;
    };
    if ip_header.version() != 4 || ip_header.ihl() < 5 {
        return CGROUP_DROP;
    }

    let source = ContainerIp {
        address: ip_header.src_addr,
    };
    let destination = ContainerIp {
        address: ip_header.dst_addr,
    };
    let source_namespace = unsafe { ISOLATION_NAMESPACES.get(&source) };
    let destination_namespace = unsafe { ISOLATION_NAMESPACES.get(&destination) };
    if isolation_packet_allowed(
        config,
        source,
        source_namespace,
        destination,
        destination_namespace,
    ) {
        CGROUP_ALLOW
    } else {
        CGROUP_DROP
    }
}

fn classify(ctx: &TcContext) -> Option<u32> {
    let ethhdr: EthHdr = ctx.load(0).ok()?;
    match ethhdr.ether_type {
        EtherType::Ipv4 => classify_ipv4(ctx),
        EtherType::Ipv6 => classify_ipv6(ctx),
        _ => None,
    }
}

fn classify_ipv4(ctx: &TcContext) -> Option<u32> {
    let iphdr: Ipv4Hdr = ctx.load(EthHdr::LEN).ok()?;
    let dest_ip = u32::from_be(iphdr.dst_addr);

    for prefix_len in [32u32, 24, 16, 8] {
        let mask = !0u32 << (32 - prefix_len);
        let key = RouteKey {
            network: (dest_ip & mask).to_be(),
            prefix_len,
        };
        if let Some(entry) = unsafe { ROUTES.get(&key) } {
            return Some(entry.ifindex);
        }
    }

    None
}

fn classify_ipv6(ctx: &TcContext) -> Option<u32> {
    let dst_first_byte: u8 = ctx.load(EthHdr::LEN + 24).ok()?;
    if dst_first_byte == 0xfd {
        let ifindex = WG_IFINDEX.get(0)?;
        return Some(*ifindex);
    }

    None
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
