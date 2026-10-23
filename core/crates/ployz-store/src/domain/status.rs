//! A domain's status: Ready, Setting up or Needs attention, with at most one
//! action, projected from what Cloud, or a standalone CLI, observed of DNS,
//! certificates and Servers.

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
    let records = match &domain.name {
        DomainName::Custom { hostname } => dns_records(hostname, &trusted.domains),
        DomainName::Generated { .. } => Vec::new(),
    };
    DomainRow {
        domain,
        status,
        reason,
        action,
        records,
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

const PORT_80_HINT: &str = "Open port 80 in the Server's firewall (for example ufw allow 80/tcp)";

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
    // Every domain lands on the same ingress Servers. Without a Cluster Domain, whose
    // status says it, the Runtime Watch's ingress Servers do.
    let standalone = match evidence.ingress_servers {
        Some(0) => Some(ClusterDomainStatus::NoServers),
        Some(_) if evidence.ingress_addresses.is_empty() => Some(ClusterDomainStatus::NoPublicIp),
        _ => None,
    };
    let cluster = evidence
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.status)
        .or(standalone.as_ref());
    match cluster {
        Some(ClusterDomainStatus::NoServers) => {
            return attention("No Server receives traffic", Some(DomainAction::AddServer));
        }
        Some(ClusterDomainStatus::NoPublicIp) => {
            return attention("No ingress Server has a public IP", None);
        }
        Some(ClusterDomainStatus::IngressDown { servers }) => {
            return attention(
                format!(
                    "Ingress isn't running on {}. Deploy to bring it back.",
                    servers.join(", ")
                ),
                Some(DomainAction::Deploy),
            );
        }
        _ => {}
    }
    let port_80 = match cluster {
        Some(ClusterDomainStatus::Port80 { addresses }) => Some(attention(
            format!(
                "Port 80 is closed on {}. {PORT_80_HINT}",
                addresses.join(", ")
            ),
            None,
        )),
        Some(ClusterDomainStatus::Port80Taken { addresses }) => Some(attention(
            format!(
                "Something else answers on port 80 of {}",
                addresses.join(", ")
            ),
            None,
        )),
        _ => None,
    };
    let hostname = match &domain.name {
        DomainName::Generated { .. } => {
            if let Some(blocked) = port_80 {
                return blocked;
            }
            return match cluster {
                Some(ClusterDomainStatus::Ready) => (DomainStatus::Ready, None, None),
                Some(ClusterDomainStatus::HttpsDown) => {
                    setting_up("HTTPS is down; Ployz is fixing it")
                }
                _ => setting_up("Setting up"),
            };
        }
        DomainName::Custom { hostname } => hostname,
    };
    let observed = observed_certificate(hostname, evidence);
    let certificate = observed.flatten();
    let ready = certificate
        .is_some_and(|certificate| certificate.status == CertificateAvailability::Available);
    let lookup = evidence
        .lookups
        .iter()
        .find(|lookup| lookup.hostname == *hostname);
    let points_here = lookup.and_then(|lookup| points_here(lookup, evidence));
    let failure = certificate
        .filter(|certificate| certificate.status == CertificateAvailability::Failure)
        .and_then(|certificate| certificate.backoff.as_ref())
        .map(|backoff| &backoff.failure_kind);
    // DNS sent elsewhere comes before the Servers' port 80: that port is not what fails.
    let misdirected = match (points_here, failure) {
        _ if ready => None,
        (Some(false), _) if lookup.is_some_and(|lookup| !lookup.addresses.is_empty()) => {
            Some("DNS points to another server")
        }
        (Some(false), _) => Some("DNS doesn't point here yet"),
        (Some(true), _) => None,
        (None, Some(CertificateFailureKind::DoesNotResolve)) => Some("Waiting for DNS"),
        (None, Some(CertificateFailureKind::ReachesElsewhere)) => {
            Some("DNS points to another server")
        }
        (None, _) => None,
    };
    if let Some(reason) = misdirected {
        return attention(reason, dns_action(hostname, evidence));
    }
    if let Some(blocked) = port_80 {
        return blocked;
    }
    if observed.is_none() {
        return setting_up("Ployz can't see the Servers right now");
    }
    if ready {
        let proxy = certificate.is_some_and(|certificate| certificate.via_proxy);
        return (
            DomainStatus::Ready,
            proxy.then(|| "via proxy".to_owned()),
            None,
        );
    }
    let retry = || {
        certificate
            .and_then(|certificate| certificate.backoff.as_ref())
            .map(|backoff| backoff.next_attempt_at.as_str())
            .map_or_else(|| "shortly".to_owned(), |at| format!("at {at}"))
    };
    let Some(failure) = failure else {
        return if certificate
            .is_some_and(|certificate| certificate.status == CertificateAvailability::Failure)
        {
            setting_up(format!("Certificate failed; retrying {}", retry()))
        } else {
            setting_up("Issuing certificate")
        };
    };
    match failure {
        CertificateFailureKind::DoesNotResolve | CertificateFailureKind::ReachesElsewhere => {
            setting_up(format!(
                "DNS points here now; the certificate is retried {}",
                retry()
            ))
        }
        CertificateFailureKind::Unreachable => {
            attention(format!("Port 80 is closed. {PORT_80_HINT}"), None)
        }
        CertificateFailureKind::RedirectsToHttps => attention(
            "Your proxy redirects to HTTPS: exempt /.well-known/acme-challenge/* from it",
            None,
        ),
        CertificateFailureKind::Authority | CertificateFailureKind::Unrecognized(_) => {
            setting_up(format!("Certificate failed; retrying {}", retry()))
        }
    }
}

