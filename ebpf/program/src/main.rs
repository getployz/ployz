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
use ployz_ebpf_common::{MAX_ROUTES, RouteEntry, RouteKey};

#[map]
static ROUTES: HashMap<RouteKey, RouteEntry> = HashMap::with_max_entries(MAX_ROUTES, 0);

#[map]
static WG_IFINDEX: Array<u32> = Array::with_max_entries(1, 0);

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
