//! Deploy warnings for Ingress Hostnames whose public DNS misses this Cluster.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Display, Formatter},
    net::IpAddr,
};

use ployz_core::{
    ClusterDnsVerdict, HttpProtocol, IngressHost, PortPublication, RequestedServiceSpec,
    cluster_dns_verdict, issuance_refusal_reason,
};

/// An Ingress Hostname that does not resolve into this Cluster.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngressDnsWarning(String);

impl Display for IngressDnsWarning {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn ingress_targets_from_ports<'a>(
    ports: impl IntoIterator<Item = &'a PortPublication>,
) -> BTreeMap<&'a IngressHost, bool> {
    let mut targets = BTreeMap::new();
    for port in ports {
        let PortPublication::Ingress {
            hostname,
            http_protocol,
            ..
        } = port
        else {
            continue;
        };
        let mentions_certificates = *http_protocol == HttpProtocol::Https;
        targets
            .entry(hostname)
            .and_modify(|mentions| *mentions |= mentions_certificates)
            .or_insert(mentions_certificates);
    }
    targets
}

fn miss_warning(
    hostname: &IngressHost,
    resolved: &[IpAddr],
    cluster_addresses: &[IpAddr],
    mentions_certificates: bool,
) -> Option<IngressDnsWarning> {
    if cluster_dns_verdict(resolved, cluster_addresses) == ClusterDnsVerdict::PointsAtCluster {
        return None;
    }
    let body = issuance_refusal_reason(hostname, resolved, cluster_addresses);
    Some(IngressDnsWarning(if mentions_certificates {
        format!("{body} A certificate cannot be issued until it points at this Cluster.")
    } else {
        body
    }))
}

fn warnings_from_targets(
    targets: BTreeMap<&IngressHost, bool>,
    cluster_addresses: &[IpAddr],
    mut resolve: impl FnMut(&IngressHost) -> Vec<IpAddr>,
) -> Vec<IngressDnsWarning> {
    targets
        .into_iter()
        .filter_map(|(hostname, mentions_certificates)| {
            miss_warning(
                hostname,
                &unique_addresses(resolve(hostname)),
                cluster_addresses,
                mentions_certificates,
            )
        })
        .collect()
}

/// Collect Deploy warnings for Ingress Hostnames that miss this Cluster.
pub fn ingress_dns_warnings<'a>(
    specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    cluster_addresses: &[IpAddr],
    resolve: impl FnMut(&IngressHost) -> Vec<IpAddr>,
) -> Vec<IngressDnsWarning> {
    warnings_from_targets(
        ingress_targets_from_ports(specs.into_iter().flat_map(|spec| spec.ports.iter())),
        cluster_addresses,
        resolve,
    )
}

