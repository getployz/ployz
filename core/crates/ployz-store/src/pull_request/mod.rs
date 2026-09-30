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

mod checks;
mod plan;
pub(crate) use checks::*;
pub(crate) use plan::*;

use crate::id::{BranchName, CommitSha, PullRequestNumber, RepositoryId, RepositoryName};
use std::collections::BTreeSet;

use ployz_core::config::{
    SavedEnvironmentIntent, ServiceGitAccess, ServiceGitBranch, ServiceSource,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::automation::{AutoDeployed, Automated, Skipped};
use crate::branch::{self, CreateBranch, SetupCommand};
use crate::deployment::{self, DeploymentStatus, DeploymentSummary};
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectName};
use crate::project::ProjectSummary;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::NodeName;
use crate::storage::Tx;
use crate::{Actor, Trusted, review, teardown};

/// A Branch closes after this long without a Deployment.
const IDLE: i64 = 7 * 24 * 60 * 60;

/// A pull request as Cloud read it from GitHub just now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub repository_id: RepositoryId,
    pub number: PullRequestNumber,
    pub title: String,
    /// Its author's login.
    pub author: String,
    /// Whether its author is a bot.
    pub bot: bool,
    /// The branch it merges from.
    pub head_branch: BranchName,
    /// That branch's head commit.
    pub head: CommitSha,
    /// The branch it merges into.
    pub target_branch: BranchName,
    #[ts(type = "number")]
    pub commits: u64,
    pub open: bool,
    /// Its merge commit, once merged.
    #[serde(default)]
    pub merge_commit: Option<CommitSha>,
    /// Once merged: the target branch's head as the Store last saw it
    /// ([`crate::ConfigStore::branch_head`]), when Cloud found the merge commit in it
    /// already. Its Conditional Saves then land with what that push deployed.
    #[serde(default)]
    pub merge_reached: Option<CommitSha>,
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
    pub repository_id: RepositoryId,
    pub number: PullRequestNumber,
}

/// Change a Project's PR plan for one repository: only the fields given change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetPrPlan {
    /// The Project; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// The repository, like `acme/app`: one some Service of the Project deploys from.
    pub repository: RepositoryName,
    /// Whether its pull requests get PR Environments.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The Environment each PR Environment is a Branch of.
    #[serde(default)]
    pub start_from: Option<EnvironmentName>,
    /// What else each copies from it, by name; the repository's Services always are.
    #[serde(default)]
    pub copy: Option<Vec<NodeName>>,
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
    pub repository: RepositoryName,
    pub repository_id: RepositoryId,
    /// The GitHub App installation its Services deploy through.
    #[ts(type = "number")]
    pub installation_id: u64,
    pub enabled: bool,
    /// None until picked, or once that Environment is gone.
    pub start_from: Option<EnvironmentName>,
    pub copy: Vec<NodeName>,
    pub setup: Vec<SetupCommand>,
    pub remove_on_close: bool,
    pub include_bots: bool,
    /// Its pull requests with a PR Environment in the Project, not being closed.
    pub open: Vec<OpenPullRequest>,
}

/// A pull request with a PR Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OpenPullRequest {
    pub number: PullRequestNumber,
    /// Empty until Cloud reports its facts.
    pub title: String,
    pub author: String,
    pub environment: EnvironmentName,
}

/// A pull request's PR Environments and whether it is ready to merge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PullRequestQuery {
    pub repository_id: RepositoryId,
    pub number: PullRequestNumber,
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
    /// Its Conditional Save there, if any.
    pub save: Option<DestinationSave>,
}

/// A PR Environment's Conditional Save into one Destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DestinationSave {
    pub id: crate::ConditionalSaveId,
    /// False once the PR Environment or the target branch changed since: save again.
    pub standing: bool,
    /// How many changes it holds.
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

