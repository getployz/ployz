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
    ServiceSettingChange,
};
use ployz_core::{
    CertificateAvailability, CertificateFailureKind, CertificateObservation, DomainPrefix,
    IngressHost, Namespace, RpcError, ServiceName,
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

mod change;
mod status;

pub use change::{DomainChange, DomainEdit};
use status::row;

/// What Cloud observed of the Organization's public traffic, passed in-process with a
/// domain read or write. Empty unless Cloud supplies it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ConfigDomainEvidence")]
pub struct DomainEvidence {
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
    /// Hostnames the Servers publish now, as the Runtime Watch last reported them.
    /// Generated prefixes avoid those of other Namespaces, and a Deploy refuses one.
    #[serde(default)]
    #[ts(as = "Option<Vec<PublishedHostname>>", optional)]
    pub published: Vec<PublishedHostname>,
}

/// A hostname a Service on the Servers publishes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PublishedHostname {
    pub hostname: Hostname,
    /// The Namespace of the Service publishing it.
    pub namespace: Namespace,
    /// The Service publishing it, by its runtime name.
    pub service: ServiceName,
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
    /// Something other than Ployz answers on port 80 of these ingress Servers.
    #[serde(rename = "port_80_taken")]
    Port80Taken {
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
/// Without a `prefix`, a Service that has a generated domain keeps it: name a
/// prefix to add another.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AddDomain {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub service: ServiceName,
    /// A custom hostname; none generates one.
    #[serde(default)]
    pub hostname: Option<Hostname>,
    /// The generated domain's prefix; none picks a free one from the Service's
    /// Private DNS name. Not with `hostname`.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub prefix: Option<DomainPrefix>,
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

/// Change the prefix of a Service's generated domain, `PREFIX.CLUSTER-DOMAIN`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetGeneratedDomain {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub service: ServiceName,
    /// Which of the Service's generated domains, by hostname or prefix; needed
    /// only when it has several.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub domain: Option<String>,
    /// Unique among the Organization's generated domains and the hostnames other
    /// Namespaces publish.
    pub prefix: DomainPrefix,
    /// The container port it reaches: omitted keeps it, `null` follows the
    /// container's `PORT`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(type = "number | null", optional)]
    pub port: Option<Option<u16>>,
}

/// A field that is present, even as `null`.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

/// A domain added or removed in Working State, staged until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DomainStaged {
    pub environment: EnvironmentSummary,
    pub domain: Domain,
    /// Its Service, when this changed it; empty when it already was so.
    pub staged: Vec<SettingPath>,
}

/// A domain given to a Service, or the one it already had.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DomainAdded {
    #[serde(flatten)]
    pub written: DomainStaged,
    /// Whether the Service lacked it; false when it already had it.
    pub added: bool,
    /// A prefix free for another generated domain, when one was asked for without
    /// a prefix and the Service already had one.
    pub free_prefix: Option<DomainPrefix>,
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
    #[serde(rename = "type")]
    pub kind: DnsRecordKind,
    /// Relative to the registrable domain; `@` is its apex.
    pub name: String,
    pub value: String,
}

/// The kind of a DNS record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub enum DnsRecordKind {
    #[serde(rename = "CNAME")]
    Cname,
    #[serde(rename = "A")]
    A,
    #[serde(rename = "AAAA")]
    Aaaa,
}

impl std::fmt::Display for DnsRecordKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Cname => "CNAME",
            Self::A => "A",
            Self::Aaaa => "AAAA",
        })
    }
}

