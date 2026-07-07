//! Reachability classification: pure policy for deciding which of a machine's
//! addresses are fleet-dialable public control endpoints versus mesh dial
//! candidates. No I/O — address enumeration lives in the crates that run on the
//! machine (the daemon and the keeper), which share this policy.

use std::net::{IpAddr, Ipv6Addr};

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

/// A globally-routable address the fleet can dial: routable and not in a private
/// (RFC 1918) or unique-local range. Having one is what makes a machine a public
/// control endpoint and a core-promotion candidate; mesh-private addresses never do.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    is_routable(ip)
        && match ip {
            IpAddr::V4(ip) => !ip.is_private(),
            IpAddr::V6(ip) => !is_unique_local(ip),
        }
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
    fn public_accepts_global_rejects_private_ula_and_local() {
        assert!(is_public(ip("203.0.113.5")));
        assert!(is_public(ip("2a01:4ff:2f0:296a::1")));
        assert!(!is_public(ip("10.0.0.1")));
        assert!(!is_public(ip("192.168.1.1")));
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
