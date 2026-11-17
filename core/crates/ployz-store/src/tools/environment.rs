//! Tools about one Environment as a whole: what is in it, pull request
//! Environments built from it, and what it needs before it can deploy.

use serde::Deserialize;
use serde_json::{Value, json};

use super::ToolId;
use super::cloud::{GithubAnswer, ServersAnswer};
use super::input::tool_input;
use super::machine::{Cx, Effect, Reach, Stop, Tool, store};
use super::refusal::{Call, Refusal, Writes, from_store};
use super::schema::REPOSITORY;
use super::service::source;
use super::step::{Ask, CloudQuery, DashboardPage, Gate};
use crate::{
    Change, ConfigsQuery, DeploymentsQuery, DiffQuery, DomainName, DomainRow, DomainsQuery, Edit,
    PrPlansQuery, ProjectsQuery, RepositoryName, ServicesQuery, SetEnvironmentProtected, SetPrPlan,
    SettingPath, SourceKind, VolumesQuery,
};
use ployz_core::Region;

tool_input! {
    pub(crate) struct EnvironmentShow {
        @place(named),
    }
}

impl Tool for EnvironmentShow {
    const NAME: &'static str = "environment_show";
    const TITLE: &'static str = "Show an Environment";
    const DESCRIPTION: &'static str = "Show an Environment: its Services with source and domains, Volumes, Configs, how many changes are staged, broken references, latest Deployment and revision. Also lists every Project with its Environments.";
    const EFFECT: Effect = Effect::Read;

