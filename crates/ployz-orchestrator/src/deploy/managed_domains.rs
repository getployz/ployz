use std::collections::BTreeSet;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use hickory_client::client::{Client, ClientHandle};
use hickory_client::proto::rr::{DNSClass, Name, RData, Record, RecordType};
use hickory_client::proto::runtime::TokioRuntimeProvider;
use hickory_client::proto::udp::UdpClientStream;
use hickory_client::proto::xfer::DnsResponse;
use ployz_store_api::{CertificateStore, MachineStore, StoreDriver};
use ployz_types::model::{CertificateRecord, CertificateState, MachineLifecycle, MachineRecord};
use ployz_types::spec::RouteSpec;

use crate::deploy::plan::ResolvedPlan;
use crate::error::Result;

pub(super) const LETS_ENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
const MAX_AUTHORITY_WALK_DEPTH: usize = 12;
const MAX_NAMESERVERS_PER_STEP: usize = 6;
const ROOT_SERVER_IPS: &[IpAddr] = &[
    IpAddr::V4(Ipv4Addr::new(198, 41, 0, 4)),
    IpAddr::V4(Ipv4Addr::new(199, 9, 14, 201)),
    IpAddr::V4(Ipv4Addr::new(192, 33, 4, 12)),
    IpAddr::V4(Ipv4Addr::new(199, 7, 91, 13)),
    IpAddr::V4(Ipv4Addr::new(192, 203, 230, 10)),
    IpAddr::V4(Ipv4Addr::new(192, 5, 5, 241)),
    IpAddr::V4(Ipv4Addr::new(192, 112, 36, 4)),
    IpAddr::V4(Ipv4Addr::new(198, 97, 190, 53)),
    IpAddr::V4(Ipv4Addr::new(192, 36, 148, 17)),
    IpAddr::V4(Ipv4Addr::new(192, 58, 128, 30)),
    IpAddr::V4(Ipv4Addr::new(193, 0, 14, 129)),
    IpAddr::V4(Ipv4Addr::new(199, 7, 83, 42)),
    IpAddr::V4(Ipv4Addr::new(202, 12, 27, 33)),
];

pub(super) async fn warnings_for_plan(
    store: &StoreDriver,
    plan: &ResolvedPlan,
) -> Result<Vec<String>> {
    let domains = collect_exact_domains(plan);
    if domains.is_empty() {
        return Ok(Vec::new());
    }

    let machines = store.list_machines().await?;
    let certificates = store.list_certificates().await?;
    let resolver = SystemDnsResolver;
    Ok(build_domain_warnings(&domains, &machines, &certificates, &resolver).await)
}

pub(super) async fn ensure_certificate_intents(
    store: &StoreDriver,
    plan: &ResolvedPlan,
) -> Result<()> {
    let now = ployz_types::time::now_unix_secs();
    for hostname in collect_exact_domains(plan) {
        if store.get_certificate(&hostname).await?.is_some() {
            continue;
        }
        store
            .upsert_certificate(&CertificateRecord {
                hostname,
                issuer_url: LETS_ENCRYPT_PRODUCTION.to_string(),
                account_id: "letsencrypt-production".into(),
                state: CertificateState::Pending,
                active_version_id: None,
                versions: Vec::new(),
                last_error: None,
                requested_at: now,
                updated_at: now,
                next_renewal_at: None,
            })
            .await?;
    }
    Ok(())
}

fn collect_exact_domains(plan: &ResolvedPlan) -> Vec<String> {
    let mut domains = BTreeSet::new();
    for service in plan.services() {
        let Some(spec) = &service.spec else {
            continue;
        };
        for route in &spec.routes {
            let RouteSpec::Http(route) = route else {
                continue;
            };
            for hostname in &route.hostnames {
                let hostname = normalize_hostname(hostname);
                if hostname.is_empty() || hostname.starts_with("*.") {
                    continue;
                }
                domains.insert(hostname);
            }
        }
    }
    domains.into_iter().collect()
}

async fn build_domain_warnings<R>(
    domains: &[String],
    machines: &[MachineRecord],
    certificates: &[CertificateRecord],
    resolver: &R,
) -> Vec<String>
where
    R: DnsResolver + Sync,
{
    let expected_ips = expected_public_ips(machines);
    let mut warnings = Vec::new();
    for domain in domains {
        warnings.extend(dns_warnings_for_domain(domain, &expected_ips, resolver).await);
        warnings.extend(tls_warnings_for_domain(domain, certificates));
    }
    warnings
}

async fn dns_warnings_for_domain<R>(
    domain: &str,
    expected_ips: &[IpAddr],
    resolver: &R,
) -> Vec<String>
where
    R: DnsResolver + Sync,
{
    if expected_ips.is_empty() {
        return vec![format!(
            "DNS for {domain} cannot be checked because this cluster has no advertised public gateway endpoint"
        )];
    }

    let resolved_ips = resolver.resolve(domain).await;
    if resolved_ips.is_empty() {
        return vec![format!(
            "Authoritative DNS for {domain} has no A/AAAA answer. Add A records pointing to: {}",
            format_ips(expected_ips)
        )];
    }

    if resolved_ips.iter().any(|ip| expected_ips.contains(ip)) {
        return Vec::new();
    }

    vec![format!(
        "Authoritative DNS for {domain} points to {}, but this cluster currently advertises: {}",
        format_ips(&resolved_ips),
        format_ips(expected_ips)
    )]
}