pub(crate) fn add_domain(
    tx: &mut dyn Tx,
    who: &Actor,
    add: &AddDomain,
    trusted: &Trusted,
) -> Result<DomainAdded, RpcError> {
    if add.hostname.is_some() && add.prefix.is_some() {
        return Err(error::invalid(
            "A domain is a custom hostname or a generated prefix, not both",
            json!({}),
        ));
    }
    let mut environment = scope::lock(tx, who, &add.environment)?;
    // Hostnames under the Cluster Domain are Ployz's to hand out, never custom.
    let cluster = cluster_of(tx, &environment, trusted)?;
    let own = environment.service(&add.service)?;
    let (own, own_prefixes) = (
        own.id.clone(),
        own.config
            .managed_hostnames
            .iter()
            .map(|managed| managed.prefix.clone())
            .collect::<Vec<_>>(),
    );
    let taken = taken_prefixes(tx, who, &environment, &own, (trusted, cluster.as_ref()))?;
    if let Some(prefix) = &add.prefix {
        refuse_taken(tx, who, &taken, prefix.as_str())?;
    }
    if let (Some(hostname), Some(cluster)) = (&add.hostname, &cluster) {
        let (hostname, cluster) = (hostname.as_str(), cluster.as_str());
        if hostname == cluster || hostname.ends_with(&format!(".{cluster}")) {
            return Err(error::invalid(
                format!(
                    "{hostname} is under {cluster}, which Ployz generates domains in: use a domain of your own"
                ),
                json!({ "cluster_domain": cluster }),
            ));
        }
    }
    if let Some(hostname) = &add.hostname {
        crate::names::refuse_taken(
            tx,
            &environment,
            crate::names::Name::Hostname(hostname.as_str()),
            Some(&own),
        )?;
    }
    let service = environment.service_mut(&add.service)?;
    let mut free = None;
    let (name, port, added, changed) = match &add.hostname {
        Some(hostname) => {
            let routes = &mut service.config.routes;
            let existing = routes
                .iter_mut()
                .find(|route| route.hostname == hostname.as_str());
            let added = existing.is_none();
            let changed = existing
                .as_ref()
                .is_none_or(|route| route.target_port != add.port);
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
            (name, add.port, added, changed)
        }
        None => {
            let base = service.config.private_dns.as_str().to_owned();
            let managed = &mut service.config.managed_hostnames;
            let mut taken = taken
                .iter()
                .map(|(prefix, _)| prefix.as_str())
                .collect::<Vec<_>>();
            // Without a prefix, a Service that has a generated domain keeps it.
            let existing = match &add.prefix {
                Some(prefix) => managed
                    .iter_mut()
                    .find(|managed| managed.prefix == prefix.as_str()),
                None => managed.first_mut(),
            };
            let (prefix, port, added, changed) = match existing {
                Some(existing) => {
                    let changed = add.port.is_some() && existing.target_port != add.port;
                    if changed {
                        existing.target_port = add.port;
                    }
                    (
                        existing.prefix.clone(),
                        existing.target_port,
                        false,
                        changed,
                    )
                }
                None => {
                    let prefix = match &add.prefix {
                        Some(prefix) => prefix.as_str().to_owned(),
                        None => free_prefix(&base, &taken),
                    };
                    managed.push(ServiceManagedHostname {
                        prefix: prefix.clone(),
                        target_port: add.port,
                    });
                    (prefix, add.port, true, true)
                }
            };
            if add.prefix.is_none() && !added {
                taken.extend(own_prefixes.iter().map(String::as_str));
                free = DomainPrefix::parse(free_prefix(&base, &taken)).ok();
            }
            (generated(prefix, trusted), port, added, changed)
        }
    };
    if changed {
        scope::save_working(tx, &mut environment)?;
    }
    Ok(DomainAdded {
        written: DomainStaged {
            environment: environment.summary,
            domain: Domain {
                service: add.service.clone(),
                name,
                port,
            },
            staged: changed
                .then(|| SettingPath::whole(&add.service))
                .into_iter()
                .collect(),
        },
        added,
        free_prefix: free,
    })
}

