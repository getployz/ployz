//! A domain's status: Ready, Setting up or Needs attention, with at most one
//! action, projected from what Cloud observed of DNS, certificates and Servers.

use super::*;

/// Whether `service` in `intent` has `domain`.
fn holds(intent: &SavedEnvironmentIntent, service: &str, domain: &Domain) -> bool {
    intent
        .services
        .iter()
        .filter(|node| node.id == service)
        .any(|node| match &domain.name {
            DomainName::Custom { hostname } => node
                .config
                .routes
                .iter()
                .any(|route| route.hostname == hostname.as_str()),
            DomainName::Generated { prefix, .. } => node
                .config
                .managed_hostnames
                .iter()
                .any(|managed| managed.prefix == *prefix),
        })
}

pub(super) fn row(
    domain: Domain,
    working: &SavedEnvironmentIntent,
    head: &crate::review::Head,
    trusted: &Trusted,
) -> DomainRow {
    let id = working
        .services
        .iter()
        .find(|service: &&SavedServiceIntent| service.slug == domain.service.as_str())
        .map(|service| service.id.clone())
        .unwrap_or_default();
    let deployed = if holds(&head.applied, &id, &domain) {
        Deployed::Yes
    } else if holds(&head.intent, &id, &domain) {
        Deployed::Deploying
    } else {
        Deployed::No
    };
    let (status, reason, action) = status(&domain, deployed, &trusted.domains);
    DomainRow {
        domain,
        status,
        reason,
        action,
    }
}

/// Whether Applied State serves a domain yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Deployed {
    Yes,
    /// A Deployment in flight ships it.
    Deploying,
    No,
}

type Status = (DomainStatus, Option<String>, Option<DomainAction>);

fn setting_up(reason: impl Into<String>) -> Status {
    (DomainStatus::SettingUp, Some(reason.into()), None)
}

fn attention(reason: impl Into<String>, action: Option<DomainAction>) -> Status {
    (DomainStatus::NeedsAttention, Some(reason.into()), action)
}

/// A domain's status: only what its user can act on needs attention; what Ployz or
/// time fixes is setting up.
fn status(domain: &Domain, deployed: Deployed, evidence: &DomainEvidence) -> Status {
    match deployed {
        Deployed::Yes => {}
        Deployed::Deploying => return setting_up("Deploying"),
        Deployed::No => {
            return (
                DomainStatus::SettingUp,
                Some("Live after your next deploy".into()),
                Some(DomainAction::Deploy),
            );
        }
    }
    // Every domain lands on the same ingress Servers.
    match evidence
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.status)
    {
        Some(ClusterDomainStatus::NoServers) => {
            return attention("No Server receives traffic", Some(DomainAction::AddServer));
        }
        Some(ClusterDomainStatus::NoPublicIp) => {
            return attention("No ingress Server has a public IP", None);
        }
        Some(ClusterDomainStatus::Port80 { addresses }) => {
            return attention(
                format!("Port 80 is closed on {}", addresses.join(", ")),
                None,
            );
        }
        _ => {}
    }
    let hostname = match &domain.name {
        DomainName::Generated { .. } => {
            return match evidence
                .cluster_domain
                .as_ref()
                .map(|cluster| &cluster.status)
            {
                Some(ClusterDomainStatus::Ready) => (DomainStatus::Ready, None, None),
                Some(ClusterDomainStatus::HttpsDown) => {
                    setting_up("HTTPS is down; Ployz is fixing it")
                }
                _ => setting_up("Setting up"),
            };
        }
        DomainName::Custom { hostname } => hostname,
    };
    let Some(certificates) = &evidence.certificates else {
        return setting_up("Cloud can't see the Servers right now");
    };
    let certificate = certificate_for(hostname, certificates);
    if certificate
        .is_some_and(|certificate| certificate.status == CertificateAvailability::Available)
    {
        let proxy = certificate.is_some_and(|certificate| certificate.via_proxy);
        return (
            DomainStatus::Ready,
            proxy.then(|| "via proxy".to_owned()),
            None,
        );
    }
    let points_here = evidence
        .lookups
        .iter()
        .find(|lookup| lookup.hostname == *hostname)
        .and_then(|lookup| points_here(lookup, evidence));
    let dns = || {
        let records = dns_records(hostname, evidence);
        (!records.is_empty()).then_some(DomainAction::Dns { records })
    };
    if points_here == Some(false) {
        return attention("DNS doesn't point here yet", dns());
    }
    let retry = || {
        certificate
            .and_then(|certificate| certificate.backoff.as_ref())
            .map(|backoff| backoff.next_attempt_at.as_str())
            .map_or_else(|| "shortly".to_owned(), |at| format!("at {at}"))
    };
    let Some(certificate) =
        certificate.filter(|certificate| certificate.status == CertificateAvailability::Failure)
    else {
        return setting_up("Issuing certificate");
    };
    let failure = certificate
        .backoff
        .as_ref()
        .map(|backoff| &backoff.failure_kind);
    match failure {
        Some(CertificateFailureKind::DoesNotResolve | CertificateFailureKind::ReachesElsewhere)
            if points_here == Some(true) =>
        {
            setting_up(format!(
                "DNS points here now; the certificate is retried {}",
                retry()
            ))
        }
        Some(CertificateFailureKind::DoesNotResolve) => attention("Waiting for DNS", dns()),
        Some(CertificateFailureKind::ReachesElsewhere) => {
            attention("DNS points to another server", dns())
        }
        Some(CertificateFailureKind::Unreachable) => attention("Port 80 is closed", None),
        Some(CertificateFailureKind::RedirectsToHttps) => attention(
            "Your proxy redirects to HTTPS: exempt /.well-known/acme-challenge/* from it",
            None,
        ),
        _ => setting_up(format!("Certificate failed; retrying {}", retry())),
    }
}

