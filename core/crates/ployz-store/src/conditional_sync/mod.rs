//! Conditional Syncs: a PR Environment's Sync into one of its Destinations that
//! goes live with the pull request's merge.
//!
//! Syncing records the picked rows, core's picks and what landing needs (the PR
//! Environment's side as synced), so landing and a later take never read the PR
//! Environment, which may be gone by then. A secret the Destination lacks arrives
//! by name only, with the value the Destination held for the merge, if any
//! ([`HoldSecret`]); the pull request's check waits until it has one. A Conditional
//! Sync stands while the PR Environment's Working State, but for what it follows
//! from its Parent, and the pull request's target branch are what they were; edits
//! in the Destination never withdraw it. When the pull request closes it freezes
//! with the merge commit if it merged and still stands, else it drops. A frozen
//! one lands with the first push to the target branch whose head contains the
//! merge commit (Cloud observes the ancestry): before the Deployment that push
//! admits in its Destination, with the deploy that push waits for CI with, or at
//! once where the push deploys nothing. A Destination that doesn't deploy the
//! branch on push gets it at the merge.
//!
//! Landing, per picked row: the Destination left it alone, or has an undeployed
//! edit of it → saved, the edit on top; it changed it live (neither its Saved nor
//! its Working State holds the value synced against) → not saved but staged,
//! `staged`; both → not saved, the pull request's value only a `hint` a take
//! stages. Landed rows stay marked until the Destination's next Saved revision.

mod held;
mod landing;
mod syncing;
pub(crate) use held::hold;
pub use held::{HoldSecret, SecretHeld};
use landing::{Planned, plan};
pub(crate) use syncing::*;

use crate::id::{BranchName, CommitSha, PullRequestNumber, RepositoryId};
use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{
    BranchChanges, BranchHostnames, BranchRole, SavedEnvironmentIntent, SavedVariableIntent,
    SavedVariableValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::branch::{
    self, Carried, Moving, SyncChanges, SyncQuery, SyncView, Synced, Take, Taken, When,
};
use crate::id::{ConditionalSyncId, EnvironmentId, Revision};
use crate::pull_request::{self, PullRequest, PullRequestRef};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, deployment, error, policy, review, teardown};

/// A Conditional Sync, as a Sync or a take answers it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConditionalSync {
    /// Pass to [`Take::from`] to use a hint it left.
    pub id: ConditionalSyncId,
    pub pull_request: PullRequestNumber,
    /// The rows it holds.
    pub rows: Vec<String>,
    pub state: ConditionalSyncState,
}

/// Where a Conditional Sync is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalSyncState {
    /// Waiting for the merge, with its PR Environment.
    Standing,
    /// Merged: waiting for a push of the merge commit.
    Frozen,
    /// Landed: what it left beside the Destination's own changes.
    Landed,
}

/// How a landed row stands in the Destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Landed {
    /// Staged in Working State as a change to deploy.
    Staged,
    /// The Destination's own edit stays; take the pull request's value to stage it.
    Hint,
}

/// A pull request's value a landed Conditional Sync left in an Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PullRequestHint {
    /// The Conditional Sync: pass to [`Take::from`].
    pub conditional_sync: ConditionalSyncId,
    pub pull_request: PullRequestNumber,
    /// `NODE.path`, as a Sync names it.
    pub row: String,
    /// The pull request's value; secrets read `{"secret": true}`.
    pub value: Value,
    pub landed: Landed,
}

/// What Cloud checks before telling the Store about a push to a branch.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PendingSyncs {
    /// Pull requests into the branch with a Conditional Sync standing: Cloud reports
    /// each that merged first, so its Conditional Syncs freeze.
    pub standing: Vec<PullRequestNumber>,
    /// Merge commits of frozen ones: Cloud reports which the new head contains
    /// ([`crate::BranchHead::merged`]).
    pub merged: Vec<CommitSha>,
}

/// A Conditional Sync as stored.
#[derive(Serialize, Deserialize)]
struct Stored {
    rows: Vec<Row>,
    /// Core's picks.
    picks: Vec<Pick>,
    /// The PR Environment's side as synced.
    landing: Landing,
    /// What its Services carry: registry credentials and Deployment Policies.
    carried: Carried,
    /// The PR Environment.
    from: EnvironmentSummary,
    /// The Destination's Saved revision landing published.
    #[serde(default)]
    landed: Option<Revision>,
}

