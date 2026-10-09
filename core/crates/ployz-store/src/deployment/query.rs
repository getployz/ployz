//! Deployment reads: the authored plan of a Deploy, one page of an Environment's
//! Deployments, and one Deployment with its Node Outcomes.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::deployment::{self, DeploymentSummary, DeploymentView};
use crate::error;
use crate::id::DeploymentId;
use crate::removal::{self, VolumeLoss};
use crate::review::{self, NodeChange};
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;

/// What `deploy` would ship, from authored state alone: nothing builds or runs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PlanQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Plan only these Services; none plans every Service.
    #[serde(default)]
    pub services: Vec<ServiceName>,
}

/// The authored review of a Deploy. What only the Servers can decide is `unresolved`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct PlanView {
    pub environment: EnvironmentSummary,
    /// Pass to `deploy --expect-version` to ship exactly this.
    pub version: String,
    pub namespace: ployz_core::Namespace,
    /// The changes of the nodes this Deploy targets.
    pub changes: Vec<NodeChange>,
    /// What the Deployment decides from the Servers: which containers start, stop or
    /// move, and where.
    pub unresolved: Vec<String>,
}

/// Where an Environment runs on the Servers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NamespaceQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// The Namespace an Environment's containers carry: fixed at its first Deployment,
/// else the one its first Deployment would take.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NamespaceView {
    pub environment: EnvironmentSummary,
    pub namespace: ployz_core::Namespace,
    /// Each deployed Service's runtime name (its Private DNS name, as Applied State
    /// has it), by the name it has now: a renamed Service's containers keep the name
    /// it was created with, and a staged Private DNS change isn't live yet.
    pub services: std::collections::BTreeMap<ServiceName, ServiceName>,
}

/// Every Namespace the Organization's Environments own on the Servers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NamespacesQuery {}

/// The Organization's Namespaces, each with the Environment that owns it. A
/// Namespace the Servers run that isn't here is in no Project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NamespacesView {
    pub namespaces: Vec<OwnedNamespace>,
}

/// A Namespace and the Environment that owns it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OwnedNamespace {
    pub namespace: ployz_core::Namespace,
    pub project: crate::ProjectName,
    pub environment: crate::EnvironmentName,
}

/// One page of an Environment's Deployments, newest first.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploymentsQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// At most this many, 1-100 [default: 20].
    #[serde(default)]
    pub limit: Option<usize>,
    /// The `next_cursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// One page of Deployments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DeploymentsView {
    pub environment: EnvironmentSummary,
    pub deployments: Vec<DeploymentSummary>,
    /// Pass as `cursor` for the next page; none on the last.
    pub next_cursor: Option<String>,
}

/// One Deployment, by ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploymentQuery {
    pub id: DeploymentId,
}

/// One Deployment, by its number in an Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NumberedDeploymentQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub number: u64,
}

pub(crate) fn numbered(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &NumberedDeploymentQuery,
) -> Result<DeploymentView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let number = i64::try_from(query.number).unwrap_or(i64::MAX);
    let rows = tx.query(
        "SELECT id FROM config_deployment WHERE environment_id = ?1 AND number = ?2",
        &[environment.summary.id.as_str().into(), number.into()],
    )?;
    let Some(row) = rows.first() else {
        return Err(error::not_found(
            format!(
                "{}/{} has no Deployment #{}",
                environment.summary.project, environment.summary.name, query.number
            ),
            json!({ "next": "ployz deployment ls" }),
        ));
    };
    let id = row.parse(0, "identity")?;
    deployment::view(tx, who, &id)
}

pub(crate) fn plan(tx: &mut dyn Tx, who: &Actor, query: &PlanQuery) -> Result<PlanView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let review = review::review(tx, &environment)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    let target = canonicalize_environment_intent(environment.working.clone());
    // Which Docker Volumes a removal deletes only the Servers can say.
    let removed: Vec<VolumeLoss> = if query.services.is_empty() {
        removal::removed(&review.head.applied, &target, &namespace)?
    } else {
        Vec::new()
    }
    .into_iter()
    .map(|volume| VolumeLoss {
        volume,
        deletes: Vec::new(),
    })
    .collect();
    let mut unresolved = vec!["operations".to_owned()];
    if !removed.is_empty() {
        unresolved.push("volume data".to_owned());
    }
    let frozen = deployment::freeze(
        &environment.summary.id,
        &target,
        &review.head.applied,
        &query.services,
        namespace.clone(),
        None,
        &removed,
    )?;
    // A Config left staged still restarts the targeted Services that mount it.
    let changes = review
        .view
        .changes
        .into_iter()
        .filter_map(|mut change| {
            change
                .restarts
                .retain(|service| query.services.is_empty() || query.services.contains(service));
            (frozen.targets(&change.node.id) || !change.restarts.is_empty()).then_some(change)
        })
        .collect();
    Ok(PlanView {
        environment: environment.summary,
        version: review.view.version,
        namespace,
        changes,
        unresolved,
    })
}

/// Every Namespace `who`'s Environments own.
pub(crate) fn namespaces(tx: &mut dyn Tx, who: &Actor) -> Result<NamespacesView, RpcError> {
    let rows = tx.query(
        "SELECT n.namespace, p.name, e.name FROM config_namespace n \
         JOIN config_environment e ON e.id = n.environment_id \
         JOIN config_project p ON p.id = e.project_id \
         WHERE n.organization_id = ?1 ORDER BY n.namespace",
        &[who.organization.as_str().into()],
    )?;
    let namespaces = rows
        .iter()
        .map(|row| {
            Ok(OwnedNamespace {
                namespace: row.parse(0, "Namespace")?,
                project: row.parse(1, "Project name")?,
                environment: row.parse(2, "Environment name")?,
            })
        })
        .collect::<Result<_, RpcError>>()?;
    Ok(NamespacesView { namespaces })
}

pub(crate) fn namespace(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &NamespaceQuery,
) -> Result<NamespaceView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    // Containers run as Applied State has them, under the name the Service has now.
    let applied = deployment::applied_state(tx, &environment.summary.id, &environment.working)?;
    let services = applied
        .services
        .iter()
        .filter_map(|applied| {
            let now = environment
                .working
                .services
                .iter()
                .find(|service| service.id == applied.id)
                .unwrap_or(applied);
            let name = ServiceName::parse(now.slug.as_str()).ok()?;
            Some((name, applied.config.private_dns.clone()))
        })
        .collect();
    Ok(NamespaceView {
        environment: environment.summary,
        namespace,
        services,
    })
}

pub(crate) fn page(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &DeploymentsQuery,
) -> Result<DeploymentsView, RpcError> {
    let limit = query.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(error::invalid("Expected a limit from 1 to 100", json!({})));
    }
    let cursor = query
        .cursor
        .as_deref()
        .map(|cursor| {
            cursor
                .parse::<u64>()
                .map_err(|_| error::invalid("Expected a cursor from a previous page", json!({})))
        })
        .transpose()?;
    let environment = scope::environment(tx, who, &query.environment)?;
    let (deployments, next) = deployment::page(tx, &environment.summary.id, limit, cursor)?;
    Ok(DeploymentsView {
        environment: environment.summary,
        deployments,
        next_cursor: next.map(|number| number.to_string()),
    })
}
