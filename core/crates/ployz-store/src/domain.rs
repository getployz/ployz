//! Public domains. A Service gets a generated hostname, `PREFIX.CLUSTER-DOMAIN`, or a
//! custom one, which only an Organization with the custom-domain capability may add.
//! Adding and removing either is staged until a Deploy; admission expands generated
//! ones into routes under the Cluster Domain Cloud reserved for the Organization.
//!
//! The Store never looks at DNS, certificates or Servers. Cloud observes them and
//! passes what it saw as [`DomainEvidence`]; from that, each domain reads Ready,
//! Setting up or Needs attention, with at most one action.

use ployz_core::config::{
    SavedEnvironmentIntent, SavedServiceIntent, ServiceManagedHostname, ServiceRoute,
    parse_environment_intent,
};
use ployz_core::{
    CertificateAvailability, CertificateFailureKind, CertificateObservation, IngressHost, RpcError,
    RpcErrorCode, ServiceName,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::id::Hostname;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::SettingPath;
use crate::storage::Tx;
use crate::{Actor, Trusted};

/// What Cloud observed of the Organization's public traffic, passed in-process with a
/// domain read or write. Empty unless Cloud supplies it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ConfigDomainEvidence")]
pub struct DomainEvidence {
    /// Whether the Organization may add custom domains: Pro, or a self-hosted Cloud.
    #[serde(default)]
    pub custom_domains: bool,
    /// The Organization's Cluster Domain, once reserved.
    #[serde(default)]
    pub cluster_domain: Option<ClusterDomain>,
    /// The Cluster's certificates as its Runtime Watch reported them; none when Cloud
    /// couldn't observe the Cluster.
    #[serde(default)]
    pub certificates: Option<Vec<CertificateObservation>>,
    /// Public addresses of the Servers that accept ingress.
    #[serde(default)]
    pub ingress_addresses: Vec<String>,
    /// What DNS answered just now, for the hostnames a check looked up.
    #[serde(default)]
    pub lookups: Vec<DnsLookup>,
}

/// The Organization's Cluster Domain: generated hostnames live one label under it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ClusterDomain {
    pub name: Hostname,
    pub status: ClusterDomainStatus,
}

/// What Cloud last found about the Cluster Domain and the Servers behind it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClusterDomainStatus {
    /// Its records or wildcard certificate aren't published yet.
    SettingUp,
    Ready,
    /// No Server carries traffic.
    NoServers,
    /// No ingress Server has a public address.
    NoPublicIp,
    /// These ingress Servers don't answer on port 80.
    #[serde(rename = "port_80")]
    Port80 {
        addresses: Vec<String>,
    },
    /// Its wildcard certificate expired.
    HttpsDown,
}

/// What DNS answered for one hostname.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DnsLookup {
    pub hostname: Hostname,
    #[serde(default)]
    pub cname: Option<String>,
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// Give a Service a public domain: `hostname` for a custom one, none for a generated
/// one under the Cluster Domain. Adding one it already has changes only its port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AddDomain {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub service: ServiceName,
    /// A custom hostname; none generates one.
    #[serde(default)]
    pub hostname: Option<Hostname>,
    /// The container port it reaches; none follows the container's `PORT`.
    #[serde(default)]
    pub port: Option<u16>,
}

/// Take a public domain off its Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveDomain {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its hostname, or a generated domain's prefix.
    pub domain: String,
}

/// A domain added or removed in Working State, staged until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DomainStaged {
    pub environment: EnvironmentSummary,
    pub domain: Domain,
    /// Its Service, when this changed it; empty when it already was so.
    pub staged: Vec<SettingPath>,
    /// What took effect at once: never anything here.
    pub immediate: Vec<SettingPath>,
}

/// A Service's public domain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Domain {
    pub service: ServiceName,
    #[serde(flatten)]
    pub name: DomainName,
    /// The container port it reaches; none follows the container's `PORT`.
    pub port: Option<u16>,
}

/// Which kind of domain, and its hostname.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainName {
    /// One label under the Cluster Domain; its hostname is known once Cloud reserved one.
    Generated {
        prefix: String,
        hostname: Option<String>,
    },
    /// A hostname the user owns.
    Custom { hostname: Hostname },
}

