//! Whether an Ingress Hostname points at this Cluster.

use std::net::IpAddr;

use crate::IngressHost;

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

impl IngressHost {
    /// Whether this hostname is the Cluster Domain or a name under it.
    #[must_use]
    pub fn under_cluster_domain(&self, cluster_domain: Option<&str>) -> bool {
        let Some(domain) = cluster_domain.filter(|domain| !domain.is_empty()) else {
            return false;
        };
        let hostname = self.as_str();
        hostname == domain || hostname.ends_with(&format!(".{domain}"))
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::{ClusterDnsVerdict, cluster_dns_verdict};
    use crate::IngressHost;

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

    #[test]
    fn under_cluster_domain_requires_a_label_boundary() {
        let domain = Some("opaque.ployz.example");
        assert!(host("web.opaque.ployz.example").under_cluster_domain(domain));
        assert!(host("opaque.ployz.example").under_cluster_domain(domain));
        assert!(!host("app.example.com").under_cluster_domain(domain));
        assert!(!host("web.opaque.ployz.example").under_cluster_domain(None));
        assert!(!host("web.opaque.ployz.example").under_cluster_domain(Some("")));
        assert!(
            !host("evilopaque.ployz.example").under_cluster_domain(domain),
            "a suffix without a label boundary is not under the Cluster Domain"
        );
    }

    fn host(hostname: &str) -> IngressHost {
        IngressHost::parse(hostname).unwrap()
    }

    fn addrs<const N: usize>(values: [&str; N]) -> Vec<IpAddr> {
        values
            .into_iter()
            .map(|value| value.parse().unwrap())
            .collect()
    }
}
