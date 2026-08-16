use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Display, Formatter},
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use ployz_core::{
    CADDY_VERIFY_PATH, ClusterDnsVerdict, CreateDomainRecordsRequest, DnsRecord, DnsRecordType,
    HttpProtocol, IngressHost, IngressHostname, InspectRequest, Machine, MachineId, MachineTarget,
    PortPublication, RequestedServiceSpec, cluster_dns_verdict, op,
};
use reqwest::{Client as HttpClient, redirect::Policy};
use thiserror::Error;

use crate::{
    caddy::SERVICE_NAME,
    connect::{Client, ConnectError},
};

const REACHABILITY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error("inspect Caddy Machine {machine_id}: {source}")]
    Inspect {
        machine_id: MachineId,
        #[source]
        source: ConnectError,
    },
    #[error("build hosted-DNS reachability client: {0}")]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    NoReachableMachines(#[from] NoReachableMachines),
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("no publicly reachable Caddy Machines found")]
pub struct NoReachableMachines;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error(
    "ingress port has no generated hostname until a cluster domain is reserved: {container_port}/{}",
    protocol_label(protocol)
)]
pub struct DomainRequired {
    pub container_port: u16,
    pub protocol: HttpProtocol,
}

fn protocol_label(protocol: &HttpProtocol) -> &'static str {
    match protocol {
        HttpProtocol::Http => "http",
        HttpProtocol::Https => "https",
    }
}

pub async fn update_records_if_reserved(client: &mut Client) -> Result<(), Error> {
    match client.domain_if_reserved().await? {
        Some(_) => update_records_for_caddy(client).await,
        None => Ok(()),
    }
}

pub async fn update_records_for_caddy(client: &mut Client) -> Result<(), Error> {
    let live = client.live_services().await?;
    let caddy_machines = live
        .services
        .iter()
        .flat_map(|service| &service.containers)
        .filter(|container| container.as_observation().service_name.as_str() == SERVICE_NAME)
        .map(|container| container.as_observation().machine_id)
        .collect::<BTreeSet<_>>();
    if caddy_machines.is_empty() {
        return Ok(());
    }

    let mut machines = Vec::new();
    for machine_id in caddy_machines {
        let target = MachineTarget::from(&machine_id);
        let details = client
            .call::<op::Inspect>(InspectRequest::default(), Some(&target))
            .await
            .map_err(|source| Error::Inspect { machine_id, source })?;
        let machine = details.machine.ok_or_else(|| Error::Inspect {
            machine_id,
            source: ConnectError::MissingMachineDetails,
        })?;
        if machine.public_ip.is_some() {
            machines.push(machine);
        }
    }

    let http = HttpClient::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(REACHABILITY_TIMEOUT)
        .build()
        .map_err(Error::from)?;
    let reachable = futures_util::future::join_all(machines.into_iter().map(|machine| {
        let http = &http;
        async move { probe_machine(http, &machine).await.then_some(machine) }
    }))
    .await
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let records = records_from_machines(&reachable)?;
    client
        .call::<op::CreateDomainRecords>(CreateDomainRecordsRequest { records }, None)
        .await
        .map(drop)
        .map_err(Into::into)
}

async fn probe_machine(http: &HttpClient, machine: &Machine) -> bool {
    let Some(public_ip) = machine.public_ip else {
        return false;
    };
    let address = SocketAddr::new(public_ip, 80);
    let Ok(response) = http
        .get(format!("http://{address}{CADDY_VERIFY_PATH}"))
        .send()
        .await
    else {
        return false;
    };
    let status = response.status().as_u16();
    let body = response.bytes().await.ok();
    reachability_matches(&machine.id, status, body.as_deref())
}

fn reachability_matches(machine_id: &MachineId, status: u16, body: Option<&[u8]>) -> bool {
    status == 200 && body == Some(machine_id.as_str().as_bytes())
}

fn records_from_machines(machines: &[Machine]) -> Result<Vec<DnsRecord>, NoReachableMachines> {
    if machines.is_empty() {
        return Err(NoReachableMachines);
    }
    let mut ipv4 = BTreeSet::new();
    let mut ipv6 = BTreeSet::new();
    for address in machines.iter().filter_map(|machine| machine.public_ip) {
        match address {
            std::net::IpAddr::V4(address) => {
                ipv4.insert(address.to_string());
            }
            std::net::IpAddr::V6(address) => {
                ipv6.insert(address.to_string());
            }
        }
    }
    let mut records = Vec::new();
    if !ipv4.is_empty() {
        records.push(DnsRecord {
            name: "*".into(),
            record_type: DnsRecordType::A,
            values: ipv4.into_iter().collect(),
        });
    }
    if !ipv6.is_empty() {
        records.push(DnsRecord {
            name: "*".into(),
            record_type: DnsRecordType::Aaaa,
            values: ipv6.into_iter().collect(),
        });
    }
    if records.is_empty() {
        Err(NoReachableMachines)
    } else {
        Ok(records)
    }
}

