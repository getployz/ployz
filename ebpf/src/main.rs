#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_OK,
    macros::{classifier, map},
    maps::{Array, HashMap},
    programs::TcContext,
};
use network_types::eth::{EthHdr, EtherType};
use network_types::ip::Ipv4Hdr;
use ployz_ebpf_common::{RouteEntry, RouteKey};

/// Max number of route entries (overlay subnets).
const MAX_ROUTES: u32 = 256;

#[map]
static ROUTES: HashMap<RouteKey, RouteEntry> = HashMap::with_max_entries(MAX_ROUTES, 0);

/// Single-element array holding the WireGuard interface index.
/// Set by userspace on attach; used for IPv6 ULA redirect.
#[map]
static WG_IFINDEX: Array<u32> = Array::with_max_entries(1, 0);

/// TC egress classifier on the Docker bridge.
/// Redirects overlay traffic (IPv4 subnets + IPv6 ULA) to WireGuard.
#[classifier]
pub fn ployz_egress(ctx: TcContext) -> i32 {
    match try_classify(&ctx) {
        Some(ifindex) => unsafe { aya_ebpf::helpers::bpf_redirect(ifindex, 0) as i32 },
        None => TC_ACT_OK,
    }
}

/// TC ingress classifier on the Docker bridge.
/// Accept all incoming traffic (WG → bridge → container).
#[classifier]
pub fn ployz_ingress(_ctx: TcContext) -> i32 {
    TC_ACT_OK
}

fn try_classify(ctx: &TcContext) -> Option<u32> {
    let ethhdr: EthHdr = ctx.load(0).ok()?;
    let ether_type = { ethhdr.ether_type };

    match ether_type {
        EtherType::Ipv4 => try_classify_ipv4(ctx),
        EtherType::Ipv6 => try_classify_ipv6(ctx),
        _ => None,
    }
}

fn try_classify_ipv4(ctx: &TcContext) -> Option<u32> {
    let iphdr: Ipv4Hdr = ctx.load(EthHdr::LEN).ok()?;
    let dst_addr = iphdr.dst_addr;
    let dest_ip = u32::from_be(dst_addr);

    // Try progressively shorter prefixes: /32, /24, /16, /8
    for prefix_len in [32u32, 24, 16, 8] {
        let mask = !0u32 << (32 - prefix_len);
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

fn try_classify_ipv6(ctx: &TcContext) -> Option<u32> {
    // IPv6 header starts after Ethernet header.
    // First byte of IPv6 is version+traffic class, dest addr is at offset 24.
    // We only need the first byte of the dest address (offset 24 from IPv6 start).
    let dst_first_byte: u8 = ctx.load(EthHdr::LEN + 24).ok()?;

    // fd00::/8 — ULA overlay addresses all start with 0xfd
    if dst_first_byte == 0xfd {
        let ifindex = unsafe { WG_IFINDEX.get(0)? };
        return Some(*ifindex);
    }

    None
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