/// A row core lands, by key. One stored before Sync may also hold a value choice,
/// which landing ignores.
#[derive(Clone, Serialize, Deserialize)]
struct Pick {
    key: String,
}

/// What landing compares: the PR Environment's Working State over its base.
#[derive(Serialize, Deserialize)]
struct Landing {
    from: SavedEnvironmentIntent,
    base: SavedEnvironmentIntent,
    hostnames: BranchHostnames,
}

#[derive(Clone, Serialize, Deserialize)]
struct Row {
    /// Core's row key.
    key: String,
    /// The Destination's value the Sync was reviewed against (core's, redacted).
    into: Value,
    shown: Shown,
    #[serde(default)]
    landed: Option<Landed>,
}

/// A row as the Sync showed it.
#[derive(Clone, Serialize, Deserialize)]
struct Shown {
    /// `NODE`, or `NODE.path`.
    row: String,
    /// The Destination changed it too.
    conflict: bool,
    /// The pull request's value; secrets read `{"secret": true}`.
    from: Value,
    /// The Destination's value.
    into: Value,
}

struct Found {
    environment: EnvironmentId,
    state: ConditionalSyncState,
    repository: RepositoryId,
    number: PullRequestNumber,
    stored: Stored,
}

/// Whether a Sync from `from` into `into` is a Conditional Sync: asked
/// `at_merge`, or from a PR Environment into one of its Destinations with `when`
/// omitted; `into` omitted, its only Destination unless asked `now`. A PR
/// Environment never syncs into a Destination now.
pub(crate) fn at_merge(
    tx: &mut dyn Tx,
    who: &Actor,
    (from, into): (&EnvironmentRef, Option<&EnvironmentRef>),
    when: Option<When>,
) -> Result<bool, RpcError> {
    if when == Some(When::AtMerge) {
        return Ok(true);
    }
    let from = scope::environment(tx, who, from)?;
    let into = match into {
        Some(into) => scope::environment(tx, who, into)?.summary,
        None if when.is_none() && pull_request::of(tx, &from.summary.id)?.is_some() => {
            return Ok(true);
        }
        None => match branch::row(tx, &from.summary.id)? {
            Some(row) => scope::load_by_id(tx, &row.parent)?.summary,
            None => return Ok(false),
        },
    };
    let Some(number) = merging_into(tx, &from.summary.id, &into.id)? else {
        return Ok(false);
    };
    if when == Some(When::Now) {
        return Err(error::invalid(
            format!(
                "{} is a PR Environment: its changes go live in {} with #{number}'s merge (when at_merge)",
                from.summary.name, into.name
            ),
            json!({}),
        ));
    }
    Ok(true)
}

