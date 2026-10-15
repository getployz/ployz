//! System events: what Cloud observed of GitHub, passed in-process only, and the
//! auto-deploys the Store admits from them.
//!
//! Cloud owns provider I/O: it reads a branch's current head (never trusting the
//! webhook's), the paths changed since the head the Store last saw, and each check
//! suite's current result. The Store owns every decision. A push deploys the latest
//! Saved State (never Working State) of each Environment whose Git Services follow
//! the branch with auto-deploy on and a watch path the change touched, each pinned to
//! the new head. With wait-for-CI it waits until every check suite of the commit
//! passed. A replayed or out-of-order observation never restores an older fact: a
//! head is compare-and-set against the one Cloud compared from, and a check suite
//! keeps its newest result.

use crate::id::{BranchName, CommitSha, RepositoryId};
use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent, ServiceSource};
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::deployment::{self, DeploymentStatus, DeploymentSummary};
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, Hostname, OrganizationId};
use crate::policy::{self, is_repository_path};
use crate::pull_request::on_merge::{MergedServices, Merging};
use crate::review;
use crate::scope::{self, EnvironmentSummary};
use crate::storage::{Row, Tx, name_of};
use crate::{Actor, Trusted, build, registry};

/// An observation Cloud made, of GitHub or of the Cluster. Never caller testimony: only Cloud's worker
/// passes one, in-process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SystemEvent {
    /// A branch's current head.
    BranchHead(BranchHead),
    /// A check suite's current result.
    CheckSuite(CheckSuite),
    /// A pull request's current facts.
    PullRequest(crate::PullRequest),
    /// Time passed: close what is due.
    Sweep(crate::Sweep),
    /// Cloud forgot the Organization's Cluster: it reset the last Server itself, or a
    /// user said the Servers were deleted and none answered. Nothing of any Environment
    /// runs anywhere Cloud can reach. Every Deployment that might still run is
    /// cancelled; Applied State lets go of every node, so nothing reads Deployed and the
    /// next Deploy runs everything; each Environment's Namespace and fixed Volume
    /// storage go too. Authored configuration and Deployment history stay. A later
    /// removal of an Environment completes as Forgotten.
    ClusterForgotten,
}

/// A branch's head as Cloud read it just now, and what changed since `base`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BranchHead {
    pub repository_id: RepositoryId,
    pub branch: BranchName,
    /// The head Cloud compared from: the Store's, as [`crate::ConfigStore::branch_head`]
    /// read it. Anything else is `conflict`: read it again and compare again.
    #[serde(default)]
    pub base: Option<CommitSha>,
    /// The head now; none once the branch was deleted.
    pub head: Option<CommitSha>,
    /// The paths `base..head` changed, when `head` is ahead of `base` and GitHub
    /// listed every one. None (a force-push, diverged or long history) deploys every
    /// Service that follows the branch.
    #[serde(default)]
    pub changed: Option<Vec<String>>,
}

/// A check suite of a commit, as Cloud read it just now.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CheckSuite {
    pub repository_id: RepositoryId,
    #[ts(type = "number")]
    pub suite: u64,
    /// The commit it checks.
    pub head: CommitSha,
    pub status: CheckStatus,
    /// GitHub's conclusion; read only once it completed.
    #[serde(default)]
    pub conclusion: Option<CheckConclusion>,
    /// When GitHub last changed it: an older result never replaces a newer one.
    pub updated: crate::GithubTimestamp,
}

/// A check suite's status as GitHub names it; a status GitHub adds later reads
/// `other`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Queued,
    InProgress,
    Completed,
    #[serde(other)]
    Other,
}

/// A completed check suite's conclusion as GitHub names it; one GitHub adds later
/// reads `other`, which doesn't pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    Success,
    Neutral,
    Skipped,
    Failure,
    Cancelled,
    TimedOut,
    ActionRequired,
    Stale,
    StartupFailure,
    #[serde(other)]
    Other,
}

impl CheckConclusion {
    /// Whether it lets a deploy waiting for CI go.
    const fn passed(self) -> bool {
        matches!(self, Self::Success | Self::Neutral | Self::Skipped)
    }
}

