//! `read`'s queries: each feature module adds one [`Query`] variant, one [`View`]
//! variant and one arm in [`run`].

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use crate::deployment::query::{
    DeploymentQuery, DeploymentsQuery, DeploymentsView, NamespaceQuery, NamespaceView, PlanQuery,
    PlanView,
};
pub use crate::review::diff::DiffQuery;
pub(crate) use crate::review::diff::diff;
pub use crate::service::query::{
    ServiceListing, ServiceQuery, ServiceView, ServicesQuery, ServicesView, SourceKind,
};
pub(crate) use crate::service::query::{service, services};
pub(crate) use crate::settings::query::environment;
pub use crate::settings::query::{EnvironmentQuery, EnvironmentView, SettingRow};
pub use crate::volume::query::{
    RemovalsQuery, RemovalsView, VolumeListing, VolumeQuery, VolumeView, VolumesQuery, VolumesView,
};
pub(crate) use crate::volume::query::{removals, volume, volumes};

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
    /// What a Branch would copy and use live, before it is created.
    BranchPlan(crate::BranchPlanQuery),
    /// The Organization's Build Order.
    BuildOrder(crate::BuildOrderQuery),
    /// What moving changes between a Branch and its Parent would stage.
    Move(crate::MoveQuery),
    /// A Project's Environments.
    Environments(crate::EnvironmentsQuery),
    /// The Organization's Projects.
    Projects(crate::ProjectsQuery),
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
    /// A planned Branch.
    BranchPlan(crate::BranchPlanView),
    /// The Organization's Build Order.
    BuildOrder(crate::BuildOrderView),
    /// A Move's changes.
    Move(crate::MoveView),
    /// A Project's Environments.
    Environments(crate::EnvironmentsView),
    /// The Organization's Projects.
    Projects(crate::ProjectsView),
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
        Query::Plan(query) => crate::deployment::query::plan(tx, who, query).map(View::Plan),
        Query::Deployments(query) => {
            crate::deployment::query::page(tx, who, query).map(View::Deployments)
        }
        Query::Deployment(query) => {
            crate::deployment::view(tx, who, &query.id).map(|view| View::Deployment(Box::new(view)))
        }
        Query::BuildLog(query) => crate::deployment::build_log(tx, who, query).map(View::BuildLog),
        Query::Services(query) => services(tx, who, query).map(View::Services),
        Query::Service(query) => service(tx, who, query).map(View::Service),
        Query::Namespace(query) => {
            crate::deployment::query::namespace(tx, who, query).map(View::Namespace)
        }
        Query::Domains(query) => crate::domain::domains(tx, who, query, trusted).map(View::Domains),
        Query::Domain(query) => crate::domain::domain(tx, who, query, trusted).map(View::Domain),
        Query::Volumes(query) => volumes(tx, who, query).map(View::Volumes),
        Query::Volume(query) => volume(tx, who, query).map(View::Volume),
        Query::Removals(query) => removals(tx, who, query).map(View::Removals),
        Query::Branch(query) => crate::branch::branch(tx, who, query).map(View::Branch),
        Query::BranchPlan(query) => {
            crate::branch::branch_plan(tx, who, query).map(View::BranchPlan)
        }
        Query::BuildOrder(_) => crate::builders::build_order(tx, who).map(View::BuildOrder),
        Query::Move(query) => crate::branch::move_view(tx, who, query).map(View::Move),
        Query::Environments(query) => {
            crate::teardown::environments(tx, who, query).map(View::Environments)
        }
        Query::Projects(_) => crate::teardown::projects(tx, who).map(View::Projects),
        Query::PrPlans(query) => crate::pull_request::plans(tx, who, query).map(View::PrPlans),
        Query::PullRequest(query) => {
            crate::pull_request::view(tx, who, query).map(View::PullRequest)
        }
    }
}

/// A [`Query`], or one of its payloads, and the view it answers with.
pub trait Ask: Clone {
    type View;
    fn query(self) -> Query;
    /// The answer, as this asker's view.
    ///
    /// # Errors
    /// `internal` for a view of another query.
    fn view(view: View) -> Result<Self::View, RpcError>;
}

impl Ask for Query {
    type View = View;
    fn query(self) -> Query {
        self
    }
    fn view(view: View) -> Result<View, RpcError> {
        Ok(view)
    }
}

macro_rules! asks {
    ($($query:ty => $variant:ident($answer:ty) $(as $unbox:tt)?),* $(,)?) => {$(
        impl Ask for $query {
            type View = $answer;
            fn query(self) -> Query {
                Query::$variant(self)
            }
            fn view(view: View) -> Result<$answer, RpcError> {
                match view {
                    View::$variant(answer) => Ok($($unbox)? answer),
                    _ => Err(crate::error::internal("The Store answered another query")),
                }
            }
        }
    )*};
}

asks!(
    EnvironmentQuery => Environment(EnvironmentView),
    DiffQuery => Diff(crate::DiffView),
    PlanQuery => Plan(PlanView),
    DeploymentsQuery => Deployments(DeploymentsView),
    DeploymentQuery => Deployment(crate::DeploymentView) as *,
    crate::BuildLogQuery => BuildLog(crate::BuildLogView),
    ServicesQuery => Services(ServicesView),
    ServiceQuery => Service(ServiceView),
    NamespaceQuery => Namespace(NamespaceView),
    crate::DomainsQuery => Domains(crate::DomainsView),
    crate::DomainQuery => Domain(crate::DomainView),
    VolumesQuery => Volumes(VolumesView),
    VolumeQuery => Volume(VolumeView),
    RemovalsQuery => Removals(RemovalsView),
    crate::BranchQuery => Branch(crate::BranchView),
    crate::BranchPlanQuery => BranchPlan(crate::BranchPlanView),
    crate::BuildOrderQuery => BuildOrder(crate::BuildOrderView),
    crate::MoveQuery => Move(crate::MoveView),
    crate::EnvironmentsQuery => Environments(crate::EnvironmentsView),
    crate::ProjectsQuery => Projects(crate::ProjectsView),
    crate::PrPlansQuery => PrPlans(crate::PrPlansView),
    crate::PullRequestQuery => PullRequest(crate::PullRequestView),
);
