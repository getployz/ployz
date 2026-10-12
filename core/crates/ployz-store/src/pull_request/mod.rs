//! PR Environments and the lifecycle of Branches the system closes.
//!
//! A Project's PR plan for a GitHub repository says whether each pull request gets
//! a PR Environment: a Branch of the plan's start-from Environment holding Own
//! Copies of the repository's Services (and whatever else the plan copies), which
//! track the pull request's head branch and run one replica each. Cloud observes
//! each pull request and passes its current facts in as a system event; the Store
//! decides. An older read never replaces a newer one: facts are ordered by GitHub's
//! `updated_at`. Opening makes the PR Environment and deploys it; renaming the head
//! branch retracks it; the merge lands its On merge list, once; closing closes it,
//! unless the plan keeps it.
//!
//! Closing a Branch, for its pull request or for sitting idle a week, removes it
//! as the Store ([`crate::removal::close`]): Cloud admits its removal from the
//! Servers under the mark (`Admit::Remove`) and the sweep deletes it once that
//! applied. Cancelling that removal leaves the Branch be until it is reopened or
//! pushed to. An idle PR Environment whose merge would land something only comes
//! off the Servers (`Admit::ShutDown`).

mod checks;
pub(crate) mod on_merge;
mod plan;
pub(crate) use checks::*;
pub(crate) use on_merge::view as on_merge;
pub use on_merge::{MergeRow, OnMerge, OnMergeInto, OnMergeQuery, Ride, SetMergeValue};
pub(crate) use on_merge::{forget_edits, set_merge_value};
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
use crate::deployment::{self, DeploymentSummary};
use crate::error;
use crate::id::{EnvironmentId, EnvironmentName, ProjectName};
use crate::project::ProjectSummary;
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::NodeName;
use crate::storage::Tx;
use crate::{Actor, Trusted, review};

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
    /// Its target branch's head as Cloud read it with the merge, when GitHub says it
    /// contains the merge commit: a push that skipped past the merge reached it.
    #[serde(default)]
    pub target_head: Option<CommitSha>,
    /// When GitHub last changed it.
    pub updated: crate::GithubTimestamp,
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
    /// What its GitHub check says; none when it says nothing: closed, merged with
    /// everything landed, or without PR Environments.
    pub check: Option<OnMergeCheck>,
}

/// One PR Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PrEnvironment {
    pub environment: EnvironmentSummary,
    /// Its latest Deployment.
    pub deployment: Option<DeploymentSummary>,
    /// What its merge lands in each Environment that deploys the target branch.
    pub destinations: Vec<OnMergeInto>,
}

/// A pull request's GitHub check: what its merge lands, never blocking it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OnMergeCheck {
    pub conclusion: OnMergeConclusion,
    pub title: String,
    /// Markdown, one line per row.
    pub summary: String,
    /// Once merged, the Destination whose History it links to: the PR Environment
    /// may be gone.
    pub merged_into: Option<EnvironmentRef>,
}

/// How the check reads on GitHub: neutral when a secret lands Not set, a row lands
/// over the Destination's own change, or the merge didn't land.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OnMergeConclusion {
    Success,
    Neutral,
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
         WHERE e.project_id = ?1 AND p.repository_id = ?2 AND NOT EXISTS (SELECT 1 FROM config_removal_mark m WHERE m.environment_id = b.environment_id AND m.state = 'removing') ORDER BY p.number",
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