/// The repository's pull requests with a PR Environment in the Project, not being closed.
fn open_in(
    tx: &mut dyn Tx,
    who: &Actor,
    project: &crate::id::ProjectId,
    repository_id: RepositoryId,
) -> Result<Vec<OpenPullRequest>, RpcError> {
    let rows = tx.query(
        "SELECT p.number, e.name FROM config_pr_environment p \
         JOIN config_environment e ON e.id = p.environment_id \
         JOIN config_environment_branch b ON b.environment_id = p.environment_id \
         WHERE e.project_id = ?1 AND p.repository_id = ?2 AND b.closing = 0 ORDER BY p.number",
        &[project.as_str().into(), repository_id.into()],
    )?;
    let mut open = Vec::new();
    for row in rows {
        let number: PullRequestNumber = row.number(0, "pull request")?;
        let environment = row.parse::<EnvironmentName>(1, "Environment name")?;
        let facts = facts(tx, who, repository_id, number)?;
        open.push(OpenPullRequest {
            number,
            title: facts.as_ref().map(|f| f.title.clone()).unwrap_or_default(),
            author: facts.map(|f| f.author).unwrap_or_default(),
            environment,
        });
    }
    Ok(open)
}

fn load(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
    repository_id: RepositoryId,
) -> Result<Option<Stored>, RpcError> {
    let rows = tx.query(
        "SELECT plan FROM config_pr_plan WHERE project_id = ?1 AND repository_id = ?2",
        &[project.as_str().into(), repository_id.into()],
    )?;
    rows.first().map(|row| row.json(0, "PR plan")).transpose()
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
    Ok(of(tx, id)?.is_some())
}

