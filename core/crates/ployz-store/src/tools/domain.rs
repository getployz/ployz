//! Tools on a Service's public domains: check one, add one, change its prefix
//! or port, remove it.

use ployz_core::{DomainPrefix, ServiceName};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at, stale};
use super::refusal::Refusal;
use crate::catalog::NODE_NAME;
use crate::{
    AddDomain, Domain, DomainAction, DomainName, DomainQuery, DomainRow, DomainStatus,
    DomainsQuery, DomainsView, EnvironmentName, Hostname, ProjectName, RemoveDomain,
    SetGeneratedDomain,
};

const HOSTNAME: &str = "^([A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?\\.)+[A-Za-z]{2,63}$";

/// A domain as a result shows it.
fn domain(domain: &Domain) -> Value {
    let (kind, prefix, hostname) = match &domain.name {
        DomainName::Generated { prefix, hostname } => {
            ("generated", Some(prefix.as_str()), hostname.as_deref())
        }
        DomainName::Custom { hostname } => ("custom", None, Some(hostname.as_str())),
    };
    json!({
        "service": domain.service,
        "kind": kind,
        "prefix": prefix,
        "hostname": hostname,
        "port": domain.port,
    })
}

/// A domain with where it stands, as `service_inspect` lists it.
pub(crate) fn row(row: &DomainRow) -> Value {
    let status = match row.status {
        DomainStatus::Ready => "ready",
        DomainStatus::SettingUp => "setting_up",
        DomainStatus::NeedsAttention => "needs_attention",
    };
    json!({
        "domain": row.domain.shown(),
        "port": row.domain.port,
        "status": status,
        "reason": row.reason,
        "dns": dns(row),
    })
}

/// The DNS records the member still has to create for `row`.
fn dns(row: &DomainRow) -> Vec<Value> {
    match &row.action {
        Some(DomainAction::Dns { records }) => records
            .iter()
            .map(|record| json!({ "type": record.kind.to_string(), "name": record.name, "value": record.value }))
            .collect(),
        Some(DomainAction::Deploy | DomainAction::AddServer) | None => Vec::new(),
    }
}

/// Whether `domain` is what users mean by `name`: its hostname, or a generated
/// domain's prefix.
fn is_named(domain: &Domain, name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    domain.shown() == name
        || matches!(&domain.name, DomainName::Generated { prefix, .. } if *prefix == name)
}

/// The row named `name`, refusing with the names there are.
fn find<'view>(view: &'view DomainsView, name: &str) -> Result<&'view DomainRow, Stop> {
    view.domains
        .iter()
        .find(|row| is_named(&row.domain, name))
        .ok_or_else(|| {
            Refusal::not_found("No domain by that name in this Environment")
                .with_valid(view.domains.iter().map(|row| row.domain.shown()))
                .into()
        })
}

