#![no_std]

/// BPF map key for an IPv4 network prefix.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RouteKey {
    pub network: u32,
    pub prefix_len: u32,
}

/// BPF map value for the interface that receives matching packets.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RouteEntry {
    pub ifindex: u32,
}

/// Raw packet event emitted by the eBPF observation tap.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PacketEvent {
    pub ts_ns: u64,
    pub src_addr: [u8; 16],
    pub dst_addr: [u8; 16],
    pub src_port: u16,
    pub dst_port: u16,
    pub pkt_len: u32,
    pub proto: u8,
    pub direction: u8,
    pub _pad: [u8; 2],
}