    fn example() -> Value {
        json!({ "environment": "staging" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(&self);
        let asks = vec![
            store(&ProjectsQuery {})?,
            store(&ServicesQuery {
                environment: environment.clone(),
            })?,
            store(&VolumesQuery {
                environment: environment.clone(),
            })?,
            store(&ConfigsQuery {
                environment: environment.clone(),
            })?,
            store(&DomainsQuery {
                environment: environment.clone(),
                service: None,
            })?,
            store(&DiffQuery {
                environment: environment.clone(),
            })?,
            store(&DeploymentsQuery {
                environment,
                limit: Some(1),
                cursor: None,
            })?,
        ];
        let mut answers = cx.read(asks)?.into_iter();
        let projects = cx.view::<ProjectsQuery>(answers.next())?;
        // An Organization with no Project yet is an empty list, not a refusal; its scoped reads all failed for it.
        if projects.projects.is_empty() && self.project.is_none() {
            return Ok(json!({
                "at": null,
                "revision": null,
                "projects": [],
                "services": [],
                "volumes": [],
                "configs": [],
                "staged": 0,
                "latest_deployment": null,
                "next": Call::to(ToolId::ProjectCreate, json!({})),
            }));
        }
        let services = cx.view::<ServicesQuery>(answers.next())?;
        let volumes = cx.view::<VolumesQuery>(answers.next())?;
        let configs = cx.view::<ConfigsQuery>(answers.next())?;
        let domains = cx.view::<DomainsQuery>(answers.next())?;
        let diff = cx.view::<DiffQuery>(answers.next())?;
        let deployments = cx.view::<DeploymentsQuery>(answers.next())?;
        let projects = projects
            .projects
            .iter()
            .map(|project| json!({ "name": project.name, "environments": project.environments }))
            .collect::<Vec<_>>();
        let services = services_with_domains(&services.services, &domains.domains);
        let latest = deployments
            .latest
            .as_ref()
            .map(|deployment| json!({ "number": deployment.number, "status": deployment.status }));
        Ok(json!({
            "at": diff.environment.to_string(),
            "revision": diff.environment.revision.0,
            "projects": projects,
            "services": services,
            "volumes": volumes.volumes.iter().map(|volume| &volume.volume.name).collect::<Vec<_>>(),
            "configs": configs.configs.iter().map(|config| &config.config.name).collect::<Vec<_>>(),
            "staged": diff.review_count(),
            "broken": diff.broken.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "latest_deployment": latest,
        }))
    }
}

fn services_with_domains(services: &[crate::ServiceListing], domains: &[DomainRow]) -> Vec<Value> {
    services
        .iter()
        .map(|listing| {
            let name = &listing.service.name;
            let domains = domains
                .iter()
                .filter(|row| row.domain.service == *name)
                .map(|row| domain_label(&row.domain.name))
                .collect::<Vec<_>>();
            json!({ "name": name, "source": source(listing.source), "domains": domains })
        })
        .collect()
}

/// A domain as an agent names it: its hostname, else a generated one's prefix.
pub(crate) fn domain_label(name: &DomainName) -> String {
    match name {
        DomainName::Generated {
            hostname: Some(hostname),
            ..
        } => hostname.clone(),
        DomainName::Generated { prefix, .. } => prefix.clone(),
        DomainName::Custom { hostname } => hostname.to_string(),
    }
}

tool_input! {
    pub(crate) struct EnvironmentPr {
        /// `true` to give each pull request its own Environment, `false` to stop.
        enabled: bool,
        /// GitHub repository whose pull requests get Environments, e.g. `acme/api`. Optional when the Project's Services deploy from only one.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(REPOSITORY))]
        repository: Option<RepositoryName>,
        @revision,
        @place(named),
    }
}

impl Tool for EnvironmentPr {
    const NAME: &'static str = "environment_pr";
    const TITLE: &'static str = "Turn PR Environments on or off";
    const DESCRIPTION: &'static str = "Turn PR Environments on or off. When on, each pull request to a GitHub repository gets its own Branch of this Environment. Takes effect right away.";
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "enabled": true, "repository": "acme/api" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let mut answers = cx
            .read(vec![
                store(&PrPlansQuery {
                    project: scope.project.clone(),
                })?,
                store(&ServicesQuery { environment: scope })?,
            ])?
            .into_iter();
        let plans = cx.view::<PrPlansQuery>(answers.next())?;
        let services = cx.view::<ServicesQuery>(answers.next())?;
        cx.fresh(&services.environment, self.expect_revision)?;
        let repository = match (self.repository, plans.plans.as_slice()) {
            (Some(repository), _) => repository,
            (None, [only]) => only.repository.clone(),
            (None, []) if !self.enabled => {
                return Ok(json!({
                    "at": services.environment.to_string(),
                    "pull_request_environments": false,
                }));
            }
            (None, []) => {
                return Err(Refusal::invalid(
                    "No repository is connected to this Project yet",
                    "repository",
                )
                .with_next(Call::to(ToolId::GithubConnect, json!({})))
                .into());
            }
            (None, plans) => {
                return Err(Refusal::invalid(
                    "This Project has several repositories; name one",
                    "repository",
                )
                .with_valid(plans.iter().map(|plan| plan.repository.as_str()))
                .into());
            }
        };
        let written = cx.write(&SetPrPlan {
            project: Some(services.environment.project.clone()),
            repository: repository.clone(),
            enabled: Some(self.enabled),
            start_from: Some(services.environment.name.clone()),
            copy: None,
            setup: None,
            remove_on_close: None,
            include_bots: None,
        })?;
        let enabled = written
            .plans
            .iter()
            .find(|plan| plan.repository == repository)
            .is_some_and(|plan| plan.enabled);
        Ok(json!({
            "at": services.environment.to_string(),
            "pull_request_environments": enabled,
            "repository": repository,
        }))
    }
}

tool_input! {
    pub(crate) struct EnvironmentProtect {
        /// `true` to block removing this Environment, `false` to allow it again.
        protected: bool,
        @place(named),
    }
}

impl Tool for EnvironmentProtect {
    const NAME: &'static str = "environment_protect";
    const TITLE: &'static str = "Protect or unprotect an Environment";
    const DESCRIPTION: &'static str = "Protect an Environment so it can't be removed, or unprotect it. Takes effect right away. Unprotecting asks a human to approve, and Branches and PR Environments can't be Protected.";
    /// Cloud asks a human before an agent unprotects one.
    const GATE: Gate = Gate::Cloud;
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "environment": "production", "protected": true })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let written = cx.write(&SetEnvironmentProtected {
            environment: scope,
            protected: self.protected,
            seen: None,
        })?;
        let listing = written
            .view
            .environments
            .iter()
            .find(|listing| listing.id == written.environment);
        Ok(json!({
            "at": listing.map(|listing| format!("{}/{}", written.view.project.name, listing.name)),
            "protected": listing.is_some_and(|listing| listing.protected),
        }))
    }
}

