//! Conditional Syncs: a PR Environment's Sync into one of its Destinations that
//! goes live with the pull request's merge.
//!
//! Syncing records the picked rows and what landing needs (the PR Environment's
//! side as synced, its secrets' values sealed out), so landing and a later take
//! never read the PR Environment, which may be gone by then. A secret the
//! Destination lacks arrives with the value held for the merge, given with the Sync
//! or by [`HoldSecret`]; the pull request's check waits until it has one. A Conditional
//! Sync stands while the PR Environment's Working State, but for what it follows
//! from its Parent, and the pull request's target branch are what they were; edits
//! in the Destination never withdraw it. When the pull request closes it freezes
//! with the merge commit if it merged and still stands, else it drops. A frozen
//! one lands with the first push to the target branch whose head contains the
//! merge commit (Cloud observes the ancestry): before the Deployment that push
//! admits in its Destination, with the deploy that push waits for CI with, or at
//! once where the push deploys nothing. A Destination that doesn't deploy the
//! branch on push gets it at the merge.

mod held;
mod syncing;
pub use held::{HoldSecret, SecretHeld};
pub(crate) use held::{held, hold};
pub(crate) use syncing::*;

use crate::id::{BranchName, CommitSha, PullRequestNumber, RepositoryId};
use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{
    Applied, Arrives, Cell, Hostnames, Plan, PlannedRow, Policy as Rules, RowId,
    SavedEnvironmentIntent, Sides, Verdict, Way, cell_at, plan,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::branch::{self, Carried, NamedRow};
use crate::id::{ConditionalSyncId, EnvironmentId, Revision};
use crate::pull_request::{self, PullRequest, PullRequestRef};
use crate::scope::{self, Environment, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, error, policy, review};

/// A Conditional Sync, as a Sync or a take answers it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConditionalSync {
    /// Pass to [`crate::Take::from`] to use a hint it left.
    pub id: ConditionalSyncId,
    pub pull_request: PullRequestNumber,
    /// The rows it holds.
    pub rows: Vec<NamedRow>,
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
    /// The Conditional Sync: pass to [`crate::Take::from`].
    pub conditional_sync: ConditionalSyncId,
    pub pull_request: PullRequestNumber,
    /// What [`crate::Take::rows`] names.
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
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

/// A Conditional Sync as stored: everything landing needs, so it never reads the
/// PR Environment, which may be gone by then.
#[derive(Serialize, Deserialize)]
struct Stored {
    picks: Vec<Pick>,
    /// The PR Environment's Working State as synced, its secrets' values sealed out.
    from: SavedEnvironmentIntent,
    /// What it and the Destination last shared then.
    base: SavedEnvironmentIntent,
    hostnames: Hostnames,
    /// What its Services carry: registry credentials and Deployment Policies.
    carried: Carried,
    /// The PR Environment.
    environment: EnvironmentSummary,
    /// The Destination's Saved revision landing published.
    #[serde(default)]
    landed: Option<Revision>,
}

/// A picked row.
#[derive(Clone, Serialize, Deserialize)]
struct Pick {
    row: RowId,
    /// The Destination's cell the Sync was reviewed against.
    reviewed: Cell,
    at: NamedRow,
    /// The pull request's value, as the Sync showed it.
    from: Value,
    /// The cell landing or a take staged in the Destination's Working State. The
    /// pick reads as staged only while Working State still holds it, so a discard
    /// offers it again.
    #[serde(default)]
    staged: Option<Cell>,
}

impl Pick {
    /// `suffix` is the Destination's generated-address suffix.
    fn landed(&self, working: &SavedEnvironmentIntent, suffix: &str) -> Landed {
        match self.staged.as_ref() == Some(&cell_at(working, &self.row, suffix)) {
            true => Landed::Staged,
            false => Landed::Hint,
        }
    }
}

/// The cells `applied` staged, by row.
fn staged_cells(applied: &Applied) -> BTreeMap<RowId, Cell> {
    applied
        .landed
        .iter()
        .map(|landed| (landed.row.clone(), landed.value.clone()))
        .collect()
}

struct Found {
    environment: EnvironmentId,
    state: ConditionalSyncState,
    pr: PullRequestRef,
    stored: Stored,
}

/// What a Conditional Sync lands in one State of its Destination.
struct Admitted {
    /// Its picks that still move there.
    moves: BTreeSet<RowId>,
    picks: BTreeSet<RowId>,
    applied: Applied,
}

/// Land the picks of `stored` that still move into `into` and that `keep` keeps,
/// with the secret values `held` for the merge. The only way a Conditional Sync's
/// rows reach a Destination: its check, landing and a take.
fn admit(
    stored: &Stored,
    into: &SavedEnvironmentIntent,
    marks: &BTreeSet<RowId>,
    held: &BTreeMap<RowId, Cell>,
    keep: impl Fn(&PlannedRow) -> bool,
) -> Result<Admitted, RpcError> {
    let none = BTreeSet::new();
    let live = branch::used_live(into).into_keys().collect();
    let plan: Plan = plan(
        Sides {
            base: Some(&stored.base),
            from: &stored.from,
            into,
            hostnames: stored.hostnames.clone(),
        },
        Rules {
            way: Way::Sync,
            from_marks: &none,
            into_marks: marks,
            live: &live,
            own: None,
        },
    );
    let moves: BTreeSet<RowId> = plan
        .rows()
        .iter()
        .filter(|row| matches!(row.verdict, Verdict::Moves { .. }))
        .filter(|row| stored.picks.iter().any(|pick| pick.row == row.id))
        .map(|row| row.id.clone())
        .collect();
    let kept = plan
        .rows()
        .iter()
        .filter(|row| moves.contains(&row.id) && keep(row))
        .map(|row| row.id.clone())
        .collect();
    let picks = branch::whole(plan.rows(), kept);
    let applied = plan.apply(&picks, held).map_err(branch::config)?;
    Ok(Admitted {
        moves,
        picks,
        applied,
    })
}

/// What `stored` lands in the Destination's Saved State `saved`: each pick it still
/// holds as the Sync was reviewed against, but a secret that needs a value only
/// where Working State `working` holds that too, so a held value never replaces one
/// the Destination staged itself. Landing and the check's `waiting` both read it.
fn admitted(
    stored: &Stored,
    (saved, working): (&SavedEnvironmentIntent, &SavedEnvironmentIntent),
    marks: &BTreeSet<RowId>,
    held: &BTreeMap<RowId, Cell>,
) -> Result<Admitted, RpcError> {
    admit(stored, saved, marks, held, |row| {
        let needs_value = matches!(
            row.verdict,
            Verdict::Moves {
                arrives: Arrives::NeedsValue,
                ..
            }
        );
        stored
            .picks
            .iter()
            .any(|pick| pick.row == row.id && pick.reviewed == row.into)
            && (!needs_value || cell_at(working, &row.id, &stored.hostnames.into) == row.into)
    })
}

/// The rows `into` marks Never sync.
fn marked(tx: &mut dyn Tx, into: &EnvironmentId) -> Result<BTreeSet<RowId>, RpcError> {
    Ok(branch::marks(tx, into, into)?.1)
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
///
/// Per picked row: the Destination's Saved State still holds what was reviewed →
/// saved, and staged too unless Working State has an edit of its own; it changed it
/// live but Working State has no edit of it → staged, `staged`; both → neither, the
/// pull request's value a `hint` a take stages. Landed rows stay marked until the
/// Destination's next Saved revision.
///
/// It writes the Destination alone, from the snapshot the Sync froze, under the
/// lock its caller took in ID order; so no Project lock, which taken here would
/// come after that lock and deadlock against a Sync. Nor arrivals: it never
/// advances a pair's base, so there is none to rewind; its picks say what it staged.
// ponytail: UndoSync of a landed one refuses; discard its staged rows instead.
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
    let into = destination.summary.id.clone();
    let held = held::held(tx, &into, &found.pr)?;
    held::forget(tx, &into, &found.pr)?;
    let Some(latest) = review::latest_saved(tx, &into)? else {
        // Nothing saved there to land onto.
        return delete(tx, id);
    };
    let marks = marked(tx, &into)?;
    let saved = admitted(
        &stored,
        (&latest.intent, &destination.working),
        &marks,
        &held,
    )?;

    // Working: the nodes arriving as Saved has them, so their ids match; then each
    // row Working State holds as Saved did, with the variables Saved gained under
    // Saved's ids.
    let introduced: BTreeSet<&str> = stored
        .picks
        .iter()
        .filter(|pick| pick.row.at() == "node")
        .map(|pick| pick.row.lineage())
        .collect();
    let arriving: BTreeSet<&str> = saved
        .picks
        .iter()
        .filter(|pick| pick.at() == "node")
        .map(RowId::lineage)
        .collect();
    let mut working = destination.working.clone();
    let next = &saved.applied.next;
    working.services.extend(
        next.services
            .iter()
            .filter(|node| arriving.contains(node.lineage_id.as_str()))
            .cloned(),
    );
    working.volumes.extend(
        next.volumes
            .iter()
            .filter(|node| arriving.contains(node.resource_lineage_id.as_str()))
            .cloned(),
    );
    let staged = admit(&stored, &working, &marks, &held, |row| {
        !introduced.contains(row.id.lineage())
            && row.into == cell_at(&latest.intent, &row.id, &stored.hostnames.into)
    })?;
    let cells = staged_cells(&staged.applied);
    let next = with_variable_ids_of(next, &working, staged.applied.next);

    let (revision, _) = review::publish(tx, who, &into, saved.applied.next, Some(&latest))?;
    if next != destination.working {
        branch::land(
            tx,
            who,
            destination,
            (&stored.from, &stored.carried),
            next,
            &staged.picks,
        )?;
    }
    tx.execute(
        "DELETE FROM config_conditional_sync \
         WHERE environment_id = ?1 AND state = 'landed' AND id <> ?2",
        &[into.as_str().into(), id.as_str().into()],
    )?;
    stored.picks.retain_mut(|pick| {
        let moves = saved.moves.contains(&pick.row) || staged.moves.contains(&pick.row);
        let left = moves && !saved.picks.contains(&pick.row);
        pick.staged = cells.get(&pick.row).cloned();
        left
    });
    if stored.picks.is_empty() {
        return delete(tx, id);
    }
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
    environment: &Environment,
) -> Result<Vec<PullRequestHint>, RpcError> {
    let id = &environment.summary.id;
    let latest = review::latest_saved(tx, id)?.map(|saved| saved.revision);
    let rows = tx.query(
        "SELECT id, number, saved FROM config_conditional_sync \
         WHERE environment_id = ?1 AND state = 'landed' ORDER BY saved_at, id",
        &[id.as_str().into()],
    )?;
    let mut hints = Vec::new();
    for row in rows {
        let stored = row.json::<Stored>(2, "Conditional Sync")?;
        if stored.landed != latest {
            continue;
        }
        let number = row.number(1, "Conditional Sync")?;
        for pick in stored.picks {
            hints.push(PullRequestHint {
                landed: pick.landed(&environment.working, &stored.hostnames.into),
                conditional_sync: row.parse(0, "Conditional Sync ID")?,
                pull_request: number,
                at: pick.at,
                value: pick.from,
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
        &PullRequestRef {
            repository_id: row.number(4, "Conditional Sync")?,
            number: row.number(5, "Conditional Sync")?,
        },
    )?;
    let marks = marked(tx, &into.summary.id)?;
    let waiting = match review::latest_saved(tx, &into.summary.id)? {
        Some(latest) => admitted(&stored, (&latest.intent, &into.working), &marks, &held)?
            .applied
            .waiting
            .iter()
            .filter_map(|row| stored.picks.iter().find(|pick| pick.row == *row))
            .map(|pick| pick.at.label())
            .collect(),
        // Nothing saved there to land onto.
        None => Vec::new(),
    };
    Ok(Some(pull_request::DestinationSync {
        id: row.parse(0, "Conditional Sync ID")?,
        standing,
        changes: stored.picks.len(),
        waiting,
    }))
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
        pr: PullRequestRef {
            repository_id: row.number(2, "Conditional Sync")?,
            number: row.number(3, "Conditional Sync")?,
        },
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
