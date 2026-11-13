//! Public domains. A Service gets a generated hostname, `PREFIX.CLUSTER-DOMAIN`, or a
//! custom one, which only an Organization with the custom-domain capability may add.
//! Adding and removing either is staged until a Deploy; admission expands generated
//! ones into routes under the Cluster Domain Cloud reserved for the Organization.
//!
//! The Store never looks at DNS, certificates or Servers. Cloud observes them, or the
//! CLI does in a Standalone Cluster, and passes what it saw as [`DomainEvidence`]; from that, each domain reads Ready,
//! Setting up or Needs attention, with at most one action.

use ployz_core::config::{
    SavedEnvironmentIntent, SavedServiceIntent, ServiceManagedHostname, ServiceRoute,
    ServiceSettingChange,
};
use ployz_core::{
    CertificateAvailability, CertificateFailureKind, CertificateHost, CertificateObservation,
    DomainPrefix, IngressHost, Namespace, RpcError, ServiceName,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::followup::FollowUp;
use crate::id::Hostname;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::SettingPath;
use crate::storage::Tx;
use crate::{Actor, Trusted};

mod change;
mod status;

pub use change::{DomainChange, DomainEdit};
use status::row;

/// What Cloud or the Standalone CLI observed of the public traffic, passed in-process with
/// a domain read or write. Empty when nothing was observed.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ConfigDomainEvidence")]
pub struct DomainEvidence {
    /// The Organization's Cluster Domain, once reserved.
    #[serde(default)]
    pub cluster_domain: Option<ClusterDomain>,
    /// The Cluster's certificates as its Runtime Watch reported them; none when the
    /// Cluster couldn't be observed.
    #[serde(default)]
    pub certificates: Option<Vec<CertificateObservation>>,
    /// Certificates the Runtime Watch listed but couldn't read: unobserved, never absent.
    #[serde(default)]
    #[ts(as = "Option<Vec<CertificateHost>>", optional)]
    pub incomplete_certificates: Vec<CertificateHost>,
    /// How many Servers accept ingress, as the Runtime Watch reported them; none when the
    /// Cluster or one of its Machines couldn't be read. Read only without a Cluster Domain,
    /// whose status already says it.
    #[serde(default)]
    #[ts(optional)]
    pub ingress_servers: Option<u32>,
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
    /// Set while the Cluster is served through a Cloudflare Tunnel: a custom hostname
    /// then reaches the Servers through the tunnel, not their public addresses.
    #[serde(default)]
    #[ts(optional)]
    pub tunnel: Option<TunnelEvidence>,
}

/// What Cloud observed of a custom hostname's way to the Cluster's Cloudflare Tunnel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TunnelEvidence {
    /// What a hostname outside the Cluster Domain's zone points its CNAME at to reach the
    /// tunnel: `<tunnel id>.cfargotunnel.com`.
    pub target: Hostname,
    /// The hostnames Cloud fetched through DNS and Cloudflare just now, and whether a
    /// Server of this Cluster answered. A hostname missing here was not looked at.
    #[serde(default)]
    #[ts(as = "Option<Vec<TunnelReach>>", optional)]
    pub reached: Vec<TunnelReach>,
}

/// What one fetch through a hostname found.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TunnelReach {
    pub hostname: Hostname,
    pub answer: TunnelAnswer,
}