impl Domain {
    /// Whether users mean it by `name`: its hostname, or a generated domain's prefix.
    fn is_named(&self, name: &str) -> bool {
        let name = name.trim().to_ascii_lowercase();
        match &self.name {
            DomainName::Generated { prefix, hostname } => {
                *prefix == name || hostname.as_ref().is_some_and(|hostname| *hostname == name)
            }
            DomainName::Custom { hostname } => hostname.as_str() == name,
        }
    }

    /// The name users address it by: its hostname, else a generated domain's prefix.
    #[must_use]
    pub fn shown(&self) -> &str {
        match &self.name {
            DomainName::Generated {
                hostname: Some(hostname),
                ..
            } => hostname,
            DomainName::Generated { prefix, .. } => prefix,
            DomainName::Custom { hostname } => hostname.as_str(),
        }
    }
}

/// An Environment's public domains.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DomainsQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Only this Service's.
    #[serde(default)]
    pub service: Option<ServiceName>,
}

/// One domain, observed afresh: Cloud re-checks its DNS and the Cluster Domain first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DomainQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its hostname, or a generated domain's prefix.
    pub domain: String,
}

/// An Environment's public domains, each with its status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DomainsView {
    pub environment: EnvironmentSummary,
    pub domains: Vec<DomainRow>,
}

/// One domain with its status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DomainView {
    pub environment: EnvironmentSummary,
    pub domain: DomainRow,
}

/// A domain, what state it is in, and the one thing to do about it, if any.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DomainRow {
    #[serde(flatten)]
    pub domain: Domain,
    pub status: DomainStatus,
    /// One short phrase on why, when it isn't plainly ready.
    pub reason: Option<String>,
    pub action: Option<DomainAction>,
}

/// Where a domain is, from its user's side.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DomainStatus {
    /// It serves HTTPS.
    Ready,
    /// It gets there without the user: a Deploy, DNS or a certificate is under way.
    SettingUp,
    /// It won't get there until the user acts.
    NeedsAttention,
}

/// The one thing the user can do about a domain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainAction {
    /// Deploy the Environment: the domain is staged.
    Deploy,
    /// Add Servers to carry traffic.
    AddServer,
    /// Point the domain here with these DNS records.
    Dns { records: Vec<DnsRecord> },
}

/// One DNS record to create at the domain's DNS provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DnsRecord {
    /// `CNAME`, `A` or `AAAA`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Relative to the registrable domain; `@` is its apex.
    pub name: String,
    pub value: String,
}