fn tls_warnings_for_domain(domain: &str, certificates: &[CertificateRecord]) -> Vec<String> {
    let Some(record) = certificates.iter().find(|record| record.hostname == domain) else {
        return vec![format!(
            "TLS for {domain} is pending; HTTPS will activate when the certificate is ready"
        )];
    };

    match record.state {
        CertificateState::Active => Vec::new(),
        CertificateState::Pending | CertificateState::Issuing | CertificateState::RenewalDue => {
            vec![format!(
                "TLS for {domain} is {}; HTTPS will activate when the certificate is ready",
                record.state
            )]
        }
        CertificateState::Failed => {
            let detail = record
                .last_error
                .as_deref()
                .unwrap_or("certificate issuance failed");
            vec![format!("TLS for {domain} failed: {detail}")]
        }
    }
}

fn expected_public_ips(machines: &[MachineRecord]) -> Vec<IpAddr> {
    let mut ips = BTreeSet::new();
    for machine in machines {
        if machine.lifecycle != MachineLifecycle::Active {
            continue;
        }
        for endpoint in &machine.endpoints {
            let Some(ip) = endpoint_ip(endpoint) else {
                continue;
            };
            if is_public_ip(ip) {
                ips.insert(ip);
            }
        }
    }
    ips.into_iter().collect()
}

fn endpoint_ip(endpoint: &str) -> Option<IpAddr> {
    endpoint
        .parse::<SocketAddr>()
        .ok()
        .map(|socket_addr| socket_addr.ip())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_broadcast())
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_unicast_link_local()
        || ip.is_unique_local()
        || ip.is_multicast()
        || ip.is_unspecified())
}

