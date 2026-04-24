use ipnet::Ipv4Net;
use std::collections::HashSet;
use std::net::Ipv4Addr;

#[must_use]
pub fn pick_candidate_subnet(
    cluster: Ipv4Net,
    prefix_len: u8,
    taken: &HashSet<Ipv4Net>,
    bias_seed: u64,
) -> Option<Ipv4Net> {
    if prefix_len < cluster.prefix_len() {
        return None;
    }

    let cluster_start = u32::from(cluster.network());
    let cluster_end = u32::from(cluster.broadcast());
    let step = 1u32 << (32 - prefix_len);

    let mut candidates = Vec::new();
    let mut addr = cluster_start;
    while addr < cluster_end {
        let network = Ipv4Addr::from(addr);
        if let Ok(subnet) = Ipv4Net::new(network, prefix_len) {
            candidates.push(subnet);
        }
        addr = addr.saturating_add(step);
    }

    if candidates.is_empty() {
        return None;
    }

    let start = (bias_seed as usize) % candidates.len();
    for offset in 0..candidates.len() {
        let index = (start + offset) % candidates.len();
        let Some(candidate) = candidates.get(index).copied() else {
            continue;
        };
        if !taken.contains(&candidate) {
            return Some(candidate);
        }
    }

    None
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
    fn picks_first_available_subnet() {
        let subnet = pick_candidate_subnet(cluster(), 24, &HashSet::new(), 0).expect("candidate");
        assert_eq!(subnet, "10.210.0.0/24".parse().expect("valid subnet"));
    }

    #[test]
    fn skips_taken_subnets() {
        let mut taken = HashSet::new();
        taken.insert("10.210.0.0/24".parse().expect("valid subnet"));
        let subnet = pick_candidate_subnet(cluster(), 24, &taken, 0).expect("candidate");
        assert_eq!(subnet, "10.210.1.0/24".parse().expect("valid subnet"));
    }

    #[test]
    fn bias_seed_rotates_candidate_scan() {
        let subnet = pick_candidate_subnet(cluster(), 24, &HashSet::new(), 3).expect("candidate");
        assert_eq!(subnet, "10.210.3.0/24".parse().expect("valid subnet"));
    }

    #[test]
    fn exhaustion_returns_none() {
        let mut taken = HashSet::new();
        taken.insert("10.0.0.0/24".parse().expect("valid subnet"));
        assert!(
            pick_candidate_subnet("10.0.0.0/24".parse().expect("valid"), 24, &taken, 0).is_none()
        );
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
}