/// A custom hostname's certificate: its own, else the `*.parent` wildcard that serves it.
fn certificate_for<'evidence>(
    hostname: &Hostname,
    certificates: &'evidence [CertificateObservation],
) -> Option<&'evidence CertificateObservation> {
    let host = IngressHost::parse(hostname.as_str()).ok()?;
    let covers = |certificate: &&CertificateObservation, wildcard: bool| {
        certificate.hostname.is_wildcard() == wildcard && certificate.hostname.covers(&host)
    };
    certificates
        .iter()
        .find(|certificate| covers(certificate, false))
        .or_else(|| {
            certificates
                .iter()
                .find(|certificate| covers(certificate, true))
        })
}

/// Whether DNS sends `lookup`'s hostname to these Servers; none when the evidence
/// can't tell.
fn points_here(lookup: &DnsLookup, evidence: &DomainEvidence) -> Option<bool> {
    let cluster = evidence
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    if let (Some(cname), Some(cluster)) = (&lookup.cname, cluster)
        && cname
            .trim_end_matches('.')
            .eq_ignore_ascii_case(cluster.as_str())
    {
        return Some(true);
    }
    if lookup.addresses.is_empty() {
        return lookup.cname.is_none().then_some(false);
    }
    if evidence.ingress_addresses.is_empty() {
        return None;
    }
    Some(
        lookup
            .addresses
            .iter()
            .any(|address| evidence.ingress_addresses.contains(address)),
    )
}

