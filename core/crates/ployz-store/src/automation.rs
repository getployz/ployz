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

use ployz_core::authority::{CommitApproval, Signed};
use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent, ServiceSource};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::build::BuildStatus;
use crate::deployment::{self, DeploymentStatus, DeploymentSummary, GitPush};
use crate::error;
use crate::id::{DeploymentId, EnvironmentId, Hostname, OrganizationId, ServiceLineageId};
use crate::policy::{self, is_repository_path};
use crate::review;
use crate::scope::{self, EnvironmentSummary};
use crate::storage::{Tx, name_of};
use crate::volume::size::VolumeResized;
use crate::{Actor, Trusted, build};

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
    /// Cloud's resize run moved a deployed Managed Volume's size, in either
    /// direction: the Store records it in the Volume's fixed storage, Applied State
    /// and, where it still says the old size, Working State, at once.
    VolumeResized(VolumeResized),
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
    /// The conclusions that let a Deployment waiting for CI go.
    const PASSING: [Self; 3] = [Self::Success, Self::Neutral, Self::Skipped];

    fn passed(self) -> bool {
        Self::PASSING.contains(&self)
    }
}

/// What an event made the Store do.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Automated {
    /// Deployments admitted: Cloud dispatches each to a runner.
    pub admitted: Vec<AutoDeployed>,
    /// Environments that would have deployed but can't, and why.
    pub skipped: Vec<Skipped>,
    /// Branches whose removal is due: Cloud admits it from the Servers under the
    /// mark (`Admit { remove }`).
    pub closing: Vec<EnvironmentSummary>,
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
        SystemEvent::BranchHead(head) => {
            crate::pull_request::withdraw_trust(tx, &who, trusted)?;
            branch_head(tx, &who, head, trusted)
        }
        SystemEvent::CheckSuite(suite) => {
            crate::pull_request::withdraw_trust(tx, &who, trusted)?;
            check_suite(tx, &who, suite, trusted)
        }
        SystemEvent::PullRequest(pull) => {
            crate::pull_request::pull_request(tx, &who, pull, trusted)
        }
        SystemEvent::Sweep(sweep) => crate::pull_request::sweep(tx, &who, sweep, trusted),
        SystemEvent::ClusterForgotten => Ok(Automated {
            cancelled: crate::teardown::forget_cluster(tx, &who)?,
            ..Automated::default()
        }),
        SystemEvent::VolumeResized(resized) => {
            crate::volume::size::record(tx, &who, resized, trusted).map(|()| Automated::default())
        }
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
    // A deleted branch deploys nothing: it locks only the holds it supersedes.
    let Some(head) = &event.head else {
        deployment::supersede_held(tx, who, (repository_id, branch), None)?;
        return Ok(Automated::default());
    };
    // Every Environment it may cancel a hold in or deploy, in ID order, before any.
    let environments = tx
        .query(
            "SELECT id FROM config_environment WHERE organization_id = ?1 ORDER BY id",
            &[organization.into()],
        )?
        .iter()
        .map(|row| row.parse::<EnvironmentId>(0, "Environment ID"))
        .collect::<Result<Vec<_>, RpcError>>()?;
    scope::lock_all(tx, environments.iter().cloned())?;
    // A Deployment still waiting for an older head's CI never ships now.
    deployment::supersede_held(tx, who, (repository_id, branch), Some(head))?;
    // The first head seen deploys every Service that follows the branch.
    let changed = event.base.as_ref().and(event.changed.as_deref());
    let mut automated = Automated::default();
    for environment in environments {
        let push = GitPush {
            repository_id: event.repository_id,
            branch: branch.clone(),
            head: head.clone(),
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
    let mut automated = Automated::default();
    // Whether every suite passed is read under each Environment's lock (`admit`): two
    // suites of the head completing at once each see the other's row committed there,
    // where either read before the lock could find the other still pending. Locked in
    // ID order before any.
    let held = deployment::held_for_head(tx, who, repository_id, &event.head)?;
    scope::lock_all(
        tx,
        held.iter().map(|(_, environment, _)| environment.clone()),
    )?;
    for (held, environment, push) in held {
        deploy(
            tx,
            who,
            &environment,
            &push,
            Select::Held(&held),
            trusted,
            &mut automated,
        )?;
    }
    Ok(automated)
}

/// Which of the Services that follow the branch deploy, besides those merges
/// landed.
#[derive(Clone, Copy)]
enum Select<'a> {
    /// Those watching one of these changed paths (`None`: all of them).
    Changed(Option<&'a [String]>),
    /// Those this Deployment waiting for CI selected, that still follow the branch.
    Held(&'a DeploymentId),
}

/// Deploy `environment`'s Services the push selects, hold them for CI, or skip it.
/// A Deployment waiting for CI that can't deploy once it passed is cancelled, saying
/// why.
fn deploy(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    push: &GitPush,
    select: Select<'_>,
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let deployed = admit(tx, who, environment, push, select, trusted);
    let ended = match deployed {
        Ok(Some(Deploy::Idle)) => Some(IDLE.to_owned()),
        deployed => record(environment, deployed, automated)?,
    };
    if let (Some(why), Select::Held(held)) = (ended, select) {
        deployment::cancel_held(tx, held, why)?;
    }
    Ok(())
}

/// Record in `automated` what an automatic Deploy into `environment` came to; why it
/// was skipped, if it was. One Environment that can't deploy never holds back the
/// others.
pub(crate) fn record(
    environment: &EnvironmentId,
    deployed: Result<Option<Deploy>, RpcError>,
    automated: &mut Automated,
) -> Result<Option<String>, RpcError> {
    let reason = match deployed {
        Ok(Some(Deploy::Admitted(deployment))) => {
            automated.admitted.push(AutoDeployed {
                environment: deployment.environment_id.clone(),
                deployment: *deployment,
            });
            return Ok(None);
        }
        Ok(Some(Deploy::NoServers)) => crate::trusted::NO_SERVERS.to_owned(),
        Ok(Some(Deploy::NeedsApproval)) => crate::pull_request::NEEDS_APPROVAL.to_owned(),
        Ok(Some(Deploy::Reapprove(changed))) => crate::auto_rule::reapprove(&changed),
        Ok(Some(Deploy::Idle) | None) => return Ok(None),
        Err(error) if crate::error::skippable(&error) => error.message,
        Err(error) => return Err(error),
    };
    automated.skipped.push(Skipped {
        environment: environment.clone(),
        reason: reason.clone(),
    });
    Ok(Some(reason))
}

/// Why a Deployment waiting for CI that nothing it deploys follows any more ends.
const IDLE: &str = "Nothing it deploys follows this branch any more";

/// Deploy pull request `event`'s head into its PR Environment `environment`, as a
/// push of it would.
pub(crate) fn deploy_head(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    event: &crate::PullRequest,
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    deploy(
        tx,
        who,
        environment,
        &event.head_push(),
        Select::Changed(None),
        trusted,
        automated,
    )
}

/// What an automatic Deploy into one Environment came to.
pub(crate) enum Deploy {
    Admitted(Box<DeploymentSummary>),
    /// Nothing it would deploy: no Service it selects follows the branch.
    Idle,
    /// It would deploy, but no Server could run it.
    NoServers,
    /// It would deploy a pull request's head no member approved.
    NeedsApproval,
    /// It would deploy settings the Environment's automatic deploy rule doesn't
    /// approve: these Services.
    Reapprove(Vec<ServiceName>),
}

/// What a push deploys of an Environment: a shut-down PR Environment comes back as a whole
/// Deploy, which names no Service and retires none; any other push names the Services it
/// selects and retires those a merge or a Save removed.
enum Plan {
    ShutDown,
    Push(Vec<deployment::Retire>),
}

impl Plan {
    fn retire(&self) -> &[deployment::Retire] {
        match self {
            Self::ShutDown => &[],
            Self::Push(retire) => retire,
        }
    }

    fn names(
        &self,
        selected: &[(&SavedServiceIntent, bool)],
    ) -> Result<Vec<ServiceName>, RpcError> {
        self.shipped(selected)
            .iter()
            .map(|(service, _)| ServiceName::parse(service.slug.as_str()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| error::corrupt("Service name"))
    }

    fn ids(&self, selected: &[(&SavedServiceIntent, bool)]) -> Vec<String> {
        self.shipped(selected)
            .iter()
            .map(|(service, _)| service.id.clone())
            .collect()
    }

    fn shipped<'a, 'b>(
        &self,
        selected: &'a [(&'b SavedServiceIntent, bool)],
    ) -> &'a [(&'b SavedServiceIntent, bool)] {
        match self {
            Self::ShutDown => &[],
            Self::Push(_) => selected,
        }
    }
}

fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &EnvironmentId,
    push: &GitPush,
    select: Select<'_>,
    trusted: &Trusted,
) -> Result<Option<Deploy>, RpcError> {
    let environment = scope::lock_id(tx, who, id)?;
    // Cancelled before its CI passed: nothing ships. Until every suite of its commit
    // passed, it keeps waiting: read under the lock, so a suite that completed in
    // another transaction is committed, or will promote the hold itself.
    let (held, held_services) = match select {
        Select::Held(held) => match deployment::load(tx, held)?.summary {
            hold if hold.status == DeploymentStatus::WaitingForCi => {
                if !passed(tx, who, push.repository_id, &push.head)? {
                    return Ok(None);
                }
                (Some(hold), deployment::held_services(tx, held)?)
            }
            _ => return Ok(None),
        },
        Select::Changed(_) => (None, Vec::new()),
    };
    // Being removed: a push leaves it be. A shut-down PR Environment (its removal
    // applied) comes back on: the push deploys all of it again.
    let removal = crate::deployment::latest_removal(tx, id)?.filter(|removal| {
        !matches!(
            removal.status,
            DeploymentStatus::Failed | DeploymentStatus::Cancelled
        )
    });
    let shut_down = removal
        .as_ref()
        .is_some_and(|removal| removal.status == DeploymentStatus::Applied)
        && crate::pr_environment::of(tx, id)?.is_some();
    if crate::removal::mark::naming(tx, id)?.is_some() || (removal.is_some() && !shut_down) {
        return Ok(None);
    }
    // A push brings back a Branch whose removal someone cancelled.
    crate::removal::mark::clear_tombstone(tx, &crate::removal::Target::Environment(id.clone()))?;
    let Some(saved) = review::latest_saved(tx, id)? else {
        return Ok(None);
    };
    let mut selected: Vec<(&SavedServiceIntent, bool)> = Vec::new();
    let mut wait = false;
    for service in &saved.intent.services {
        if !crate::git::tracks(service, push.repository_id, &push.branch) {
            continue;
        }
        let policy = policy::load(tx, id, &service.id)?;
        let follows = policy.auto_deploy;
        let pushed = follows
            && match (select, &held) {
                (Select::Changed(changed), _) => {
                    changed.is_none_or(|changed| policy.watches(changed))
                }
                // None selected is a shut-down Environment's: all of it. By ID: a
                // Service renamed while it waited is still the one it selected.
                (Select::Held(_), Some(_)) => {
                    held_services.is_empty() || held_services.contains(&service.id)
                }
                (Select::Held(_), None) => false,
            };
        if pushed {
            wait |= policy.wait_for_ci;
            selected.push((service, follows));
        }
    }
    let plan = match shut_down {
        true => Plan::ShutDown,
        false => {
            // A push ships every Saved change, so what a Save removed goes with it,
            // unless it never ran.
            let head = deployment::head(tx, &environment)?;
            let mut removed = BTreeSet::new();
            for service in deployment::Landing::of(&head)
                .dropped(&saved.intent)
                .services
            {
                removed.insert(ServiceLineageId::parse(service.id.as_str())?);
            }
            Plan::Push(deployment::retirements(tx, id, &saved.intent, &removed)?)
        }
    };
    // Only what it selects makes a push deploy.
    if selected.is_empty() {
        return Ok(Some(Deploy::Idle));
    }
    if !crate::pull_request::admits(tx, id, (push.repository_id, &push.head), trusted)? {
        return Ok(Some(Deploy::NeedsApproval));
    }
    if trusted.runnable().is_err() {
        return Ok(Some(Deploy::NoServers));
    }
    let names = plan.names(&selected)?;
    // A push of a live rule's branch deploys only the settings the rule approves: any
    // other Deployment would fail on every Server, so it waits for a person instead.
    let rule = crate::auto_rule::stored(tx, id)?
        .as_ref()
        .and_then(crate::auto_rule::opened);
    if let Some(rule) = rule.filter(|rule| {
        !rule.ended
            && crate::auto_rule::branch(rule).is_some_and(|(repository, branch)| {
                repository == push.repository_id && branch == push.branch
            })
    }) {
        let target = crate::deployment::query::saved_target(
            tx,
            who,
            trusted,
            (&environment, &saved.intent),
            &[],
        )?;
        let namespace = deployment::namespace(tx, who, &environment.summary)?;
        let ships: Vec<ServiceName> = selected
            .iter()
            .filter(|(_, follows)| *follows)
            .map(|(service, _)| service.config.private_dns.clone())
            .collect();
        let sources = crate::auto_rule::sources(&saved.intent);
        let changed = crate::auto_rule::changed(&rule, &namespace, &target, &sources, &ships);
        if !changed.is_empty() {
            return Ok(Some(Deploy::Reapprove(changed)));
        }
    }
    if wait && !passed(tx, who, push.repository_id, &push.head)? {
        let ids = plan.ids(&selected);
        deployment::hold(
            tx,
            who,
            &environment.summary,
            push,
            &names,
            &ids,
            saved.revision,
        )?;
        return Ok(None);
    }
    // A Service that builds the head is pinned to it. Another Git Service the push
    // ships for its Saved change stays on the commit it last built here; one that
    // never built builds the head when it follows the branch, else its own head.
    let mut pins: BTreeMap<ServiceName, CommitSha> = BTreeMap::new();
    for service in &saved.intent.services {
        let follows = selected
            .iter()
            .any(|(selected, follows)| *follows && selected.id == service.id);
        let pin = if follows {
            Some(push.head.clone())
        } else if let ServiceSource::Git { repository_id, .. } = &service.config.source {
            let repository = RepositoryId::parse(*repository_id)?;
            match last_built(tx, id, service)? {
                Some(commit)
                    if crate::pull_request::admits(tx, id, (repository, &commit), trusted)? =>
                {
                    Some(commit)
                }
                Some(_) => None,
                None => crate::git::tracks(service, push.repository_id, &push.branch)
                    .then(|| push.head.clone()),
            }
        } else {
            None
        };
        if let Some(pin) = pin {
            pins.insert(service.config.private_dns.clone(), pin);
        }
    }
    // Generated domains expand under the Cluster Domain Cloud reserved for the
    // Environment's last Deploy. A PR Environment that opened before its first Deploy,
    // waiting for a review, takes the one its start-from's last Deploy reserved, as its
    // first Deploy does when it opens and deploys at once.
    let cluster_domain = match deployment::cluster_domain(tx, id)? {
        Some(domain) => Some(domain),
        None => match crate::branch::row(tx, id)? {
            Some(branch) => deployment::cluster_domain(tx, &branch.parent)?,
            None => None,
        },
    };
    let fresh = DeploymentId::fresh();
    let admission = match &held {
        Some(hold) => deployment::Admission::Promote(&hold.id),
        None => deployment::Admission::New {
            id: &fresh,
            push: Some(push),
        },
    };
    // A skipped Deploy leaves nothing behind, such as a reserved Namespace.
    let summary = crate::storage::attempt(tx, |tx| {
        auto_admit(
            tx,
            who,
            (&environment, &saved),
            (&names, plan.retire()),
            (cluster_domain.as_ref(), &pins, None),
            admission,
            trusted,
        )
    })?;
    Ok(Some(Deploy::Admitted(Box::new(summary))))
}

