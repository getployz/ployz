use ipnet::Ipv4Net;
use std::collections::HashSet;
use std::net::Ipv4Addr;

pub struct Ipam {
    cluster: Ipv4Net,
    prefix_len: u8,
    allocated: HashSet<Ipv4Net>,
}

impl Ipam {
    #[must_use]
    pub fn new(cluster: Ipv4Net, prefix_len: u8) -> Self {
        Self {
            cluster,
            prefix_len,
            allocated: HashSet::new(),
        }
    }

    #[must_use]
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

    pub fn allocate(&mut self) -> Option<Ipv4Net> {
        let cluster_start = u32::from(self.cluster.network());
        let cluster_end = u32::from(self.cluster.broadcast());
        let step = 1u32 << (32 - self.prefix_len);

        let mut addr = cluster_start;
        while addr < cluster_end {
            let network = Ipv4Addr::from(addr);
            if let Ok(subnet) = Ipv4Net::new(network, self.prefix_len)
                && !self.allocated.contains(&subnet)
            {
                self.allocated.insert(subnet);
                return Some(subnet);
            }
            addr += step;
        }

        None
    }
}

#[must_use]
pub fn machine_ip(subnet: &Ipv4Net) -> Ipv4Addr {
    let start = u32::from(subnet.network());
    Ipv4Addr::from(start + 1)
}

#[must_use]
pub fn container_ip(subnet: &Ipv4Net) -> Ipv4Addr {
    let start = u32::from(subnet.network());
    Ipv4Addr::from(start + 2)
}

pub struct SubnetIpam {
    subnet: Ipv4Net,
    allocated: HashSet<Ipv4Addr>,
}

impl SubnetIpam {
    #[must_use]
    pub fn new(subnet: Ipv4Net) -> Self {
        Self {
            subnet,
            allocated: HashSet::new(),
        }
    }

    #[must_use]
    pub fn with_allocated(subnet: Ipv4Net, existing: impl IntoIterator<Item = Ipv4Addr>) -> Self {
        Self {
            subnet,
            allocated: existing.into_iter().collect(),
        }
    }

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
        "10.210.0.0/16".parse().expect("valid cluster")
    }

    #[test]
    fn allocates_first_subnet() {
        let mut ipam = Ipam::new(cluster(), 24);
        let subnet = ipam.allocate().expect("allocate subnet");
        assert_eq!(
            subnet,
            "10.210.0.0/24".parse::<Ipv4Net>().expect("valid subnet")
        );
    }

    #[test]
    fn allocates_sequential_subnets() {
        let mut ipam = Ipam::new(cluster(), 24);
        let s1 = ipam.allocate().expect("subnet 1");
        let s2 = ipam.allocate().expect("subnet 2");
        let s3 = ipam.allocate().expect("subnet 3");
        assert_eq!(s1.to_string(), "10.210.0.0/24");
        assert_eq!(s2.to_string(), "10.210.1.0/24");
        assert_eq!(s3.to_string(), "10.210.2.0/24");
    }

    #[test]
    fn skips_allocated_subnets() {
        let existing: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let mut ipam = Ipam::with_allocated(cluster(), 24, [existing]);
        let subnet = ipam.allocate().expect("next subnet");
        assert_eq!(subnet.to_string(), "10.210.1.0/24");
    }

    #[test]
    fn machine_ip_is_dot_one() {
        let subnet: Ipv4Net = "10.210.5.0/24".parse().expect("valid subnet");
        assert_eq!(machine_ip(&subnet), Ipv4Addr::new(10, 210, 5, 1));
    }

    #[test]
    fn container_ip_is_dot_two() {
        let subnet: Ipv4Net = "10.210.5.0/24".parse().expect("valid subnet");
        assert_eq!(container_ip(&subnet), Ipv4Addr::new(10, 210, 5, 2));
    }

    #[test]
    fn exhaustion_returns_none() {
        let small: Ipv4Net = "10.0.0.0/24".parse().expect("valid small subnet");
        let mut ipam = Ipam::new(small, 24);
        assert!(ipam.allocate().is_some());
        assert!(ipam.allocate().is_none());
    }

    #[test]
    fn subnet_ipam_starts_at_dot_three() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let mut ipam = SubnetIpam::new(subnet);
        let ip = ipam.allocate().expect("allocate ip");
        assert_eq!(ip, Ipv4Addr::new(10, 210, 0, 3));
    }

    #[test]
    fn subnet_ipam_sequential() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let mut ipam = SubnetIpam::new(subnet);
        assert_eq!(ipam.allocate().expect("ip 1"), Ipv4Addr::new(10, 210, 0, 3));
        assert_eq!(ipam.allocate().expect("ip 2"), Ipv4Addr::new(10, 210, 0, 4));
        assert_eq!(ipam.allocate().expect("ip 3"), Ipv4Addr::new(10, 210, 0, 5));
    }

    #[test]
    fn subnet_ipam_skips_preallocated() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let mut ipam = SubnetIpam::with_allocated(subnet, [Ipv4Addr::new(10, 210, 0, 3)]);
        let ip = ipam.allocate().expect("allocate ip");
        assert_eq!(ip, Ipv4Addr::new(10, 210, 0, 4));
    }

    #[test]
    fn subnet_ipam_release_and_reuse() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let mut ipam = SubnetIpam::new(subnet);
        let ip = ipam.allocate().expect("allocate ip");
        assert_eq!(ip, Ipv4Addr::new(10, 210, 0, 3));
        ipam.release(&ip);
        let ip2 = ipam.allocate().expect("reallocate ip");
        assert_eq!(ip2, Ipv4Addr::new(10, 210, 0, 3));
    }

    #[test]
    fn subnet_ipam_exhaustion() {
        let subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let mut ipam = SubnetIpam::new(subnet);
        for _ in 0..252 {
            assert!(ipam.allocate().is_some());
        }
        assert!(ipam.allocate().is_none());
    }

    #[test]
    fn configurable_prefix_len_22() {
        let mut ipam = Ipam::new(cluster(), 22);
        let s1 = ipam.allocate().expect("subnet 1");
        let s2 = ipam.allocate().expect("subnet 2");
        assert_eq!(s1.to_string(), "10.210.0.0/22");
        assert_eq!(s2.to_string(), "10.210.4.0/22");
    }
}
