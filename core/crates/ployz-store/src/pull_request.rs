//! PR Environments and the lifecycle of Branches the system closes.
//!
//! A Project's PR plan for a GitHub repository says whether each pull request gets
//! a PR Environment: a Branch of the plan's start-from Environment holding Own
//! Copies of the repository's Services (and whatever else the plan copies), which
//! track the pull request's head branch and run one replica each. Cloud observes
//! each pull request and passes its current facts in as a system event; the Store
//! decides. An older read never replaces a newer one: facts are ordered by GitHub's
//! `updated_at`. Opening makes the PR Environment and deploys it; renaming the head
//! branch retracks it; closing closes it, unless the plan keeps it.
//!
//! Closing a Branch, for its pull request or for sitting idle a week, marks it
//! `closing`. Cloud admits its removal from the Servers (`Admit { remove }`, with
//! the runtime evidence it gathers) and the sweep deletes it once that applied.

use std::collections::BTreeSet;

use ployz_core::config::{
    SavedEnvironmentIntent, ServiceGitAccess, ServiceGitBranch, ServiceSource,
};
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::automation::{AutoDeployed, Automated, Skipped};
use crate::branch::{self, CreateBranch, SetupCommand};
use crate::command::{Admit, ProjectSummary};
use crate::deployment::{self, DeploymentStatus, DeploymentSummary};
use crate::domain::{ClusterDomain, ClusterDomainStatus};
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, EnvironmentName, Hostname, ProjectName};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, Trusted, build, review, teardown};

/// A Branch closes after this long without a Deployment.
const IDLE: i64 = 7 * 24 * 60 * 60;

/// A pull request as Cloud read it from GitHub just now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    #[ts(type = "number")]
    pub repository_id: u64,
    #[ts(type = "number")]
    pub number: u64,
    pub title: String,
    /// Its author's login.
    pub author: String,
    /// Whether its author is a bot.
    pub bot: bool,
    /// The branch it merges from.
    pub head_branch: String,
    /// That branch's head commit.
    pub head: String,
    /// The branch it merges into.
    pub target_branch: String,
    #[ts(type = "number")]
    pub commits: u64,
    pub open: bool,
    /// Its merge commit, once merged.
    #[serde(default)]
    pub merge_commit: Option<String>,
    /// GitHub's `updated_at`, like `2026-09-29T10:00:00Z`.
    pub updated: String,
}

/// Close what is due: Branches idle for a week, and closing Branches whose removal
/// from the Servers applied, or never needed one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Sweep {
    /// Cloud's clock, in seconds since the Unix epoch.
    #[ts(type = "number")]
    pub now: i64,
}

/// A pull request whose GitHub check Cloud publishes again from
/// [`crate::Query::PullRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PullRequestRef {
    #[ts(type = "number")]
    pub repository_id: u64,
    #[ts(type = "number")]
    pub number: u64,
}

/// Change a Project's PR plan for one repository: only the fields given change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetPrPlan {
    /// The Project; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// The repository, like `acme/app`: one some Service of the Project deploys from.
    pub repository: String,
    /// Whether its pull requests get PR Environments.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The Environment each PR Environment is a Branch of.
    #[serde(default)]
    pub start_from: Option<EnvironmentName>,
    /// What else each copies from it, by name; the repository's Services always are.
    #[serde(default)]
    pub copy: Option<Vec<String>>,
    /// Commands to run in its Own Copies before they first deploy.
    #[serde(default)]
    pub setup: Option<Vec<SetupCommand>>,
    /// Remove a PR Environment when its pull request closes.
    #[serde(default)]
    pub remove_on_close: Option<bool>,
    /// Make PR Environments for bots' pull requests too.
    #[serde(default)]
    pub include_bots: Option<bool>,
}

/// List a Project's PR plans.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PrPlansQuery {
    /// The Project; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
}

/// A Project's PR plan for each repository its Services deploy from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PrPlansView {
    pub project: ProjectSummary,
    pub plans: Vec<PrPlan>,
}