fn normalize_hostname(hostname: &str) -> String {
    hostname.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn format_ips(ips: &[IpAddr]) -> String {
    ips.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

trait DnsResolver {
    fn resolve(&self, hostname: &str) -> impl Future<Output = Vec<IpAddr>> + Send;
}

struct SystemDnsResolver;

impl DnsResolver for SystemDnsResolver {
    async fn resolve(&self, hostname: &str) -> Vec<IpAddr> {
        AuthoritativeResolver::default().resolve(hostname).await
    }
}

#[derive(Default)]
struct AuthoritativeResolver;

impl AuthoritativeResolver {
    async fn resolve(&self, hostname: &str) -> Vec<IpAddr> {
        let hostname = normalize_hostname(hostname);
        if hostname.is_empty() {
            return Vec::new();
        }
        self.resolve_with_depth(&hostname, 0).await
    }

    async fn resolve_with_depth(&self, hostname: &str, depth: usize) -> Vec<IpAddr> {
        if depth > MAX_AUTHORITY_WALK_DEPTH {
            return Vec::new();
        }
        let Some(authorities) = self.find_authorities(hostname, depth).await else {
            return Vec::new();
        };
        let name = fqdn_name(hostname);
        let mut ips = Vec::new();
        ips.extend(self.query_ips(&authorities, &name, RecordType::A).await);
        ips.extend(self.query_ips(&authorities, &name, RecordType::AAAA).await);
        ips.sort();
        ips.dedup();
        ips
    }

    async fn find_authorities(&self, hostname: &str, depth: usize) -> Option<Vec<IpAddr>> {
        let labels = hostname.split('.').collect::<Vec<_>>();
        if labels.len() < 2 {
            return None;
        }
        let mut authorities = ROOT_SERVER_IPS.to_vec();
        for label_index in (0..labels.len()).rev() {
            let zone = labels[label_index..].join(".");
            let zone_name = fqdn_name(&zone);
            let Some(response) = query_first(&authorities, &zone_name, RecordType::NS).await else {
                continue;
            };
            let ns_names = extract_ns_names(response.answers())
                .into_iter()
                .chain(extract_ns_names(response.name_servers()))
                .collect::<Vec<_>>();
            if ns_names.is_empty() {
                continue;
            }
            let mut next_authorities = glue_ips(response.additionals(), &ns_names);
            if next_authorities.is_empty() {
                next_authorities = self.resolve_ns_names(&ns_names, depth + 1).await;
            }
            if !next_authorities.is_empty() {
                authorities = next_authorities;
            }
        }
        Some(authorities)
    }

    async fn resolve_ns_names(&self, ns_names: &[Name], depth: usize) -> Vec<IpAddr> {
        let mut ips = Vec::new();
        for ns_name in ns_names.iter().take(MAX_NAMESERVERS_PER_STEP) {
            ips.extend(
                Box::pin(self.resolve_with_depth(&name_without_trailing_dot(ns_name), depth)).await,
            );
        }
        ips.sort();
        ips.dedup();
        ips
    }

    async fn query_ips(
        &self,
        authorities: &[IpAddr],
        name: &Name,
        record_type: RecordType,
    ) -> Vec<IpAddr> {
        let Some(response) = query_first(authorities, name, record_type).await else {
            return Vec::new();
        };
        let mut ips = Vec::new();
        for record in response.answers() {
            match record.data() {
                RData::A(address) => ips.push(IpAddr::V4((*address).into())),
                RData::AAAA(address) => ips.push(IpAddr::V6((*address).into())),
                RData::CNAME(target) => {
                    ips.extend(
                        Box::pin(self.resolve_with_depth(&name_without_trailing_dot(target), 0))
                            .await,
                    );
                }
                RData::ANAME(target) => {
                    ips.extend(
                        Box::pin(self.resolve_with_depth(&name_without_trailing_dot(target), 0))
                            .await,
                    );
                }
                _ => {}
            }
        }
        ips.sort();
        ips.dedup();
        ips
    }
}

async fn query_first(
    authorities: &[IpAddr],
    name: &Name,
    record_type: RecordType,
) -> Option<DnsResponse> {
    for authority in authorities.iter().take(MAX_NAMESERVERS_PER_STEP) {
        if let Some(response) = query_authority(*authority, name, record_type).await {
            return Some(response);
        }
    }
    None
}

async fn query_authority(
    authority: IpAddr,
    name: &Name,
    record_type: RecordType,
) -> Option<DnsResponse> {
    let conn = UdpClientStream::builder(
        SocketAddr::new(authority, 53),
        TokioRuntimeProvider::default(),
    )
    .build();
    let Ok((mut client, background)) = Client::connect(conn).await else {
        return None;
    };
    tokio::spawn(background);
    client
        .query(name.clone(), DNSClass::IN, record_type)
        .await
        .ok()
}

fn extract_ns_names(records: &[Record]) -> Vec<Name> {
    let mut names = records
        .iter()
        .filter_map(|record| match record.data() {
            RData::NS(name) => Some(name.0.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn glue_ips(records: &[Record], ns_names: &[Name]) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for record in records {
        if !ns_names.iter().any(|name| name.eq_case(record.name())) {
            continue;
        }
        match record.data() {
            RData::A(address) => ips.push(IpAddr::V4((*address).into())),
            RData::AAAA(address) => ips.push(IpAddr::V6((*address).into())),
            _ => {}
        }
    }
    ips.sort();
    ips.dedup();
    ips
}

fn fqdn_name(name: &str) -> Name {
    Name::from_str(&format!("{name}.")).unwrap_or_else(|_| Name::root())
}

fn name_without_trailing_dot(name: &Name) -> String {
    name.to_utf8().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{MachineId, OverlayIp, PublicKey};
    use std::collections::BTreeMap;

    struct FakeResolver {
        ips: Vec<IpAddr>,
    }

    impl DnsResolver for FakeResolver {
        async fn resolve(&self, _hostname: &str) -> Vec<IpAddr> {
            self.ips.clone()
        }
    }

    #[test]
    fn endpoint_parsing_keeps_public_ips_only() {
        let machines = vec![
            machine("active", MachineLifecycle::Active, &["8.8.8.8:51820"]),
            machine("private", MachineLifecycle::Active, &["10.0.0.1:51820"]),
            machine("standby", MachineLifecycle::Standby, &["1.1.1.1:51820"]),
        ];

        assert_eq!(
            expected_public_ips(&machines),
            vec![IpAddr::from([8, 8, 8, 8])]
        );
    }

    #[tokio::test]
    async fn dns_warning_reports_missing_record() {
        let warnings = dns_warnings_for_domain(
            "example.com",
            &[IpAddr::from([8, 8, 8, 8])],
            &FakeResolver { ips: Vec::new() },
        )
        .await;

        assert_eq!(
            warnings,
            vec![
                "Authoritative DNS for example.com has no A/AAAA answer. Add A records pointing to: 8.8.8.8"
            ]
        );
    }

    #[tokio::test]
    async fn dns_warning_is_empty_when_record_matches() {
        let warnings = dns_warnings_for_domain(
            "example.com",
            &[IpAddr::from([8, 8, 8, 8])],
            &FakeResolver {
                ips: vec![IpAddr::from([8, 8, 8, 8])],
            },
        )
        .await;

        assert!(warnings.is_empty());
    }

    #[tokio::test]
    async fn dns_warning_reports_mismatch() {
        let warnings = dns_warnings_for_domain(
            "example.com",
            &[IpAddr::from([8, 8, 8, 8])],
            &FakeResolver {
                ips: vec![IpAddr::from([1, 1, 1, 1])],
            },
        )
        .await;

        assert_eq!(
            warnings,
            vec![
                "Authoritative DNS for example.com points to 1.1.1.1, but this cluster currently advertises: 8.8.8.8"
            ]
        );
    }

    fn machine(id: &str, lifecycle: MachineLifecycle, endpoints: &[&str]) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("overlay ip")),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: endpoints
                .iter()
                .map(|endpoint| endpoint.to_string())
                .collect(),
            lifecycle,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }
}