/// Read every domain, refusing when the Environment moved past `expect`.
fn domains(
    cx: &mut Cx<'_>,
    environment: crate::EnvironmentRef,
    expect: Option<u64>,
) -> Result<DomainsView, Stop> {
    let view = cx.one(&DomainsQuery {
        environment,
        service: None,
    })?;
    match expect.is_some_and(|expect| view.environment.revision.0 != expect) {
        true => Err(stale(cx.call())),
        false => Ok(view),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DomainAdd {
    #[schemars(with = "String", pattern(NODE_NAME), example = &"web")]
    service: ServiceName,
    /// Prefix for a generated domain, PREFIX.cluster-domain. Omit both prefix and hostname for a free one.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    prefix: Option<DomainPrefix>,
    /// A hostname the member owns, e.g. app.example.com. Not with prefix.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(HOSTNAME), length(max = 253))]
    hostname: Option<Hostname>,
    /// Container port it reaches. Default: the container's PORT.
    #[serde(default)]
    #[schemars(range(min = 1, max = 65535))]
    port: Option<u16>,
    /// Write only if the place is still at this revision. Omit unless you just read it.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DomainAdd {
    const NAME: &'static str = "domain_add";
    const DESCRIPTION: &'static str = "Give a Service a public domain: a generated one under the cluster domain, or a hostname the member owns. Staged until the next Deploy. A custom hostname comes back with the DNS records the member must create.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "service": "web", "hostname": "app.example.com" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        if self.prefix.is_some() && self.hostname.is_some() {
            return Err(Refusal::invalid("Give prefix or hostname, not both", "prefix").into());
        }
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        cx.expect(&scope, self.expect_revision)?;
        let added = cx.write(&AddDomain {
            environment: scope.clone(),
            service: self.service.clone(),
            hostname: self.hostname,
            prefix: self.prefix,
            port: self.port,
        })?;
        let view = cx.one(&DomainsQuery {
            environment: scope,
            service: Some(self.service),
        })?;
        let written = &added.written.domain;
        let dns = view
            .domains
            .iter()
            .find(|row| row.domain.name == written.name)
            .map(dns)
            .unwrap_or_default();
        Ok(json!({
            "at": at(&added.written.environment),
            "added": added.added,
            "domain": domain(written),
            "dns": dns,
            "free_prefix": added.free_prefix,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DomainCheck {
    /// The domain, by hostname or a generated domain's prefix.
    #[schemars(length(min = 1), example = &"app.example.com")]
    domain: String,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DomainCheck {
    const NAME: &'static str = "domain_check";
    const DESCRIPTION: &'static str =
        "Check a domain now: its status, why, and DNS records still to create.";
    const READ_ONLY: bool = true;

    fn example() -> Value {
        json!({ "domain": "app.example.com" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let view = cx.one(&DomainQuery {
            environment,
            domain: self.domain,
        })?;
        Ok(json!({
            "at": at(&view.environment),
            "service": view.domain.domain.service,
            "domain": row(&view.domain),
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DomainUpdate {
    /// The domain, by hostname or a generated domain's prefix.
    #[schemars(length(min = 1), example = &"app.example.com")]
    domain: String,
    /// New prefix for a generated domain.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    prefix: Option<DomainPrefix>,
    /// Container port it reaches.
    #[serde(default)]
    #[schemars(range(min = 1, max = 65535))]
    port: Option<u16>,
    /// Write only if the place is still at this revision. Omit unless you just read it.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DomainUpdate {
    const NAME: &'static str = "domain_update";
    const DESCRIPTION: &'static str =
        "Change a generated domain's prefix, or the port any domain reaches. Staged.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "domain": "whoami-2", "prefix": "whoami" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let view = domains(cx, scope.clone(), self.expect_revision)?;
        let found = find(&view, &self.domain)?.domain.clone();
        let staged = match (self.prefix, self.port, &found.name) {
            (None, None, _) => {
                return Err(Refusal::invalid("Give prefix or port", "prefix").into());
            }
            (Some(_), _, DomainName::Custom { .. }) => {
                return Err(
                    Refusal::invalid("Only a generated domain has a prefix", "prefix").into(),
                );
            }
            (Some(prefix), port, DomainName::Generated { .. }) => {
                cx.write(&SetGeneratedDomain {
                    environment: scope,
                    service: found.service.clone(),
                    domain: Some(self.domain),
                    prefix,
                    port: port.map(Some),
                })?
            }
            (None, Some(port), name) => {
                let (hostname, prefix) = match name {
                    DomainName::Custom { hostname } => (Some(hostname.clone()), None),
                    DomainName::Generated { prefix, .. } => (
                        None,
                        Some(
                            DomainPrefix::parse(prefix.clone())
                                .map_err(|_| Refusal::internal("A stored prefix did not parse"))?,
                        ),
                    ),
                };
                cx.write(&AddDomain {
                    environment: scope,
                    service: found.service.clone(),
                    hostname,
                    prefix,
                    port: Some(port),
                })?
                .written
            }
        };
        Ok(json!({
            "at": at(&staged.environment),
            "domain": domain(&staged.domain),
            "staged": true,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DomainRemove {
    /// The domain, by hostname or a generated domain's prefix.
    #[schemars(length(min = 1), example = &"app.example.com")]
    domain: String,
    /// Write only if the place is still at this revision. Omit unless you just read it.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for DomainRemove {
    const NAME: &'static str = "domain_remove";
    const DESCRIPTION: &'static str = "Take a public domain off its Service. Staged: a domain already serving asks a human to approve at the next Publish or Deploy.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "domain": "app.example.com" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let view = domains(cx, scope.clone(), self.expect_revision)?;
        let found = find(&view, &self.domain)?;
        // Only a domain waiting for its first Deploy goes without approval.
        let asks_approval = !(found.status == DomainStatus::SettingUp
            && matches!(found.action, Some(DomainAction::Deploy)));
        let staged = cx.write(&RemoveDomain {
            environment: scope,
            domain: self.domain,
        })?;
        Ok(json!({
            "at": at(&staged.environment),
            "removed": staged.domain.shown(),
            "asks_approval": asks_approval,
        }))
    }
}
