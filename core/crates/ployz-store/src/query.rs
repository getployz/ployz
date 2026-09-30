//! `read`'s queries, registered once in the [`queries!`] table: each row is a
//! [`Query`] variant, its [`View`] variant, and the call that answers it.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub use crate::deployment::query::{
    DeploymentQuery, DeploymentsQuery, DeploymentsView, NamespaceQuery, NamespaceView,
    NumberedDeploymentQuery, PlanQuery, PlanView,
};
pub use crate::review::diff::DiffQuery;
pub use crate::service::query::{
    ServiceListing, ServiceQuery, ServiceView, ServicesQuery, ServicesView, SourceKind,
};
pub use crate::settings::query::{EnvironmentQuery, EnvironmentView, SettingRow};
pub use crate::volume::query::{
    RemovalsQuery, RemovalsView, VolumeListing, VolumeQuery, VolumeView, VolumesQuery, VolumesView,
};

use crate::Call;
use crate::storage::Tx;

/// A [`Query`], or one of its payloads, and the view it answers with.
pub trait Ask: Serialize {
    /// What it answers with.
    type View;

    /// Answer it in `at`'s transaction.
    ///
    /// # Errors
    /// What answering it refuses.
    #[doc(hidden)]
    fn answer(&self, at: &mut Call<'_>) -> Result<Self::View, RpcError>;

    /// The whole query, tagged, as `read` takes it over HTTPS.
    fn to_query(&self) -> Value;

    /// Its answer out of a [`View`], as this query's own.
    ///
    /// # Errors
    /// `internal` for a view of another query.
    fn view(view: View) -> Result<Self::View, RpcError>;
}

/// The View variant's type: boxed when the row says `as *`.
macro_rules! view_type {
    ($answer:ty) => { $answer };
    ($answer:ty, *) => { Box<$answer> };
}

/// Register every query. A row reads `/// doc` `Variant(Payload) -> View [as *] =>
/// CALL;`, where `CALL` answers it with the names bound in the table's header.
macro_rules! queries {
    (
        |$tx:ident, $who:ident, $trusted:ident, $q:ident|
        $(
            $(#[doc = $doc:literal])*
            $variant:ident($payload:ty) -> $answer:ty $(as $unbox:tt)? => $call:expr;
        )*
    ) => {
        /// One question about authored configuration, answered from one consistent state.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "query", rename_all = "snake_case")]
        #[ts(rename = "ConfigQuery")]
        pub enum Query {
            $($(#[doc = $doc])* $variant($payload),)*
        }

        /// A [`Query`]'s answer, under the query's name.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
        #[serde(tag = "view", rename_all = "snake_case")]
        #[ts(rename = "ConfigView")]
        pub enum View {
            $($(#[doc = $doc])* $variant(view_type!($answer $(, $unbox)?)),)*
        }

        impl Ask for Query {
            type View = View;

            fn answer(&self, at: &mut Call<'_>) -> Result<View, RpcError> {
                match self {
                    $(Self::$variant(query) => {
                        query.answer(at).map(|answer| View::$variant(answer.into()))
                    })*
                }
            }

            fn to_query(&self) -> Value {
                serde_json::to_value(self).expect("queries are JSON")
            }

            fn view(view: View) -> Result<View, RpcError> {
                Ok(view)
            }
        }

        $(
            impl Ask for $payload {
                type View = $answer;

                fn answer(&self, at: &mut Call<'_>) -> Result<$answer, RpcError> {
                    let $q = self;
                    let $tx: &mut dyn Tx = &mut *at.tx;
                    let ($who, $trusted) = (at.who, at.trusted);
                    let _ = ($trusted, $q);
                    $call
                }

                fn to_query(&self) -> Value {
                    let mut query = serde_json::to_value(self).expect("queries are JSON");
                    if let Some(fields) = query.as_object_mut() {
                        fields.insert("query".to_owned(), crate::command::snake(stringify!($variant)).into());
                    }
                    query
                }

                fn view(view: View) -> Result<$answer, RpcError> {
                    match view {
                        View::$variant(answer) => Ok($($unbox)? answer),
                        _ => Err(crate::error::internal("The Store answered another query")),
                    }
                }
            }
        )*
    };
}

queries! {
    |tx, who, trusted, q|
    /// An Environment's Settings.
    Environment(EnvironmentQuery) -> EnvironmentView
        => crate::settings::query::environment(tx, who, q);
    /// What Publish would save and Deploy would apply.
    Diff(DiffQuery) -> crate::DiffView => crate::review::diff::diff(tx, who, q);
    /// What a Deploy would ship, from authored state alone.
    Plan(PlanQuery) -> PlanView => crate::deployment::query::plan(tx, who, q);
    /// One page of an Environment's Deployments.
    Deployments(DeploymentsQuery) -> DeploymentsView
        => crate::deployment::query::page(tx, who, q);
    /// One Deployment.
    Deployment(DeploymentQuery) -> crate::DeploymentView as *
        => crate::deployment::view(tx, who, &q.id);
    /// One Deployment, by its number in an Environment.
    NumberedDeployment(NumberedDeploymentQuery) -> crate::DeploymentView as *
        => crate::deployment::query::numbered(tx, who, q);
    /// One Git build of a Deployment, with its log.
    BuildLog(crate::BuildLogQuery) -> crate::BuildLogView
        => crate::deployment::build_log(tx, who, q);
    /// An Environment's Services.
    Services(ServicesQuery) -> ServicesView => crate::service::query::services(tx, who, q);
    /// One Service.
    Service(ServiceQuery) -> ServiceView => crate::service::query::service(tx, who, q);
    /// Where an Environment runs on the Servers.
    Namespace(NamespaceQuery) -> NamespaceView
        => crate::deployment::query::namespace(tx, who, q);
    /// An Environment's public domains.
    Domains(crate::DomainsQuery) -> crate::DomainsView
        => crate::domain::domains(tx, who, q, trusted);
    /// One public domain, which Cloud observes afresh first.
    Domain(crate::DomainQuery) -> crate::DomainView => crate::domain::domain(tx, who, q, trusted);
    /// An Environment's Volumes.
    Volumes(VolumesQuery) -> VolumesView => crate::volume::query::volumes(tx, who, q);
    /// One Volume.
    Volume(VolumeQuery) -> VolumeView => crate::volume::query::volume(tx, who, q);
    /// The deployed Volumes a full Deploy would remove.
    Removals(RemovalsQuery) -> RemovalsView => crate::volume::query::removals(tx, who, q);
    /// A Branch: its Parent, Live Nodes and pending Update.
    Branch(crate::BranchQuery) -> crate::BranchView => crate::branch::branch(tx, who, q);
    /// What a Branch would copy and use live, before it is created.
    BranchPlan(crate::BranchPlanQuery) -> crate::BranchPlanView
        => crate::branch::branch_plan(tx, who, q);
    /// The Organization's Build Order.
    BuildOrder(crate::BuildOrderQuery) -> crate::BuildOrderView
        => crate::builders::build_order(tx, who);
    /// What moving changes between a Branch and its Parent would stage.
    Move(crate::MoveQuery) -> crate::MoveView => crate::branch::move_view(tx, who, q);
    /// A Project's Environments.
    Environments(crate::EnvironmentsQuery) -> crate::EnvironmentsView
        => crate::teardown::environments(tx, who, q);
    /// The Organization's Projects.
    Projects(crate::ProjectsQuery) -> crate::ProjectsView => crate::teardown::projects(tx, who);
    /// A Project's PR plans.
    PrPlans(crate::PrPlansQuery) -> crate::PrPlansView
        => crate::pull_request::plans(tx, who, q);
    /// A pull request's PR Environments and GitHub check.
    PullRequest(crate::PullRequestQuery) -> crate::PullRequestView
        => crate::pull_request::view(tx, who, q);
}