tool_input! {
    pub(crate) struct EnvironmentRegion {
        /// The region to run its Services in, as the Servers' `region` label names it, e.g. `fsn`.
        #[serde(default)]
        #[schemars(with = "Option<String>", example = &"fsn")]
        region: Option<Region>,
        /// `true` to clear the region, so its Services run in any region.
        #[serde(default)]
        clear: bool,
        @place(named),
    }
}

impl Tool for EnvironmentRegion {
    const NAME: &'static str = "environment_region";
    const TITLE: &'static str = "Set an Environment's region";
    const DESCRIPTION: &'static str = "Set the region an Environment's Services run in, or clear it so they run in any region. A region is the value of a Server's `region` label. Staged until the next deploy, which moves each Service to a Server in the region. A Service whose Volume is on a Server outside the region is not started until the Volume is moved.";
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "environment": "production", "region": "fsn" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(&self);
        let path = SettingPath::REGION;
        let change = match (&self.region, self.clear) {
            (Some(region), false) => {
                if !cx.is_local() {
                    let answer = cx
                        .read(vec![Ask::Cloud(CloudQuery::Servers { only: None })])?
                        .into_iter()
                        .next();
                    if let Some(regions) = cx.cloud::<ServersAnswer>(answer)?.regions {
                        crate::refuse_unknown_region(region, &regions)
                            .map_err(|error| from_store(error, cx.call(), Writes::None))?;
                    }
                }
                Change::Set {
                    path,
                    value: json!(region),
                }
            }
            (None, true) => Change::Unset { path },
            (None, false) | (Some(_), true) => {
                return Err(Refusal::invalid(
                    "Name a region, or set clear to run in any region",
                    "region",
                )
                .into());
            }
        };
        let edited = cx.write(&Edit {
            environment,
            expect: None,
            changes: vec![change],
        })?;
        Ok(json!({
            "at": edited.environment.to_string(),
            "region": self.region,
            "staged": !edited.staged.is_empty(),
        }))
    }
}

tool_input! {
    pub(crate) struct EnvironmentSetup {
        @place(named),
    }
}

/// Cloud's answer about Servers, with the Cluster Domain a Cloud that knows it names
/// beside them.
#[derive(Deserialize)]
struct SetupServers {
    #[serde(flatten)]
    answer: ServersAnswer,
    #[serde(default)]
    cluster_domain: Option<String>,
}

impl Tool for EnvironmentSetup {
    const NAME: &'static str = "environment_setup";
    const TITLE: &'static str = "Check what an Environment needs";
    const DESCRIPTION: &'static str = "Check what an Environment still needs before it can deploy: Servers, a Cluster Domain, and a GitHub connection if a Service builds from a repository. Returns whether it is ready and the dashboard page for each thing missing.";
    const EFFECT: Effect = Effect::Read;
    const REACH: Reach = Reach::CloudOnly {
        local_next: Some(ToolId::EnvironmentShow),
    };

    fn example() -> Value {
        json!({ "environment": "staging" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(&self);
        let mut answers = cx
            .read(vec![
                Ask::Cloud(CloudQuery::Servers { only: None }),
                Ask::Cloud(CloudQuery::GithubConnection),
                store(&ServicesQuery { environment })?,
            ])?
            .into_iter();
        let servers = cx.cloud::<SetupServers>(answers.next())?;
        let github_unless_unclaimed = cx.cloud::<GithubAnswer>(answers.next());
        let services = cx.view::<ServicesQuery>(answers.next())?;
        let builds = services
            .services
            .iter()
            .any(|listing| listing.source == SourceKind::Git);
        let unconnected = builds && !github_unless_unclaimed?.connected;
        let needs = [
            (
                servers.answer.servers.is_empty(),
                "servers",
                DashboardPage::Servers,
            ),
            (
                servers.cluster_domain.is_none(),
                "cluster domain",
                DashboardPage::Servers,
            ),
            (unconnected, "GitHub connection", DashboardPage::Github),
        ]
        .into_iter()
        .filter(|(missing, ..)| *missing)
        .map(|(_, what, page)| json!({ "what": what, "where": page }))
        .collect::<Vec<_>>();
        Ok(json!({
            "at": services.environment.to_string(),
            "ready": needs.is_empty(),
            "needs": needs,
        }))
    }
}
