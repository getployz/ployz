#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_OK,
    macros::{classifier, map},
    maps::{Array, HashMap, PerfEventArray},
    programs::TcContext,
};
use network_types::eth::{EthHdr, EtherType};
use network_types::ip::Ipv4Hdr;
use ployz_ebpf_common::{PacketEvent, RouteEntry, RouteKey};

/// Max number of route entries (overlay subnets).
const MAX_ROUTES: u32 = 256;

#[map]
static ROUTES: HashMap<RouteKey, RouteEntry> = HashMap::with_max_entries(MAX_ROUTES, 0);

/// Single-element array holding the WireGuard interface index.
/// Set by userspace on attach; used for IPv6 ULA redirect.
#[map]
static WG_IFINDEX: Array<u32> = Array::with_max_entries(1, 0);

/// Observation toggle. [0] = 0 (off) or 1 (on). Checked per-packet.
#[map]
static OBSERVE_FLAG: Array<u32> = Array::with_max_entries(1, 0);

/// Per-packet events sent to userspace when observation is enabled.
#[map]
static EVENTS: PerfEventArray<PacketEvent> = PerfEventArray::new(0);

/// TC egress classifier on the Docker bridge.
/// Redirects overlay traffic (IPv4 subnets + IPv6 ULA) to WireGuard.
#[classifier]
pub fn ployz_egress(ctx: TcContext) -> i32 {
    let target = try_classify(&ctx);
    try_emit(&ctx, 0);
    match target {
        Some(ifindex) => unsafe { aya_ebpf::helpers::bpf_redirect(ifindex, 0) as i32 },
        None => TC_ACT_OK,
    }
}

/// TC ingress classifier on the Docker bridge.
/// Accept all incoming traffic (WG → bridge → container).
#[classifier]
pub fn ployz_ingress(ctx: TcContext) -> i32 {
    try_emit(&ctx, 1);
    TC_ACT_OK
}

// ---------------------------------------------------------------------------
// Forwarding logic (unchanged)
// ---------------------------------------------------------------------------

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
    let dst_first_byte: u8 = ctx.load(EthHdr::LEN + 24).ok()?;

    // fd00::/8 — ULA overlay addresses all start with 0xfd
    if dst_first_byte == 0xfd {
        let ifindex = unsafe { WG_IFINDEX.get(0)? };
        return Some(*ifindex);
    }

    None
}

// ---------------------------------------------------------------------------
// Observation tap — only active when OBSERVE_FLAG[0] == 1
// ---------------------------------------------------------------------------

fn try_emit(ctx: &TcContext, direction: u8) {
    if unsafe { OBSERVE_FLAG.get(0) } != Some(&1) {
        return;
    }

    let ethhdr: EthHdr = match ctx.load(0) {
        Ok(h) => h,
        Err(_) => return,
    };

    let event = match ethhdr.ether_type {
        EtherType::Ipv4 => build_ipv4_event(ctx, direction),
        EtherType::Ipv6 => build_ipv6_event(ctx, direction),
        _ => None,
    };

    if let Some(evt) = event {
        let _ = EVENTS.output(ctx, &evt, 0);
    }
}

/// Minimal IPv6 header for raw byte extraction.
#[repr(C)]
#[derive(Clone, Copy)]
struct Ipv6HdrRaw {
    _vtcfl: [u8; 4], // version + traffic class + flow label
    _payload_len: [u8; 2],
    next_hdr: u8,
    _hop_limit: u8,
    src_addr: [u8; 16],
    dst_addr: [u8; 16],
}

fn build_ipv4_event(ctx: &TcContext, direction: u8) -> Option<PacketEvent> {
    let iphdr: Ipv4Hdr = ctx.load(EthHdr::LEN).ok()?;
    let proto = iphdr.proto as u8;

    // Convert v4 addresses to v4-mapped-v6: ::ffff:a.b.c.d
    let src_raw = iphdr.src_addr.to_ne_bytes();
    let mut src_addr = [0u8; 16];
    src_addr[10] = 0xff;
    src_addr[11] = 0xff;
    src_addr[12] = src_raw[0];
    src_addr[13] = src_raw[1];
    src_addr[14] = src_raw[2];
    src_addr[15] = src_raw[3];

    let dst_raw = iphdr.dst_addr.to_ne_bytes();
    let mut dst_addr = [0u8; 16];
    dst_addr[10] = 0xff;
    dst_addr[11] = 0xff;
    dst_addr[12] = dst_raw[0];
    dst_addr[13] = dst_raw[1];
    dst_addr[14] = dst_raw[2];
    dst_addr[15] = dst_raw[3];

    // Standard 20-byte IPv4 header (no options) — covers 99.9% of traffic
    let (src_port, dst_port) = extract_ports(ctx, EthHdr::LEN + 20, proto);

    Some(PacketEvent {
        ts_ns: unsafe { aya_ebpf::helpers::bpf_ktime_get_ns() },
        src_addr,
        dst_addr,
        src_port,
        dst_port,
        pkt_len: ctx.len(),
        proto,
        direction,
        _pad: [0; 2],
    })
}

fn build_ipv6_event(ctx: &TcContext, direction: u8) -> Option<PacketEvent> {
    let ip6hdr: Ipv6HdrRaw = ctx.load(EthHdr::LEN).ok()?;
    let proto = ip6hdr.next_hdr;

    // IPv6 fixed header is 40 bytes
    let (src_port, dst_port) = extract_ports(ctx, EthHdr::LEN + 40, proto);

    Some(PacketEvent {
        ts_ns: unsafe { aya_ebpf::helpers::bpf_ktime_get_ns() },
        src_addr: ip6hdr.src_addr,
        dst_addr: ip6hdr.dst_addr,
        src_port,
        dst_port,
        pkt_len: ctx.len(),
        proto,
        direction,
        _pad: [0; 2],
    })
}

/// Extract src/dst ports for TCP (6) and UDP (17).
/// Both protocols have ports as the first 4 bytes of the transport header.
fn extract_ports(ctx: &TcContext, transport_offset: usize, proto: u8) -> (u16, u16) {
    if proto != 6 && proto != 17 {
        return (0, 0);
    }
    let src: u16 = match ctx.load(transport_offset) {
        Ok(v) => u16::from_be(v),
        Err(_) => return (0, 0),
    };
    let dst: u16 = match ctx.load(transport_offset + 2) {
        Ok(v) => u16::from_be(v),
        Err(_) => return (0, 0),
    };
    (src, dst)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
