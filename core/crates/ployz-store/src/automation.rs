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
use std::collections::BTreeMap;

use ployz_core::config::SavedServiceIntent;
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::deployment::{self, DeploymentStatus, DeploymentSummary};
use crate::error;
use crate::id::{ConditionalSaveId, DeploymentId, EnvironmentId, Hostname, OrganizationId};
use crate::policy::{self, is_repository_path};
use crate::review;
use crate::scope::{self, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, Trusted, build, registry};

/// An observation Cloud made of GitHub. Never caller testimony: only Cloud's worker
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
    /// The merge commits of frozen Conditional Saves ([`crate::PendingSaves::merged`])
    /// Cloud found `head` is or descends from: this push carries those saves.
    #[serde(default)]
    pub merged: Vec<CommitSha>,
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
    /// GitHub's status: `queued`, `in_progress`, `completed`, ….
    pub status: String,
    /// GitHub's conclusion once completed.
    #[serde(default)]
    pub conclusion: Option<String>,
    /// When GitHub last changed it: an older result never replaces a newer one.
    pub updated: crate::GithubTimestamp,
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
    /// Branches the Store is closing that still run on the Servers: Cloud admits
    /// their removal (`Admit { remove }`) with the runtime evidence it gathers.
    pub closing: Vec<EnvironmentSummary>,
    /// Environments deleted: nothing of them runs on the Servers any more.
    pub removed: Vec<EnvironmentSummary>,
    /// Pull requests whose GitHub check Cloud publishes again.
    pub checks: Vec<crate::PullRequestRef>,
}

/// One auto-deploy.
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