pub(crate) fn add_domain(
    tx: &mut dyn Tx,
    who: &Actor,
    add: &AddDomain,
    trusted: &Trusted,
) -> Result<DomainStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &add.environment)?;
    // Hostnames are unique across the Organization: all its Environments share the
    // Servers' ingress and one Cluster Domain.
    let everywhere = other_environments(tx, who, &environment)?
        .into_iter()
        .chain(std::iter::once(environment.working.clone()))
        .flat_map(|intent| intent.services)
        .map(|service| (service.id, service.config))
        .collect::<Vec<_>>();
    let service = environment.service_mut(&add.service)?;
    let (name, port, changed) = match &add.hostname {
        Some(hostname) => {
            let elsewhere = everywhere.iter().any(|(id, config)| {
                *id != service.id
                    && config
                        .routes
                        .iter()
                        .any(|route| route.hostname == hostname.as_str())
            });
            if elsewhere {
                return Err(error::conflict(
                    "Another Service already has this domain",
                    json!({}),
                ));
            }
            let routes = &mut service.config.routes;
            let existing = routes
                .iter_mut()
                .find(|route| route.hostname == hostname.as_str());
            let changed = existing
                .as_ref()
                .is_none_or(|route| route.target_port != add.port);
            // Only adding or retargeting a custom domain needs the capability.
            if changed && !trusted.domains.custom_domains {
                return Err(RpcError {
                    code: RpcErrorCode::Unsupported,
                    message: "Custom domains need Ployz Pro".into(),
                    details: json!({ "next": "ployz billing upgrade" }),
                });
            }
            match existing {
                Some(route) => route.target_port = add.port,
                None => routes.push(ServiceRoute {
                    id: route_id(hostname.as_str()),
                    hostname: hostname.to_string(),
                    target_port: add.port,
                }),
            }
            let name = DomainName::Custom {
                hostname: hostname.clone(),
            };
            (name, add.port, changed)
        }
        None => {
            let managed = &mut service.config.managed_hostnames;
            // A Service needs one generated domain; adding it again keeps it.
            let changed = match managed.first_mut() {
                Some(existing) if add.port.is_none() || existing.target_port == add.port => false,
                Some(existing) => {
                    existing.target_port = add.port;
                    true
                }
                None => {
                    let taken = everywhere
                        .iter()
                        .flat_map(|(_, config)| &config.managed_hostnames)
                        .map(|managed| managed.prefix.as_str())
                        .collect::<Vec<_>>();
                    let prefix = free_prefix(service.config.private_dns.as_str(), &taken);
                    managed.push(ServiceManagedHostname {
                        prefix,
                        target_port: add.port,
                    });
                    true
                }
            };
            let managed = managed
                .first()
                .expect("a generated domain was just ensured");
            let name = generated(managed.prefix.clone(), trusted);
            (name, managed.target_port, changed)
        }
    };
    if changed {
        scope::save_working(tx, &mut environment)?;
    }
    Ok(DomainStaged {
        environment: environment.summary,
        domain: Domain {
            service: add.service.clone(),
            name,
            port,
        },
        staged: if changed {
            vec![SettingPath::whole(&add.service)]
        } else {
            Vec::new()
        },
        immediate: Vec::new(),
    })
}

pub(crate) fn remove_domain(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &RemoveDomain,
    trusted: &Trusted,
) -> Result<DomainStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &remove.environment)?;
    let found = domains_of(&environment.working, trusted)
        .into_iter()
        .find(|domain| domain.is_named(&remove.domain))
        .ok_or_else(|| no_domain(&environment, trusted))?;
    let service = environment.service_mut(&found.service)?;
    match &found.name {
        DomainName::Custom { hostname } => service
            .config
            .routes
            .retain(|route| route.hostname != hostname.as_str()),
        DomainName::Generated { prefix, .. } => service
            .config
            .managed_hostnames
            .retain(|managed| managed.prefix != *prefix),
    }
    scope::save_working(tx, &mut environment)?;
    Ok(DomainStaged {
        environment: environment.summary,
        staged: vec![SettingPath::whole(&found.service)],
        domain: found,
        immediate: Vec::new(),
    })
}

pub(crate) fn domains(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &DomainsQuery,
    trusted: &Trusted,
) -> Result<DomainsView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    if let Some(service) = &query.service {
        environment.service(service)?;
    }
    let head = crate::deployment::head(tx, &environment)?;
    let domains = domains_of(&environment.working, trusted)
        .into_iter()
        .filter(|domain| {
            query
                .service
                .as_ref()
                .is_none_or(|name| *name == domain.service)
        })
        .map(|domain| row(domain, &environment.working, &head, trusted))
        .collect();
    Ok(DomainsView {
        environment: environment.summary,
        domains,
    })
}

pub(crate) fn domain(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &DomainQuery,
    trusted: &Trusted,
) -> Result<DomainView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let found = domains_of(&environment.working, trusted)
        .into_iter()
        .find(|domain| domain.is_named(&query.domain))
        .ok_or_else(|| no_domain(&environment, trusted))?;
    let head = crate::deployment::head(tx, &environment)?;
    Ok(DomainView {
        domain: row(found, &environment.working, &head, trusted),
        environment: environment.summary,
    })
}

/// Whether any Service of `intent` has a generated domain, which needs a Cluster Domain to deploy.
pub(crate) fn has_generated(intent: &SavedEnvironmentIntent) -> bool {
    intent
        .services
        .iter()
        .any(|service| !service.config.managed_hostnames.is_empty())
}

