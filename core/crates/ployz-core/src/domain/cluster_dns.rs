//! Whether an Ingress Hostname points at this Cluster.

use std::net::IpAddr;

/// Whether resolved addresses intersect this Cluster's Machine public addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClusterDnsVerdict {
    PointsAtCluster,
    ResolvesElsewhere,
    DoesNotResolve,
}

/// Intersect resolved addresses with Machine public addresses.
///
/// An empty `resolved` set is "does not resolve", including lookup failure.
#[must_use]
pub fn cluster_dns_verdict(resolved: &[IpAddr], cluster_addresses: &[IpAddr]) -> ClusterDnsVerdict {
    if resolved.is_empty() {
        return ClusterDnsVerdict::DoesNotResolve;
    }
    if resolved
        .iter()
        .any(|address| cluster_addresses.contains(address))
    {
        ClusterDnsVerdict::PointsAtCluster
    } else {
        ClusterDnsVerdict::ResolvesElsewhere
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::{ClusterDnsVerdict, cluster_dns_verdict};

    #[test]
    fn verdict_covers_subset_none_empty_and_mix() {
        let cluster = addrs(["192.0.2.1", "192.0.2.2"]);
        let outside = addrs(["198.51.100.10"]);

        assert_eq!(
            cluster_dns_verdict(&addrs(["192.0.2.2"]), &cluster),
            ClusterDnsVerdict::PointsAtCluster
        );
        assert_eq!(
            cluster_dns_verdict(&outside, &cluster),
            ClusterDnsVerdict::ResolvesElsewhere
        );
        assert_eq!(
            cluster_dns_verdict(&[], &cluster),
            ClusterDnsVerdict::DoesNotResolve
        );
        assert_eq!(
            cluster_dns_verdict(&addrs(["198.51.100.10", "192.0.2.1"]), &cluster),
            ClusterDnsVerdict::PointsAtCluster
        );
    }

    fn addrs<const N: usize>(values: [&str; N]) -> Vec<IpAddr> {
        values
            .into_iter()
            .map(|value| value.parse().unwrap())
            .collect()
    }
}
