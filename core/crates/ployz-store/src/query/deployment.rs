//! Deployment reads: the authored plan of a Deploy, one page of an Environment's
//! Deployments, and one Deployment with its Node Outcomes.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::deployment::{self, DeploymentSummary};
use crate::error;
use crate::id::DeploymentId;
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

pub(crate) fn plan(tx: &mut dyn Tx, who: &Actor, query: &PlanQuery) -> Result<PlanView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let review = review::review(tx, &environment)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    let frozen = deployment::freeze(
        &environment.summary.id,
        &canonicalize_environment_intent(environment.working.clone()),
        &review.head.applied,
        &query.services,
        namespace.clone(),
    )?;
    let changes = review
        .view
        .changes
        .into_iter()
        .filter(|change| frozen.targets(&change.node.id))
        .collect();
    Ok(PlanView {
        environment: environment.summary,
        version: review.view.version,
        namespace,
        changes,
        unresolved: vec!["operations".to_owned()],
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
