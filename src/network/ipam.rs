use ipnet::Ipv4Net;
use std::collections::HashSet;
use std::net::Ipv4Addr;

const SUBNET_PREFIX_LEN: u8 = 24;

/// Allocates /24 subnets from a cluster-wide network prefix.
pub struct Ipam {
    cluster: Ipv4Net,
    allocated: HashSet<Ipv4Net>,
}

impl Ipam {
    /// Create an IPAM with no existing allocations.
    pub fn new(cluster: Ipv4Net) -> Self {
        Self {
            cluster,
            allocated: HashSet::new(),
        }
    }

    /// Create an IPAM pre-loaded with existing subnet allocations.
    pub fn with_allocated(cluster: Ipv4Net, allocated: impl IntoIterator<Item = Ipv4Net>) -> Self {
        Self {
            cluster,
            allocated: allocated.into_iter().collect(),
        }
    }

    /// Allocate the next available /24 subnet.
    ///
    /// Iterates through all /24 boundaries in the cluster network,
    /// returns the first one not already allocated.
    pub fn allocate(&mut self) -> Option<Ipv4Net> {
        let cluster_start = u32::from(self.cluster.network());
        let cluster_end = u32::from(self.cluster.broadcast());
        let step = 1u32 << (32 - SUBNET_PREFIX_LEN);

        let mut addr = cluster_start;
        while addr < cluster_end {
            let network = Ipv4Addr::from(addr);
            if let Ok(subnet) = Ipv4Net::new(network, SUBNET_PREFIX_LEN) {
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

/// First usable address in a /24 subnet (the .1 address).
pub fn machine_ip(subnet: &Ipv4Net) -> Ipv4Addr {
    let start = u32::from(subnet.network());
    Ipv4Addr::from(start + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster() -> Ipv4Net {
        "10.210.0.0/16".parse().unwrap()
    }

    #[test]
    fn allocates_first_subnet() {
        let mut ipam = Ipam::new(cluster());
        let subnet = ipam.allocate().unwrap();
        assert_eq!(subnet, "10.210.0.0/24".parse::<Ipv4Net>().unwrap());
    }

    #[test]
    fn allocates_sequential_subnets() {
        let mut ipam = Ipam::new(cluster());
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
        let mut ipam = Ipam::with_allocated(cluster(), [existing]);
        let subnet = ipam.allocate().unwrap();
        assert_eq!(subnet.to_string(), "10.210.1.0/24");
    }

    #[test]
    fn machine_ip_is_dot_one() {
        let subnet: Ipv4Net = "10.210.5.0/24".parse().unwrap();
        assert_eq!(machine_ip(&subnet), Ipv4Addr::new(10, 210, 5, 1));
    }

    #[test]
    fn exhaustion_returns_none() {
        // /30 gives only one /24 boundary — but actually no /24 fits in a /30
        // Use /24 which only has one /24 in it
        let small: Ipv4Net = "10.0.0.0/24".parse().unwrap();
        let mut ipam = Ipam::new(small);
        let s1 = ipam.allocate();
        assert!(s1.is_some());
        let s2 = ipam.allocate();
        assert!(s2.is_none());
    }
}
