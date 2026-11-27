//! Services as `service ls` and `service inspect` show them: every Service in Working
//! State, and every deployed one a staged removal dropped from it, each with what the
//! next Deploy does to it.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    ReviewLifecycleKind, RowId, SavedEnvironmentIntent, SavedServiceIntent, ServiceSettingChange,
    ServiceSource, ServiceTemplate, ValuePart,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::Actor;
use crate::id::ServiceLineageId;
use crate::review;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::service::{ServiceSummary, summary};
use crate::storage::Tx;

/// Every Service of an Environment. One Environment's Services are one bounded
/// document, so the list comes whole.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServicesQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// An Environment's Services, by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ServicesView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// Its Services, by name.
    pub services: Vec<ServiceListing>,
}

/// One Service and what the next Deploy does to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceListing {
    #[serde(flatten)]
    pub service: ServiceSummary,
    /// Its node's row: what a Sync or Never sync names the whole Service by.
    pub row: RowId,
    /// Where its image comes from.
    pub source: SourceKind,
    /// What the next Deploy does to it; none when it is deployed as it is.
    pub change: Option<ReviewLifecycleKind>,
    /// The Service Template it was created from, if any.
    pub template: Option<ServiceTemplate>,
    /// The Services of its Environment it reads, from a variable or a file of a
    /// Config it mounts: the references that order a Deploy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<ServiceReference>>", optional)]
    pub references: Vec<ServiceReference>,
}

/// A Service another reads, and the keys it reads of it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceReference {
    pub service: ServiceLineageId,
    /// Sorted.
    pub keys: Vec<String>,
}

/// Where a Service's image comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Nothing yet: a Deploy skips it.
    Empty,
    /// Built from a directory `ployz up` uploaded.
    Uploaded,
    /// Built from a repository.
    Git,
    /// Pulled from a registry by reference; nothing is built.
    Image,
}

/// One Service by name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub service: ServiceName,
}

/// One Service: its identity, its Settings and its staged changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    #[serde(flatten)]
    pub service: ServiceListing,
    /// The lineage its Environment copies share.
    pub lineage: ServiceLineageId,
    /// Its Settings as one object, the shape `set --patch` takes. A removed Service
    /// shows what is deployed.
    pub values: Map<String, Value>,
    /// Its Settings the next Deploy changes.
    pub changes: Vec<ServiceSettingChange>,
}

pub(crate) fn services(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &ServicesQuery,
) -> Result<ServicesView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let services = listed(tx, &environment)?
        .into_iter()
        .map(|listed| listed.listing)
        .collect();
    Ok(ServicesView {
        environment: environment.summary,
        services,
    })
}

pub(crate) fn service(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &crate::Trusted,
    query: &ServiceQuery,
) -> Result<ServiceView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    // A name removed and added again lists twice; the one in Working State wins.
    let found = listed(tx, &environment)?
        .into_iter()
        .filter(|listed| listed.listing.service.name == query.service)
        .min_by_key(|listed| listed.listing.change == Some(ReviewLifecycleKind::Delete));
    let Some(mut listed) = found else {
        return Err(scope::no_service(
            &query.service,
            &environment.summary.name,
            &environment.working,
        ));
    };
    let head = crate::deployment::head(tx, &environment)?;
    let id = listed.node.id.clone();
    let rows = listed.changes.iter_mut().map(|row| (id.as_str(), row));
    crate::domain::name_generated(tx, &environment, &head, trusted, rows)?;
    let values = crate::settings::query::values(tx, &environment, &listed.node)?;
    Ok(ServiceView {
        environment: environment.summary,
        lineage: ServiceLineageId::parse(listed.node.lineage_id.as_str())
            .map_err(|_| crate::error::corrupt("Service lineage"))?,
        values,
        changes: listed.changes,
        service: listed.listing,
    })
}

struct Listed {
    listing: ServiceListing,
    node: SavedServiceIntent,
    changes: Vec<ServiceSettingChange>,
}

/// Working State's Services, then deployed ones it dropped, sorted by name.
fn listed(tx: &mut dyn Tx, environment: &scope::Environment) -> Result<Vec<Listed>, RpcError> {
    let review = review::review(tx, environment)?;
    let built = crate::deployment::receipts(tx, &environment.summary.id)?;
    let working = &environment.working;
    let head = &review.head.intent;
    let removed = head
        .services
        .iter()
        .filter(|node| working.services.iter().all(|kept| kept.id != node.id))
        .map(|node| (head, node));
    let mut listed = working
        .services
        .iter()
        .map(|node| (working, node))
        .chain(removed)
        .map(|(intent, node)| {
            let change = review
                .view
                .changes
                .iter()
                .find(|change| change.node.id == node.id);
            Ok(Listed {
                listing: ServiceListing {
                    service: summary(node)?,
                    row: RowId::node(&node.lineage_id),
                    source: match node.config.source {
                        ServiceSource::Empty { .. }
                            if built.contains_key(&node.config.private_dns) =>
                        {
                            SourceKind::Uploaded
                        }
                        ServiceSource::Empty { .. } => SourceKind::Empty,
                        ServiceSource::Git { .. } => SourceKind::Git,
                        ServiceSource::Image { .. } => SourceKind::Image,
                    },
                    change: change.map(|change| change.lifecycle),
                    template: node.config.template.clone(),
                    references: references(intent, node)?,
                },
                changes: change
                    .map(|change| change.settings.clone())
                    .unwrap_or_default(),
                node: node.clone(),
            })
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    listed.sort_by(|a, b| a.listing.service.name.cmp(&b.listing.service.name));
    Ok(listed)
}

/// The Services of `intent` that `node` reads, in `intent`'s order, itself included when it
/// names itself; a lineage `intent` doesn't hold, a node a Branch uses live, is left out.
fn references(
    intent: &SavedEnvironmentIntent,
    node: &SavedServiceIntent,
) -> Result<Vec<ServiceReference>, RpcError> {
    let mut read: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (lineage, key) in crate::branch::parts_read(intent, node).filter_map(ValuePart::service_ref)
    {
        read.entry(lineage).or_default().insert(key);
    }
    intent
        .services
        .iter()
        .filter_map(|service| Some((service, read.remove(service.lineage_id.as_str())?)))
        .map(|(service, keys)| {
            Ok(ServiceReference {
                service: summary(service)?.id,
                keys: keys.into_iter().map(str::to_owned).collect(),
            })
        })
        .collect()
}
