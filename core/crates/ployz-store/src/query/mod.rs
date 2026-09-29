//! `read`'s queries. Each family lives in its own module and adds one [`Query`]
//! variant, one [`View`] variant, and one arm in [`run`].

mod deployment;
mod diff;
mod environment;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};

pub use deployment::{DeploymentQuery, DeploymentsQuery, DeploymentsView, PlanQuery, PlanView};
pub use diff::DiffQuery;
pub use environment::{EnvironmentQuery, EnvironmentView, SettingRow};

use crate::Actor;
use crate::storage::Tx;

/// One question about authored configuration, answered from one consistent state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum Query {
    Environment(EnvironmentQuery),
    Diff(DiffQuery),
    Plan(PlanQuery),
    Deployments(DeploymentsQuery),
    Deployment(DeploymentQuery),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum View {
    Environment(EnvironmentView),
    Diff(crate::DiffView),
    Plan(PlanView),
    Deployments(DeploymentsView),
    Deployment(crate::DeploymentView),
}

pub(crate) fn run(tx: &mut dyn Tx, who: &Actor, query: &Query) -> Result<View, RpcError> {
    match query {
        Query::Environment(query) => environment::run(tx, who, query).map(View::Environment),
        Query::Diff(query) => diff::run(tx, who, query).map(View::Diff),
        Query::Plan(query) => deployment::plan(tx, who, query).map(View::Plan),
        Query::Deployments(query) => deployment::page(tx, who, query).map(View::Deployments),
        Query::Deployment(query) => deployment::show(tx, who, query).map(View::Deployment),
    }
}