/// The pull request whose merge a Sync from `from` into `into` waits for: `from`
/// is its PR Environment, and `into` one of its Destinations.
pub(crate) fn merging_into(
    tx: &mut dyn Tx,
    from: &EnvironmentId,
    into: &EnvironmentId,
) -> Result<Option<PullRequestNumber>, RpcError> {
    let rows = tx.query(
        "SELECT p.facts FROM config_pr_environment e JOIN config_pull_request p \
         ON p.organization_id = e.organization_id AND p.repository_id = e.repository_id \
         AND p.number = e.number WHERE e.environment_id = ?1",
        &[from.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let facts: PullRequest = row.json(0, "pull request")?;
    let project = scope::project_of(tx, from)?.id;
    let destinations =
        pull_request::destinations_of(tx, &project, facts.repository_id, &facts.target_branch)?;
    Ok(destinations.contains(into).then_some(facts.number))
}

/// A Conditional Sync's sides: the PR Environment and the Destination.
struct Sides {
    pr: Environment,
    into: Environment,
    facts: PullRequest,
}

fn sides(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &EnvironmentRef,
    into: Option<&EnvironmentRef>,
    lock: bool,
) -> Result<Sides, RpcError> {
    let pr = scope::environment(tx, who, from)?;
    let Some(PullRequestRef {
        repository_id,
        number,
    }) = pull_request::of(tx, &pr.summary.id)?
    else {
        return Err(error::invalid(
            format!(
                "{} is not a PR Environment: its changes sync now",
                pr.summary.name
            ),
            json!({}),
        ));
    };
    let facts = pull_request::facts(tx, who, repository_id, number)?
        .ok_or_else(|| error::corrupt("pull request"))?;
    let project = scope::project_of(tx, &pr.summary.id)?.id;
    let destinations =
        pull_request::destinations_of(tx, &project, repository_id, &facts.target_branch)?;
    let mut names = Vec::new();
    for id in &destinations {
        names.push(scope::load_by_id(tx, id)?.summary.name.to_string());
    }
    let into = match into {
        Some(at) => scope::environment(tx, who, at)?.summary.id,
        None => match destinations.as_slice() {
            [one] => one.clone(),
            [] => {
                return Err(error::conflict(
                    format!(
                        "Nothing deploys {} now: there is nowhere to sync into",
                        facts.target_branch
                    ),
                    json!({}),
                ));
            }
            _ => {
                return Err(error::invalid(
                    format!("Name the Destination: {}", names.join(", ")),
                    json!({ "valid_children": names }),
                ));
            }
        },
    };
    if !destinations.contains(&into) {
        return Err(error::conflict(
            format!(
                "That Environment doesn't deploy {}: sync into one that does",
                facts.target_branch
            ),
            json!({ "valid_children": names }),
        ));
    }
    let (pr, into) = scope::load_pair(tx, who, (&pr.summary.id, &into), lock)?;
    Ok(Sides { pr, into, facts })
}

/// The PR Environment's Working State into the Destination's, as the review shows it.
fn moving(tx: &mut dyn Tx, sides: &Sides) -> Result<Moving, RpcError> {
    Moving::conditional(tx, &sides.pr, &sides.into)
}

/// What a Conditional Sync would hold, as the Sync view shows it.
pub(crate) fn sync_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &SyncQuery,
) -> Result<SyncView, RpcError> {
    let sides = sides(tx, who, &query.from, query.into.as_ref(), false)?;
    let moving = moving(tx, &sides)?;
    let held = held::held(
        tx,
        &sides.into.summary.id,
        sides.facts.repository_id,
        sides.facts.number,
    )?;
    let mut view = branch::sync_view_of(&moving, sides.pr, sides.into, Some(sides.facts.number))?;
    for row in &mut view.rows {
        row.value_set = row.secret && held::is_held(&held, &row.key);
    }
    Ok(view)
}

/// The Environments the standing Conditional Syncs of `event`'s pull request
/// involve: their PR Environments and Destinations.
pub(crate) fn involved(
    tx: &mut dyn Tx,
    who: &Actor,
    event: &PullRequest,
) -> Result<Vec<EnvironmentId>, RpcError> {
    let rows = tx.query(
        "SELECT pr_environment_id, environment_id FROM config_conditional_sync \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3 AND state = 'standing'",
        &[
            who.organization.as_str().into(),
            event.repository_id.into(),
            event.number.into(),
        ],
    )?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.parse(0, "Environment ID")?);
        ids.push(row.parse::<EnvironmentId>(1, "Environment ID")?);
    }
    Ok(ids)
}

/// A Follow staged the Parent's changes in `branch`, whose Working State was at
/// revision `before`: what stood there still stands, as only the author's own
/// edits withdraw a Conditional Sync.
pub(crate) fn followed(
    tx: &mut dyn Tx,
    branch: &EnvironmentSummary,
    before: Revision,
) -> Result<(), RpcError> {
    if branch.revision == before {
        return Ok(());
    }
    tx.execute(
        "UPDATE config_conditional_sync SET working_revision = ?3 \
         WHERE pr_environment_id = ?1 AND state = 'standing' AND working_revision = ?2",
        &[
            branch.id.as_str().into(),
            scope::revision_param(before)?.into(),
            scope::revision_param(branch.revision)?.into(),
        ],
    )?;
    Ok(())
}

/// Withdraw every standing Conditional Sync of `event`'s pull request.
pub(crate) fn withdraw(tx: &mut dyn Tx, who: &Actor, event: &PullRequest) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_conditional_sync \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3 AND state = 'standing'",
        &[
            who.organization.as_str().into(),
            event.repository_id.into(),
            event.number.into(),
        ],
    )?;
    Ok(())
}