pub(crate) fn set_generated_domain(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetGeneratedDomain,
    trusted: &Trusted,
) -> Result<DomainStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &set.environment)?;
    let prefix = set.prefix.as_str();
    let own = environment.service(&set.service)?.id.clone();
    let cluster = cluster_of(tx, &environment, trusted)?;
    let taken = taken_prefixes(tx, who, &environment, &own, (trusted, cluster.as_ref()))?;
    refuse_taken(tx, who, &taken, prefix)?;
    let generated_domains = domains_of(&environment.working, trusted)
        .into_iter()
        .filter_map(|domain| match &domain.name {
            DomainName::Generated { prefix, .. } if domain.service == set.service => {
                Some((prefix.clone(), domain))
            }
            DomainName::Generated { .. } | DomainName::Custom { .. } => None,
        })
        .collect::<Vec<_>>();
    let shown = || {
        generated_domains
            .iter()
            .map(|(_, domain)| domain.shown().to_owned())
            .collect::<Vec<_>>()
    };
    let chosen = match (&set.domain, generated_domains.as_slice()) {
        (_, []) => {
            return Err(error::not_found(
                format!("{} has no generated domain to change", set.service),
                json!({ "next": format!("ployz domain add {}", set.service) }),
            ));
        }
        (Some(name), _) => generated_domains
            .iter()
            .find(|(_, domain)| domain.is_named(name))
            .ok_or_else(|| {
                error::not_found(
                    format!("{} has no generated domain {name}", set.service),
                    json!({ "valid_children": shown() }),
                )
            })?,
        (None, [only]) => only,
        (None, _) => {
            return Err(error::ambiguous(
                format!(
                    "{} has several generated domains: name the one to change",
                    set.service
                ),
                json!({ "valid_children": shown() }),
            ));
        }
    };
    let current = chosen.0.clone();
    let service = environment.service_mut(&set.service)?;
    let managed = &mut service.config.managed_hostnames;
    if prefix != current && managed.iter().any(|managed| managed.prefix == prefix) {
        return Err(error::conflict(
            format!("{} already has the generated domain {prefix}", set.service),
            json!({ "prefix": prefix }),
        ));
    }
    let managed = managed
        .iter_mut()
        .find(|managed| managed.prefix == current)
        .ok_or_else(|| {
            error::not_found(
                format!("{} has no generated domain {current}", set.service),
                json!({ "valid_children": shown() }),
            )
        })?;
    let port = set.port.unwrap_or(managed.target_port);
    let changed = managed.prefix != prefix || managed.target_port != port;
    prefix.clone_into(&mut managed.prefix);
    managed.target_port = port;
    if changed {
        scope::save_working(tx, &mut environment)?;
    }
    Ok(DomainStaged {
        environment: environment.summary,
        domain: Domain {
            service: set.service.clone(),
            name: generated(prefix.to_owned(), trusted),
            port,
        },
        staged: changed
            .then(|| SettingPath::whole(&set.service))
            .into_iter()
            .collect(),
    })
}

/// Refuse `prefix` when another Service's generated domain or another Namespace's
/// published hostname holds it.
fn refuse_taken(
    tx: &mut dyn Tx,
    who: &Actor,
    taken: &[(String, Option<&PublishedHostname>)],
    prefix: &str,
) -> Result<(), RpcError> {
    match taken.iter().find(|(taken, _)| taken == prefix) {
        Some((_, Some(published))) => Err(clash(tx, who, published)?),
        Some((_, None)) => Err(error::conflict(
            format!("Another generated domain of this Organization is {prefix}"),
            json!({ "prefix": prefix }),
        )),
        None => Ok(()),
    }
}

/// Each prefix a generated domain of Service `own` in `environment` can't take,
/// with who holds it: another Service's generated domain in the Organization
/// (none), or a hostname another Namespace publishes under `cluster`.
fn taken_prefixes<'t>(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &Environment,
    own: &str,
    (trusted, cluster): (&'t Trusted, Option<&Hostname>),
) -> Result<Vec<(String, Option<&'t PublishedHostname>)>, RpcError> {
    let mut taken: Vec<(String, Option<&PublishedHostname>)> =
        crate::names::other_environments(tx, environment)?
            .iter()
            .chain(std::iter::once(&environment.working))
            .flat_map(|intent| &intent.services)
            .filter(|service| service.id != own)
            .flat_map(|service| &service.config.managed_hostnames)
            .map(|managed| (managed.prefix.clone(), None))
            .collect();
    if let Some(cluster) = cluster {
        let namespace = crate::deployment::namespace(tx, who, &environment.summary, false)?;
        let under = format!(".{cluster}");
        taken.extend(
            trusted
                .domains
                .published
                .iter()
                .filter(|published| published.namespace != namespace)
                .filter_map(|published| {
                    let prefix = published.hostname.as_str().strip_suffix(&under)?;
                    Some((prefix.to_owned(), Some(published)))
                })
                .collect::<Vec<_>>(),
        );
    }
    Ok(taken)
}

/// The Cluster Domain `environment`'s generated domains live under: the one Cloud
/// reserved, else the one its last Deploy used.
fn cluster_of(
    tx: &mut dyn Tx,
    environment: &Environment,
    trusted: &Trusted,
) -> Result<Option<Hostname>, RpcError> {
    match &trusted.domains.cluster_domain {
        Some(cluster) => Ok(Some(cluster.name.clone())),
        None => crate::deployment::cluster_domain(tx, &environment.summary.id),
    }
}