/// Who answered a fetch through a hostname. A fetch that finished says one of these; one
/// that was never made leaves the hostname out of [`TunnelEvidence::reached`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TunnelAnswer {
    /// A Server of this Cluster: the hostname reaches the tunnel.
    Cluster,
    /// Something else, or nothing: DNS sends the hostname elsewhere, or nowhere.
    Elsewhere,
    /// Nobody: the connection was refused, its TLS failed, or it timed out.
    Unanswered,
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
    /// These ingress Servers run no Ingress Proxy; a Deploy brings it back.
    IngressDown {
        servers: Vec<String>,
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
    pub(crate) fn is_named(&self, name: &str) -> bool {
        let name = crate::id::normalize_hostname(name.to_owned());
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
    /// The records that point a custom domain here, whatever its status; empty for a
    /// generated one, or when Cloud can't name them yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<DnsRecord>>")]
    pub records: Vec<DnsRecord>,
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
    // Hostnames under a Cluster Domain Ployz reserved are Ployz's to hand out, never custom. Behind a Cloudflare
    // Tunnel the zone is the user's own, and a name in it is as good as any other.
    let cluster = trusted.cluster_domain();
    let own = environment.service(&add.service)?;
    if let (Some(hostname), Some(cluster), None) = (&add.hostname, cluster, &trusted.domains.tunnel)
    {
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
    if let Some(hostname) = &add.hostname
        && !own
            .config
            .routes
            .iter()
            .any(|route| route.hostname == hostname.as_str())
    {
        Hostname::parse_custom(hostname.as_str())?;
    }
    let (own, own_prefixes) = (
        own.id.clone(),
        own.config
            .managed_hostnames
            .iter()
            .map(|managed| managed.prefix.clone())
            .collect::<Vec<_>>(),
    );
    let taken = taken_prefixes(tx, who, &environment, &own, (trusted, cluster))?;
    if let Some(prefix) = &add.prefix {
        refuse_taken(tx, who, &taken, prefix.as_str())?;
    }
    // A custom name one label under the Cluster Domain is spelled like a generated domain,
    // so no generated domain of the Organization may hold its label.
    if let (Some(hostname), Some(cluster)) = (&add.hostname, cluster)
        && let Some(label) = label_under(hostname.as_str(), cluster)
    {
        let generated_by = |holder: &str| {
            error::conflict(
                format!("{hostname} is already the generated domain of {holder}"),
                json!({ "hostname": hostname }),
            )
        };
        if own_prefixes.iter().any(|prefix| prefix == label) {
            let summary = &environment.summary;
            return Err(generated_by(&format!(
                "{} in {}/{}",
                add.service, summary.project, summary.name
            )));
        }
        match taken.iter().find(|(taken, _)| taken == label) {
            Some((_, Holder::Generated(holder))) => return Err(generated_by(holder)),
            Some((_, Holder::Published(published))) => return Err(clash(tx, who, published)?),
            // Another Service's custom domain is refused below, by name.
            Some((_, Holder::Custom { .. })) | None => {}
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
                    refuse_too_long(&prefix, cluster, "prefix")?;
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
        scope::save_working(tx, &mut environment, trusted)?;
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
    let cluster = trusted.cluster_domain();
    let taken = taken_prefixes(tx, who, &environment, &own, (trusted, cluster))?;
    refuse_taken(tx, who, &taken, prefix)?;
    refuse_too_long(prefix, cluster, "prefix")?;
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
                FollowUp::DomainAdd {
                    service: set.service.to_string(),
                }
                .details(),
            ));
        }
        (Some(name), _) => generated_domains
            .iter()
            .find(|(_, domain)| domain.is_named(name))
            .ok_or_else(|| {
                error::not_found(
                    format!("{} has no generated domain {name}", set.service),
                    error::valid(shown()),
                )
            })?,
        (None, [only]) => only,
        (None, _) => {
            return Err(error::ambiguous(
                format!(
                    "{} has several generated domains: name the one to change",
                    set.service
                ),
                error::valid(shown()),
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
                error::valid(shown()),
            )
        })?;
    let port = set.port.unwrap_or(managed.target_port);
    let changed = managed.prefix != prefix || managed.target_port != port;
    prefix.clone_into(&mut managed.prefix);
    managed.target_port = port;
    if changed {
        scope::save_working(tx, &mut environment, trusted)?;
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

/// Who holds a label a generated domain can't take.
enum Holder<'t> {
    /// Another Service's generated domain: `service in project/environment`.
    Generated(String),
    /// A custom domain one label under the Cluster Domain, by its hostname and
    /// `service in project/environment`.
    Custom { hostname: String, holder: String },
    /// A hostname another Namespace publishes under the Cluster Domain.
    Published(&'t PublishedHostname),
}

/// Refuse `prefix` when a generated domain, a custom domain under the Cluster Domain or
/// another Namespace's published hostname holds it.
fn refuse_taken(
    tx: &mut dyn Tx,
    who: &Actor,
    taken: &[(String, Holder<'_>)],
    prefix: &str,
) -> Result<(), RpcError> {
    match taken.iter().find(|(taken, _)| taken == prefix) {
        Some((_, Holder::Published(published))) => Err(clash(tx, who, published)?),
        Some((_, Holder::Generated(_))) => Err(error::conflict(
            format!("Another generated domain of this Organization is {prefix}"),
            json!({ "prefix": prefix }),
        )),
        Some((_, Holder::Custom { hostname, holder })) => Err(error::conflict(
            format!("{hostname} is already the custom domain of {holder}"),
            json!({ "prefix": prefix, "hostname": hostname }),
        )),
        None => Ok(()),
    }
}

/// The longest label a hostname can carry.
const DNS_LABEL_MAX: usize = 63;

/// The longest hostname DNS can name.
const DNS_NAME_MAX: usize = 253;

/// The hostname of a generated domain: the one label `prefix` under the Cluster Domain.
pub(crate) fn generated_hostname(
    prefix: &(impl std::fmt::Display + ?Sized),
    cluster: &impl std::fmt::Display,
) -> String {
    format!("{prefix}.{cluster}")
}

/// The label of `hostname` one level under `cluster`, when it is one.
fn label_under<'h>(hostname: &'h str, cluster: &Hostname) -> Option<&'h str> {
    let label = hostname.strip_suffix(cluster.as_str())?.strip_suffix('.')?;
    (!label.is_empty() && !label.contains('.')).then_some(label)
}

/// Each label a generated domain of Service `own` in `environment` can't take, with who
/// holds it: another Service's generated domain in the Organization, any Service's custom
/// domain one label under `cluster` (its own included), or a hostname another Namespace
/// publishes under `cluster`.
fn taken_prefixes<'t>(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &Environment,
    own: &str,
    (trusted, cluster): (&'t Trusted, Option<&Hostname>),
) -> Result<Vec<(String, Holder<'t>)>, RpcError> {
    let others = crate::names::other_environments(tx, environment)?;
    let summary = &environment.summary;
    let environments = others
        .iter()
        .map(|other| {
            (
                crate::scope::environment_label(&other.project, &other.name),
                &other.working,
            )
        })
        .chain(std::iter::once((summary.to_string(), &environment.working)));
    let mut taken = Vec::new();
    for (environment, intent) in environments {
        for service in &intent.services {
            let holder = || format!("{} in {environment}", service.slug);
            if service.id != own {
                taken.extend(
                    service
                        .config
                        .managed_hostnames
                        .iter()
                        .map(|managed| (managed.prefix.clone(), Holder::Generated(holder()))),
                );
            }
            if let Some(cluster) = cluster {
                taken.extend(service.config.routes.iter().filter_map(|route| {
                    let label = label_under(&route.hostname, cluster)?;
                    let custom = Holder::Custom {
                        hostname: route.hostname.clone(),
                        holder: holder(),
                    };
                    Some((label.to_owned(), custom))
                }));
            }
        }
    }
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
                    Some((prefix.to_owned(), Holder::Published(published)))
                }),
        );
    }
    Ok(taken)
}