/// What an event made the Store do.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Automated {
    /// Deployments admitted: Cloud dispatches each to a runner.
    pub admitted: Vec<AutoDeployed>,
    /// Environments whose deploy waits for the commit's CI.
    pub waiting: Vec<EnvironmentId>,
    /// Environments that would have deployed but can't, and why.
    pub skipped: Vec<Skipped>,
    /// Branches whose removal is due: Cloud admits it from the Servers under the
    /// mark (`Admit { remove }`).
    pub closing: Vec<EnvironmentSummary>,
    /// Idle PR Environments that hold something for their merge, still on the
    /// Servers: Cloud takes each off and keeps it (`Admit { shut_down }`).
    #[serde(default)]
    pub shutting_down: Vec<EnvironmentSummary>,
    /// Environments deleted: nothing of them runs on the Servers any more.
    pub removed: Vec<EnvironmentSummary>,
    /// Pull requests whose GitHub check Cloud publishes again.
    pub checks: Vec<crate::PullRequestRef>,
    /// Deployments cancelled without a runner, oldest first: their Cluster is gone.
    pub cancelled: Vec<DeploymentId>,
}

/// A Deployment that an event admitted, and the Environment it deploys to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct AutoDeployed {
    pub environment: EnvironmentId,
    pub deployment: DeploymentSummary,
}

/// An Environment an event could not deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Skipped {
    pub environment: EnvironmentId,
    /// Users read it: it holds no secret.
    pub reason: String,
}

pub(crate) fn system(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
    event: &SystemEvent,
    trusted: &Trusted,
) -> Result<Automated, RpcError> {
    let who = Actor::system(organization.clone());
    match event {
        SystemEvent::BranchHead(head) => branch_head(tx, &who, head, trusted),
        SystemEvent::CheckSuite(suite) => check_suite(tx, &who, suite, trusted),
        SystemEvent::PullRequest(pull) => {
            crate::pull_request::pull_request(tx, &who, pull, trusted)
        }
        SystemEvent::Sweep(sweep) => crate::pull_request::sweep(tx, &who, sweep),
        SystemEvent::ClusterForgotten => Ok(Automated {
            cancelled: crate::teardown::forget_cluster(tx, &who)?,
            ..Automated::default()
        }),
    }
}