/// Check-suite conclusions that let a waiting deploy go.
const PASSED: [&str; 3] = ["success", "neutral", "skipped"];

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
    // The first head seen deploys every Service that follows the branch.
    let changed = event.base.as_ref().and(event.changed.as_deref());
    let environments = tx.query(
        "SELECT id FROM config_environment WHERE organization_id = ?1 ORDER BY id",
        &[organization.into()],
    )?;
    let mut carried =
        crate::conditional_save::carried(tx, who, event.repository_id, branch, &event.merged)?;
    let mut automated = Automated::default();
    for row in environments {
        let environment = row.parse::<EnvironmentId>(0, "Environment ID")?;
        let push = Push {
            repository_id: event.repository_id,
            branch,
            head,
        };
        let saves = carried.remove(&environment).unwrap_or_default();
        deploy(
            tx,
            who,
            &environment,
            &push,
            (Select::Changed(changed), &saves),
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
    let text_ok = |text: &str| text.len() <= 64 && !text.chars().any(char::is_control);
    if !text_ok(&event.status) || !event.conclusion.as_deref().is_none_or(text_ok) {
        return Err(error::invalid(
            "Expected GitHub's status and conclusion",
            json!({}),
        ));
    }
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
            event.status.as_str().into(),
            event.conclusion.as_deref().into(),
            event.updated.as_str().into(),
        ],
    )?;
    let waiting = tx.query(
        "SELECT environment_id, branch, services, saves FROM config_waiting_deploy \
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
        let saves: Vec<ConditionalSaveId> = row.json(3, "waiting deploy")?;
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
            (Select::Services(&services), &saves),
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

/// Which of the Services that follow the branch deploy.
enum Select<'a> {
    /// Those watching one of these changed paths; `None`: all of them.
    Changed(Option<&'a [String]>),
    /// Those a push selected earlier, by ID, that still follow it.
    Services(&'a [String]),
}

/// Deploy `environment`'s Services the push selects, wait for CI, or skip it; the
/// frozen Conditional Saves it carries there land first, wait with it, or land now.
fn deploy(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    push: &Push<'_>,
    (select, saves): (Select<'_>, &[ConditionalSaveId]),
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    match admit(tx, who, environment, push, (select, saves), trusted) {
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
    (select, saves): (Select<'_>, &[ConditionalSaveId]),
    trusted: &Trusted,
) -> Result<Option<Deploy>, RpcError> {
    let mut environment = scope::lock_id(tx, who, id)?;
    // Where nothing deploys, what the push carries saves now.
    let land = |tx: &mut dyn Tx, environment: &mut scope::Environment| {
        saves
            .iter()
            .try_for_each(|save| crate::conditional_save::land(tx, who, save, environment))
    };
    // Closing, or being removed: a push leaves it be. A shut-down PR Environment
    // (its removal applied) comes back on: the push deploys all of it again.
    let removing = crate::teardown::removing(tx, id)?;
    let shut_down = removing
        .as_ref()
        .is_some_and(|removal| removal.status == DeploymentStatus::Applied)
        && crate::pull_request::of(tx, id)?.is_some();
    if crate::pull_request::closing(tx, id)? || (removing.is_some() && !shut_down) {
        land(tx, &mut environment)?;
        return Ok(None);
    }
    let Some(saved) = review::latest_saved(tx, id)? else {
        land(tx, &mut environment)?;
        return Ok(None);
    };
    let mut selected: Vec<&SavedServiceIntent> = Vec::new();
    let mut wait = false;
    for service in &saved.intent.services {
        if !crate::git::tracks(service, push.repository_id, push.branch) {
            continue;
        }
        let policy = policy::load(tx, id, &service.id)?;
        let chosen = policy.auto_deploy
            && match select {
                Select::Changed(None) => true,
                Select::Changed(Some(changed)) => policy.watches(changed),
                Select::Services(ids) => ids.contains(&service.id),
            };
        if chosen {
            wait |= policy.wait_for_ci;
            selected.push(service);
        }
    }
    let key: [crate::storage::Param<'_>; 3] = [
        id.as_str().into(),
        push.repository_id.into(),
        push.branch.as_str().into(),
    ];
    let [environment_param, repository_param, branch_param] = key;
    if selected.is_empty() || trusted.runnable().is_err() {
        tx.execute(
            "DELETE FROM config_waiting_deploy \
             WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3",
            &key,
        )?;
        land(tx, &mut environment)?;
        return Ok((!selected.is_empty()).then_some(Deploy::NoServers));
    }
    if wait && !passed(tx, who, push)? {
        let services: Vec<&str> = selected.iter().map(|service| service.id.as_str()).collect();
        tx.execute(
            "INSERT INTO config_waiting_deploy \
             (environment_id, repository_id, branch, organization_id, head, services, saves) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (environment_id, repository_id, branch) \
             DO UPDATE SET head = excluded.head, services = excluded.services, \
             saves = excluded.saves",
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
                serde_json::to_string(saves).expect("Conditional Save IDs are JSON").as_str().into(),
            ],
        )?;
        return Ok(Some(Deploy::Waiting));
    }
    tx.execute(
        "DELETE FROM config_waiting_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3",
        &key,
    )?;
    // What the push carries lands in Saved State first, so it deploys too.
    let selected: Vec<SavedServiceIntent> = selected.into_iter().cloned().collect();
    let (saved, selected) = match saves.is_empty() {
        true => (saved, selected),
        false => {
            let ids: Vec<String> = selected.iter().map(|service| service.id.clone()).collect();
            land(tx, &mut environment)?;
            let saved =
                review::latest_saved(tx, id)?.ok_or_else(|| error::corrupt("Saved State"))?;
            let selected = saved
                .intent
                .services
                .iter()
                .filter(|service| ids.contains(&service.id))
                .cloned()
                .collect();
            (saved, selected)
        }
    };
    let names = match shut_down {
        true => Vec::new(),
        false => selected
            .iter()
            .map(|service| ServiceName::parse(service.slug.as_str()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| error::corrupt("Service name"))?,
    };
    let pins: BTreeMap<ServiceName, CommitSha> = selected
        .iter()
        .map(|service| (service.config.private_dns.clone(), push.head.clone()))
        .collect();
    // Generated domains expand under the Cluster Domain Cloud reserved for the
    // Environment's last Deploy.
    let cluster_domain = deployment::cluster_domain(tx, id)?;
    // A skipped Deploy leaves nothing behind, such as a reserved Namespace.
    let summary = crate::storage::attempt(tx, |tx| {
        auto_admit(
            tx,
            who,
            (&environment, &saved),
            &names,
            (cluster_domain.as_ref(), &pins),
        )
    })?;
    Ok(Some(Deploy::Admitted(Box::new(summary))))
}

/// Admit the Store's own Deployment of Saved revision `saved` of `environment`:
/// `services` (none: every one), with generated domains under `cluster_domain` and
/// each Git Service of `pins` pinned to its commit. A targeted Deploy removes
/// nothing, and neither does one of an Environment with nothing applied.
pub(crate) fn auto_admit(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, saved): (&scope::Environment, &review::Saved),
    services: &[ServiceName],
    (cluster_domain, pins): (Option<&Hostname>, &BTreeMap<ServiceName, CommitSha>),
) -> Result<DeploymentSummary, RpcError> {
    let id = &environment.summary.id;
    crate::deployment::admit::needs_cluster_domain(environment, &saved.intent, cluster_domain)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let head = deployment::head(tx, environment)?;
    let mut frozen = deployment::freeze(
        id,
        &saved.intent,
        &head.applied,
        services,
        namespace,
        cluster_domain,
        &[],
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
    for suite in suites {
        passed &= suite.text(0)? == "completed" && PASSED.contains(&suite.text(1)?);
    }
    Ok(passed)
}