/// The pull request closed: each of its Conditional Syncs freezes with the merge
/// commit if it merged and still stands where it is still a Destination, and drops
/// otherwise. Frozen ones land at once where nothing deploys the target branch on
/// push, or where the head Cloud found the merge commit in
/// ([`PullRequest::merge_reached`]) deployed nothing; the next push that contains
/// the merge commit carries the rest. Runs before its PR Environments close.
pub(crate) fn settle(tx: &mut dyn Tx, who: &Actor, event: &PullRequest) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT id, pr_environment_id, environment_id, working_revision, target_branch \
         FROM config_conditional_sync \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3 AND state = 'standing' \
         ORDER BY saved_at, id",
        &[
            who.organization.as_str().into(),
            event.repository_id.into(),
            event.number.into(),
        ],
    )?;
    let mut frozen = Vec::new();
    for row in rows {
        let id = row.parse::<ConditionalSyncId>(0, "Conditional Sync ID")?;
        let pr = row.parse::<EnvironmentId>(1, "Environment ID")?;
        let into = row.parse::<EnvironmentId>(2, "Environment ID")?;
        let environment = scope::lock_id(tx, who, &pr)?;
        let project = scope::project_of(tx, &pr)?.id;
        let stands = match &event.merge_commit {
            Some(_) => {
                u64::try_from(row.int(3)?).ok() == Some(environment.summary.revision.0)
                    && row.text(4)? == event.target_branch.as_str()
                    && !pull_request::closing(tx, &pr)?
                    && pull_request::destinations_of(
                        tx,
                        &project,
                        event.repository_id,
                        &event.target_branch,
                    )?
                    .contains(&into)
            }
            None => false,
        };
        if stands {
            tx.execute(
                "UPDATE config_conditional_sync \
                 SET state = 'frozen', pr_environment_id = NULL, merge_commit = ?2 WHERE id = ?1",
                &[
                    id.as_str().into(),
                    event.merge_commit.as_ref().map(CommitSha::as_str).into(),
                ],
            )?;
            frozen.push((id, into));
        } else {
            delete(tx, &id)?;
        }
    }
    // Values held for a merge that lands nowhere go with it.
    tx.execute(
        "DELETE FROM config_held_secret \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3 \
         AND environment_id NOT IN (SELECT environment_id FROM config_conditional_sync \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3 AND state = 'frozen')",
        &[
            who.organization.as_str().into(),
            event.repository_id.into(),
            event.number.into(),
        ],
    )?;
    for (id, into) in frozen {
        let mut destination = scope::lock_id(tx, who, &into)?;
        if !deploys_on_push(tx, &into, event.repository_id, &event.target_branch)? {
            land(tx, who, &id, &mut destination)?;
            continue;
        }
        let Some(reached) = &event.merge_reached else {
            continue;
        };
        let head = crate::automation::head(
            tx,
            &who.organization,
            event.repository_id,
            &event.target_branch,
        )?;
        if head.as_ref() != Some(reached) {
            // A later push carries it.
            continue;
        }
        // A deploy waiting for CI at that head lands it; else that head deployed
        // nothing here, so it lands now.
        if !wait_with(tx, &into, event, reached, &id)? {
            land(tx, who, &id, &mut destination)?;
        }
    }
    Ok(())
}