/// Refuses a generated domain whose hostname under `cluster` can't be served: a valid
/// prefix under a long Cluster Domain can pass 253 characters, which Deploy would refuse.
/// `shorter` names what to shorten.
fn refuse_too_long(
    prefix: &str,
    cluster: Option<&Hostname>,
    shorter: &str,
) -> Result<(), RpcError> {
    let Some(cluster) = cluster else {
        return Ok(());
    };
    let hostname = generated_hostname(prefix, cluster);
    let Err(refused) = IngressHost::parse(&hostname) else {
        return Ok(());
    };
    let message = if hostname.len() > DNS_NAME_MAX {
        format!(
            "{hostname} is {} characters, over a hostname's {DNS_NAME_MAX}: choose a shorter {shorter}",
            hostname.len()
        )
    } else {
        format!("{hostname} can't be served: {refused}")
    };
    Err(error::invalid(
        message,
        json!({ "hostname": hostname, "cluster_domain": cluster.as_str() }),
    ))
}

/// Refuses copying the new Services `picks` names when a suffix makes a generated
/// domain's subdomain longer than a DNS label's 63 characters, which no hostname can
/// carry. A Branch adds its name to the prefixes it copies.
pub(crate) fn refuse_long_subdomains(
    plan: &ployz_core::config::Plan,
    picks: &std::collections::BTreeSet<ployz_core::config::RowId>,
) -> Result<(), RpcError> {
    match plan
        .copied_prefixes(picks)
        .into_iter()
        .find(|prefix| prefix.len() > DNS_LABEL_MAX)
    {
        Some(prefix) => Err(error::invalid(
            format!(
                "{prefix} is {} characters, over a subdomain's {DNS_LABEL_MAX}: choose a shorter branch name",
                prefix.len()
            ),
            json!({ "prefix": prefix }),
        )),
        None => Ok(()),
    }
}