/// Fills in the full hostname of each generated domain the Service changes in
/// `changes` stage: as `head` deploys it, else under the Cluster Domain.
pub(crate) fn name_generated<'a>(
    tx: &mut dyn Tx,
    environment: &Environment,
    head: &crate::review::Head,
    trusted: &Trusted,
    changes: impl IntoIterator<Item = (&'a str, &'a mut ServiceSettingChange)>,
) -> Result<(), RpcError> {
    let rows = changes
        .into_iter()
        .filter(|(_, row)| row.path.ends_with(".managedHostnames"))
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok(());
    }
    let deployed = crate::deployment::cluster_domains(tx, &environment.summary.id, head)?;
    let cluster = cluster_of(tx, environment, trusted)?;
    for (service, row) in rows {
        let hostnames = [&row.before, &row.after]
            .into_iter()
            .filter_map(serde_json::Value::as_array)
            .flatten()
            .filter_map(|managed| managed.get("prefix")?.as_str())
            .filter_map(|prefix| {
                let cluster = deployed
                    .get(&(service.to_owned(), prefix.to_owned()))
                    .or(cluster.as_ref())?;
                Some((prefix.to_owned(), format!("{prefix}.{cluster}")))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        row.hostnames = (!hostnames.is_empty()).then_some(hostnames);
    }
    Ok(())
}

/// What other Namespaces than `own` publish: no hostname of `own`'s Environment
/// may take it.
fn published_elsewhere<'t>(
    trusted: &'t Trusted,
    own: &'t Namespace,
) -> impl Iterator<Item = &'t PublishedHostname> {
    trusted
        .domains
        .published
        .iter()
        .filter(move |published| published.namespace != *own)
}

/// Refuse to deploy a hostname of `intent` another Namespace than `namespace`
/// publishes, generated ones expanded under `cluster_domain`.
///
/// # Errors
/// `conflict` naming who publishes it.
pub(crate) fn check_published(
    tx: &mut dyn Tx,
    who: &Actor,
    intent: &SavedEnvironmentIntent,
    (cluster_domain, namespace): (Option<&Hostname>, &Namespace),
    trusted: &Trusted,
) -> Result<(), RpcError> {
    let expanded = expand(intent, cluster_domain);
    let ours = |hostname: &Hostname| {
        expanded
            .services
            .iter()
            .flat_map(|service| &service.config.routes)
            .any(|route| route.hostname == hostname.as_str())
    };
    match published_elsewhere(trusted, namespace).find(|published| ours(&published.hostname)) {
        Some(published) => Err(clash(tx, who, published)?),
        None => Ok(()),
    }
}

/// Why `published`'s hostname can't be taken: it names the Project and Environment
/// whose Namespace publishes it, or says no Project owns that Namespace and how to
/// take it off the Servers.
fn clash(
    tx: &mut dyn Tx,
    who: &Actor,
    published: &PublishedHostname,
) -> Result<RpcError, RpcError> {
    let PublishedHostname {
        hostname,
        namespace,
        service,
    } = published;
    let owner = tx.query(
        "SELECT p.name, e.name FROM config_namespace n \
         JOIN config_environment e ON e.id = n.environment_id \
         JOIN config_project p ON p.id = e.project_id \
         WHERE n.organization_id = ?1 AND n.namespace = ?2",
        &[who.organization.as_str().into(), namespace.as_str().into()],
    )?;
    Ok(match owner.first() {
        Some(row) => {
            let (project, environment) = (row.text(0)?, row.text(1)?);
            error::conflict(
                format!("{hostname} is already published by {service} in {project}/{environment}"),
                json!({ "hostname": hostname, "project": project, "environment": environment }),
            )
        }
        None => error::conflict(
            format!(
                "{hostname} is already published by {service} in Namespace {namespace}, which \
                 isn't in any Project: remove that Namespace from the Servers first"
            ),
            json!({
                "hostname": hostname,
                "namespace": namespace,
                "next": format!("ployz server clean --namespace {namespace} --confirm {namespace}"),
            }),
        ),
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

/// Every domain in `intent`: each Service's custom domains, then its generated ones.
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