pub fn expand_ingress_ports(
    spec: &mut RequestedServiceSpec,
    cluster_domain: Option<&str>,
) -> Result<(), DomainRequired> {
    let domain = cluster_domain.filter(|domain| !domain.is_empty());
    // A Service Name plus a reserved hosted domain is always a valid Ingress Hostname.
    let assigned = domain.map(|domain| {
        IngressHost::parse(format!("{}.{domain}", spec.name))
            .expect("service name and reserved cluster domain form a hostname")
    });
    let mut extras = Vec::new();
    for port in &mut spec.ports {
        let PortPublication::Ingress {
            hostname,
            load_balancer_port,
            container_port,
            http_protocol,
        } = port
        else {
            continue;
        };
        match hostname {
            IngressHostname::AssignFromClusterDomain => {
                *hostname = IngressHostname::Explicit {
                    hostname: assigned.clone().ok_or(DomainRequired {
                        container_port: container_port.get(),
                        protocol: *http_protocol,
                    })?,
                };
            }
            IngressHostname::Explicit { hostname: existing } => {
                if let (Some(domain), Some(assigned)) = (domain, assigned.as_ref())
                    && !existing.under_cluster_domain(Some(domain))
                {
                    extras.push(PortPublication::Ingress {
                        hostname: IngressHostname::Explicit {
                            hostname: assigned.clone(),
                        },
                        load_balancer_port: *load_balancer_port,
                        container_port: *container_port,
                        http_protocol: *http_protocol,
                    });
                }
            }
        }
    }
    spec.ports.extend(extras);
    Ok(())
}

/// An Ingress Hostname that does not resolve into this Cluster.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngressDnsWarning(String);