/// Refuse each generated prefix `after` gives a Service that `before` didn't, when the
/// hostname it makes is too long. A Branch adds its name to the prefixes it copies, so a
/// hostname that fits its Parent can be too long on the Branch.
pub(crate) fn refuse_too_long_copies(
    cluster: Option<&Hostname>,
    (before, after): (&SavedEnvironmentIntent, &SavedEnvironmentIntent),
) -> Result<(), RpcError> {
    let new: Vec<&str> = after
        .services
        .iter()
        .flat_map(|service| {
            let held = before
                .services
                .iter()
                .find(|old| old.lineage_id == service.lineage_id);
            service
                .config
                .managed_hostnames
                .iter()
                .map(|hostname| hostname.prefix.as_str())
                .filter(move |prefix| {
                    !held.is_some_and(|old| {
                        old.config
                            .managed_hostnames
                            .iter()
                            .any(|hostname| hostname.prefix == *prefix)
                    })
                })
        })
        .collect();
    new.into_iter()
        .try_for_each(|prefix| refuse_too_long(prefix, cluster, "branch name"))
}

/// The Cluster Domain a write checks generated names under.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Under<'a> {
    /// The one Cloud knows now, from the write's evidence. Without one, as in the
    /// hidden local Store, no generated name spells a hostname.
    Cloud(Option<&'a Hostname>),
    /// Any, as for a Follow, which has no evidence: a custom domain may share a
    /// hostname with the prefix its first label spells.
    Any,
}

impl Under<'_> {
    /// The generated prefix `hostname` would be the hostname of, if any.
    fn prefix(self, hostname: &str) -> Option<&str> {
        match self {
            Self::Cloud(cluster) => label_under(hostname, cluster?),
            Self::Any => hostname.split_once('.').map(|(label, _)| label),
        }
    }
}

