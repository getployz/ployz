use ipnet::Ipv4Net;
use std::collections::HashSet;
use std::net::Ipv4Addr;

/// Allocates subnets from a cluster-wide IPv4 prefix.
pub struct Ipam {
    cluster: Ipv4Net,
    prefix_len: u8,
    allocated: HashSet<Ipv4Net>,
}

impl Ipam {
    /// Create an IPAM with configurable subnet prefix length.
    pub fn new(cluster: Ipv4Net, prefix_len: u8) -> Self {
        Self {
            cluster,
            prefix_len,
            allocated: HashSet::new(),
        }
    }

    /// Create an IPAM pre-loaded with existing subnet allocations.
    pub fn with_allocated(
        cluster: Ipv4Net,
        prefix_len: u8,
        allocated: impl IntoIterator<Item = Ipv4Net>,
    ) -> Self {
        Self {
            cluster,
            prefix_len,
            allocated: allocated.into_iter().collect(),
        }
    }

    /// Allocate the next available subnet.
    pub fn allocate(&mut self) -> Option<Ipv4Net> {
        let cluster_start = u32::from(self.cluster.network());
        let cluster_end = u32::from(self.cluster.broadcast());
        let step = 1u32 << (32 - self.prefix_len);

        let mut addr = cluster_start;
        while addr < cluster_end {
            let network = Ipv4Addr::from(addr);
            if let Ok(subnet) = Ipv4Net::new(network, self.prefix_len) {
                if !self.allocated.contains(&subnet) {
                    self.allocated.insert(subnet);
                    return Some(subnet);
                }
            }
            addr += step;
        }

        None
    }
}

/// First usable address in a subnet (the .1 gateway address).
pub fn machine_ip(subnet: &Ipv4Net) -> Ipv4Addr {
    let start = u32::from(subnet.network());
    Ipv4Addr::from(start + 1)
}

/// Second usable address in a subnet (the .2 address).
/// Used for the WG container on the bridge network (.1 is the Docker gateway).
pub fn container_ip(subnet: &Ipv4Net) -> Ipv4Addr {
    let start = u32::from(subnet.network());
    Ipv4Addr::from(start + 2)
}

/// Allocates individual IPs within a machine's /24 subnet.
/// Reserves .0 (network), .1 (docker gw), .2 (backbone WG). Workloads get .3–.254.
pub struct SubnetIpam {
    subnet: Ipv4Net,
    allocated: HashSet<Ipv4Addr>,
}

impl SubnetIpam {
    pub fn new(subnet: Ipv4Net) -> Self {
        Self {
            subnet,
            allocated: HashSet::new(),
        }
    }

    pub fn with_allocated(subnet: Ipv4Net, existing: impl IntoIterator<Item = Ipv4Addr>) -> Self {
        Self {
            subnet,
            allocated: existing.into_iter().collect(),
        }
    }

    /// Allocate the next available IP starting at .3 (skipping .0 network, .1 gateway, .2 backbone).
    pub fn allocate(&mut self) -> Option<Ipv4Addr> {
        let base = u32::from(self.subnet.network());
        for offset in 3..255u32 {
            let ip = Ipv4Addr::from(base + offset);
            if !self.allocated.contains(&ip) {
                self.allocated.insert(ip);
                return Some(ip);
            }
        }
        None
    }

    pub fn release(&mut self, ip: &Ipv4Addr) {
        self.allocated.remove(ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster() -> Ipv4Net {
        "10.210.0.0/16".parse().unwrap()
    }

    #[test]
    fn allocates_first_subnet() {
        let mut ipam = Ipam::new(cluster(), 24);
        let subnet = ipam.allocate().unwrap();
        assert_eq!(subnet, "10.210.0.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn allocates_sequential_subnets() {
        let mut ipam = Ipam::new(cluster(), 24);
        let s1 = ipam.allocate().unwrap();
        let s2 = ipam.allocate().unwrap();
        let s3 = ipam.allocate().unwrap();
        assert_eq!(s1.to_string(), "10.210.0.0/24");
        assert_eq!(s2.to_string(), "10.210.1.0/24");
        assert_eq!(s3.to_string(), "10.210.2.0/24");
    }

    #[test]
    fn skips_allocated_subnets() {
        let existing: Ipv4Net = "10.210.0.0/24".parse().unwrap();
        let mut ipam = Ipam::with_allocated(cluster(), 24, [existing]);
        let subnet = ipam.allocate().unwrap();
        assert_eq!(subnet.to_string(), "10.210.1.0/24");
    }

    #[test]
    fn machine_ip_is_dot_one() {
        let subnet: Ipv4Net = "10.210.5.0/24".parse().unwrap();
        assert_eq!(machine_ip(&subnet), Ipv4Addr::new(10, 210, 5, 1));
    }

    #[test]
    fn container_ip_is_dot_two() {
        let subnet: Ipv4Net = "10.210.5.0/24".parse().unwrap();
        assert_eq!(container_ip(&subnet), Ipv4Addr::new(10, 210, 5, 2));
    }

    #[test]
    fn exhaustion_returns_none() {
        let small: Ipv4Net = "10.0.0.0/24".parse().unwrap();
        let mut ipam = Ipam::new(small, 24);
        let s1 = ipam.allocate();
        assert!(s1.is_some());
        let s2 = ipam.allocate();
        assert!(s2.is_none());
    }

    #[test]
    fn subnet_ipam_starts_at_dot_three() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().unwrap();
        let mut ipam = SubnetIpam::new(subnet);
        let ip = ipam.allocate().unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 210, 0, 3));
    }

    #[test]
    fn subnet_ipam_sequential() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().unwrap();
        let mut ipam = SubnetIpam::new(subnet);
        assert_eq!(ipam.allocate().unwrap(), Ipv4Addr::new(10, 210, 0, 3));
        assert_eq!(ipam.allocate().unwrap(), Ipv4Addr::new(10, 210, 0, 4));
        assert_eq!(ipam.allocate().unwrap(), Ipv4Addr::new(10, 210, 0, 5));
    }

    #[test]
    fn subnet_ipam_skips_preallocated() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().unwrap();
        let mut ipam = SubnetIpam::with_allocated(subnet, [Ipv4Addr::new(10, 210, 0, 3)]);
        let ip = ipam.allocate().unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 210, 0, 4));
    }

    #[test]
    fn subnet_ipam_release_and_reuse() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().unwrap();
        let mut ipam = SubnetIpam::new(subnet);
        let ip = ipam.allocate().unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 210, 0, 3));
        ipam.release(&ip);
        let ip2 = ipam.allocate().unwrap();
        assert_eq!(ip2, Ipv4Addr::new(10, 210, 0, 3));
    }

    #[test]
    fn subnet_ipam_exhaustion() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().unwrap();
        let mut ipam = SubnetIpam::new(subnet);
        // .3 through .254 = 252 addresses
        for _ in 0..252 {
            assert!(ipam.allocate().is_some());
        }
        assert!(ipam.allocate().is_none());
    }

    #[test]
    fn configurable_prefix_len_22() {
        let mut ipam = Ipam::new(cluster(), 22);
        let s1 = ipam.allocate().unwrap();
        let s2 = ipam.allocate().unwrap();
        assert_eq!(s1.to_string(), "10.210.0.0/22");
        assert_eq!(s2.to_string(), "10.210.4.0/22");
    }
}
