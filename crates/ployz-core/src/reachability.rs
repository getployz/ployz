//! Reachability classification: pure policy for deciding which of a machine's
//! addresses are fleet-dialable public control endpoints versus mesh dial
//! candidates. No I/O — address enumeration lives in the crates that run on the
//! machine (the daemon and the Host Runner), which share this policy.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A routable unicast address: not loopback, unspecified, multicast, or link-local.
#[must_use]
pub fn is_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_link_local()
        }
        IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_unicast_link_local()
        }
    }
}

/// A unique-local IPv6 address (fc00::/7).
#[must_use]
pub fn is_unique_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// A globally-routable address the fleet can dial from anywhere on the Internet.
/// Excludes private, loopback, link-local, CGNAT (100.64/10), documentation,
/// benchmarking, reserved, and unique-local ranges — none of which a peer on the
/// Internet can reach. Having one makes a machine a public control endpoint and a
/// core-promotion candidate; mesh-private addresses never do.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_global_v4(ip),
        IpAddr::V6(ip) => is_global_v6(ip),
    }
}

fn is_global_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_documentation()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && a != 0 // 0.0.0.0/8 "this network"
        && a < 240 // 240.0.0.0/4 reserved (Class E) and 255.255.255.255 broadcast
        && !(a == 100 && (b & 0xc0) == 64) // 100.64.0.0/10 CGNAT / shared
        && !(a == 198 && (b & 0xfe) == 18) // 198.18.0.0/15 benchmarking
}

fn is_global_v6(ip: Ipv6Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !ip.is_unicast_link_local()
        && !is_unique_local(ip)
        && !is_documentation_v6(ip)
}

/// The IPv6 documentation range 2001:db8::/32.
fn is_documentation_v6(ip: Ipv6Addr) -> bool {
    ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8
}

/// Order mesh dial candidates so a peer prefers the closest path: same-LAN private
/// first, then unique-local, then public.
#[must_use]
pub fn mesh_sort_key(ip: IpAddr) -> (u8, IpAddr) {
    match ip {
        IpAddr::V4(ip) if ip.is_private() => (0, IpAddr::V4(ip)),
        IpAddr::V6(ip) if is_unique_local(ip) => (1, IpAddr::V6(ip)),
        IpAddr::V4(ip) => (2, IpAddr::V4(ip)),
        IpAddr::V6(ip) => (3, IpAddr::V6(ip)),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_public, mesh_sort_key};
    use std::net::IpAddr;

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("valid ip")
    }

    #[test]
    fn public_accepts_global_rejects_private_cgnat_docs_ula_and_local() {
        assert!(is_public(ip("8.8.8.8")));
        assert!(is_public(ip("2a01:4ff:2f0:296a::1")));
        assert!(!is_public(ip("10.0.0.1")));
        assert!(!is_public(ip("192.168.1.1")));
        assert!(!is_public(ip("100.64.0.1"))); // CGNAT
        assert!(!is_public(ip("203.0.113.5"))); // documentation (TEST-NET-3)
        assert!(!is_public(ip("fc00::1")));
        assert!(!is_public(ip("fe80::1")));
        assert!(!is_public(ip("127.0.0.1")));
    }

    #[test]
    fn mesh_order_prefers_private_then_ula_then_public() {
        let mut ips = vec![ip("203.0.113.5"), ip("10.0.0.1"), ip("fc00::1")];
        ips.sort_by_key(|ip| mesh_sort_key(*ip));
        assert_eq!(ips, vec![ip("10.0.0.1"), ip("fc00::1"), ip("203.0.113.5")]);
    }
}