/// One repository's PR plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PrPlan {
    pub repository: String,
    pub enabled: bool,
    /// None until picked, or once that Environment is gone.
    pub start_from: Option<EnvironmentName>,
    pub copy: Vec<String>,
    pub setup: Vec<SetupCommand>,
    pub remove_on_close: bool,
    pub include_bots: bool,
}

/// A pull request's PR Environments and whether it is ready to merge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PullRequestQuery {
    #[ts(type = "number")]
    pub repository_id: u64,
    #[ts(type = "number")]
    pub number: u64,
}

/// What Cloud publishes as the pull request's GitHub check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PullRequestView {
    /// The latest facts Cloud reported; none before the first.
    pub pull_request: Option<PullRequest>,
    /// Its PR Environments, one per Project, not being closed.
    pub environments: Vec<PrEnvironment>,
    /// Ready to merge: nothing waits to be saved into an Environment that deploys
    /// its target branch.
    pub passing: bool,
    /// Why, in a few words.
    pub reason: String,
}

/// One PR Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PrEnvironment {
    pub environment: EnvironmentSummary,
    /// Its latest Deployment.
    pub deployment: Option<DeploymentSummary>,
    /// Where its merge lands: each Environment that deploys the target branch.
    pub destinations: Vec<Destination>,
}

/// An Environment a PR Environment's changes would be saved into.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Destination {
    pub name: EnvironmentName,
    /// The PR Environment's changes a Save would move there.
    pub changes: usize,
}

/// A plan as stored, by Environment ID and Service lineage.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Stored {
    enabled: bool,
    start_from: Option<EnvironmentId>,
    copy: Vec<String>,
    setup: Vec<Setup>,
    remove_on_close: bool,
    include_bots: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Setup {
    lineage: String,
    command: String,
}

/// A repository no plan was saved for: off, removed on close.
fn off() -> Stored {
    Stored {
        remove_on_close: true,
        ..Stored::default()
    }
}

pub(crate) fn set_plan(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetPrPlan,
) -> Result<PrPlansView, RpcError> {
    let project = scope::project(tx, who, set.project.as_ref())?;
    // Serialize plan writes per Project.
    tx.execute(
        "UPDATE config_project SET name = name WHERE id = ?1",
        &[project.id.as_str().into()],
    )?;
    let repositories = repositories(tx, &project.id)?;
    let Some((repository_id, _)) = repositories
        .iter()
        .find(|(_, name)| *name == set.repository)
    else {
        let names: Vec<&str> = repositories.iter().map(|(_, name)| name.as_str()).collect();
        return Err(error::not_found(
            format!(
                "No Service of Project {} deploys from {} through the GitHub App",
                project.name, set.repository
            ),
            json!({
                "did_you_mean": error::did_you_mean(&set.repository, names.iter().copied()),
                "valid_children": names,
            }),
        ));
    };
    let mut plan = load(tx, &project.id, *repository_id)?.unwrap_or_else(off);
    if let Some(enabled) = set.enabled {
        plan.enabled = enabled;
    }
    if let Some(remove) = set.remove_on_close {
        plan.remove_on_close = remove;
    }
    if let Some(bots) = set.include_bots {
        plan.include_bots = bots;
    }
    if let Some(name) = &set.start_from {
        let environment = scope::environment(
            tx,
            who,
            &EnvironmentRef {
                project: Some(project.name.clone()),
                environment: Some(name.clone()),
            },
        )?;
        if pr_environment(tx, &environment.summary.id)? {
            return Err(error::invalid(
                format!("{name} is a PR Environment: start from one that isn't"),
                json!({ "start_from": name }),
            ));
        }
        plan.start_from = Some(environment.summary.id);
    }
    if set.copy.is_some() || set.setup.is_some() {
        let Some(start) = plan
            .start_from
            .as_ref()
            .map(|id| start_from(tx, id))
            .transpose()?
            .flatten()
        else {
            return Err(error::invalid(
                "Pick the Environment PR Environments start from first: copies and Setup Commands name its nodes",
                json!({ "next": format!("ployz env pr {} --from ENV --project {}", set.repository, project.name) }),
            ));
        };
        if let Some(copy) = &set.copy {
            plan.copy = copy
                .iter()
                .map(|name| branch::lineage_named(&start.working, name))
                .collect::<Result<_, _>>()?;
        }
        if let Some(setup) = &set.setup {
            plan.setup = setup
                .iter()
                .map(|setup| {
                    let service = start.service(&setup.service)?;
                    Ok(Setup {
                        lineage: service.lineage_id.clone(),
                        command: branch::setup_command(setup)?,
                    })
                })
                .collect::<Result<_, RpcError>>()?;
        }
    }
    let name = set.repository.as_str();
    tx.execute(
        "INSERT INTO config_pr_plan (project_id, repository_id, organization_id, repository, plan) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (project_id, repository_id) DO UPDATE SET repository = excluded.repository, plan = excluded.plan",
        &[
            project.id.as_str().into(),
            repository(*repository_id)?.into(),
            who.organization.as_str().into(),
            name.into(),
            serde_json::to_string(&plan).expect("a plan is JSON").as_str().into(),
        ],
    )?;
    plans(
        tx,
        who,
        &PrPlansQuery {
            project: Some(project.name),
        },
    )
}