/// The commit Service `service` (its ID) last built in Environment `id`, under
/// whatever runtime name it had then: each Deployment's target nodes say.
fn last_built(
    tx: &mut dyn Tx,
    id: &EnvironmentId,
    service: &SavedServiceIntent,
) -> Result<Option<CommitSha>, RpcError> {
    let (built, reused) = (name_of(BuildStatus::Built), name_of(BuildStatus::Reused));
    let rows = tx.query(
        &format!(
            "SELECT b.commit_sha, b.service, d.nodes, d.saved_revision FROM config_build b \
         JOIN config_deployment d ON d.id = b.deployment_id \
         WHERE d.environment_id = ?1 AND d.nodes LIKE ?2 AND b.commit_sha IS NOT NULL \
         AND b.status IN ('{built}', '{reused}') ORDER BY d.admitted DESC"
        ),
        &[
            id.as_str().into(),
            deployment::node_pattern(&service.id).as_str().into(),
        ],
    )?;
    for row in &rows {
        let nodes: Vec<deployment::TargetNode> = row.json(2, "Deployment")?;
        let built = row.text(1)?;
        let ours = nodes.iter().any(|node| {
            node.service().is_some_and(|ours| {
                ours.id.as_str() == service.id && ours.runtime.as_str() == built
            })
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
            .and_then(|then| crate::git::built_from(&then.config.source))
            .is_some_and(|then| crate::git::built_from(&service.config.source) == Some(then));
        return if same {
            row.parse(0, "commit").map(Some)
        } else {
            Ok(None)
        };
    }
    Ok(None)
}

/// Admit the Store's own Deployment of Saved revision `saved` of `environment`:
/// `services` and the teardown of `retire`, from [`deployment::retirements`]
/// (neither: every Service, removing any other its Namespace runs), with
/// generated domains under `cluster_domain` and each Git Service of `pins` pinned
/// to its commit, as `admission` queues it. It deletes only the Volume data
/// accepted when `saved` was saved, while the Servers still show the same.
pub(crate) fn auto_admit(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, saved): (&scope::Environment, &review::Saved),
    (services, retire): (&[ServiceName], &[deployment::Retire]),
    (cluster_domain, pins, approval): (
        Option<&Hostname>,
        &BTreeMap<ServiceName, CommitSha>,
        Option<&Signed<CommitApproval>>,
    ),
    admission: deployment::Admission<'_>,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let id = &environment.summary.id;
    let namespace = deployment::reserve_namespace(tx, who, &environment.summary)?;
    crate::deployment::admit::gate(
        tx,
        who,
        (environment, &saved.intent),
        (cluster_domain, &namespace),
        trusted,
    )?;
    let head = deployment::head(tx, environment)?;
    // Diffed against what may run once the Deployments in flight end, not Applied
    // State alone: those may still add, change or remove any node.
    let landing = deployment::Landing::of(&head);
    let full = services.is_empty() && retire.is_empty();
    let losses = consented(&landing.dropped(&saved.intent), saved, &namespace, trusted)?;
    let dropped = match full {
        true => deployment::dropped(tx, id, &saved.intent, &landing)?,
        false => Vec::new(),
    };
    let opening = deployment::Opening::of(tx, id, &namespace)?;
    let frozen = deployment::freeze(
        id,
        &who.organization,
        &saved.intent,
        &landing,
        if full {
            deployment::Scope {
                targeting: deployment::Targeting::Full { prune: true },
                retire: &dropped,
                losses: &losses,
            }
        } else {
            deployment::Scope {
                targeting: deployment::Targeting::Named(deployment::Narrow {
                    services,
                    every_saved: true,
                }),
                retire,
                losses: &losses,
            }
        },
        namespace.clone(),
        cluster_domain,
        opening.as_ref(),
    )?;
    if let Some(opening) = &opening {
        let ships = deployment::Ships {
            targeting: match full {
                true => deployment::Targeting::Full { prune: true },
                false => deployment::Targeting::Named(services.to_vec()),
            },
            removals: ployz_core::ServiceRemovals::default(),
        };
        let target = deployment::lower_held(
            tx,
            id,
            &saved.intent,
            &ships,
            (namespace.clone(), cluster_domain),
            &who.organization,
        )?
        .created(!full);
        crate::pull_request::preview::cover(
            tx,
            id,
            &opening.intent(&saved.intent),
            (&namespace, &target),
        )?;
    }
    let pins: BTreeMap<ServiceName, CommitSha> = pins
        .iter()
        .filter(|(name, _)| {
            frozen
                .nodes
                .iter()
                .any(|node| node.service().is_some_and(|ours| ours.runtime == *name))
        })
        .map(|(name, commit)| (name.clone(), commit.clone()))
        .collect();
    // Each Service it ships for a Saved change is a target too, so the planner runs it.
    let targets: Vec<ServiceName> = match full {
        true => Vec::new(),
        false => frozen
            .nodes
            .iter()
            .filter_map(|node| match node {
                deployment::TargetNode::Service { name, .. } => Some(name.clone()),
                deployment::TargetNode::Retire { .. }
                | deployment::TargetNode::Volume { .. }
                | deployment::TargetNode::Config { .. } => None,
            })
            .collect(),
    };
    let summary = deployment::admit(
        tx,
        who,
        (admission, &targets, None, None),
        environment,
        saved.revision,
        &frozen,
    )?;
    build::pin_admitted(tx, &summary.id, (&pins, approval), trusted)?;
    Ok(summary)
}

/// The losses of the Volumes `dropped` that `saved` holds consent for, while the
/// Servers still show them unchanged. Without that evidence a push keeps the data,
/// and the next Deploy asks again.
fn consented(
    dropped: &SavedEnvironmentIntent,
    saved: &review::Saved,
    namespace: &ployz_core::Namespace,
    trusted: &Trusted,
) -> Result<Vec<crate::VolumeLoss>, RpcError> {
    let removed: Vec<_> = crate::deployment::loss::removed(dropped, &saved.intent, namespace)?
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
    match crate::deployment::loss::held(removed, trusted.volumes.as_ref()) {
        Ok((losses, unanswered)) if unanswered.is_empty() => Ok(losses
            .into_iter()
            .filter(|loss| crate::deployment::loss::covered(loss, &saved.accepted))
            .collect()),
        Ok(_) | Err(_) => Ok(Vec::new()),
    }
}

/// A SQL condition over an unaliased `config_deployment` row: a check suite of its
/// pushed commit completed without passing, by the rule [`passed`] lets it go by.
pub(crate) static CI_FAILED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let passing = CheckConclusion::PASSING
        .iter()
        .map(|conclusion| format!("'{}'", name_of(conclusion)))
        .collect::<Vec<_>>()
        .join(", ");
    let completed = name_of(CheckStatus::Completed);
    format!(
        "EXISTS (SELECT 1 FROM config_check_suite s \
         WHERE s.organization_id = config_deployment.organization_id \
         AND s.repository_id = config_deployment.push_repository_id \
         AND s.head = config_deployment.push_head AND s.status = '{completed}' \
         AND COALESCE(s.conclusion, '') NOT IN ({passing}))"
    )
});

/// Whether every check suite of commit `head` of `repository_id` completed and
/// passed; false until GitHub reported one.
fn passed(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    head: &CommitSha,
) -> Result<bool, RpcError> {
    let suites = tx.query(
        "SELECT status, conclusion FROM config_check_suite \
         WHERE organization_id = ?1 AND repository_id = ?2 AND head = ?3",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            head.as_str().into(),
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