impl Display for IngressDnsWarning {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn join_addresses(addresses: &[IpAddr]) -> String {
    if addresses.is_empty() {
        return "this Cluster's Machine addresses (none are published)".into();
    }
    addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn ingress_targets<'a>(
    specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
) -> BTreeMap<&'a IngressHost, bool> {
    let mut targets = BTreeMap::new();
    for spec in specs {
        for port in &spec.ports {
            let PortPublication::Ingress {
                hostname: IngressHostname::Explicit { hostname },
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
    }
    targets
}

fn miss_warning(
    hostname: &IngressHost,
    resolved: &[IpAddr],
    cluster_addresses: &[IpAddr],
    mentions_certificates: bool,
) -> Option<IngressDnsWarning> {
    let should = join_addresses(cluster_addresses);
    let body = match cluster_dns_verdict(resolved, cluster_addresses) {
        ClusterDnsVerdict::PointsAtCluster => return None,
        ClusterDnsVerdict::DoesNotResolve => {
            format!("Ingress Hostname {hostname} does not resolve; it should resolve to {should}.")
        }
        ClusterDnsVerdict::ResolvesElsewhere => format!(
            "Ingress Hostname {hostname} resolves to {}; it should resolve to {should}.",
            join_addresses(resolved)
        ),
    };
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
    warnings_from_targets(ingress_targets(specs), cluster_addresses, resolve)
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

/// Resolve Ingress Hostnames and warn when they miss this Cluster.
pub async fn resolve_ingress_dns_warnings<'a>(
    specs: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    cluster_addresses: &[IpAddr],
) -> Vec<IngressDnsWarning> {
    let targets = ingress_targets(specs);
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
    use std::net::IpAddr;

    use std::num::NonZeroU16;

    use ployz_core::{
        DnsRecord, DnsRecordType, HttpProtocol, IngressHostname, Machine, MachineId,
        PortPublication, RequestedServiceSpec,
    };

    use super::{
        DomainRequired, NoReachableMachines, expand_ingress_ports, ingress_dns_warnings,
        reachability_matches, records_from_machines, resolve_ingress_addresses,
    };

    #[test]
    fn wildcard_records_include_each_present_address_family_once() {
        let ipv4 = machine('1', "192.0.2.1");
        let ipv6 = machine('2', "2001:db8::1");

        assert_eq!(
            records_from_machines(std::slice::from_ref(&ipv4)).unwrap(),
            vec![DnsRecord {
                name: "*".into(),
                record_type: DnsRecordType::A,
                values: vec!["192.0.2.1".into()],
            }]
        );
        assert_eq!(
            records_from_machines(std::slice::from_ref(&ipv6)).unwrap(),
            vec![DnsRecord {
                name: "*".into(),
                record_type: DnsRecordType::Aaaa,
                values: vec!["2001:db8::1".into()],
            }]
        );
        assert_eq!(
            records_from_machines(&[ipv4, ipv6]).unwrap(),
            vec![
                DnsRecord {
                    name: "*".into(),
                    record_type: DnsRecordType::A,
                    values: vec!["192.0.2.1".into()],
                },
                DnsRecord {
                    name: "*".into(),
                    record_type: DnsRecordType::Aaaa,
                    values: vec!["2001:db8::1".into()],
                },
            ]
        );
        assert_eq!(records_from_machines(&[]), Err(NoReachableMachines));
    }

    #[test]
    fn reachability_requires_status_200_and_the_exact_machine_id_bytes() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let exact = machine_id.as_str().as_bytes();

        assert!(reachability_matches(&machine_id, 200, Some(exact)));
        assert!(!reachability_matches(&machine_id, 204, Some(exact)));
        assert!(!reachability_matches(&machine_id, 200, Some(b"")));
        assert!(!reachability_matches(&machine_id, 200, Some(b"wrong")));
        let mut newline = exact.to_vec();
        newline.push(b'\n');
        assert!(!reachability_matches(&machine_id, 200, Some(&newline)));
        assert!(!reachability_matches(&machine_id, 200, None));
    }

    #[test]
    fn assigned_ingress_hostnames_require_a_reserved_cluster_domain() {
        let mut spec = requested(vec![ingress(
            IngressHostname::AssignFromClusterDomain,
            HttpProtocol::Https,
        )]);
        assert_eq!(
            expand_ingress_ports(&mut spec, None),
            Err(DomainRequired {
                container_port: 8080,
                protocol: HttpProtocol::Https,
            })
        );
        assert_eq!(
            expand_ingress_ports(&mut spec, Some(""))
                .unwrap_err()
                .to_string(),
            "ingress port has no generated hostname until a cluster domain is reserved: 8080/https"
        );
    }

    #[test]
    fn reserved_domain_assigns_and_duplicates_external_ingress_hostnames() {
        let mut spec = requested(vec![
            ingress(IngressHostname::AssignFromClusterDomain, HttpProtocol::Http),
            ingress(explicit("app.example.com"), HttpProtocol::Https),
            ingress(explicit("api.opaque.ployz.example"), HttpProtocol::Http),
            PortPublication::Host {
                bind: ployz_core::HostBind::All,
                published_port: NonZeroU16::new(8080).unwrap(),
                container_port: NonZeroU16::new(8080).unwrap(),
                transport_protocol: ployz_core::TransportProtocol::Tcp,
            },
        ]);

        expand_ingress_ports(&mut spec, Some("opaque.ployz.example")).unwrap();
        assert_eq!(
            spec.ports,
            vec![
                ingress(explicit("web.opaque.ployz.example"), HttpProtocol::Http),
                ingress(explicit("app.example.com"), HttpProtocol::Https),
                ingress(explicit("api.opaque.ployz.example"), HttpProtocol::Http,),
                PortPublication::Host {
                    bind: ployz_core::HostBind::All,
                    published_port: NonZeroU16::new(8080).unwrap(),
                    container_port: NonZeroU16::new(8080).unwrap(),
                    transport_protocol: ployz_core::TransportProtocol::Tcp,
                },
                ingress(explicit("web.opaque.ployz.example"), HttpProtocol::Https),
            ]
        );
    }

    fn requested(ports: Vec<PortPublication>) -> RequestedServiceSpec {
        serde_json::from_value(serde_json::json!({
            "name": "web",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" },
            "ports": ports,
        }))
        .unwrap()
    }

    fn explicit(hostname: &str) -> IngressHostname {
        IngressHostname::explicit(hostname).unwrap()
    }

    fn ingress(hostname: IngressHostname, http_protocol: HttpProtocol) -> PortPublication {
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
            ingress(
                IngressHostname::AssignFromClusterDomain,
                HttpProtocol::Https,
            ),
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
    fn assigned_hostname_warns_after_expansion() {
        let cluster = ["192.0.2.1".parse().unwrap()];
        let mut spec = requested(vec![ingress(
            IngressHostname::AssignFromClusterDomain,
            HttpProtocol::Https,
        )]);
        expand_ingress_ports(&mut spec, Some("opaque.ployz.example")).unwrap();

        let warnings = ingress_dns_warnings([&spec], &cluster, |hostname| {
            assert_eq!(hostname.as_str(), "web.opaque.ployz.example");
            Vec::new()
        });
        assert_eq!(
            warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "Ingress Hostname web.opaque.ployz.example does not resolve; it should resolve to 192.0.2.1. A certificate cannot be issued until it points at this Cluster."
            ]
        );
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

    fn machine(seed: char, public_ip: &str) -> Machine {
        let mut machine: Machine = serde_json::from_value(serde_json::json!({
            "id": seed.to_string().repeat(32),
            "name": format!("machine-{seed}"),
            "subnet": format!("10.210.{}.0/24", seed.to_digit(10).unwrap()),
            "management_address": format!("fdcc::{seed}"),
            "public_key": vec![seed as u8; 32],
        }))
        .unwrap();
        machine.public_ip = Some(public_ip.parse::<IpAddr>().unwrap());
        machine
    }
}
