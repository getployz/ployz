//! `read`'s queries. Each family lives in its own module and adds one [`Query`]
//! variant, one [`View`] variant, one arm in [`run`], and a typed method on
//! [`ConfigStore`](crate::ConfigStore) that calls the same function.

pub(crate) mod deployment;
mod diff;
mod environment;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use deployment::{
    DeploymentQuery, DeploymentsQuery, DeploymentsView, NamespaceQuery, NamespaceView, PlanQuery,
    PlanView,
};
pub use diff::DiffQuery;
pub(crate) use diff::diff;
pub(crate) use environment::environment;
pub use environment::{EnvironmentQuery, EnvironmentView, SettingRow};

use crate::Actor;
use crate::storage::Tx;

/// One question about authored configuration, answered from one consistent state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "query", rename_all = "snake_case")]
#[ts(rename = "ConfigQuery")]
pub enum Query {
    /// An Environment's Settings.
    Environment(EnvironmentQuery),
    /// What Publish would save and Deploy would apply.
    Diff(DiffQuery),
    /// What a Deploy would ship, from authored state alone.
    Plan(PlanQuery),
    /// One page of an Environment's Deployments.
    Deployments(DeploymentsQuery),
    /// One Deployment.
    Deployment(DeploymentQuery),
    /// Where an Environment runs on the Servers.
    Namespace(NamespaceQuery),
}

/// A [`Query`]'s answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "view", rename_all = "snake_case")]
#[ts(rename = "ConfigView")]
pub enum View {
    /// An Environment's Settings.
    Environment(EnvironmentView),
    /// An Environment's changes.
    Diff(crate::DiffView),
    /// A Deploy's plan.
    Plan(PlanView),
    /// A page of Deployments.
    Deployments(DeploymentsView),
    /// One Deployment.
    Deployment(crate::DeploymentView),
    /// An Environment's Namespace.
    Namespace(NamespaceView),
}

pub(crate) fn run(tx: &mut dyn Tx, who: &Actor, query: &Query) -> Result<View, RpcError> {
    match query {
        Query::Environment(query) => environment(tx, who, query).map(View::Environment),
        Query::Diff(query) => diff(tx, who, query).map(View::Diff),
        Query::Plan(query) => deployment::plan(tx, who, query).map(View::Plan),
        Query::Deployments(query) => deployment::page(tx, who, query).map(View::Deployments),
        Query::Deployment(query) => {
            crate::deployment::view(tx, who, &query.id).map(View::Deployment)
        }
        Query::Namespace(query) => deployment::namespace(tx, who, query).map(View::Namespace),
    }
}