/// The head the Store last saw of a branch; none before its first push or once deleted.
pub(crate) fn head(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> Result<Option<CommitSha>, RpcError> {
    let rows = tx.query(
        "SELECT head FROM config_branch \
         WHERE organization_id = ?1 AND repository_id = ?2 AND branch = ?3",
        &[
            organization.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
        ],
    )?;
    rows.first()
        .and_then(|row| row.optional_text(0).transpose())
        .transpose()?
        .map(|head| CommitSha::parse(head).map_err(|_| error::corrupt("branch head")))
        .transpose()
}

fn branch_head(
    tx: &mut dyn Tx,
    who: &Actor,
    event: &BranchHead,
    trusted: &Trusted,
) -> Result<Automated, RpcError> {
    let branch = &event.branch;
    if event
        .changed
        .iter()
        .flatten()
        .any(|path| !is_repository_path(path))
    {
        return Err(error::invalid(
            "Changed paths are canonical repository-relative paths",
            json!({}),
        ));
    }
    let repository_id = event.repository_id;
    let organization = who.organization.as_str();
    let stored = head(tx, &who.organization, repository_id, branch)?;
    if stored == event.head {
        // Seen already: a replay, or a head that moved and came back.
        return Ok(Automated::default());
    }
    if stored != event.base {
        return Err(error::conflict(
            "The branch's head moved since Cloud read it: compare from the latest",
            json!({ "head": stored }),
        ));
    }
    let new = event.head.as_ref().map(CommitSha::as_str);
    let written = match &stored {
        None => tx.execute(
            "INSERT INTO config_branch (organization_id, repository_id, branch, head) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (organization_id, repository_id, branch) \
             DO UPDATE SET head = excluded.head WHERE config_branch.head IS NULL",
            &[
                organization.into(),
                repository_id.into(),
                branch.as_str().into(),
                new.into(),
            ],
        )?,
        Some(old) => tx.execute(
            "UPDATE config_branch SET head = ?4 \
             WHERE organization_id = ?1 AND repository_id = ?2 AND branch = ?3 AND head = ?5",
            &[
                organization.into(),
                repository_id.into(),
                branch.as_str().into(),
                new.into(),
                old.as_str().into(),
            ],
        )?,
    };
    if written != 1 {
        return Err(error::conflict(
            "The branch's head moved since Cloud read it: compare from the latest",
            json!({}),
        ));
    }
    // A deploy still waiting for an older head's CI never ships now.
    tx.execute(
        "DELETE FROM config_waiting_deploy \
         WHERE organization_id = ?1 AND repository_id = ?2 AND branch = ?3",
        &[
            organization.into(),
            repository_id.into(),
            branch.as_str().into(),
        ],
    )?;
    let Some(head) = &event.head else {
        return Ok(Automated::default());
    };
    remember(tx, who, repository_id, branch, head)?;
    // The first head seen deploys every Service that follows the branch.
    let changed = event.base.as_ref().and(event.changed.as_deref());
    let environments = tx.query(
        "SELECT id FROM config_environment WHERE organization_id = ?1 ORDER BY id",
        &[organization.into()],
    )?;
    let mut automated = Automated::default();
    for row in environments {
        let environment = row.parse::<EnvironmentId>(0, "Environment ID")?;
        let push = Push {
            repository_id: event.repository_id,
            branch,
            head,
        };
        deploy(
            tx,
            who,
            &environment,
            &push,
            Select::Changed(changed),
            trusted,
            &mut automated,
        )?;
    }
    Ok(automated)
}

fn check_suite(
    tx: &mut dyn Tx,
    who: &Actor,
    event: &CheckSuite,
    trusted: &Trusted,
) -> Result<Automated, RpcError> {
    let repository_id = event.repository_id;
    let suite = i64::try_from(event.suite)
        .map_err(|_| error::invalid("Expected a check suite ID", json!({})))?;
    let organization = who.organization.as_str();
    tx.execute(
        "INSERT INTO config_check_suite \
         (organization_id, repository_id, suite, head, status, conclusion, updated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (organization_id, repository_id, suite) DO UPDATE SET \
         head = excluded.head, status = excluded.status, conclusion = excluded.conclusion, \
         updated = excluded.updated WHERE excluded.updated >= config_check_suite.updated",
        &[
            organization.into(),
            repository_id.into(),
            suite.into(),
            event.head.as_str().into(),
            name_of(event.status).as_str().into(),
            event.conclusion.map(name_of).as_deref().into(),
            event.updated.as_str().into(),
        ],
    )?;
    let waiting = tx.query(
        "SELECT environment_id, branch, services FROM config_waiting_deploy \
         WHERE organization_id = ?1 AND repository_id = ?2 AND head = ?3 \
         ORDER BY environment_id, branch",
        &[
            organization.into(),
            repository_id.into(),
            event.head.as_str().into(),
        ],
    )?;
    let mut automated = Automated::default();
    for row in waiting {
        let environment = row.parse::<EnvironmentId>(0, "Environment ID")?;
        let services: Vec<String> = row.json(2, "waiting deploy")?;
        let branch: BranchName = row.parse(1, "waiting deploy")?;
        let push = Push {
            repository_id: event.repository_id,
            branch: &branch,
            head: &event.head,
        };
        deploy(
            tx,
            who,
            &environment,
            &push,
            Select::Services(&services),
            trusted,
            &mut automated,
        )?;
    }
    Ok(automated)
}

struct Push<'a> {
    repository_id: RepositoryId,
    branch: &'a BranchName,
    head: &'a CommitSha,
}