pub(crate) fn plans(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &PrPlansQuery,
) -> Result<PrPlansView, RpcError> {
    let project = scope::project(tx, who, query.project.as_ref())?;
    let mut plans = Vec::new();
    for (repository_id, name) in repositories(tx, &project.id)? {
        let stored = load(tx, &project.id, repository_id)?.unwrap_or_else(off);
        let start = match &stored.start_from {
            Some(id) => start_from(tx, id)?,
            None => None,
        };
        let named = |lineage: &str| {
            start
                .as_ref()
                .and_then(|start| branch::name_of(&start.working, lineage))
        };
        plans.push(PrPlan {
            repository: name,
            enabled: stored.enabled,
            start_from: start.as_ref().map(|start| start.summary.name.clone()),
            copy: stored
                .copy
                .iter()
                .filter_map(|lineage| named(lineage))
                .collect(),
            setup: stored
                .setup
                .iter()
                .filter_map(|setup| {
                    Some(SetupCommand {
                        service: ServiceName::parse(named(&setup.lineage)?).ok()?,
                        command: setup.command.clone(),
                    })
                })
                .collect(),
            remove_on_close: stored.remove_on_close,
            include_bots: stored.include_bots,
        });
    }
    Ok(PrPlansView {
        project: ProjectSummary {
            id: project.id,
            name: project.name,
        },
        plans,
    })
}

/// Every repository a Service of the Project deploys from through the GitHub App,
/// by ID, with its name, sorted by name.
fn repositories(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
) -> Result<Vec<(u64, String)>, RpcError> {
    let rows = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 ORDER BY id",
        &[project.as_str().into()],
    )?;
    let mut found = std::collections::BTreeMap::new();
    for row in rows {
        let id =
            EnvironmentId::parse(row.text(0)?).map_err(|_| error::corrupt("Environment ID"))?;
        for service in scope::load_by_id(tx, &id)?.working.services {
            if let ServiceSource::Git {
                repository,
                repository_id,
                access: ServiceGitAccess::GithubInstallation { .. },
                ..
            } = service.config.source
            {
                found.entry(repository_id).or_insert(repository);
            }
        }
    }
    let mut found: Vec<(u64, String)> = found.into_iter().collect();
    found.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(found)
}

fn load(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
    repository_id: u64,
) -> Result<Option<Stored>, RpcError> {
    let rows = tx.query(
        "SELECT plan FROM config_pr_plan WHERE project_id = ?1 AND repository_id = ?2",
        &[project.as_str().into(), repository(repository_id)?.into()],
    )?;
    rows.first()
        .map(|row| serde_json::from_str(row.text(0)?).map_err(|_| error::corrupt("PR plan")))
        .transpose()
}

