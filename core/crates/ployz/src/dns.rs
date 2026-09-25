//! Deploy warnings for Ingress Hostnames that do not reach this Cluster.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Display, Formatter},
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{
    HOSTNAME_VERIFY_PATH, HostnameVerdict, HttpProtocol, IngressHost, MachineId, PortPublication,
    RequestedServiceSpec, VerifyAnswer, hostname_verdict, refusal_reason,
};
use reqwest::{Client, redirect::Policy};

/// An Ingress Hostname that does not reach this Cluster.
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

fn warnings_from_targets(
    targets: BTreeMap<&IngressHost, bool>,
    cluster_addresses: &[IpAddr],
    mut verdict: impl FnMut(&IngressHost) -> HostnameVerdict,
) -> Vec<IngressDnsWarning> {
    targets
        .into_iter()
        .filter_map(|(hostname, mentions_certificates)| {
            let HostnameVerdict::Refused(refusal) = verdict(hostname) else {
                return None;
            };
            let body = refusal_reason(hostname, refusal, cluster_addresses);
            Some(IngressDnsWarning(if mentions_certificates {
                format!("{body} A certificate cannot be issued until then.")
            } else {
                body
            }))
        })
        .collect()
}

/// Collect Deploy warnings for Ingress Hostnames that do not reach this Cluster.
pub fn ingress_dns_warnings<'a>(
    specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    cluster_addresses: &[IpAddr],
    verdict: impl FnMut(&IngressHost) -> HostnameVerdict,
) -> Vec<IngressDnsWarning> {
    warnings_from_targets(
        ingress_targets_from_ports(specs.into_iter().flat_map(|spec| spec.ports.iter())),
        cluster_addresses,
        verdict,
    )
}

/// Resolve A/AAAA addresses for an Ingress Hostname. Lookup failure is an empty set.
pub async fn resolve_ingress_addresses(hostname: &IngressHost) -> Vec<IpAddr> {
    match tokio::net::lookup_host((hostname.as_str(), 0)).await {
        Ok(addresses) => addresses
            .map(|address| address.ip())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ponytail: mirrors ployzd's `verify_answer`; the two crates share only ployz-core, which has no HTTP client.
async fn verify_answer(
    client: &Client,
    hostname: &IngressHost,
    address: SocketAddr,
) -> VerifyAnswer {
    let Ok(response) = client
        .get(format!("http://{address}{HOSTNAME_VERIFY_PATH}"))
        .header(reqwest::header::HOST, hostname.as_str())
        .send()
        .await
    else {
        return VerifyAnswer::NoAnswer;
    };
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|location| location.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    VerifyAnswer::Answered {
        status,
        location,
        body: response.text().await.unwrap_or_default(),
    }
}

async fn probe_ingress_hostname(
    client: &Client,
    hostname: &IngressHost,
    machine_ids: &[MachineId],
    cluster_addresses: &[IpAddr],
) -> HostnameVerdict {
    let addresses = resolve_ingress_addresses(hostname).await;
    let answers = join_all(addresses.into_iter().map(|address| async move {
        let answer = verify_answer(client, hostname, SocketAddr::new(address, 80)).await;
        (address, answer)
    }))
    .await;
    hostname_verdict(&answers, machine_ids, cluster_addresses)
}

/// Probe Ingress Hostnames from planned ports and warn when they do not reach this Cluster.
pub async fn resolve_ingress_dns_warnings_for_ports<'a>(
    ports: impl IntoIterator<Item = &'a PortPublication>,
    machine_ids: &[MachineId],
    cluster_addresses: &[IpAddr],
) -> Vec<IngressDnsWarning> {
    let targets = ingress_targets_from_ports(ports);
    let Ok(client) = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return Vec::new();
    };
    let mut verdicts = BTreeMap::new();
    for hostname in targets.keys().copied() {
        verdicts.insert(
            hostname,
            probe_ingress_hostname(&client, hostname, machine_ids, cluster_addresses).await,
        );
    }
    warnings_from_targets(targets, cluster_addresses, |hostname| verdicts[hostname])
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use ployz_core::{
        ClusterRoute, HostnameVerdict, HttpProtocol, IngressHost, PortPublication, Refusal,
        RequestedServiceSpec,
    };

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
    fn only_hostnames_that_miss_the_cluster_warn_and_only_https_mentions_certificates() {
        let cluster = ["192.0.2.1".parse().unwrap()];
        let spec = requested(vec![
            ingress(explicit("app.example.com"), HttpProtocol::Https),
            ingress(explicit("plain.example.com"), HttpProtocol::Http),
            ingress(explicit("proxied.example.com"), HttpProtocol::Https),
            ingress(explicit("mix.example.com"), HttpProtocol::Http),
            ingress(explicit("mix.example.com"), HttpProtocol::Https),
            PortPublication::Host {
                bind: ployz_core::HostBind::All,
                published_port: NonZeroU16::new(8080).unwrap(),
                container_port: NonZeroU16::new(8080).unwrap(),
                transport_protocol: ployz_core::TransportProtocol::Tcp,
            },
        ]);

        let warnings =
            ingress_dns_warnings([&spec], &cluster, |hostname| match hostname.as_str() {
                "app.example.com" => HostnameVerdict::Refused(Refusal::RedirectsToHttps),
                "plain.example.com" => HostnameVerdict::Refused(Refusal::ReachesElsewhere),
                "proxied.example.com" => HostnameVerdict::ReachesCluster(ClusterRoute::ViaProxy),
                "mix.example.com" => HostnameVerdict::Refused(Refusal::DoesNotResolve),
                other => panic!("unexpected {other}"),
            });

        assert_eq!(
            warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "app.example.com redirects HTTP to HTTPS before reaching this Cluster. Exempt /.well-known/acme-challenge/* from HTTPS redirects in your proxy. A certificate cannot be issued until then.",
                "mix.example.com does not resolve. Add a DNS record pointing at 192.0.2.1. A certificate cannot be issued until then.",
                "plain.example.com answers from another server. Point it at 192.0.2.1.",
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
