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

// Safety: RouteKey and RouteEntry are #[repr(C)] with only primitive fields,
// satisfying the requirements for aya::Pod. The Pod impl lives in the
// userspace loader (src/adapters/ebpf/dataplane.rs) since this crate is no_std.