/// Refuse each hostname `after` newly spells two ways under the Cluster Domain: as one
/// Service's generated prefix and as another's custom domain, anywhere in the
/// Organization. Every write of Working State comes through here, so a Branch that
/// suffixes the prefixes it copies, a Sync or a restore can't mint a name a custom
/// domain holds. What `before` already held is not new.
pub(crate) fn refuse_new_shared_hostnames(
    tx: &mut dyn Tx,
    environment: &Environment,
    under: Under<'_>,
    (before, after): (&SavedEnvironmentIntent, &SavedEnvironmentIntent),
) -> Result<(), RpcError> {
    if matches!(under, Under::Cloud(None)) {
        return Ok(());
    }
    let mut prefixes = Vec::new();
    let mut labels = Vec::new();
    for service in &after.services {
        let held = before.services.iter().find(|held| held.id == service.id);
        for managed in &service.config.managed_hostnames {
            if held.is_none_or(|held| {
                held.config
                    .managed_hostnames
                    .iter()
                    .all(|was| was.prefix != managed.prefix)
            }) {
                prefixes.push(managed.prefix.as_str());
            }
        }
        for route in &service.config.routes {
            if let Some(label) = under.prefix(&route.hostname)
                && held.is_none_or(|held| {
                    held.config
                        .routes
                        .iter()
                        .all(|was| was.hostname != route.hostname)
                })
            {
                labels.push((route.hostname.as_str(), label));
            }
        }
    }
    if prefixes.is_empty() && labels.is_empty() {
        return Ok(());
    }
    let others = crate::names::other_environments(tx, environment)?;
    let summary = &environment.summary;
    let environments = others
        .iter()
        .map(|other| {
            (
                crate::scope::environment_label(&other.project, &other.name),
                &other.working,
            )
        })
        .chain(std::iter::once((summary.to_string(), after)));
    for (place, intent) in environments {
        for service in &intent.services {
            let holder = || format!("{} in {place}", service.slug);
            for route in &service.config.routes {
                if let Some(label) = under.prefix(&route.hostname)
                    && prefixes.contains(&label)
                {
                    return Err(error::conflict(
                        format!(
                            "{} is already the custom domain of {}",
                            route.hostname,
                            holder()
                        ),
                        json!({ "prefix": label, "hostname": route.hostname }),
                    ));
                }
            }
            for managed in &service.config.managed_hostnames {
                if let Some((hostname, _)) =
                    labels.iter().find(|(_, label)| *label == managed.prefix)
                {
                    return Err(error::conflict(
                        format!("{hostname} is already the generated domain of {}", holder()),
                        json!({ "hostname": hostname }),
                    ));
                }
            }
        }
    }
    Ok(())
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
    let cluster = match trusted.cluster_domain() {
        Some(cluster) => Some(cluster.clone()),
        None => crate::deployment::cluster_domain(tx, &environment.summary.id)?,
    };
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
                Some((prefix.to_owned(), generated_hostname(prefix, cluster)))
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
            FollowUp::ServerClean {
                namespace: namespace.to_string(),
            }
            .within(json!({ "hostname": hostname, "namespace": namespace })),
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
    scope::save_working(tx, &mut environment, trusted)?;
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
    generated_prefixes(intent).next().is_some()
}

/// The prefix of each generated domain in `intent`, as `domain rm` names it.
pub(crate) fn generated_prefixes(intent: &SavedEnvironmentIntent) -> impl Iterator<Item = &str> {
    intent
        .services
        .iter()
        .flat_map(|service| &service.config.managed_hostnames)
        .map(|managed| managed.prefix.as_str())
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
            let hostname = generated_hostname(&prefix, &cluster_domain);
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
        .map(|cluster| generated_hostname(&prefix, &cluster.name));
    DomainName::Generated { prefix, hostname }
}

/// Every domain in `intent`: each Service's custom domains, then its generated ones.
fn domains_of(intent: &SavedEnvironmentIntent, trusted: &Trusted) -> Vec<Domain> {
    let mut domains = Vec::new();
    for service in &intent.services {
        let name = service.slug.clone();
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
        error::valid(names),
    )
}

#[cfg(test)]
mod generated_hostname_tests {
    use super::{DNS_LABEL_MAX, DNS_NAME_MAX, generated_hostname, label_under};
    use crate::id::Hostname;

    #[test]
    fn a_generated_hostname_is_one_label_under_the_cluster_domain() {
        let cluster = Hostname::parse("apps.example.com").unwrap();
        assert_eq!(generated_hostname("web", &cluster), "web.apps.example.com");
        assert_eq!(label_under("web.apps.example.com", &cluster), Some("web"));
        assert_eq!(label_under("a.web.apps.example.com", &cluster), None);
        assert_eq!((DNS_LABEL_MAX, DNS_NAME_MAX), (63, 253));
    }
}