fn unique_addresses(addresses: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    addresses
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Resolve A/AAAA addresses for an Ingress Hostname. Lookup failure is an empty set.
pub async fn resolve_ingress_addresses(hostname: &IngressHost) -> Vec<IpAddr> {
    match tokio::net::lookup_host((hostname.as_str(), 0)).await {
        Ok(addresses) => unique_addresses(addresses.map(|address| address.ip())),
        Err(_) => Vec::new(),
    }
}

/// Resolve Ingress Hostnames from planned ports and warn when they miss this Cluster.
pub async fn resolve_ingress_dns_warnings_for_ports<'a>(
    ports: impl IntoIterator<Item = &'a PortPublication>,
    cluster_addresses: &[IpAddr],
) -> Vec<IngressDnsWarning> {
    let targets = ingress_targets_from_ports(ports);
    let mut resolved = BTreeMap::new();
    for hostname in targets.keys().copied() {
        resolved.insert(hostname, resolve_ingress_addresses(hostname).await);
    }
    warnings_from_targets(targets, cluster_addresses, |hostname| {
        resolved
            .remove(hostname)
            .expect("Ingress Hostname was resolved before warning")
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use ployz_core::{HttpProtocol, IngressHost, PortPublication, RequestedServiceSpec};

    use super::{ingress_dns_warnings, resolve_ingress_addresses};

    fn requested(ports: Vec<PortPublication>) -> RequestedServiceSpec {
        serde_json::from_value(serde_json::json!({
            "name": "web",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" },
            "ports": ports,
        }))
        .unwrap()
    }

    fn explicit(hostname: &str) -> IngressHost {
        IngressHost::parse(hostname).unwrap()
    }

    fn ingress(hostname: IngressHost, http_protocol: HttpProtocol) -> PortPublication {
        PortPublication::Ingress {
            hostname,
            load_balancer_port: NonZeroU16::new(80).unwrap(),
            container_port: NonZeroU16::new(8080).unwrap(),
            http_protocol,
        }
    }

    #[test]
    fn ingress_hostname_warnings_cover_every_hostname_the_same_way() {
        let cluster = ["192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap()];
        let elsewhere = vec!["198.51.100.10".parse().unwrap()];
        let spec = requested(vec![
            ingress(explicit("app.example.com"), HttpProtocol::Https),
            ingress(explicit("web.opaque.ployz.example"), HttpProtocol::Https),
            ingress(explicit("plain.example.com"), HttpProtocol::Http),
            ingress(explicit("api.opaque.ployz.example"), HttpProtocol::Http),
            PortPublication::Host {
                bind: ployz_core::HostBind::All,
                published_port: NonZeroU16::new(8080).unwrap(),
                container_port: NonZeroU16::new(8080).unwrap(),
                transport_protocol: ployz_core::TransportProtocol::Tcp,
            },
        ]);

        let warnings =
            ingress_dns_warnings([&spec], &cluster, |hostname| match hostname.as_str() {
                "app.example.com" | "web.opaque.ployz.example" => elsewhere.clone(),
                "plain.example.com" | "api.opaque.ployz.example" => Vec::new(),
                other => panic!("unexpected {other}"),
            });

        let lines = warnings.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            lines,
            [
                "Ingress Hostname api.opaque.ployz.example does not resolve; it should resolve to 192.0.2.1, 192.0.2.2.",
                "Ingress Hostname app.example.com resolves to 198.51.100.10; it should resolve to 192.0.2.1, 192.0.2.2. A certificate cannot be issued until it points at this Cluster.",
                "Ingress Hostname plain.example.com does not resolve; it should resolve to 192.0.2.1, 192.0.2.2.",
                "Ingress Hostname web.opaque.ployz.example resolves to 198.51.100.10; it should resolve to 192.0.2.1, 192.0.2.2. A certificate cannot be issued until it points at this Cluster.",
            ]
        );
        for hostname in ["plain.example.com", "api.opaque.ployz.example"] {
            let http = lines
                .iter()
                .find(|line| line.contains(hostname))
                .expect("http hostname warning");
            assert!(
                !http.to_ascii_lowercase().contains("certificate"),
                "http warnings must not mention certificates: {http}"
            );
        }
    }

    #[test]
    fn pointing_at_any_cluster_address_is_enough_and_https_wins_for_one_hostname() {
        let cluster = ["192.0.2.1".parse().unwrap()];
        let spec = requested(vec![
            ingress(explicit("ok.example.com"), HttpProtocol::Https),
            ingress(explicit("mix.example.com"), HttpProtocol::Http),
            ingress(explicit("mix.example.com"), HttpProtocol::Https),
        ]);
        let warnings =
            ingress_dns_warnings([&spec], &cluster, |hostname| match hostname.as_str() {
                "ok.example.com" => vec![
                    "198.51.100.10".parse().unwrap(),
                    "192.0.2.1".parse().unwrap(),
                ],
                "mix.example.com" => Vec::new(),
                other => panic!("unexpected {other}"),
            });
        assert_eq!(
            warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "Ingress Hostname mix.example.com does not resolve; it should resolve to 192.0.2.1. A certificate cannot be issued until it points at this Cluster."
            ]
        );
    }

    #[test]
    fn warning_display_uses_the_unpublished_address_phrase() {
        let spec = requested(vec![ingress(
            explicit("app.example.com"),
            HttpProtocol::Http,
        )]);
        let warnings =
            ingress_dns_warnings([&spec], &[], |_| vec!["198.51.100.10".parse().unwrap()]);
        assert_eq!(
            warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "Ingress Hostname app.example.com resolves to 198.51.100.10; it should resolve to this Cluster's Machine addresses (none are published)."
            ]
        );
    }

    #[tokio::test]
    async fn invalid_tlds_resolve_to_nothing() {
        assert!(
            resolve_ingress_addresses(
                &ployz_core::IngressHost::parse("no-such-host.invalid").unwrap()
            )
            .await
            .is_empty()
        );
    }
}
