//! Tools about one Environment as a whole: what is in it, pull request
//! Environments built from it, and what it needs before it can deploy.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at, store};
use super::refusal::{Call, Refusal};
use super::step::{Ask, CloudQuery, DashboardPage};
use crate::catalog::NODE_NAME;
use crate::{
    ConfigsQuery, DeploymentsQuery, DiffQuery, DomainName, DomainRow, DomainsQuery,
    EnvironmentName, PrPlansQuery, ProjectName, ProjectsQuery, RepositoryName, ServicesQuery,
    SetPrPlan, SourceKind, VolumesQuery,
};

pub(crate) const REPOSITORY: &str = "^[A-Za-z0-9-]+/[A-Za-z0-9._-]+$";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvironmentShow {
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for EnvironmentShow {
    const NAME: &'static str = "environment_show";
    const DESCRIPTION: &'static str = "Where you are: Projects and Environments, Services with source and domains, Volumes, Configs, how many changes are Staged, and the latest Deployment.";
    const READ_ONLY: bool = true;

    fn example() -> Value {
        json!({ "environment": "staging" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(self.project.as_ref(), self.environment.as_ref());
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
            .deployments
            .first()
            .map(|deployment| json!({ "number": deployment.number, "status": deployment.status }));
        Ok(json!({
            "at": at(&diff.environment),
            "revision": diff.environment.revision.0,
            "projects": projects,
            "services": services,
            "volumes": volumes.volumes.iter().map(|volume| &volume.volume.name).collect::<Vec<_>>(),
            "configs": configs.configs.iter().map(|config| &config.config.name).collect::<Vec<_>>(),
            "staged": diff.total_count,
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

pub(crate) const fn source(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Empty => "empty",
        SourceKind::Uploaded => "uploaded",
        SourceKind::Git => "repository",
        SourceKind::Image => "image",
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvironmentPr {
    /// true to build an Environment for each pull request, false to stop.
    enabled: bool,
    /// GitHub repository owner/name whose pull requests get Environments. Omit when the Project has one.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(REPOSITORY))]
    repository: Option<RepositoryName>,
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

impl Tool for EnvironmentPr {
    const NAME: &'static str = "environment_pr";
    const DESCRIPTION: &'static str =
        "Turn on or off an Environment per GitHub pull request, built from this Environment.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "enabled": true, "repository": "acme/api" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let mut answers = cx
            .read(vec![
                store(&PrPlansQuery {
                    project: scope.project.clone(),
                })?,
                store(&ServicesQuery {
                    environment: scope.clone(),
                })?,
            ])?
            .into_iter();
        let plans = cx.view::<PrPlansQuery>(answers.next())?;
        let services = cx.view::<ServicesQuery>(answers.next())?;
        if self
            .expect_revision
            .is_some_and(|expect| services.environment.revision.0 != expect)
        {
            return Err(super::machine::stale(cx.call()));
        }
        let repository = match (self.repository, plans.plans.as_slice()) {
            (Some(repository), _) => repository,
            (None, [only]) => only.repository.clone(),
            (None, []) => {
                return Err(Refusal::invalid(
                    "No repository is connected to this Project yet",
                    "repository",
                )
                .with_next(Call::new("github_connect", json!({})))
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
            "at": at(&services.environment),
            "pull_request_environments": enabled,
            "repository": repository,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvironmentSetup {
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

/// Cloud's answer about Servers. Provisional: the Cloud adapter fixes it.
#[derive(Deserialize)]
struct ServersAnswer {
    #[serde(default)]
    servers: Vec<Value>,
    #[serde(default)]
    cluster_domain: Option<String>,
}

/// Cloud's answer about GitHub. Provisional: the Cloud adapter fixes it.
#[derive(Deserialize)]
pub(crate) struct GithubAnswer {
    #[serde(default)]
    pub(crate) connected: bool,
    #[serde(default)]
    pub(crate) repositories: Vec<String>,
    #[serde(default)]
    pub(crate) connect_at: Option<String>,
}

impl Tool for EnvironmentSetup {
    const NAME: &'static str = "environment_setup";
    const DESCRIPTION: &'static str = "What this Environment still needs before it can deploy: servers, a GitHub connection, a cluster domain.";
    const READ_ONLY: bool = true;
    const LOCAL_NEXT: Option<&'static str> = Some("environment_show");

    fn example() -> Value {
        json!({ "environment": "staging" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let environment = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let mut answers = cx
            .read(vec![
                Ask::Cloud(CloudQuery::Servers { only: None }),
                Ask::Cloud(CloudQuery::GithubConnection),
                store(&ServicesQuery { environment })?,
            ])?
            .into_iter();
        let servers = cx.cloud::<ServersAnswer>(answers.next())?;
        let github = cx.cloud::<GithubAnswer>(answers.next())?;
        let services = cx.view::<ServicesQuery>(answers.next())?;
        let builds = services
            .services
            .iter()
            .any(|listing| listing.source == SourceKind::Git);
        let needs = [
            (
                servers.servers.is_empty(),
                "servers",
                DashboardPage::Servers,
            ),
            (
                servers.cluster_domain.is_none(),
                "cluster domain",
                DashboardPage::Servers,
            ),
            (
                builds && !github.connected,
                "GitHub connection",
                DashboardPage::Github,
            ),
        ]
        .into_iter()
        .filter(|(missing, ..)| *missing)
        .map(|(_, what, page)| json!({ "what": what, "where": page }))
        .collect::<Vec<_>>();
        Ok(json!({
            "at": at(&services.environment),
            "ready": needs.is_empty(),
            "needs": needs,
        }))
    }
}
