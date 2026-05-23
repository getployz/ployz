#![no_std]

/// BPF map key: an IPv4 network prefix.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RouteKey {
    /// Network address in network byte order.
    pub network: u32,
    /// Prefix length (e.g. 24 for a /24).
    pub prefix_len: u32,
}

/// BPF map value: the interface to redirect matching packets to.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RouteEntry {
    /// Target interface index (from `if_nametoindex`).
    pub ifindex: u32,
}

/// Raw packet event emitted by the eBPF observation tap.
/// Userspace aggregates these into flows and enriches with service metadata.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PacketEvent {
    /// Monotonic timestamp (bpf_ktime_get_ns).
    pub ts_ns: u64,
    /// Source address (native IPv6, or v4-mapped for IPv4).
    pub src_addr: [u8; 16],
    /// Destination address.
    pub dst_addr: [u8; 16],
    /// Source port (host byte order). 0 for non-TCP/UDP.
    pub src_port: u16,
    /// Destination port (host byte order). 0 for non-TCP/UDP.
    pub dst_port: u16,
    /// Total packet length from skb.
    pub pkt_len: u32,
    /// IP protocol number (6=TCP, 17=UDP).
    pub proto: u8,
    /// 0 = egress (bridge→WG), 1 = ingress (WG→bridge).
    pub direction: u8,
    pub _pad: [u8; 2],
}

// Safety: RouteKey, RouteEntry, and PacketEvent are #[repr(C)] with only
// primitive fields, satisfying the requirements for aya::Pod. The Pod impl
// lives in the userspace loader since this crate is no_std.