/// Which of the Services that follow the branch deploy, besides those merges
/// landed.
enum Select<'a> {
    /// Those watching one of these changed paths (`None`: all of them).
    Changed(Option<&'a [String]>),
    /// Those a push selected earlier, by ID, that still follow it.
    Services(&'a [String]),
}

/// How many of a branch's latest heads the Store remembers.
const RECENT: usize = 50;

fn remember(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
    head: &CommitSha,
) -> Result<(), RpcError> {
    let mut heads = recent(tx, who, repository_id, branch)?;
    heads.retain(|seen| seen != head);
    heads.push(head.clone());
    let drop = heads.len().saturating_sub(RECENT);
    heads.drain(..drop);
    tx.execute(
        "UPDATE config_branch SET recent = ?4 \
         WHERE organization_id = ?1 AND repository_id = ?2 AND branch = ?3",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
            serde_json::to_string(&heads)
                .expect("heads are JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(())
}

/// The branch's recent heads, oldest first.
fn recent(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> Result<Vec<CommitSha>, RpcError> {
    tx.query(
        "SELECT recent FROM config_branch \
         WHERE organization_id = ?1 AND repository_id = ?2 AND branch = ?3",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
        ],
    )?
    .first()
    .map_or_else(|| Ok(Vec::new()), |row| row.json(0, "recent heads"))
}

/// Deploy `environment`'s Services the push selects, wait for CI, or skip it.
fn deploy(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    push: &Push<'_>,
    select: Select<'_>,
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    match admit(tx, who, environment, push, select, trusted) {
        Ok(Some(Deploy::Admitted(deployment))) => automated.admitted.push(AutoDeployed {
            environment: environment.clone(),
            deployment: *deployment,
        }),
        Ok(Some(Deploy::Waiting)) => automated.waiting.push(environment.clone()),
        Ok(Some(Deploy::NoServers)) => automated.skipped.push(Skipped {
            environment: environment.clone(),
            reason: crate::trusted::NO_SERVERS.to_owned(),
        }),
        Ok(None) => {}
        // One Environment that can't deploy never holds back the others.
        Err(error) if skippable(&error) => {
            automated.skipped.push(Skipped {
                environment: environment.clone(),
                reason: error.message,
            });
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Deploy what a merge at `commit` into `branch` changed and removed in
/// `environment`, `services`: now when the branch's head is or came after `commit`,
/// else with the push that carries `commit`. Kept until a Deployment takes them.
pub(crate) fn deploy_landed(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    repository_id: RepositoryId,
    merge: &Merging<'_>,
    mut pending: MergedServices,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let (branch, commit) = (merge.target, merge.commit);
    let earlier = tx.query(
        "SELECT services, removed_services FROM config_merge_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 AND commit_sha = ?4",
        &[
            environment.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
            commit.as_str().into(),
        ],
    )?;
    if let Some(row) = earlier.first() {
        pending.extend(merged_services(row)?);
    }
    tx.execute(
        "INSERT INTO config_merge_deploy \
         (environment_id, repository_id, branch, commit_sha, seq, organization_id, services, \
          removed_services) \
         SELECT ?1, ?2, ?3, ?4, COALESCE(MAX(seq), 0) + 1, ?5, ?6, ?7 FROM config_merge_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 \
         ON CONFLICT (environment_id, repository_id, branch, commit_sha) \
         DO UPDATE SET services = excluded.services, \
         removed_services = excluded.removed_services",
        &[
            environment.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
            commit.as_str().into(),
            who.organization.as_str().into(),
            json_of(&pending.updates).as_str().into(),
            json_of(&pending.removals).as_str().into(),
        ],
    )?;
    let Some(head) = head(tx, &who.organization, repository_id, branch)? else {
        return Ok(());
    };
    let recent = recent(tx, who, repository_id, branch)?;
    let reached = |at: &CommitSha| *at == head || recent.contains(at);
    // A push that skipped past the merge commit reached it all the same, when GitHub
    // says the head Cloud read with the merge contains it.
    if !reached(commit) && !merge.target_head.is_some_and(reached) {
        return Ok(());
    }
    // What still waits for this head's CI keeps waiting with it.
    let waiting: Vec<String> = tx
        .query(
            "SELECT services FROM config_waiting_deploy \
             WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 AND head = ?4",
            &[
                environment.as_str().into(),
                repository_id.into(),
                branch.as_str().into(),
                head.as_str().into(),
            ],
        )?
        .first()
        .map_or_else(|| Ok(Vec::new()), |row| row.json(0, "waiting deploy"))?;
    let push = Push {
        repository_id,
        branch,
        head: &head,
    };
    deploy(
        tx,
        who,
        environment,
        &push,
        Select::Services(&waiting),
        merge.trusted,
        automated,
    )
}

/// The Service IDs a merge Deploy row holds: `services`, then `removed_services`.
fn merged_services(row: &Row) -> Result<MergedServices, RpcError> {
    Ok(MergedServices {
        updates: row.json(0, "merge deploy")?,
        removals: row.json(1, "merge deploy")?,
    })
}

fn json_of(ids: &BTreeSet<impl Serialize>) -> String {
    serde_json::to_string(ids).expect("Service IDs are JSON")
}

/// What merges into the branch of `push` landed in `environment` that no Deployment
/// took yet and `push` carries: their merge commits, and the Services they changed
/// and removed. A head that is one of those commits carries it and the merges before
/// it; any other head all.
fn landed(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    push: &Push<'_>,
) -> Result<(Vec<String>, MergedServices), RpcError> {
    let rows = tx.query(
        "SELECT services, removed_services, commit_sha FROM config_merge_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 ORDER BY seq",
        &[
            environment.as_str().into(),
            push.repository_id.into(),
            push.branch.as_str().into(),
        ],
    )?;
    let mut commits = rows
        .iter()
        .map(|row| row.text(2).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(at) = commits
        .iter()
        .position(|commit| commit == push.head.as_str())
    {
        commits.truncate(at + 1);
    }
    let mut services = MergedServices::default();
    for row in rows.iter().take(commits.len()) {
        services.extend(merged_services(row)?);
    }
    Ok((commits, services))
}

/// Whether an automatic Deploy that failed with `error` is skipped, leaving the
/// others to go on: the Environment can't deploy until someone changes it.
pub(crate) fn skippable(error: &RpcError) -> bool {
    matches!(
        error.code,
        RpcErrorCode::InvalidArgument
            | RpcErrorCode::Unsupported
            | RpcErrorCode::NotFound
            | RpcErrorCode::Conflict
    )
}

enum Deploy {
    Admitted(Box<DeploymentSummary>),
    Waiting,
    /// It would deploy, but no Server could run it.
    NoServers,
}

fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    push: &Push<'_>,
    select: Select<'_>,
    trusted: &Trusted,
) -> Result<Option<Deploy>, RpcError> {
    let environment = scope::lock_id(tx, who, id)?;
    // Being removed: a push leaves it be. A shut-down PR Environment (its removal
    // applied) comes back on: the push deploys all of it again.
    let removal = crate::removal::latest(tx, id)?.filter(|removal| {
        !matches!(
            removal.status,
            DeploymentStatus::Failed | DeploymentStatus::Cancelled
        )
    });
    let shut_down = removal
        .as_ref()
        .is_some_and(|removal| removal.status == DeploymentStatus::Applied)
        && crate::pull_request::of(tx, id)?.is_some();
    if crate::removal::mark::naming(tx, id)?.is_some() || (removal.is_some() && !shut_down) {
        return Ok(None);
    }
    // A push brings back a Branch whose removal someone cancelled.
    crate::removal::mark::clear_tombstone(tx, &crate::removal::Target::Environment(id.clone()))?;
    let Some(saved) = review::latest_saved(tx, id)? else {
        return Ok(None);
    };
    // What a merge landed deploys whatever the Service follows: its config changed.
    let (merges, landed) = landed(tx, id, push)?;
    // Each selected Service, and whether it builds the pushed head.
    let mut selected: Vec<(&SavedServiceIntent, bool)> = Vec::new();
    let mut wait = false;
    for service in &saved.intent.services {
        let tracks = crate::git::tracks(service, push.repository_id, push.branch);
        let merged = landed.updates.contains(&service.id);
        if !tracks && !merged {
            continue;
        }
        let policy = policy::load(tx, id, &service.id)?;
        let follows = tracks && policy.auto_deploy;
        let pushed = follows
            && match select {
                Select::Changed(changed) => changed.is_none_or(|changed| policy.watches(changed)),
                Select::Services(ids) => ids.contains(&service.id),
            };
        if pushed || merged {
            wait |= tracks && policy.wait_for_ci;
            selected.push((service, follows));
        }
    }
    let key: [crate::storage::Param<'_>; 3] = [
        id.as_str().into(),
        push.repository_id.into(),
        push.branch.as_str().into(),
    ];
    let [environment_param, repository_param, branch_param] = key;
    // What the merges removed goes with them, unless it never ran. A shut-down
    // Environment deploys all of it, which tears every one down.
    let retire = match shut_down {
        true => Vec::new(),
        false => deployment::retirements(tx, id, &saved.intent, &landed.removals)?,
    };
    let idle = selected.is_empty() && retire.is_empty();
    if idle || trusted.runnable().is_err() {
        tx.execute(
            "DELETE FROM config_waiting_deploy \
             WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3",
            &key,
        )?;
        if idle {
            forget_landed(tx, id, push, &merges)?;
        }
        return Ok((!idle).then_some(Deploy::NoServers));
    }
    if wait && !passed(tx, who, push)? {
        let services: Vec<&str> = selected
            .iter()
            .map(|(service, _)| service.id.as_str())
            .collect();
        tx.execute(
            "INSERT INTO config_waiting_deploy \
             (environment_id, repository_id, branch, organization_id, head, services) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (environment_id, repository_id, branch) \
             DO UPDATE SET head = excluded.head, services = excluded.services",
            &[
                environment_param,
                repository_param,
                branch_param,
                who.organization.as_str().into(),
                push.head.as_str().into(),
                serde_json::to_string(&services)
                    .expect("Service IDs are JSON")
                    .as_str()
                    .into(),
            ],
        )?;
        return Ok(Some(Deploy::Waiting));
    }
    tx.execute(
        "DELETE FROM config_waiting_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3",
        &key,
    )?;
    // A Service that builds the head is pinned to it; another Git Service to the
    // commit it last built here, or its branch's head when it never built.
    let mut pins: BTreeMap<ServiceName, CommitSha> = BTreeMap::new();
    for (service, follows) in &selected {
        let pin = if *follows {
            Some(push.head.clone())
        } else if crate::git::is_git(service) {
            last_built(tx, id, service)?
        } else {
            None
        };
        if let Some(pin) = pin {
            pins.insert(service.config.private_dns.clone(), pin);
        }
    }
    let selected: Vec<SavedServiceIntent> = selected
        .into_iter()
        .map(|(service, _)| service.clone())
        .collect();
    let names = match shut_down {
        true => Vec::new(),
        false => selected
            .iter()
            .map(|service| ServiceName::parse(service.slug.as_str()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| error::corrupt("Service name"))?,
    };
    // Generated domains expand under the Cluster Domain Cloud reserved for the
    // Environment's last Deploy.
    let cluster_domain = deployment::cluster_domain(tx, id)?;
    // A skipped Deploy leaves nothing behind, such as a reserved Namespace.
    let summary = crate::storage::attempt(tx, |tx| {
        auto_admit(
            tx,
            who,
            (&environment, &saved),
            (&names, &retire),
            (cluster_domain.as_ref(), &pins),
            trusted,
        )
    })?;
    forget_landed(tx, id, push, &merges)?;
    Ok(Some(Deploy::Admitted(Box::new(summary))))
}

/// What a Deployment of `environment` shipped, which landed merges stop waiting on.
pub(crate) enum Shipped<'a> {
    /// A full Deploy of a Saved revision: each Service it holds.
    All(&'a SavedEnvironmentIntent),
    /// A narrowed Deploy: these Services, by ID.
    Services(BTreeSet<&'a str>),
}

/// Forget what landed merges changed in `environment` that a Deployment just took,
/// `shipped`.
pub(crate) fn shipped_landed(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    shipped: &Shipped<'_>,
) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT services, removed_services, repository_id, branch, commit_sha \
         FROM config_merge_deploy WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    for row in &rows {
        let mut pending = merged_services(row)?;
        let repository_id: RepositoryId = row.number(2, "merge deploy")?;
        let branch = row.text(3)?;
        let commit = row.text(4)?;
        let before = (pending.updates.len(), pending.removals.len());
        match shipped {
            // Its outcome isn't known yet, so a removal stays for the next push, which
            // finds nothing left to tear down once this Deploy did.
            Shipped::All(saved) => {
                let kept = |id: &str| saved.services.iter().any(|service| service.id == id);
                pending.updates.retain(|service| !kept(service));
            }
            Shipped::Services(services) => {
                pending
                    .updates
                    .retain(|service| !services.contains(service.as_str()));
            }
        }
        if (pending.updates.len(), pending.removals.len()) == before {
            continue;
        }
        let key = [
            environment.as_str().into(),
            repository_id.into(),
            branch.into(),
            commit.into(),
        ];
        if pending.is_empty() {
            tx.execute(
                "DELETE FROM config_merge_deploy WHERE environment_id = ?1 \
                 AND repository_id = ?2 AND branch = ?3 AND commit_sha = ?4",
                &key,
            )?;
        } else {
            let [environment, repository_id, branch, commit] = key;
            tx.execute(
                "UPDATE config_merge_deploy SET services = ?5, removed_services = ?6 \
                 WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 \
                 AND commit_sha = ?4",
                &[
                    environment,
                    repository_id,
                    branch,
                    commit,
                    json_of(&pending.updates).as_str().into(),
                    json_of(&pending.removals).as_str().into(),
                ],
            )?;
        }
    }
    Ok(())
}

/// Forget the merges at `commits` into the branch of `push` that landed in
/// `environment`: a Deployment took them, or nothing is left of them.
fn forget_landed(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    push: &Push<'_>,
    commits: &[String],
) -> Result<(), RpcError> {
    for commit in commits {
        tx.execute(
            "DELETE FROM config_merge_deploy WHERE environment_id = ?1 \
             AND repository_id = ?2 AND branch = ?3 AND commit_sha = ?4",
            &[
                environment.as_str().into(),
                push.repository_id.into(),
                push.branch.as_str().into(),
                commit.as_str().into(),
            ],
        )?;
    }
    Ok(())
}

/// The commit Service `service` (its ID) last built in Environment `id`, under
/// whatever runtime name it had then: each Deployment's target nodes say.
fn last_built(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    service: &SavedServiceIntent,
) -> Result<Option<CommitSha>, RpcError> {
    let rows = tx.query(
        "SELECT b.commit_sha, b.service, d.nodes, d.saved_revision FROM config_build b \
         JOIN config_deployment d ON d.id = b.deployment_id \
         WHERE d.environment_id = ?1 AND d.nodes LIKE ?2 AND b.commit_sha IS NOT NULL \
         AND b.status IN ('built', 'reused') ORDER BY d.admitted DESC",
        &[
            id.as_str().into(),
            format!("%\"{}\"%", service.id).as_str().into(),
        ],
    )?;
    for row in &rows {
        let nodes: Vec<deployment::TargetNode> = row.json(2, "Deployment")?;
        let built = row.text(1)?;
        let ours = nodes.iter().any(|node| match node {
            deployment::TargetNode::Service { id, runtime, .. } => {
                id.as_str() == service.id && runtime.as_str() == built
            }
            deployment::TargetNode::Volume { .. } | deployment::TargetNode::Config { .. } => false,
        });
        if !ours {
            continue;
        }
        // A build of another repository or branch is no commit of this source.
        let saved = deployment::saved_at(tx, id, row.number(3, "revision")?)?;
        let same = saved
            .services
            .iter()
            .find(|then| then.id == service.id)
            .is_some_and(|then| same_source(&then.config.source, &service.config.source));
        return if same {
            row.parse(0, "commit").map(Some)
        } else {
            Ok(None)
        };
    }
    Ok(None)
}

/// Whether `a` and `b` build the same branch of the same repository.
fn same_source(a: &ServiceSource, b: &ServiceSource) -> bool {
    matches!(
        (a, b),
        (
            ServiceSource::Git { repository_id, branch, .. },
            ServiceSource::Git { repository_id: then, branch: then_branch, .. },
        ) if repository_id == then && branch == then_branch
    )
}

/// Admit the Store's own Deployment of Saved revision `saved` of `environment`:
/// `services` and the teardown of `retire`, from [`deployment::retirements`]
/// (neither: every Service), with generated domains under `cluster_domain` and
/// each Git Service of `pins` pinned to its commit. It deletes only the Volume
/// data accepted when `saved` was saved, while the Servers still show the same.
pub(crate) fn auto_admit(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, saved): (&scope::Environment, &review::Saved),
    (services, retire): (&[ServiceName], &[deployment::Retire]),
    (cluster_domain, pins): (Option<&Hostname>, &BTreeMap<ServiceName, CommitSha>),
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let id = &environment.summary.id;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    crate::deployment::admit::gate(
        tx,
        who,
        (environment, &saved.intent),
        (cluster_domain, &namespace),
        trusted,
    )?;
    let head = deployment::head(tx, environment)?;
    let losses = consented(&head.applied, saved, &namespace, trusted)?;
    let mut frozen = deployment::freeze(
        id,
        &saved.intent,
        &head.applied,
        if services.is_empty() && retire.is_empty() {
            deployment::Scope::All { losses: &losses }
        } else {
            deployment::Scope::Narrow {
                services,
                retire,
                losses: &losses,
            }
        },
        namespace,
        cluster_domain,
    )?;
    frozen.credentials = registry::freeze(tx, id, &saved.intent, &frozen)?;
    let deployment = DeploymentId::parse(uuid::Uuid::new_v4().to_string())?;
    let summary = deployment::admit(
        tx,
        who,
        (&deployment, services, None, None),
        id,
        saved.revision,
        &frozen,
    )?;
    build::pin(tx, &deployment, pins)?;
    Ok(summary)
}

/// The losses `saved` holds consent for that the Servers still show unchanged.
/// Without that evidence a push keeps the data, and the next Deploy asks again.
fn consented(
    applied: &SavedEnvironmentIntent,
    saved: &review::Saved,
    namespace: &ployz_core::Namespace,
    trusted: &Trusted,
) -> Result<Vec<crate::VolumeLoss>, RpcError> {
    let removed: Vec<_> = crate::removal::removed(applied, &saved.intent, namespace)?
        .into_iter()
        .filter(|volume| {
            saved
                .accepted
                .iter()
                .any(|loss| loss.volume.id == volume.id)
        })
        .collect();
    if removed.is_empty() {
        return Ok(Vec::new());
    }
    match crate::removal::held(removed, trusted.volumes.as_ref()) {
        Ok((losses, unanswered)) if unanswered.is_empty() => Ok(losses
            .into_iter()
            .filter(|loss| crate::removal::covered(loss, &saved.accepted))
            .collect()),
        Ok(_) | Err(_) => Ok(Vec::new()),
    }
}

/// Whether every check suite of the pushed commit completed and passed; false
/// until GitHub reported one.
fn passed(tx: &mut dyn Tx, who: &Actor, push: &Push<'_>) -> Result<bool, RpcError> {
    let suites = tx.query(
        "SELECT status, conclusion FROM config_check_suite \
         WHERE organization_id = ?1 AND repository_id = ?2 AND head = ?3",
        &[
            who.organization.as_str().into(),
            push.repository_id.into(),
            push.head.as_str().into(),
        ],
    )?;
    let mut passed = !suites.is_empty();
    let completed = name_of(CheckStatus::Completed);
    for suite in suites {
        passed &= suite.text(0)? == completed
            && suite
                .optional_text(1)?
                .and_then(|conclusion| serde_json::from_value(json!(conclusion)).ok())
                .is_some_and(CheckConclusion::passed);
    }
    Ok(passed)
}