/// The records that point `hostname` here: a CNAME to the Cluster Domain, which
/// follows the ingress Servers, or A/AAAA records to their addresses at an apex or
/// without a Cluster Domain.
fn dns_records(hostname: &Hostname, evidence: &DomainEvidence) -> Vec<DnsRecord> {
    // ponytail: the registrable domain is taken as the last two labels; a public-suffix
    // list makes example.co.uk right.
    let labels = hostname.as_str().split('.').collect::<Vec<_>>();
    let name = match labels.len().checked_sub(2) {
        Some(sub @ 1..) => labels.get(..sub).unwrap_or_default().join("."),
        _ => "@".to_owned(),
    };
    if let Some(cluster) = &evidence.cluster_domain
        && name != "@"
    {
        return vec![DnsRecord {
            kind: DnsRecordKind::Cname,
            name,
            value: cluster.name.to_string(),
        }];
    }
    evidence
        .ingress_addresses
        .iter()
        .map(|address| DnsRecord {
            kind: if address.contains(':') {
                DnsRecordKind::Aaaa
            } else {
                DnsRecordKind::A
            },
            name: name.clone(),
            value: address.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> Hostname {
        Hostname::parse(name).unwrap()
    }

    fn custom(name: &str) -> Domain {
        Domain {
            service: ServiceName::parse("web").unwrap(),
            name: DomainName::Custom {
                hostname: host(name),
            },
            port: None,
        }
    }

    fn evidence(certificate: Option<(&str, Option<&str>)>) -> DomainEvidence {
        DomainEvidence {
            custom_domains: true,
            cluster_domain: Some(ClusterDomain {
                name: host("acme.ployz.app"),
                status: ClusterDomainStatus::Ready,
            }),
            certificates: Some(
                certificate
                    .map(|(status, failure)| {
                        serde_json::from_value(serde_json::json!({
                            "hostname": "app.example.com",
                            "status": status,
                            "backoff": failure.map(|kind| serde_json::json!({
                                "failure_kind": kind,
                                "next_attempt_at": "2026-09-29T12:00:00Z",
                                "failures": 1,
                            })),
                        }))
                        .unwrap()
                    })
                    .into_iter()
                    .collect(),
            ),
            ingress_addresses: vec!["203.0.113.7".into()],
            lookups: Vec::new(),
            published: Vec::new(),
        }
    }

    #[test]
    fn a_dns_refusal_needs_attention_until_a_fresh_lookup_points_here() {
        let domain = custom("app.example.com");
        let mut evidence = evidence(Some(("failure", Some("does_not_resolve"))));
        let (seen, _, action) = status(&domain, Deployed::Yes, &evidence);
        assert_eq!(seen, DomainStatus::NeedsAttention);
        assert_eq!(
            action,
            Some(DomainAction::Dns {
                records: vec![DnsRecord {
                    kind: DnsRecordKind::Cname,
                    name: "app".into(),
                    value: "acme.ployz.app".into(),
                }]
            })
        );
        evidence.lookups.push(DnsLookup {
            hostname: host("app.example.com"),
            cname: Some("acme.ployz.app.".into()),
            addresses: vec!["203.0.113.7".into()],
        });
        let (seen, reason, action) = status(&domain, Deployed::Yes, &evidence);
        assert_eq!(seen, DomainStatus::SettingUp);
        assert_eq!(
            reason.as_deref(),
            Some("DNS points here now; the certificate is retried at 2026-09-29T12:00:00Z")
        );
        assert_eq!(action, None);
    }

    #[test]
    fn a_fresh_lookup_elsewhere_needs_dns_even_while_issuing() {
        let domain = custom("example.com");
        let mut evidence = evidence(None);
        evidence.lookups.push(DnsLookup {
            hostname: host("example.com"),
            cname: None,
            addresses: vec!["198.51.100.1".into()],
        });
        let (seen, _, action) = status(&domain, Deployed::Yes, &evidence);
        assert_eq!(seen, DomainStatus::NeedsAttention);
        let Some(DomainAction::Dns { records }) = action else {
            panic!("expected DNS records");
        };
        assert_eq!(
            records.first().map(|record| record.kind),
            Some(DnsRecordKind::A)
        );
        assert_eq!(
            records.first().map(|record| record.name.as_str()),
            Some("@")
        );
    }

    #[test]
    fn statuses_follow_deploys_certificates_and_the_cluster_domain() {
        let domain = custom("app.example.com");
        let ready = evidence(Some(("available", None)));
        assert_eq!(
            status(&domain, Deployed::No, &ready),
            (
                DomainStatus::SettingUp,
                Some("Live after your next deploy".into()),
                Some(DomainAction::Deploy)
            )
        );
        assert_eq!(
            status(&domain, Deployed::Deploying, &ready).0,
            DomainStatus::SettingUp
        );
        assert_eq!(
            status(&domain, Deployed::Yes, &ready).0,
            DomainStatus::Ready
        );
        let pending = evidence(Some(("pending", None)));
        assert_eq!(
            status(&domain, Deployed::Yes, &pending).0,
            DomainStatus::SettingUp
        );
        let authority = evidence(Some(("failure", Some("authority"))));
        assert_eq!(
            status(&domain, Deployed::Yes, &authority).0,
            DomainStatus::SettingUp
        );
        let mut offline = ready.clone();
        offline.cluster_domain = offline.cluster_domain.map(|cluster| ClusterDomain {
            status: ClusterDomainStatus::NoServers,
            ..cluster
        });
        assert_eq!(
            status(&domain, Deployed::Yes, &offline),
            (
                DomainStatus::NeedsAttention,
                Some("No Server receives traffic".into()),
                Some(DomainAction::AddServer)
            )
        );
    }

    #[test]
    fn free_prefixes_count_up_and_stay_labels() {
        assert_eq!(free_prefix("web", &[]), "web");
        assert_eq!(free_prefix("web", &["web", "web-2"]), "web-3");
        let long = "a".repeat(63);
        let next = free_prefix(&long, &[long.as_str()]);
        assert_eq!(next.len(), 63);
        assert!(next.ends_with("-2"));
    }
}