/// Show the records that point a custom domain here, when the evidence names them.
fn dns_action(hostname: &Hostname, evidence: &DomainEvidence) -> Option<DomainAction> {
    let records = dns_records(hostname, evidence);
    (!records.is_empty()).then_some(DomainAction::Dns { records })
}

/// A custom hostname's certificate: its own, else the `*.parent` wildcard that serves it.
/// None when it can't be told: the Servers couldn't be seen, or the row of the certificate
/// that would serve it couldn't be read.
fn observed_certificate<'evidence>(
    hostname: &Hostname,
    evidence: &'evidence DomainEvidence,
) -> Option<Option<&'evidence CertificateObservation>> {
    let certificates = evidence.certificates.as_deref()?;
    let Ok(host) = IngressHost::parse(hostname.as_str()) else {
        return Some(None);
    };
    let covers = |certificate: &CertificateHost, wildcard: bool| {
        certificate.is_wildcard() == wildcard && certificate.covers(&host)
    };
    let unread = |wildcard| {
        evidence
            .incomplete_certificates
            .iter()
            .any(|certificate| covers(certificate, wildcard))
    };
    let find = |wildcard| {
        certificates
            .iter()
            .find(|certificate| covers(&certificate.hostname, wildcard))
    };
    if let Some(own) = find(false) {
        return Some(Some(own));
    }
    if unread(false) {
        return None;
    }
    match find(true) {
        None if unread(true) => None,
        wildcard => Some(wildcard),
    }
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
            ..DomainEvidence::default()
        }
    }

    fn certificate_host(name: &str) -> CertificateHost {
        serde_json::from_value(serde_json::json!(name)).unwrap()
    }

    #[test]
    fn a_certificate_whose_row_is_unread_reads_unobserved_not_absent() {
        let domain = custom("app.example.com");
        let unobserved = setting_up("Ployz can't see the Servers right now");
        let mut own = evidence(None);
        own.incomplete_certificates = vec![certificate_host("app.example.com")];
        assert_eq!(status(&domain, Deployed::Yes, &own), unobserved);
        let mut wildcard = evidence(None);
        wildcard.incomplete_certificates = vec![certificate_host("*.example.com")];
        assert_eq!(status(&domain, Deployed::Yes, &wildcard), unobserved);
        // Its own certificate serves it, whatever the wildcard's row says.
        let mut issued = evidence(Some(("available", None)));
        issued.incomplete_certificates = vec![certificate_host("*.example.com")];
        assert_eq!(
            status(&domain, Deployed::Yes, &issued).0,
            DomainStatus::Ready
        );
        // Another hostname's unread row says nothing about this one.
        let mut other = evidence(None);
        other.incomplete_certificates = vec![certificate_host("shop.example.org")];
        assert_eq!(
            status(&domain, Deployed::Yes, &other),
            setting_up("Issuing certificate")
        );
    }

    #[test]
    fn without_a_cluster_domain_the_ingress_servers_say_whether_traffic_lands() {
        let domain = custom("app.example.com");
        let standalone = |servers: Option<u32>, addresses: &[&str]| {
            let mut evidence = evidence(Some(("available", None)));
            evidence.cluster_domain = None;
            evidence.ingress_servers = servers;
            evidence.ingress_addresses = addresses.iter().map(|&address| address.into()).collect();
            status(&domain, Deployed::Yes, &evidence)
        };
        assert_eq!(
            standalone(Some(0), &[]),
            attention("No Server receives traffic", Some(DomainAction::AddServer))
        );
        assert_eq!(
            standalone(Some(1), &[]),
            attention("No ingress Server has a public IP", None)
        );
        assert_eq!(standalone(Some(1), &["203.0.113.7"]).0, DomainStatus::Ready);
        // Unobserved Servers say nothing, as before.
        assert_eq!(standalone(None, &[]).0, DomainStatus::Ready);
        // Cloud's Cluster Domain status speaks for the Servers when there is one.
        let mut cloud = evidence(Some(("available", None)));
        cloud.ingress_servers = Some(0);
        assert_eq!(
            status(&domain, Deployed::Yes, &cloud).0,
            DomainStatus::Ready
        );
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
        let unreachable = evidence(Some(("failure", Some("unreachable"))));
        assert_eq!(
            status(&domain, Deployed::Yes, &unreachable),
            (
                DomainStatus::NeedsAttention,
                Some(
                    "Port 80 is closed. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)"
                        .into()
                ),
                None
            )
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
        for (port_80, reason) in [
            (
                ClusterDomainStatus::Port80 {
                    addresses: vec!["203.0.113.7".into()],
                },
                "Port 80 is closed on 203.0.113.7. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)",
            ),
            (
                ClusterDomainStatus::Port80Taken {
                    addresses: vec!["203.0.113.7".into(), "2001:db8::1".into()],
                },
                "Something else answers on port 80 of 203.0.113.7, 2001:db8::1",
            ),
        ] {
            let mut blocked = ready.clone();
            blocked.cluster_domain = blocked.cluster_domain.map(|cluster| ClusterDomain {
                status: port_80,
                ..cluster
            });
            assert_eq!(
                status(&domain, Deployed::Yes, &blocked),
                (DomainStatus::NeedsAttention, Some(reason.into()), None)
            );
        }
        let mut down = ready.clone();
        down.cluster_domain = down.cluster_domain.map(|cluster| ClusterDomain {
            status: ClusterDomainStatus::IngressDown {
                servers: vec!["web-1".into()],
            },
            ..cluster
        });
        assert_eq!(
            status(&domain, Deployed::Yes, &down),
            (
                DomainStatus::NeedsAttention,
                Some("Ingress isn't running on web-1. Deploy to bring it back.".into()),
                Some(DomainAction::Deploy)
            )
        );
    }

    #[test]
    fn dns_sent_elsewhere_shows_before_a_closed_port_80_with_its_records() {
        let domain = custom("app.example.com");
        let records = Some(DomainAction::Dns {
            records: vec![DnsRecord {
                kind: DnsRecordKind::Cname,
                name: "app".into(),
                value: "acme.ployz.app".into(),
            }],
        });
        let closed = |certificate| {
            let mut evidence = evidence(certificate);
            evidence.cluster_domain = evidence.cluster_domain.map(|cluster| ClusterDomain {
                status: ClusterDomainStatus::Port80 {
                    addresses: vec!["203.0.113.7".into()],
                },
                ..cluster
            });
            evidence
        };
        let mut looked_up = closed(None);
        looked_up.lookups.push(DnsLookup {
            hostname: host("app.example.com"),
            cname: Some("redirect.example.net.".into()),
            addresses: vec!["202.58.206.59".into()],
        });
        assert_eq!(
            status(&domain, Deployed::Yes, &looked_up),
            (
                DomainStatus::NeedsAttention,
                Some("DNS points to another server".into()),
                records.clone()
            )
        );
        let refused = closed(Some(("failure", Some("reaches_elsewhere"))));
        assert_eq!(
            status(&domain, Deployed::Yes, &refused),
            (
                DomainStatus::NeedsAttention,
                Some("DNS points to another server".into()),
                records
            )
        );
        let pending = closed(Some(("pending", None)));
        assert_eq!(
            status(&domain, Deployed::Yes, &pending).1.as_deref(),
            Some(
                "Port 80 is closed on 203.0.113.7. Open port 80 in the Server's firewall (for example ufw allow 80/tcp)"
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