/// The pull request (repository ID, number) Environment `id` is the PR Environment of.
pub(crate) fn of(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<PullRequestRef>, RpcError> {
    let rows = tx.query(
        "SELECT repository_id, number FROM config_pr_environment WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first().map(pull_request_ref).transpose()
}

/// A pull request's repository and number, the first two columns of `row`.
fn pull_request_ref(row: &crate::storage::Row) -> Result<PullRequestRef, RpcError> {
    Ok(PullRequestRef {
        repository_id: row.number(0, "pull request")?,
        number: row.number(1, "pull request")?,
    })
}

/// The latest facts Cloud reported of a pull request.
pub(crate) fn facts(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    pr: PullRequestNumber,
) -> Result<Option<PullRequest>, RpcError> {
    tx.query(
        "SELECT facts FROM config_pull_request \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            pr.into(),
        ],
    )?
    .first()
    .map(|row| row.json(0, "pull request"))
    .transpose()
}

/// The pull request of every PR Environment in `environment`'s Project not being
/// closed: a change there may move any of their checks.
pub(crate) fn project_checks(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<PullRequestRef>, RpcError> {
    let rows = tx.query(
        "SELECT DISTINCT p.repository_id, p.number FROM config_pr_environment p \
         JOIN config_environment e ON e.id = p.environment_id \
         JOIN config_environment s ON s.project_id = e.project_id \
         JOIN config_environment_branch b ON b.environment_id = p.environment_id \
         WHERE s.id = ?1 AND b.closing = 0 ORDER BY p.repository_id, p.number",
        &[environment.as_str().into()],
    )?;
    rows.iter().map(pull_request_ref).collect()
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
    trusted: &Trusted,
) -> Result<Automated, RpcError> {
    validate(event)?;
    let organization = who.organization.as_str();
    let (repository_id, number) = (event.repository_id, event.number);
    let before = facts(tx, who, repository_id, number)?;
    let written = tx.execute(
        "INSERT INTO config_pull_request (organization_id, repository_id, number, facts, updated) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (organization_id, repository_id, number) DO UPDATE SET \
         facts = excluded.facts, updated = excluded.updated \
         WHERE excluded.updated >= config_pull_request.updated",
        &[
            organization.into(),
            repository_id.into(),
            number.into(),
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
    // Everything this event may touch, locked first and in ID order: its PR
    // Environments, where their saves land, and where new ones start from.
    let mut touched: Vec<EnvironmentId> = current.iter().map(|(id, _)| id.clone()).collect();
    touched.extend(crate::conditional_save::involved(tx, who, event)?);
    touched.extend(start_froms(tx, who, event.repository_id)?);
    scope::lock_all(tx, touched)?;
    let renamed = before
        .as_ref()
        .is_some_and(|before| before.head_branch != event.head_branch);
    if renamed {
        for (environment, _) in &current {
            retrack(tx, who, environment, event)?;
        }
    }
    // Saves were made for the old target branch's Destinations: a new target
    // withdraws them, so retargeting back never revives an old approval.
    let retargeted = before
        .as_ref()
        .is_some_and(|before| before.target_branch != event.target_branch);
    if retargeted {
        crate::conditional_save::withdraw(tx, who, event)?;
    }
    if !event.open {
        crate::conditional_save::settle(tx, who, event)?;
        for (environment, project) in &current {
            let plan = load(tx, project, event.repository_id)?.unwrap_or_else(off);
            if plan.remove_on_close {
                close(tx, who, environment, &mut automated)?;
            }
        }
    } else {
        open(tx, who, (event, trusted), &current, &mut automated)?;
    }
    let has_environments = !current.is_empty() || !automated.admitted.is_empty();
    if has_environments || retargeted {
        automated.checks.push(PullRequestRef {
            repository_id: event.repository_id,
            number: event.number,
        });
    }
    Ok(automated)
}

/// The Environments PR Environments of the repository start from, as its plans say.
fn start_froms(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
) -> Result<Vec<EnvironmentId>, RpcError> {
    let rows = tx.query(
        "SELECT plan FROM config_pr_plan WHERE organization_id = ?1 AND repository_id = ?2",
        &[who.organization.as_str().into(), repository_id.into()],
    )?;
    let mut ids = Vec::new();
    for row in rows {
        let plan: Stored = row.json(0, "PR plan")?;
        ids.extend(plan.start_from);
    }
    Ok(ids)
}

/// The pull request's PR Environments not being closed, with their Projects.
fn current(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    pr: PullRequestNumber,
) -> Result<Vec<(EnvironmentId, crate::id::ProjectId)>, RpcError> {
    let rows = tx.query(
        "SELECT p.environment_id, e.project_id FROM config_pr_environment p \
         JOIN config_environment e ON e.id = p.environment_id \
         JOIN config_environment_branch b ON b.environment_id = p.environment_id \
         WHERE p.organization_id = ?1 AND p.repository_id = ?2 AND p.number = ?3 AND b.closing = 0 \
         ORDER BY p.environment_id",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            pr.into(),
        ],
    )?;
    rows.iter()
        .map(|row| {
            Ok((
                row.parse::<EnvironmentId>(0, "Environment ID")?,
                row.parse::<crate::id::ProjectId>(1, "Project ID")?,
            ))
        })
        .collect()
}

/// Make a PR Environment in every Project whose plan is on and has none yet.
fn open(
    tx: &mut dyn Tx,
    who: &Actor,
    (event, trusted): (&PullRequest, &Trusted),
    current: &[(EnvironmentId, crate::id::ProjectId)],
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT project_id, plan FROM config_pr_plan \
         WHERE organization_id = ?1 AND repository_id = ?2 ORDER BY project_id",
        &[who.organization.as_str().into(), event.repository_id.into()],
    )?;
    for row in rows {
        let project = row.parse::<crate::id::ProjectId>(0, "Project ID")?;
        let plan: Stored = row.json(1, "PR plan")?;
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
        if trusted.runnable().is_err() {
            automated.skipped.push(Skipped {
                environment: start.summary.id.clone(),
                reason: crate::trusted::NO_SERVERS.to_owned(),
            });
            continue;
        }
        match crate::storage::attempt(tx, |tx| create(tx, who, &start, &plan, event)) {
            Ok(Some(deployed)) => automated.admitted.push(deployed),
            Ok(None) => {}
            Err(error) if crate::automation::skippable(&error) => {
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
    let ours = |source: &ServiceSource| matches!(source, ServiceSource::Git { repository_id, .. } if *repository_id == event.repository_id.get());
    let mut copy: Vec<NodeName> = working
        .services
        .iter()
        .filter(|service| ours(&service.config.source))
        .filter_map(|service| branch::node_of(working, &service.lineage_id))
        .collect();
    if copy.is_empty() {
        return Ok(None);
    }
    for lineage in &plan.copy {
        if let Some(name) = branch::node_of(working, lineage)
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
            let Some(NodeName::Service(service)) = branch::node_of(working, &setup.lineage) else {
                return None;
            };
            copy.contains(&NodeName::Service(service.clone())).then(|| {
                Ok(SetupCommand {
                    service,
                    command: setup.command.clone(),
                })
            })
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    // Generated domains expand under the Cluster Domain of the start-from's last
    // Deploy. Refused before anything is written: a PR Environment that can't deploy
    // isn't made.
    let cluster_domain = deployment::cluster_domain(tx, &start.summary.id)?;
    crate::deployment::admit::needs_cluster_domain(start, working, cluster_domain.as_ref())?;
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
    // Copies keep their Deployment Policy, except that the repository's Services
    // deploy on push whatever the start-from's says; wait for CI and watch paths stay.
    for service in environment
        .working
        .services
        .iter()
        .filter(|service| ours(&service.config.source))
    {
        let mut policy = crate::policy::load(tx, &id, &service.id)?;
        if !policy.auto_deploy {
            policy.auto_deploy = true;
            crate::policy::store(tx, who, &id, &service.id, &policy)?;
        }
    }
    tx.execute(
        "INSERT INTO config_pr_environment (environment_id, organization_id, repository_id, number) \
         VALUES (?1, ?2, ?3, ?4)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            event.repository_id.into(),
            event.number.into(),
        ],
    )?;
    // It deploys what it saves first: all of it, the repository's Services at the head
    // Cloud read.
    let latest = review::latest_saved(tx, &id)?;
    review::publish(tx, who, &id, environment.working.clone(), latest.as_ref())?;
    let saved = review::latest_saved(tx, &id)?.ok_or_else(|| error::corrupt("Saved State"))?;
    let pins = environment
        .working
        .services
        .iter()
        .filter(|service| ours(&service.config.source))
        .map(|service| (service.config.private_dns.clone(), event.head.clone()))
        .collect();
    let deployment = crate::automation::auto_admit(
        tx,
        who,
        (&environment, &saved),
        &[],
        (cluster_domain.as_ref(), &pins),
    )?;
    Ok(Some(AutoDeployed {
        environment: id,
        deployment,
    }))
}

/// `pr-N`, or `pr-N-2`… when that is taken in the Project.
fn free_name(
    tx: &mut dyn Tx,
    start: &Environment,
    number: PullRequestNumber,
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
            && *repository_id == event.repository_id.get()
        {
            *branch = ServiceGitBranch::Connected {
                name: event.head_branch.to_string(),
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

/// Start closing a Branch: stop what it is deploying, then remove it as far as
/// nothing needs the Servers.
fn close(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    scope::lock_id(tx, who, id)?;
    mark_closing(tx, id)?;
    // At most one Deployment of an Environment is in flight: a new one supersedes a queued one.
    if let Some(running) = deployment::in_flight(tx, id)?
        && matches!(
            running.status,
            DeploymentStatus::Queued | DeploymentStatus::Running
        )
    {
        deployment::cancel(tx, who, &running.id)?;
    }
    settle(tx, who, id, automated)
}

/// Mark Branch `id` closing: nothing deploys it on push, and the sweep deletes it
/// once nothing of it can run on the Servers. No-op for an Environment that isn't a Branch.
pub(crate) fn mark_closing(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_environment_branch SET closing = 1 WHERE environment_id = ?1",
        &[id.as_str().into()],
    )?;
    Ok(())
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
    if deployment::in_flight(tx, id)?.is_some() {
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
    match teardown::on_servers(tx, id)? {
        None => {}
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
        let id = row.parse::<EnvironmentId>(0, "Environment ID")?;
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

fn validate(event: &PullRequest) -> Result<(), RpcError> {
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