/// `saved` with each generated domain as a route under `cluster_domain`, as the Servers
/// need it. Without a Cluster Domain they are left out: only a plan lowers without one.
pub(crate) fn expand(
    saved: &SavedEnvironmentIntent,
    cluster_domain: Option<&Hostname>,
) -> SavedEnvironmentIntent {
    let mut saved = saved.clone();
    for service in &mut saved.services {
        let managed = std::mem::take(&mut service.config.managed_hostnames);
        let Some(cluster_domain) = cluster_domain else {
            continue;
        };
        for ServiceManagedHostname {
            prefix,
            target_port,
        } in managed
        {
            let hostname = format!("{prefix}.{cluster_domain}");
            service.config.routes.push(ServiceRoute {
                id: route_id(&hostname),
                hostname,
                target_port,
            });
        }
    }
    saved
}

/// A route ID stable per hostname, so re-adding or re-expanding one never churns it.
fn route_id(hostname: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, hostname.as_bytes()).to_string()
}

/// `base`, else the first of `base-2`, `base-3`, … no generated domain of the
/// Organization has: every Environment's hostnames share one Cluster Domain.
fn free_prefix(base: &str, taken: &[&str]) -> String {
    let mut candidate = base.to_owned();
    for n in 2.. {
        if !taken.contains(&candidate.as_str()) {
            break;
        }
        let suffix = format!("-{n}");
        let room = 63usize.saturating_sub(suffix.len());
        candidate = format!(
            "{}{suffix}",
            base.get(..room).unwrap_or(base).trim_end_matches('-')
        );
    }
    candidate
}

fn generated(prefix: String, trusted: &Trusted) -> DomainName {
    let hostname = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| format!("{prefix}.{}", cluster.name));
    DomainName::Generated { prefix, hostname }
}

/// Every domain in `intent`: each Service's custom domains, then its generated one.
fn domains_of(intent: &SavedEnvironmentIntent, trusted: &Trusted) -> Vec<Domain> {
    let mut domains = Vec::new();
    for service in &intent.services {
        let Ok(name) = ServiceName::parse(service.slug.as_str()) else {
            continue;
        };
        for route in &service.config.routes {
            if let Ok(hostname) = Hostname::parse(route.hostname.as_str()) {
                domains.push(Domain {
                    service: name.clone(),
                    name: DomainName::Custom { hostname },
                    port: route.target_port,
                });
            }
        }
        for managed in &service.config.managed_hostnames {
            domains.push(Domain {
                service: name.clone(),
                name: generated(managed.prefix.clone(), trusted),
                port: managed.target_port,
            });
        }
    }
    domains
}

fn no_domain(environment: &Environment, trusted: &Trusted) -> RpcError {
    let names = domains_of(&environment.working, trusted)
        .iter()
        .map(|domain| domain.shown().to_owned())
        .collect::<Vec<_>>();
    error::not_found(
        format!("No such domain in Environment {}", environment.summary.name),
        json!({ "valid_children": names }),
    )
}

/// Working State of every other Environment of the Organization: hostnames are
/// unique across all of them.
fn other_environments(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &Environment,
) -> Result<Vec<SavedEnvironmentIntent>, RpcError> {
    tx.query(
        "SELECT working FROM config_environment WHERE organization_id = ?1 AND id <> ?2",
        &[
            who.organization.as_str().into(),
            environment.summary.id.as_str().into(),
        ],
    )?
    .iter()
    .map(|row| {
        serde_json::from_str(row.text(0)?)
            .ok()
            .and_then(|value| parse_environment_intent(value).ok())
            .ok_or_else(|| error::corrupt("Working State"))
    })
    .collect()
}

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

fn row(
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
            kind: "CNAME".into(),
            name,
            value: cluster.name.to_string(),
        }];
    }
    evidence
        .ingress_addresses
        .iter()
        .map(|address| DnsRecord {
            kind: if address.contains(':') { "AAAA" } else { "A" }.into(),
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
                    kind: "CNAME".into(),
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
            records.first().map(|record| record.kind.as_str()),
            Some("A")
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
