//! `read`'s queries. Each family lives in its own module and adds one [`Query`]
//! variant, one [`View`] variant, one arm in [`run`], and a typed method on
//! [`ConfigStore`](crate::ConfigStore) that calls the same function.

pub(crate) mod deployment;
mod diff;
mod environment;
mod service;
mod volume;

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
pub use service::{
    ServiceListing, ServiceQuery, ServiceView, ServicesQuery, ServicesView, SourceKind,
};
pub(crate) use service::{service, services};
pub use volume::{
    RemovalsQuery, RemovalsView, VolumeListing, VolumeQuery, VolumeView, VolumesQuery, VolumesView,
};
pub(crate) use volume::{removals, volume, volumes};

use crate::storage::Tx;
use crate::{Actor, Trusted};

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
    /// One Git build of a Deployment, with its log.
    BuildLog(crate::BuildLogQuery),
    /// An Environment's Services.
    Services(ServicesQuery),
    /// One Service.
    Service(ServiceQuery),
    /// Where an Environment runs on the Servers.
    Namespace(NamespaceQuery),
    /// An Environment's public domains.
    Domains(crate::DomainsQuery),
    /// One public domain, which Cloud observes afresh first.
    Domain(crate::DomainQuery),
    /// An Environment's Volumes.
    Volumes(VolumesQuery),
    /// One Volume.
    Volume(VolumeQuery),
    /// The deployed Volumes a full Deploy would remove.
    Removals(RemovalsQuery),
    /// A Branch: its Parent, Live Nodes and pending Update.
    Branch(crate::BranchQuery),
    /// The Organization's Build Order.
    BuildOrder(crate::BuildOrderQuery),
    /// A Project's Environments.
    Environments(crate::EnvironmentsQuery),
    /// A Project's PR plans.
    PrPlans(crate::PrPlansQuery),
    /// A pull request's PR Environments and GitHub check.
    PullRequest(crate::PullRequestQuery),
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
    Deployment(Box<crate::DeploymentView>),
    /// One Git build with its log.
    BuildLog(crate::BuildLogView),
    /// An Environment's Services.
    Services(ServicesView),
    /// One Service.
    Service(ServiceView),
    /// An Environment's Namespace.
    Namespace(NamespaceView),
    /// An Environment's public domains.
    Domains(crate::DomainsView),
    /// One public domain.
    Domain(crate::DomainView),
    /// An Environment's Volumes.
    Volumes(VolumesView),
    /// One Volume.
    Volume(VolumeView),
    /// What a full Deploy would remove.
    Removals(RemovalsView),
    /// A Branch.
    Branch(crate::BranchView),
    /// The Organization's Build Order.
    BuildOrder(crate::BuildOrderView),
    /// A Project's Environments.
    Environments(crate::EnvironmentsView),
    /// A Project's PR plans.
    PrPlans(crate::PrPlansView),
    /// A pull request's PR Environments and GitHub check.
    PullRequest(crate::PullRequestView),
}

pub(crate) fn run(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &Query,
    trusted: &Trusted,
) -> Result<View, RpcError> {
    match query {
        Query::Environment(query) => environment(tx, who, query).map(View::Environment),
        Query::Diff(query) => diff(tx, who, query).map(View::Diff),
        Query::Plan(query) => deployment::plan(tx, who, query).map(View::Plan),
        Query::Deployments(query) => deployment::page(tx, who, query).map(View::Deployments),
        Query::Deployment(query) => {
            crate::deployment::view(tx, who, &query.id).map(|view| View::Deployment(Box::new(view)))
        }
        Query::BuildLog(query) => crate::deployment::build_log(tx, who, query).map(View::BuildLog),
        Query::Services(query) => services(tx, who, query).map(View::Services),
        Query::Service(query) => service(tx, who, query).map(View::Service),
        Query::Namespace(query) => deployment::namespace(tx, who, query).map(View::Namespace),
        Query::Domains(query) => crate::domain::domains(tx, who, query, trusted).map(View::Domains),
        Query::Domain(query) => crate::domain::domain(tx, who, query, trusted).map(View::Domain),
        Query::Volumes(query) => volumes(tx, who, query).map(View::Volumes),
        Query::Volume(query) => volume(tx, who, query).map(View::Volume),
        Query::Removals(query) => removals(tx, who, query).map(View::Removals),
        Query::Branch(query) => crate::branch::branch(tx, who, query).map(View::Branch),
        Query::BuildOrder(_) => crate::builders::build_order(tx, who).map(View::BuildOrder),
        Query::Environments(query) => {
            crate::teardown::environments(tx, who, query).map(View::Environments)
        }
        Query::PrPlans(query) => crate::pull_request::plans(tx, who, query).map(View::PrPlans),
        Query::PullRequest(query) => {
            crate::pull_request::view(tx, who, query).map(View::PullRequest)
        }
    }
}
