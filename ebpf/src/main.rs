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

const MAX_ROUTES: u32 = 256;

#[map]
static ROUTES: HashMap<RouteKey, RouteEntry> = HashMap::with_max_entries(MAX_ROUTES, 0);

#[map]
static WG_IFINDEX: Array<u32> = Array::with_max_entries(1, 0);

#[map]
static OBSERVE_FLAG: Array<u32> = Array::with_max_entries(1, 0);

#[map]
static EVENTS: PerfEventArray<PacketEvent> = PerfEventArray::new(0);

#[classifier]
pub fn ployz_egress(ctx: TcContext) -> i32 {
    let target = classify(&ctx);
    emit_packet_event(&ctx, 0);
    match target {
        Some(ifindex) => unsafe { aya_ebpf::helpers::bpf_redirect(ifindex, 0) as i32 },
        None => TC_ACT_OK,
    }
}

#[classifier]
pub fn ployz_ingress(ctx: TcContext) -> i32 {
    emit_packet_event(&ctx, 1);
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

fn emit_packet_event(ctx: &TcContext, direction: u8) {
    if OBSERVE_FLAG.get(0) != Some(&1) {
        return;
    }

    let ethhdr: EthHdr = match ctx.load(0) {
        Ok(value) => value,
        Err(_) => return,
    };
    let event = match ethhdr.ether_type {
        EtherType::Ipv4 => build_ipv4_event(ctx, direction),
        EtherType::Ipv6 => build_ipv6_event(ctx, direction),
        _ => None,
    };

    if let Some(event) = event {
        let _ = EVENTS.output(ctx, &event, 0);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Ipv6HdrRaw {
    _vtcfl: [u8; 4],
    _payload_len: [u8; 2],
    next_hdr: u8,
    _hop_limit: u8,
    src_addr: [u8; 16],
    dst_addr: [u8; 16],
}

fn build_ipv4_event(ctx: &TcContext, direction: u8) -> Option<PacketEvent> {
    let iphdr: Ipv4Hdr = ctx.load(EthHdr::LEN).ok()?;
    let proto = iphdr.proto as u8;
    let (src_port, dst_port) = extract_ports(ctx, EthHdr::LEN + 20, proto);

    Some(PacketEvent {
        ts_ns: unsafe { aya_ebpf::helpers::bpf_ktime_get_ns() },
        src_addr: v4_mapped_addr(iphdr.src_addr.to_ne_bytes()),
        dst_addr: v4_mapped_addr(iphdr.dst_addr.to_ne_bytes()),
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

fn v4_mapped_addr(raw: [u8; 4]) -> [u8; 16] {
    let mut output = [0u8; 16];
    output[10] = 0xff;
    output[11] = 0xff;
    output[12] = raw[0];
    output[13] = raw[1];
    output[14] = raw[2];
    output[15] = raw[3];
    output
}

fn extract_ports(ctx: &TcContext, transport_offset: usize, proto: u8) -> (u16, u16) {
    if proto != 6 && proto != 17 {
        return (0, 0);
    }
    let src = match ctx.load(transport_offset) {
        Ok(value) => u16::from_be(value),
        Err(_) => return (0, 0),
    };
    let dst = match ctx.load(transport_offset + 2) {
        Ok(value) => u16::from_be(value),
        Err(_) => return (0, 0),
    };
    (src, dst)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
