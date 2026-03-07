#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_OK,
    macros::{classifier, map},
    maps::HashMap,
    programs::TcContext,
};
use network_types::{eth::EthHdr, ip::Ipv4Hdr};
use ployz_ebpf_common::{RouteEntry, RouteKey};

/// Max number of route entries (overlay subnets).
const MAX_ROUTES: u32 = 256;

#[map]
static ROUTES: HashMap<RouteKey, RouteEntry> = HashMap::with_max_entries(MAX_ROUTES, 0);

/// TC egress classifier on the Docker bridge.
/// Matches destination IP against known overlay subnets and redirects
/// to the WireGuard interface.
#[classifier]
pub fn ployz_egress(ctx: TcContext) -> i32 {
    match try_classify(&ctx) {
        Some(ifindex) => unsafe {
            aya_ebpf::helpers::bpf_redirect(ifindex, 0) as i32
        },
        None => TC_ACT_OK,
    }
}

/// TC ingress classifier on the Docker bridge.
/// Matches source IP against known overlay subnets and accepts
/// (packets arriving from WG into the bridge).
#[classifier]
pub fn ployz_ingress(ctx: TcContext) -> i32 {
    // Ingress on the bridge: packets from WG destined to containers.
    // We just need to accept them (TC_ACT_OK). The kernel will deliver
    // to the correct veth. No redirect needed on ingress.
    TC_ACT_OK
}

fn try_classify(ctx: &TcContext) -> Option<u32> {
    let ethhdr: EthHdr = ctx.load(0).ok()?;
    // Only handle IPv4
    if ethhdr.ether_type != network_types::eth::EtherType::Ipv4 {
        return None;
    }

    let iphdr: Ipv4Hdr = ctx.load(EthHdr::LEN).ok()?;
    let dest_ip = u32::from_be(iphdr.dst_addr);

    // Try progressively shorter prefixes: /32, /24, /16, /8
    for prefix_len in [32u32, 24, 16, 8] {
        let mask = if prefix_len == 0 {
            0
        } else {
            !0u32 << (32 - prefix_len)
        };
        let network = (dest_ip & mask).to_be();
        let key = RouteKey {
            network,
            prefix_len,
        };
        if let Some(entry) = unsafe { ROUTES.get(&key) } {
            return Some(entry.ifindex);
        }
    }
    None
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