/// The pull request of PR Environment `from` when `into` is one of its Destinations.
pub(crate) fn destined(
    tx: &mut dyn Tx,
    from: &EnvironmentId,
    into: &EnvironmentId,
) -> Result<Option<PullRequestRef>, RpcError> {
    let rows = tx.query(
        "SELECT e.repository_id, e.number, p.facts FROM config_pr_environment e \
         JOIN config_pull_request p ON p.organization_id = e.organization_id \
         AND p.repository_id = e.repository_id AND p.number = e.number \
         WHERE e.environment_id = ?1",
        &[from.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let pr = pull_request_ref(row)?;
    let facts: PullRequest = row.json(2, "pull request")?;
    let project = scope::project_of(tx, from)?.id;
    let destinations = destinations_of(tx, &project, pr.repository_id, &facts.target_branch)?;
    Ok(destinations.contains(into).then_some(pr))
}

/// A pull request's repository and number, the first two columns of `row`.
fn pull_request_ref(row: &crate::storage::Row) -> Result<PullRequestRef, RpcError> {
    Ok(PullRequestRef {
        repository_id: row.number(0, "pull request")?,
        number: row.number(1, "pull request")?,
    })
}

/// The latest facts Cloud reported of a pull request, merged once any event said so:
/// a merge reported before newer facts still counts.
pub(crate) fn facts(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    pr: PullRequestNumber,
) -> Result<Option<PullRequest>, RpcError> {
    let rows = tx.query(
        "SELECT facts, merged FROM config_pull_request \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            pr.into(),
        ],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let mut facts: PullRequest = row.json(0, "pull request")?;
    if facts.merge_commit.is_none()
        && let Some(merged) = row.optional_text(1)?
    {
        let merged: Merged =
            serde_json::from_str(merged).map_err(|_| error::corrupt("pull request merge"))?;
        facts.merge_commit = Some(merged.commit);
        facts.target_branch = merged.into;
        facts.open = false;
    }
    Ok(Some(facts))
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
         WHERE s.id = ?1 AND NOT EXISTS (SELECT 1 FROM config_removal_mark m WHERE m.environment_id = b.environment_id AND m.state = 'removing') ORDER BY p.repository_id, p.number",
        &[environment.as_str().into()],
    )?;
    rows.iter().map(pull_request_ref).collect()
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
    // Merged is for good: whatever order the events come in, the first merge stays,
    // and lands once.
    let mut first_merge = None;
    if let Some(commit) = &event.merge_commit {
        let merged = tx.execute(
            "UPDATE config_pull_request SET merged = ?1 \
             WHERE organization_id = ?2 AND repository_id = ?3 AND number = ?4 \
             AND merged IS NULL",
            &[
                serde_json::to_string(&Merged {
                    commit: commit.clone(),
                    into: event.target_branch.clone(),
                })
                .expect("a merge is JSON")
                .as_str()
                .into(),
                organization.into(),
                repository_id.into(),
                number.into(),
            ],
        )?;
        if merged == 1 {
            first_merge = Some(commit);
        }
    }
    let mut automated = Automated::default();
    let fresh = written != 0;
    if !fresh && first_merge.is_none() {
        // Older than what the Store holds: never undo a newer fact.
        return Ok(automated);
    }
    // A merge lands from every PR Environment, one closing too: a close may come
    // before the merge.
    let landing = match first_merge {
        Some(_) => current(tx, who, event.repository_id, event.number, true)?,
        None => Vec::new(),
    };
    let current = current(tx, who, event.repository_id, event.number, false)?;
    // Everything this event may touch, locked first and in ID order: its PR
    // Environments, where new ones start from, and at the merge the Destinations it
    // lands in.
    let mut touched: Vec<EnvironmentId> = current.iter().map(|(id, _)| id.clone()).collect();
    touched.extend(landing.iter().map(|(id, _)| id.clone()));
    touched.extend(start_froms(tx, who, event.repository_id)?);
    let target = facts(tx, who, repository_id, number)?.map(|facts| facts.target_branch);
    if let Some(target) = &target {
        for (_, project) in &landing {
            touched.extend(destinations_of(tx, project, repository_id, target)?);
        }
    }
    scope::lock_all(tx, touched)?;
    // A land that fails leaves its PR Environment open, so nothing it held is lost.
    let mut kept = BTreeSet::new();
    if let (Some(commit), Some(target)) = (first_merge, &target) {
        let mut unlanded = Vec::new();
        for (environment, _) in &landing {
            let at = on_merge::Merging {
                commit,
                target,
                target_head: event.target_head.as_ref(),
                trusted,
            };
            let all = on_merge::land(tx, who, environment, &at, (&mut automated, &mut unlanded))?;
            if !all {
                kept.insert(environment.clone());
                // A close that came before the merge is undone: it stays open.
                tx.execute(
                    "DELETE FROM config_removal_mark WHERE environment_id = ?1 AND state = 'removing'",
                    &[environment.as_str().into()],
                )?;
            }
        }
        on_merge::record_unlanded(
            tx,
            who,
            &PullRequestRef {
                repository_id,
                number,
            },
            &unlanded,
        )?;
    }
    if fresh {
        let renamed = before
            .as_ref()
            .is_some_and(|before| before.head_branch != event.head_branch);
        if renamed {
            for (environment, _) in &current {
                retrack(tx, who, environment, event)?;
            }
        }
        let retargeted = before
            .as_ref()
            .is_some_and(|before| before.target_branch != event.target_branch);
        if retargeted {
            for (environment, _) in &current {
                forget_edits(tx, environment)?;
            }
            automated.checks.push(PullRequestRef {
                repository_id,
                number,
            });
        }
    }
    // A merge closes it whichever event told of it first; older facts never reopen.
    if !event.open {
        for (environment, project) in &current {
            let plan = load(tx, project, event.repository_id)?.unwrap_or_else(off);
            if plan.remove_on_close && !kept.contains(environment) {
                let because = crate::removal::mark::Closed::PullRequest;
                crate::removal::close(tx, who, environment, because, &mut automated)?;
            }
        }
    } else if fresh {
        for (environment, _) in &current {
            let target = crate::removal::Target::Environment(environment.clone());
            crate::removal::mark::clear_tombstone(tx, &target)?;
        }
        open(tx, who, (event, trusted), &current, &mut automated)?;
    }
    let reported = PullRequestRef {
        repository_id,
        number,
    };
    let has_environments =
        !current.is_empty() || !landing.is_empty() || !automated.admitted.is_empty();
    if has_environments && !automated.checks.contains(&reported) {
        automated.checks.push(reported);
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

/// The pull request's PR Environments not being closed, or with `closing` every
/// one, with their Projects.
fn current(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    pr: PullRequestNumber,
    closing: bool,
) -> Result<Vec<(EnvironmentId, crate::id::ProjectId)>, RpcError> {
    let open = if closing {
        ""
    } else {
        "AND NOT EXISTS (SELECT 1 FROM config_removal_mark m \
         WHERE m.environment_id = b.environment_id AND m.state = 'removing')"
    };
    let rows = tx.query(
        &format!(
            "SELECT p.environment_id, e.project_id FROM config_pr_environment p \
             JOIN config_environment e ON e.id = p.environment_id \
             JOIN config_environment_branch b ON b.environment_id = p.environment_id \
             WHERE p.organization_id = ?1 AND p.repository_id = ?2 AND p.number = ?3 \
             {open} ORDER BY p.environment_id"
        ),
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
        match crate::storage::attempt(tx, |tx| create(tx, who, &start, &plan, (event, trusted))) {
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
    (event, trusted): (&PullRequest, &Trusted),
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
    // Deploy. A PR Environment its Deploy refuses isn't made: the caller rolls it back.
    let cluster_domain = deployment::cluster_domain(tx, &start.summary.id)?;
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
    review::publish(
        tx,
        who,
        &id,
        environment.working.clone(),
        latest.as_ref(),
        None,
    )?;
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
        (&[], &[]),
        (cluster_domain.as_ref(), &pins),
        trusted,
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
        review::publish(tx, who, id, tracked, Some(&latest), None)?;
    }
    let tracked = track(environment.working.clone(), event);
    if tracked != environment.working {
        environment.working = tracked;
        scope::save_working(tx, &mut environment)?;
    }
    Ok(())
}

/// Take PR Environment `id` off the Servers, keeping it: its next push deploys it
/// again.
fn shut_down(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let environment = scope::lock_id(tx, who, id)?;
    if deployment::in_flight(tx, id)?.is_none() && crate::removal::on_servers(tx, id)?.is_some() {
        automated.shutting_down.push(environment.summary);
    }
    Ok(())
}

/// What the idle rule reads of each Branch: `environment_id`, kept, whether a
/// removal names it (or a cancelled one did), its latest admission (-1 for none),
/// its Branches and whether it is a Default.
const IDLE_FACTS: &str = "SELECT b.environment_id, b.kept, \
     (SELECT COUNT(*) FROM config_removal_mark m WHERE (m.environment_id = b.environment_id AND m.state <> 'removed') \
      OR (m.state = 'removing' AND m.project_id = (SELECT e.project_id FROM config_environment e WHERE e.id = b.environment_id))), \
     (SELECT COALESCE(MAX(d.admitted), -1) FROM config_deployment d WHERE d.environment_id = b.environment_id), \
     (SELECT COUNT(*) FROM config_environment_branch c WHERE c.parent_id = b.environment_id), \
     (SELECT COUNT(*) FROM config_project p WHERE p.default_environment_id = b.environment_id) \
     FROM config_environment_branch b";

/// When the sweep closes a Branch for sitting idle, from an [`IDLE_FACTS`] row: a
/// week after its latest Deployment. Never when it is kept, being removed or its
/// removal was cancelled, never
/// deployed, a Parent or the Default Environment.
fn idle_close(row: &crate::storage::Row) -> Result<Option<i64>, RpcError> {
    let (kept, marked, admitted) = (row.int(1)?, row.int(2)?, row.int(3)?);
    let held = kept != 0 || marked != 0 || admitted < 0 || row.int(4)? != 0 || row.int(5)? != 0;
    Ok((!held).then_some(admitted + IDLE))
}

/// When a Branch closes for sitting idle, in seconds since the Unix epoch.
pub(crate) fn closes_at(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<Option<i64>, RpcError> {
    let rows = tx.query(
        &format!("{IDLE_FACTS} WHERE b.environment_id = ?1"),
        &[id.as_str().into()],
    )?;
    rows.first().map_or(Ok(None), idle_close)
}

/// Move every removal on, then close each Branch idle past its deadline.
pub(crate) fn sweep(tx: &mut dyn Tx, who: &Actor, sweep: &Sweep) -> Result<Automated, RpcError> {
    let rows = tx.query(
        &format!("{IDLE_FACTS} WHERE b.organization_id = ?1 ORDER BY b.environment_id"),
        &[who.organization.as_str().into()],
    )?;
    // Closing would lose what it holds for its merge: an idle PR Environment that
    // holds something only comes off the Servers.
    let mut idle = Vec::new();
    let mut held = Vec::new();
    for row in &rows {
        let id = row.parse::<EnvironmentId>(0, "Environment ID")?;
        let closes_at = idle_close(row)?;
        if closes_at.is_some_and(|at| sweep.now >= at) && on_merge::holds(tx, who, &id)? {
            held.push(id);
        } else {
            idle.push((id, closes_at));
        }
    }
    let mut automated = crate::removal::sweep(tx, who, sweep.now, &idle)?;
    for id in &held {
        shut_down(tx, who, id, &mut automated)?;
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
    if event.open && event.merge_commit.is_some() {
        return Err(error::invalid(
            "An open pull request has no merge commit",
            json!({}),
        ));
    }
    Ok(())
}

/// Where a pull request merged, kept for good once it did.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Merged {
    commit: CommitSha,
    into: BranchName,
}

/// The open pull requests into `branch` of `repository_id` with a PR Environment, not
/// known to have merged: Cloud reports each that did before the push, so its merge
/// lands first.
pub(crate) fn awaiting_merge(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> Result<Vec<PullRequestNumber>, RpcError> {
    let rows = tx.query(
        "SELECT DISTINCT p.number, r.facts FROM config_pr_environment p \
         JOIN config_environment_branch b ON b.environment_id = p.environment_id \
         JOIN config_pull_request r ON r.organization_id = p.organization_id \
         AND r.repository_id = p.repository_id AND r.number = p.number \
         WHERE p.organization_id = ?1 AND p.repository_id = ?2 \
         AND NOT EXISTS (SELECT 1 FROM config_removal_mark m \
         WHERE m.environment_id = b.environment_id AND m.state = 'removing') \
         AND r.merged IS NULL ORDER BY p.number",
        &[who.organization.as_str().into(), repository_id.into()],
    )?;
    let mut awaiting = Vec::new();
    for row in rows {
        let facts: PullRequest = row.json(1, "pull request")?;
        let number = row.number(0, "pull request")?;
        if facts.open && facts.target_branch == *branch && !awaiting.contains(&number) {
            awaiting.push(number);
        }
    }
    Ok(awaiting)
}