/// Attach frozen Conditional Sync `id` to the deploy waiting for CI at `head` in `into`.
fn wait_with(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    event: &PullRequest,
    head: &CommitSha,
    id: &ConditionalSyncId,
) -> Result<bool, RpcError> {
    let key: [crate::storage::Param<'_>; 4] = [
        into.as_str().into(),
        event.repository_id.into(),
        event.target_branch.as_str().into(),
        head.as_str().into(),
    ];
    let rows = tx.query(
        "SELECT saves FROM config_waiting_deploy \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 AND head = ?4",
        &key,
    )?;
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    let mut syncs: Vec<ConditionalSyncId> = row.json(0, "waiting deploy")?;
    syncs.push(id.clone());
    let [environment, repository, branch, head] = key;
    tx.execute(
        "UPDATE config_waiting_deploy SET saves = ?5 \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 AND head = ?4",
        &[
            environment,
            repository,
            branch,
            head,
            serde_json::to_string(&syncs)
                .expect("Conditional Sync IDs are JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(true)
}

/// Whether `environment` deploys `branch` on push: a Service of its latest Saved
/// State follows it with auto-deploy on.
fn deploys_on_push(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> Result<bool, RpcError> {
    let Some(saved) = review::latest_saved(tx, environment)? else {
        return Ok(false);
    };
    for service in &saved.intent.services {
        let follows = crate::git::tracks(service, repository_id, branch);
        if follows && policy::load(tx, environment, &service.id)?.auto_deploy {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The frozen Conditional Syncs a push of `branch` carries, by Destination: those whose merge
/// commit Cloud found in the new head, oldest first.
pub(crate) fn carried(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
    merged: &[CommitSha],
) -> Result<BTreeMap<EnvironmentId, Vec<ConditionalSyncId>>, RpcError> {
    let mut carried: BTreeMap<EnvironmentId, Vec<ConditionalSyncId>> = BTreeMap::new();
    if merged.is_empty() {
        return Ok(carried);
    }
    let rows = tx.query(
        "SELECT id, environment_id, merge_commit FROM config_conditional_sync \
         WHERE organization_id = ?1 AND repository_id = ?2 AND target_branch = ?3 AND state = 'frozen' \
         ORDER BY saved_at, id",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
        ],
    )?;
    for row in rows {
        let commit = row.text(2)?;
        if merged.iter().any(|merged| merged.as_str() == commit) {
            carried
                .entry(row.parse::<EnvironmentId>(1, "Environment ID")?)
                .or_default()
                .push(row.parse(0, "Conditional Sync ID")?);
        }
    }
    Ok(carried)
}

pub(crate) fn pending(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> Result<PendingSyncs, RpcError> {
    let rows = tx.query(
        "SELECT DISTINCT state, number, merge_commit FROM config_conditional_sync \
         WHERE organization_id = ?1 AND repository_id = ?2 AND target_branch = ?3 \
         AND state IN ('standing', 'frozen') ORDER BY state, number, merge_commit",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
        ],
    )?;
    let mut pending = PendingSyncs::default();
    for row in rows {
        match row.variant(0, "Conditional Sync")? {
            ConditionalSyncState::Standing => {
                let number = row.number(1, "Conditional Sync")?;
                if !pending.standing.contains(&number) {
                    pending.standing.push(number);
                }
            }
            ConditionalSyncState::Frozen => {
                let commit = row.parse(2, "Conditional Sync")?;
                if !pending.merged.contains(&commit) {
                    pending.merged.push(commit);
                }
            }
            ConditionalSyncState::Landed => return Err(error::corrupt("Conditional Sync")),
        }
    }
    Ok(pending)
}

/// Land frozen Conditional Sync `id` in `destination`, whose lock the caller holds.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &ConditionalSyncId,
    destination: &mut Environment,
) -> Result<(), RpcError> {
    let Some(found) = load(tx, who, id)? else {
        return Ok(());
    };
    if found.state != ConditionalSyncState::Frozen || found.environment != destination.summary.id {
        return Ok(());
    }
    let mut stored = found.stored;
    let into = &destination.summary.id;
    let held = held::held(tx, into, found.repository, found.number)?;
    held::forget(tx, into, found.repository, found.number)?;
    let Some(latest) = review::latest_saved(tx, into)? else {
        // Nothing saved there to land onto.
        return delete(tx, id);
    };
    let Planned {
        mut saved,
        mut next,
        picks,
        left,
    } = plan(&stored, &latest.intent, &destination.working)?;
    held::fill(&mut saved, &held);
    held::fill(&mut next, &held);

    // Publish, stage, then what's left of the Conditional Sync.
    let (revision, _) = review::publish(tx, who, &destination.summary.id, saved, Some(&latest))?;
    if next != destination.working {
        branch::land(
            tx,
            who,
            destination,
            (&stored.landing.from, &stored.carried),
            next,
            &picks,
        )?;
    }
    tx.execute(
        "DELETE FROM config_conditional_sync \
         WHERE environment_id = ?1 AND state = 'landed' AND id <> ?2",
        &[destination.summary.id.as_str().into(), id.as_str().into()],
    )?;
    if left.is_empty() {
        return delete(tx, id);
    }
    stored.rows = left;
    stored.landed = Some(revision);
    tx.execute(
        "UPDATE config_conditional_sync SET state = 'landed', saved = ?2 WHERE id = ?1",
        &[id.as_str().into(), document(&stored).as_str().into()],
    )?;
    Ok(())
}

/// The pull requests' values landed Conditional Syncs left in `environment`, until
/// its next Saved revision.
pub(crate) fn hints(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<PullRequestHint>, RpcError> {
    let latest = review::latest_saved(tx, environment)?.map(|saved| saved.revision);
    let rows = tx.query(
        "SELECT id, number, saved FROM config_conditional_sync \
         WHERE environment_id = ?1 AND state = 'landed' ORDER BY saved_at, id",
        &[environment.as_str().into()],
    )?;
    let mut hints = Vec::new();
    for row in rows {
        let stored = row.json::<Stored>(2, "Conditional Sync")?;
        if stored.landed != latest {
            continue;
        }
        let number = row.number(1, "Conditional Sync")?;
        for held in stored.rows {
            hints.push(PullRequestHint {
                conditional_sync: row.parse(0, "Conditional Sync ID")?,
                pull_request: number,
                row: held.shown.row,
                value: held.shown.from,
                landed: held.landed.unwrap_or(Landed::Hint),
            });
        }
    }
    Ok(hints)
}

/// PR Environment `pr`'s Conditional Sync into Destination `into`, as the pull
/// request's page shows it.
pub(crate) fn standing_in(
    tx: &mut dyn Tx,
    pr: &Environment,
    into: &Environment,
    target: Option<&BranchName>,
) -> Result<Option<pull_request::DestinationSync>, RpcError> {
    let rows = tx.query(
        "SELECT id, working_revision, target_branch, saved, repository_id, number \
         FROM config_conditional_sync \
         WHERE pr_environment_id = ?1 AND environment_id = ?2 AND state = 'standing'",
        &[
            pr.summary.id.as_str().into(),
            into.summary.id.as_str().into(),
        ],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let branch = row.text(2)?;
    let standing = u64::try_from(row.int(1)?).ok() == Some(pr.summary.revision.0)
        && target.is_some_and(|target| branch == target.as_str());
    let stored = row.json::<Stored>(3, "Conditional Sync")?;
    let held = held::held(
        tx,
        &into.summary.id,
        row.number(4, "Conditional Sync")?,
        row.number(5, "Conditional Sync")?,
    )?;
    Ok(Some(pull_request::DestinationSync {
        id: row.parse(0, "Conditional Sync ID")?,
        standing,
        changes: stored.rows.len(),
        waiting: held::waiting(&stored, &into.working, &held),
    }))
}

/// Core's comparison of the synced side into `into`, with what `into` uses live.
fn against(
    stored: &Stored,
    into: &SavedEnvironmentIntent,
    picks: Option<Vec<String>>,
) -> Result<BranchChanges, RpcError> {
    let landing = &stored.landing;
    Moving::landed(
        &stored.from.id,
        (&landing.from, &landing.base),
        &landing.hostnames,
        into,
    )
    .compare(into, picks)
}

/// `next` with each variable `before` lacked under the ID `saved` gave it (by
/// Service lineage and key), so Saved and Working State agree.
fn with_variable_ids_of(
    saved: &SavedEnvironmentIntent,
    before: &SavedEnvironmentIntent,
    mut next: SavedEnvironmentIntent,
) -> SavedEnvironmentIntent {
    for node in &mut next.services {
        let had: BTreeSet<&str> = before
            .services
            .iter()
            .find(|own| own.id == node.id)
            .map(|own| own.variables.iter().map(|v| v.id.as_str()).collect())
            .unwrap_or_default();
        let Some(in_saved) = saved
            .services
            .iter()
            .find(|own| own.lineage_id == node.lineage_id)
        else {
            continue;
        };
        for variable in &mut node.variables {
            if had.contains(variable.id.as_str()) {
                continue;
            }
            if let Some(own) = in_saved.variables.iter().find(|v| v.key == variable.key) {
                variable.id.clone_from(&own.id);
            }
        }
    }
    next
}

fn load(tx: &mut dyn Tx, who: &Actor, id: &ConditionalSyncId) -> Result<Option<Found>, RpcError> {
    let rows = tx.query(
        "SELECT environment_id, state, repository_id, number, saved FROM config_conditional_sync \
         WHERE id = ?1 AND organization_id = ?2",
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Found {
        environment: row.parse(0, "Environment ID")?,
        state: row.variant(1, "Conditional Sync")?,
        repository: row.number(2, "Conditional Sync")?,
        number: row.number(3, "Conditional Sync")?,
        stored: row.json(4, "Conditional Sync")?,
    }))
}

fn write(tx: &mut dyn Tx, id: &ConditionalSyncId, stored: &Stored) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_conditional_sync SET saved = ?2 WHERE id = ?1",
        &[id.as_str().into(), document(stored).as_str().into()],
    )?;
    Ok(())
}

fn delete(tx: &mut dyn Tx, id: &ConditionalSyncId) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_conditional_sync WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    Ok(())
}

fn document(stored: &Stored) -> String {
    serde_json::to_string(stored).expect("a Conditional Sync is JSON")
}