/// The start-from Environment, unless it is gone.
fn start_from(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<Environment>, RpcError> {
    let exists = tx.query(
        "SELECT id FROM config_environment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    if exists.is_empty() {
        return Ok(None);
    }
    scope::load_by_id(tx, id).map(Some)
}

fn pr_environment(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<bool, RpcError> {
    Ok(!tx
        .query(
            "SELECT environment_id FROM config_pr_environment WHERE environment_id = ?1",
            &[id.as_str().into()],
        )?
        .is_empty())
}

/// Whether the Store is closing this Branch: nothing deploys it on push.
pub(crate) fn closing(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<bool, RpcError> {
    Ok(!tx
        .query(
            "SELECT environment_id FROM config_environment_branch WHERE environment_id = ?1 AND closing = 1",
            &[id.as_str().into()],
        )?
        .is_empty())
}

pub(crate) fn pull_request(
    tx: &mut dyn Tx,
    who: &Actor,
    event: &PullRequest,
) -> Result<Automated, RpcError> {
    validate(event)?;
    let organization = who.organization.as_str();
    let repository_id = repository(event.repository_id)?;
    let number = number(event.number)?;
    let key: [crate::storage::Param<'_>; 3] =
        [organization.into(), repository_id.into(), number.into()];
    let before: Option<PullRequest> = match tx
        .query(
            "SELECT facts FROM config_pull_request \
             WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3",
            &key,
        )?
        .first()
    {
        Some(row) => {
            Some(serde_json::from_str(row.text(0)?).map_err(|_| error::corrupt("pull request"))?)
        }
        None => None,
    };
    let [organization_param, repository_param, number_param] = key;
    let written = tx.execute(
        "INSERT INTO config_pull_request (organization_id, repository_id, number, facts, updated) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (organization_id, repository_id, number) DO UPDATE SET \
         facts = excluded.facts, updated = excluded.updated \
         WHERE excluded.updated >= config_pull_request.updated",
        &[
            organization_param,
            repository_param,
            number_param,
            serde_json::to_string(event)
                .expect("facts are JSON")
                .as_str()
                .into(),
            event.updated.as_str().into(),
        ],
    )?;
    let mut automated = Automated::default();
    if written == 0 {
        // Older than what the Store holds: never undo a newer fact.
        return Ok(automated);
    }
    let current = current(tx, who, event.repository_id, event.number)?;
    let renamed = before
        .as_ref()
        .is_some_and(|before| before.head_branch != event.head_branch);
    if renamed {
        for (environment, _) in &current {
            retrack(tx, who, environment, event)?;
        }
    }
    if !event.open {
        for (environment, project) in &current {
            let plan = load(tx, project, event.repository_id)?.unwrap_or_else(off);
            if plan.remove_on_close {
                close(tx, who, environment, &mut automated)?;
            }
        }
    } else {
        open(tx, who, event, &current, &mut automated)?;
    }
    let has_environments = !current.is_empty() || !automated.admitted.is_empty();
    if has_environments {
        automated.checks.push(PullRequestRef {
            repository_id: event.repository_id,
            number: event.number,
        });
    }
    Ok(automated)
}

/// The pull request's PR Environments not being closed, with their Projects.
fn current(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: u64,
    pr: u64,
) -> Result<Vec<(EnvironmentId, crate::id::ProjectId)>, RpcError> {
    let rows = tx.query(
        "SELECT p.environment_id, e.project_id FROM config_pr_environment p \
         JOIN config_environment e ON e.id = p.environment_id \
         JOIN config_environment_branch b ON b.environment_id = p.environment_id \
         WHERE p.organization_id = ?1 AND p.repository_id = ?2 AND p.number = ?3 AND b.closing = 0 \
         ORDER BY p.environment_id",
        &[
            who.organization.as_str().into(),
            repository(repository_id)?.into(),
            number(pr)?.into(),
        ],
    )?;
    rows.iter()
        .map(|row| {
            Ok((
                EnvironmentId::parse(row.text(0)?).map_err(|_| error::corrupt("Environment ID"))?,
                crate::id::ProjectId::parse(row.text(1)?)
                    .map_err(|_| error::corrupt("Project ID"))?,
            ))
        })
        .collect()
}

/// Make a PR Environment in every Project whose plan is on and has none yet.
fn open(
    tx: &mut dyn Tx,
    who: &Actor,
    event: &PullRequest,
    current: &[(EnvironmentId, crate::id::ProjectId)],
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT project_id, plan FROM config_pr_plan \
         WHERE organization_id = ?1 AND repository_id = ?2 ORDER BY project_id",
        &[
            who.organization.as_str().into(),
            repository(event.repository_id)?.into(),
        ],
    )?;
    for row in rows {
        let project =
            crate::id::ProjectId::parse(row.text(0)?).map_err(|_| error::corrupt("Project ID"))?;
        let plan: Stored =
            serde_json::from_str(row.text(1)?).map_err(|_| error::corrupt("PR plan"))?;
        if !plan.enabled
            || (event.bot && !plan.include_bots)
            || current.iter().any(|(_, has)| *has == project)
        {
            continue;
        }
        let Some(start) = plan
            .start_from
            .as_ref()
            .map(|id| start_from(tx, id))
            .transpose()?
            .flatten()
        else {
            continue;
        };
        match create(tx, who, &start, &plan, event) {
            Ok(Some(deployed)) => automated.admitted.push(deployed),
            Ok(None) => {}
            Err(error)
                if matches!(
                    error.code,
                    RpcErrorCode::InvalidArgument
                        | RpcErrorCode::Unsupported
                        | RpcErrorCode::NotFound
                        | RpcErrorCode::Conflict
                ) =>
            {
                // The plan can't make one: nothing to retry until someone changes it.
                automated.skipped.push(Skipped {
                    environment: start.summary.id.clone(),
                    reason: error.message,
                });
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Make the pull request's PR Environment from `start` and admit its first
/// Deployment. None when no Service of `start` deploys from the repository.
fn create(
    tx: &mut dyn Tx,
    who: &Actor,
    start: &Environment,
    plan: &Stored,
    event: &PullRequest,
) -> Result<Option<AutoDeployed>, RpcError> {
    let working = &start.working;
    let ours = |source: &ServiceSource| matches!(source, ServiceSource::Git { repository_id, .. } if *repository_id == event.repository_id);
    let mut copy: Vec<String> = working
        .services
        .iter()
        .filter(|service| ours(&service.config.source))
        .map(|service| service.slug.clone())
        .collect();
    if copy.is_empty() {
        return Ok(None);
    }
    for lineage in &plan.copy {
        if let Some(name) = branch::name_of(working, lineage)
            && !copy.contains(&name)
        {
            copy.push(name);
        }
    }
    let setup = plan
        .setup
        .iter()
        .filter_map(|setup| {
            // Setup Commands for Services it doesn't copy are left out.
            let name = branch::name_of(working, &setup.lineage)?;
            copy.contains(&name).then(|| {
                Ok(SetupCommand {
                    service: ServiceName::parse(name)
                        .map_err(|_| error::corrupt("Service name"))?,
                    command: setup.command.clone(),
                })
            })
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    let name = free_name(tx, start, event.number)?;
    let id = EnvironmentId::parse(uuid::Uuid::new_v4().to_string())?;
    let from = EnvironmentRef {
        project: Some(start.summary.project.clone()),
        environment: Some(start.summary.name.clone()),
    };
    branch::create_branch(
        tx,
        who,
        &CreateBranch {
            id: id.clone(),
            from,
            name: name.clone(),
            copy,
            live: Vec::new(),
            setup,
            keep: false,
            fix: None,
        },
    )?;
    let mut environment = scope::lock_id(tx, who, &id)?;
    environment.working = derive(environment.working, event);
    scope::save_working(tx, &mut environment)?;
    tx.execute(
        "INSERT INTO config_pr_environment (environment_id, organization_id, repository_id, number) \
         VALUES (?1, ?2, ?3, ?4)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            repository(event.repository_id)?.into(),
            number(event.number)?.into(),
        ],
    )?;
    // Generated domains expand under the Cluster Domain of the start-from's last Deploy.
    let cluster_domain = cluster_domain(tx, &start.summary.id)?;
    let trusted = Trusted {
        domains: crate::DomainEvidence {
            cluster_domain: cluster_domain.map(|name| ClusterDomain {
                name,
                status: ClusterDomainStatus::Ready,
            }),
            ..crate::DomainEvidence::default()
        },
        ..Trusted::default()
    };
    let admit = Admit {
        id: DeploymentId::parse(uuid::Uuid::new_v4().to_string())?,
        environment: EnvironmentRef {
            project: Some(environment.summary.project.clone()),
            environment: Some(name),
        },
        services: Vec::new(),
        version: None,
        upload: None,
        retry: None,
        remove: false,
        accept_volume_loss: Vec::new(),
    };
    let deployment = crate::command::admit(tx, who, &admit, &trusted)?;
    // The repository's Services build the head Cloud read.
    let pins = environment
        .working
        .services
        .iter()
        .filter(|service| ours(&service.config.source))
        .map(|service| (service.config.private_dns.clone(), event.head.clone()))
        .collect();
    build::pin(tx, &admit.id, &pins)?;
    Ok(Some(AutoDeployed {
        environment: id,
        deployment,
    }))
}

/// `pr-N`, or `pr-N-2`… when that is taken in the Project.
fn free_name(
    tx: &mut dyn Tx,
    start: &Environment,
    number: u64,
) -> Result<EnvironmentName, RpcError> {
    for attempt in 1.. {
        let name = match attempt {
            1 => format!("pr-{number}"),
            n => format!("pr-{number}-{n}"),
        };
        let taken = tx.query(
            "SELECT e.id FROM config_environment e JOIN config_environment s ON s.project_id = e.project_id \
             WHERE s.id = ?1 AND e.name = ?2",
            &[start.summary.id.as_str().into(), name.as_str().into()],
        )?;
        if taken.is_empty() {
            return EnvironmentName::parse(name);
        }
    }
    unreachable!("some name is free")
}

/// A PR Environment's configuration: the repository's Services track the head
/// branch, and every Own Copy runs one replica.
fn derive(mut intent: SavedEnvironmentIntent, event: &PullRequest) -> SavedEnvironmentIntent {
    intent = track(intent, event);
    for service in &mut intent.services {
        service.config.replicas = 1;
    }
    intent
}

/// `intent` with the repository's Services tracking the pull request's head branch.
fn track(mut intent: SavedEnvironmentIntent, event: &PullRequest) -> SavedEnvironmentIntent {
    for service in &mut intent.services {
        if let ServiceSource::Git {
            repository_id,
            branch,
            ..
        } = &mut service.config.source
            && *repository_id == event.repository_id
        {
            *branch = ServiceGitBranch::Connected {
                name: event.head_branch.clone(),
            };
        }
    }
    intent
}

/// The head branch was renamed: the PR Environment tracks the new name, in Working
/// and Saved State.
fn retrack(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    event: &PullRequest,
) -> Result<(), RpcError> {
    let mut environment = scope::lock_id(tx, who, id)?;
    if let Some(latest) = review::latest_saved(tx, id)? {
        let tracked = track(latest.intent.clone(), event);
        review::publish(tx, who, id, tracked, Some(&latest))?;
    }
    let tracked = track(environment.working.clone(), event);
    if tracked != environment.working {
        environment.working = tracked;
        scope::save_working(tx, &mut environment)?;
    }
    Ok(())
}

fn cluster_domain(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<Hostname>, RpcError> {
    tx.query(
        "SELECT cluster_domain FROM config_deployment \
         WHERE environment_id = ?1 AND cluster_domain <> '' ORDER BY number DESC LIMIT 1",
        &[id.as_str().into()],
    )?
    .first()
    .map(|row| Hostname::parse(row.text(0)?).map_err(|_| error::corrupt("Cluster Domain")))
    .transpose()
}

/// Start closing a Branch: stop what it is deploying, then remove it as far as
/// nothing needs the Servers.
fn close(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    scope::lock_id(tx, who, id)?;
    tx.execute(
        "UPDATE config_environment_branch SET closing = 1 WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    for running in deployment::history(tx, id, i64::MAX)?
        .into_iter()
        .filter(|deployment| {
            matches!(
                deployment.status,
                DeploymentStatus::Queued | DeploymentStatus::Running
            )
        })
    {
        deployment::cancel(tx, who, &running.id)?;
    }
    settle(tx, who, id, automated)
}

/// Move a closing Branch on: delete it once nothing of it can run on the Servers,
/// else ask Cloud to admit its removal, unless one is under way.
fn settle(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let environment = scope::lock_id(tx, who, id)?;
    let history = deployment::history(tx, id, i64::MAX)?;
    if history
        .iter()
        .any(|deployment| deployment.status.in_flight())
    {
        return Ok(());
    }
    let skip = |automated: &mut Automated, error: RpcError| {
        automated.skipped.push(Skipped {
            environment: id.clone(),
            reason: error.message,
        });
    };
    if let Err(error) = teardown::guard(tx, &environment) {
        skip(automated, error);
        return Ok(());
    }
    let ran = history
        .iter()
        .find(|deployment| deployment.runner.is_some());
    match ran {
        None => {}
        Some(ran) if ran.remove && ran.status == DeploymentStatus::Applied => {}
        Some(ran) if ran.remove && ran.status == DeploymentStatus::Unknown => {
            skip(
                automated,
                error::conflict(
                    format!(
                        "Nobody knows whether removing {} ran: remove it by hand",
                        environment.summary.name
                    ),
                    json!({}),
                ),
            );
            return Ok(());
        }
        Some(_) => {
            automated.closing.push(environment.summary);
            return Ok(());
        }
    }
    teardown::purge(tx, id)?;
    automated.removed.push(environment.summary);
    Ok(())
}

pub(crate) fn sweep(tx: &mut dyn Tx, who: &Actor, sweep: &Sweep) -> Result<Automated, RpcError> {
    let rows = tx.query(
        "SELECT b.environment_id, b.kept, b.closing, \
         (SELECT COALESCE(MAX(d.admitted), -1) FROM config_deployment d WHERE d.environment_id = b.environment_id), \
         (SELECT COUNT(*) FROM config_environment_branch c WHERE c.parent_id = b.environment_id), \
         (SELECT COUNT(*) FROM config_project p WHERE p.default_environment_id = b.environment_id) \
         FROM config_environment_branch b WHERE b.organization_id = ?1 ORDER BY b.environment_id",
        &[who.organization.as_str().into()],
    )?;
    let mut automated = Automated::default();
    for row in rows {
        let id =
            EnvironmentId::parse(row.text(0)?).map_err(|_| error::corrupt("Environment ID"))?;
        if row.int(2)? != 0 {
            settle(tx, who, &id, &mut automated)?;
            continue;
        }
        // Idle: not kept, deployed at least once, a week ago; never the Default
        // Environment or a Parent.
        let idle = row.int(1)? == 0
            && row.int(3)? >= 0
            && sweep.now - row.int(3)? >= IDLE
            && row.int(4)? == 0
            && row.int(5)? == 0;
        if idle {
            close(tx, who, &id, &mut automated)?;
        }
    }
    Ok(automated)
}

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &PullRequestQuery,
) -> Result<PullRequestView, RpcError> {
    let pull_request: Option<PullRequest> = match tx
        .query(
            "SELECT facts FROM config_pull_request \
             WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3",
            &[
                who.organization.as_str().into(),
                repository(query.repository_id)?.into(),
                number(query.number)?.into(),
            ],
        )?
        .first()
    {
        Some(row) => {
            Some(serde_json::from_str(row.text(0)?).map_err(|_| error::corrupt("pull request"))?)
        }
        None => None,
    };
    let target = pull_request
        .as_ref()
        .map(|facts| facts.target_branch.clone())
        .unwrap_or_default();
    let mut environments = Vec::new();
    for (id, project) in current(tx, who, query.repository_id, query.number)? {
        let environment = scope::load_by_id(tx, &id)?;
        let mut destinations = Vec::new();
        for into in destinations_of(tx, &project, query.repository_id, &target)? {
            let into = scope::load_by_id(tx, &into)?;
            destinations.push(Destination {
                changes: branch::changes_into(tx, &environment, &into)?,
                name: into.summary.name,
            });
        }
        environments.push(PrEnvironment {
            deployment: deployment::history(tx, &id, 1)?.into_iter().next(),
            environment: environment.summary,
            destinations,
        });
    }
    let (passing, reason) = check(&environments, &target);
    Ok(PullRequestView {
        pull_request,
        environments,
        passing,
        reason,
    })
}

/// Whether the pull request is ready to merge, and why.
fn check(environments: &[PrEnvironment], target: &str) -> (bool, String) {
    let destinations: Vec<&Destination> = environments
        .iter()
        .flat_map(|environment| &environment.destinations)
        .collect();
    if destinations.is_empty() {
        return (true, format!("No environment deploys {target}"));
    }
    let changes: usize = destinations
        .iter()
        .map(|destination| destination.changes)
        .sum();
    if changes == 0 {
        let names: BTreeSet<&str> = destinations
            .iter()
            .map(|destination| destination.name.as_str())
            .collect();
        return (
            true,
            format!(
                "No changes for {}",
                names.into_iter().collect::<Vec<_>>().join(", ")
            ),
        );
    }
    let plural = if changes == 1 { "change" } else { "changes" };
    (false, format!("{changes} {plural} to save in Ployz"))
}

/// Where a merge into `target` lands: each Environment of the Project, other than
/// PR Environments, whose latest Saved State has a Service from the repository
/// tracking `target`, with none above it tracking it too.
fn destinations_of(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
    repository_id: u64,
    target: &str,
) -> Result<Vec<EnvironmentId>, RpcError> {
    let rows = tx.query(
        "SELECT e.id, COALESCE(b.parent_id, '') FROM config_environment e \
         LEFT JOIN config_environment_branch b ON b.environment_id = e.id \
         WHERE e.project_id = ?1 AND e.id NOT IN (SELECT environment_id FROM config_pr_environment) \
         ORDER BY e.id",
        &[project.as_str().into()],
    )?;
    let mut parents = std::collections::BTreeMap::new();
    let mut tracking = BTreeSet::new();
    for row in rows {
        let id =
            EnvironmentId::parse(row.text(0)?).map_err(|_| error::corrupt("Environment ID"))?;
        let tracks = review::latest_saved(tx, &id)?.is_some_and(|saved| {
            saved.intent.services.iter().any(|service| {
                matches!(
                    &service.config.source,
                    ServiceSource::Git { repository_id: at, branch: ServiceGitBranch::Connected { name }, .. }
                        if *at == repository_id && name == target
                )
            })
        });
        if tracks {
            tracking.insert(id.clone());
        }
        parents.insert(id, row.text(1)?.to_owned());
    }
    let above = |id: &EnvironmentId| {
        let mut at = parents.get(id).cloned().unwrap_or_default();
        let mut hops = 0;
        while !at.is_empty() && hops < parents.len() {
            if tracking.iter().any(|tracked| tracked.as_str() == at) {
                return true;
            }
            at = parents
                .iter()
                .find(|(candidate, _)| candidate.as_str() == at)
                .map(|(_, parent)| parent.clone())
                .unwrap_or_default();
            hops += 1;
        }
        false
    };
    Ok(tracking.iter().filter(|id| !above(id)).cloned().collect())
}

fn validate(event: &PullRequest) -> Result<(), RpcError> {
    crate::git::valid_branch(&event.head_branch)?;
    crate::git::valid_branch(&event.target_branch)?;
    for commit in std::iter::once(&event.head).chain(&event.merge_commit) {
        if !ployz_core::is_lower_hex(commit, 40) {
            return Err(error::invalid(
                "A commit is a full lowercase Git commit",
                json!({}),
            ));
        }
    }
    let text_ok =
        |text: &str, most: usize| text.len() <= most && !text.chars().any(char::is_control);
    if !text_ok(&event.title, 1024) || !text_ok(&event.author, 255) {
        return Err(error::invalid(
            "Expected GitHub's title and login",
            json!({}),
        ));
    }
    crate::automation::timestamp(&event.updated)
}

fn repository(id: u64) -> Result<i64, RpcError> {
    i64::try_from(id).map_err(|_| error::invalid("Expected a GitHub repository ID", json!({})))
}

fn number(number: u64) -> Result<i64, RpcError> {
    i64::try_from(number).map_err(|_| error::invalid("Expected a pull request number", json!({})))
}
