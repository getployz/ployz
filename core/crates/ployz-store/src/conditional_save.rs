//! Conditional Saves: a PR Environment's Save into one of its Destinations that
//! goes live with the pull request's merge.
//!
//! Saving records the picked rows, core's picks and what landing needs (the PR
//! Environment's side as saved, sealed values included), so landing and a later
//! take never read the PR Environment, which may be gone by then. A save stands
//! while the PR Environment's Working State and the pull request's target branch
//! are what they were; edits in the Destination never withdraw it. When the pull
//! request closes it freezes with the merge commit if it merged and still stands,
//! else it drops. A frozen save lands with the first push to the target branch
//! whose head contains the merge commit (Cloud observes the ancestry): before the
//! Deployment that push admits in its Destination, with the deploy that push waits
//! for CI with, or at once where the push deploys nothing. A Destination that
//! doesn't deploy the branch on push saves it at the merge.
//!
//! Landing, per picked row: the Destination left it alone, or has an undeployed
//! edit of it → saved, the edit on top; it changed it live (neither its Saved nor
//! its Working State holds the value saved against) → not saved but staged,
//! `staged`; both → not saved, the pull request's value only a `hint` a take
//! stages. Landed rows stay marked until the Destination's next Saved revision.

use crate::id::{BranchName, CommitSha, PullRequestNumber, RepositoryId};
use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{
    BranchChanges, BranchHostnames, BranchPick, BranchRole, SavedEnvironmentIntent,
    SavedVariableIntent, SavedVariableValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::branch::{self, Carried, MoveRow, MoveView, Moved, Moving, Save, Take, Way, When};
use crate::id::{ConditionalSaveId, EnvironmentId, Revision};
use crate::pull_request::{self, PullRequest, PullRequestRef};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, deployment, error, policy, review, teardown};

/// A Conditional Save, as a Move answers it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConditionalSave {
    /// Pass to [`Take::from`] to use a hint it left.
    pub id: ConditionalSaveId,
    pub pull_request: PullRequestNumber,
    /// The rows it holds.
    pub rows: Vec<String>,
    pub state: SaveState,
}

/// Where a Conditional Save is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SaveState {
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

/// A pull request's value a landed Conditional Save left in an Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PullRequestHint {
    /// The Conditional Save: pass to [`Take::from`].
    pub save: ConditionalSaveId,
    pub pull_request: PullRequestNumber,
    /// `NODE.path`, as a Move names it.
    pub row: String,
    /// The pull request's value; secrets read `{"secret": true}`.
    pub value: Value,
    pub landed: Landed,
}

/// What Cloud checks before telling the Store about a push to a branch.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PendingSaves {
    /// Pull requests into the branch with a Conditional Save standing: Cloud reports
    /// each that merged first, so its saves freeze.
    pub standing: Vec<PullRequestNumber>,
    /// Merge commits of frozen ones: Cloud reports which the new head contains
    /// ([`crate::BranchHead::merged`]).
    pub merged: Vec<CommitSha>,
}

/// A Conditional Save as stored.
#[derive(Serialize, Deserialize)]
struct Stored {
    rows: Vec<Row>,
    /// Core's picks, sealed values included.
    picks: Vec<BranchPick>,
    /// The PR Environment's side as saved.
    landing: Landing,
    /// What its Services carry: registry credentials and Deployment Policies.
    carried: Carried,
    /// The PR Environment.
    from: EnvironmentSummary,
    /// The Destination's Saved revision landing published.
    #[serde(default)]
    landed: Option<Revision>,
}

/// What landing compares: the PR Environment's Working State over its base, with
/// its Parent's deployed values on offer.
#[derive(Serialize, Deserialize)]
struct Landing {
    from: SavedEnvironmentIntent,
    base: SavedEnvironmentIntent,
    hostnames: BranchHostnames,
    parent: Option<SavedEnvironmentIntent>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Row {
    /// Core's row key.
    key: String,
    /// The Destination's value the save was reviewed against (core's, redacted).
    into: Value,
    shown: MoveRow,
    #[serde(default)]
    landed: Option<Landed>,
    /// For a secret the pull request changed that the Destination holds too: the
    /// pull request's variable, sealed. Core never moves a secret over one the
    /// receiver has, so it only ever lands as a hint a take stages.
    #[serde(default)]
    secret: Option<SavedVariableIntent>,
}

struct Found {
    environment: EnvironmentId,
    state: SaveState,
    number: PullRequestNumber,
    stored: Stored,
}

/// Whether a Save is a Conditional Save: asked `at_merge`, or from a PR Environment
/// with `when` omitted. A PR Environment never saves now.
pub(crate) fn at_merge(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &EnvironmentRef,
    when: Option<When>,
) -> Result<bool, RpcError> {
    if when == Some(When::AtMerge) {
        return Ok(true);
    }
    let from = scope::environment(tx, who, from)?;
    let pr = pull_request::of(tx, &from.summary.id)?.is_some();
    if pr && when == Some(When::Now) {
        return Err(error::invalid(
            format!(
                "{} is a PR Environment: its changes go live with its merge (when at_merge)",
                from.summary.name
            ),
            json!({}),
        ));
    }
    Ok(pr)
}

/// A Conditional Save's sides: the PR Environment and the Destination.
struct Sides {
    pr: Environment,
    into: Environment,
    facts: PullRequest,
    row: branch::Row,
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
                "{} is not a PR Environment: its changes save now",
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
                        "Nothing deploys {} now: there is nowhere to save into",
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
                "That Environment doesn't deploy {}: save into one that does",
                facts.target_branch
            ),
            json!({ "valid_children": names }),
        ));
    }
    let (pr, into) = scope::load_pair(tx, who, (&pr.summary.id, &into), lock)?;
    let row = branch::row(tx, &pr.summary.id)?.ok_or_else(|| error::corrupt("Branch"))?;
    Ok(Sides {
        pr,
        into,
        facts,
        row,
    })
}

/// The PR Environment's Working State into the Destination's over its base, with
/// its Parent's deployed values on offer, as the review shows it.
fn moving(tx: &mut dyn Tx, sides: &Sides) -> Result<Moving, RpcError> {
    let parent = scope::load_by_id(tx, &sides.row.parent)?;
    let applied = deployment::head(tx, &parent)?.applied;
    Moving::save(tx, &sides.pr, &sides.into, &sides.row, applied)
}

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &EnvironmentRef,
    into: Option<&EnvironmentRef>,
) -> Result<MoveView, RpcError> {
    let sides = sides(tx, who, from, into, false)?;
    let moving = moving(tx, &sides)?;
    let changes = moving.compare(&sides.into.working, None)?;
    let rows = changes
        .rows
        .iter()
        .filter_map(|row| branch::move_row(&moving, &sides.pr, &sides.into, row))
        .collect();
    Ok(MoveView {
        version: branch::version(&sides.into, &changes.review),
        from: sides.pr.summary,
        into: sides.into.summary,
        rows,
    })
}

/// Save a PR Environment's picked changes for one Destination, replacing its save
/// there; `picks: []` withdraws it.
pub(crate) fn save(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &crate::SealingKey,
    request: &Save,
) -> Result<Moved, RpcError> {
    let sides = sides(tx, who, &request.from, request.into.as_ref(), true)?;
    let checks = vec![PullRequestRef {
        repository_id: sides.facts.repository_id,
        number: sides.facts.number,
    }];
    let withdraw = request.picks.as_ref().is_some_and(Vec::is_empty);
    let replace = |tx: &mut dyn Tx| {
        tx.execute(
            "DELETE FROM config_conditional_save \
             WHERE pr_environment_id = ?1 AND environment_id = ?2 AND state = 'standing'",
            &[
                sides.pr.summary.id.as_str().into(),
                sides.into.summary.id.as_str().into(),
            ],
        )
    };
    if withdraw {
        replace(tx)?;
        return Ok(Moved {
            branch: Some(branch::view(tx, &sides.pr)?),
            from: sides.pr.summary,
            into: sides.into.summary,
            staged: Vec::new(),
            conditional_save: None,
            checks,
        });
    }
    if !sides.facts.open {
        return Err(error::conflict(
            format!("PR #{} is closed", sides.facts.number),
            json!({}),
        ));
    }
    if pull_request::closing(tx, &sides.pr.summary.id)? {
        return Err(error::conflict(
            format!("{} is closing", sides.pr.summary.name),
            json!({}),
        ));
    }
    if let Some(removal) = teardown::removing(tx, &sides.pr.summary.id)? {
        return Err(teardown::being_removed(&sides.pr, &removal));
    }
    let moving = moving(tx, &sides)?;
    let changes = moving.compare(&sides.into.working, None)?;
    let version = branch::version(&sides.into, &changes.review);
    if request
        .version
        .as_ref()
        .is_some_and(|asked| *asked != version)
    {
        return Err(error::conflict(
            "Changed since you reviewed: review the move again",
            json!({ "version": version }),
        ));
    }
    let picks = branch::picks(
        &moving,
        &sides.into,
        sealing,
        &changes.rows,
        request.picks.as_deref(),
    )?;
    // Core refuses picks it couldn't land, such as a new Service's variable without it.
    moving.compare(&sides.into.working, Some(picks.clone()))?;
    let picked: BTreeSet<String> = picks.iter().map(|pick| pick.key.clone()).collect();
    let mut rows: Vec<Row> = changes
        .rows
        .iter()
        .filter(|row| picked.contains(&row.key.to_string()))
        .filter_map(|row| {
            Some(Row {
                key: row.key.to_string(),
                into: row.into.clone(),
                shown: branch::move_row(&moving, &sides.pr, &sides.into, row)?,
                landed: None,
                secret: None,
            })
        })
        .collect();
    rows.extend(secret_hints(&moving, &sides.into));
    let names = rows.iter().map(|row| row.shown.row.clone()).collect();
    let Way::Save { parent, .. } = moving.way else {
        return Err(error::internal("A Conditional Save moves a Save"));
    };
    let stored = Stored {
        rows,
        picks,
        carried: Carried::of(tx, &sides.pr.summary.id, &moving.from)?,
        landing: Landing {
            from: moving.from,
            base: moving.base,
            hostnames: moving.hostnames,
            parent,
        },
        from: sides.pr.summary.clone(),
        landed: None,
    };
    replace(tx)?;
    let id = ConditionalSaveId::parse(uuid::Uuid::new_v4().to_string())?;
    tx.execute(
        "INSERT INTO config_conditional_save (id, organization_id, environment_id, state, \
         pr_environment_id, repository_id, number, target_branch, working_revision, merge_commit, \
         saved_at, saved) VALUES (?1, ?2, ?3, 'standing', ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            sides.into.summary.id.as_str().into(),
            sides.pr.summary.id.as_str().into(),
            sides.facts.repository_id.into(),
            sides.facts.number.into(),
            sides.facts.target_branch.as_str().into(),
            scope::revision_param(sides.pr.summary.revision)?.into(),
            deployment::now().into(),
            document(&stored).as_str().into(),
        ],
    )?;
    Ok(Moved {
        branch: Some(branch::view(tx, &sides.pr)?),
        from: sides.pr.summary,
        into: sides.into.summary,
        staged: Vec::new(),
        conditional_save: Some(ConditionalSave {
            id,
            pull_request: sides.facts.number,
            rows: names,
            state: SaveState::Standing,
        }),
        checks,
    })
}

/// The secrets the PR Environment changed that the Destination holds too: core
/// moves none of them, so each is kept, sealed, to land as a hint.
fn secret_hints(moving: &Moving, into: &Environment) -> Vec<Row> {
    let secret = |variable: &&SavedVariableIntent| {
        matches!(variable.value, SavedVariableValue::Secret { .. })
    };
    let mut rows = Vec::new();
    for service in &moving.from.services {
        let lineage = &service.lineage_id;
        let Some(theirs) = into
            .working
            .services
            .iter()
            .find(|own| own.lineage_id == *lineage)
        else {
            continue;
        };
        let base = moving
            .base
            .services
            .iter()
            .find(|own| own.lineage_id == *lineage);
        for variable in service.variables.iter().filter(secret) {
            let fingerprint = |service: &ployz_core::config::SavedServiceIntent| {
                service
                    .variables
                    .iter()
                    .find(|own| own.key == variable.key)
                    .map(|own| own.value_fingerprint.clone())
            };
            let Some(held) = fingerprint(theirs) else {
                continue;
            };
            if held == variable.value_fingerprint
                || base.and_then(fingerprint).as_ref() == Some(&variable.value_fingerprint)
            {
                continue;
            }
            let key = format!("{lineage}:variables.{}", variable.key);
            rows.push(Row {
                shown: MoveRow {
                    row: moving.name(&into.working, &key),
                    conflict: true,
                    choice: None,
                    from: json!({ "secret": true }),
                    into: json!({ "secret": true }),
                },
                key,
                into: Value::Null,
                landed: None,
                secret: Some(variable.clone()),
            });
        }
    }
    rows
}

/// Stage the picked hints (omitted: every one) of a landed Conditional Save in its
/// Destination: the pull request's value replaces the Destination's own edit.
pub(crate) fn take(tx: &mut dyn Tx, who: &Actor, take: &Take) -> Result<Moved, RpcError> {
    let id = &take.from;
    let missing = || error::not_found(format!("No Conditional Save {id}"), json!({}));
    let found = load(tx, who, id)?.ok_or_else(missing)?;
    let mut into = scope::lock_id(tx, who, &found.environment)?;
    if let Some(at) = &take.into
        && scope::environment(tx, who, at)?.summary.id != into.summary.id
    {
        return Err(error::invalid(
            format!("Conditional Save {id} landed in {}", into.summary.name),
            json!({}),
        ));
    }
    // Read again under the Destination's lock.
    let found = load(tx, who, id)?.ok_or_else(missing)?;
    if take.version.is_some() {
        review::check(&review::review(tx, &into)?, take.version.as_deref())?;
    }
    let latest = review::latest_saved(tx, &into.summary.id)?.map(|saved| saved.revision);
    let gone = || {
        error::conflict(
            "That value isn't there to use any more: read the diff again",
            json!({}),
        )
    };
    let mut stored = found.stored;
    if found.state != SaveState::Landed || stored.landed != latest {
        return Err(gone());
    }
    let hints: Vec<&Row> = stored
        .rows
        .iter()
        .filter(|row| row.landed == Some(Landed::Hint))
        .collect();
    let mut chosen = BTreeSet::new();
    match &take.rows {
        None => chosen.extend(hints.iter().map(|row| row.key.clone())),
        Some(asked) => {
            for asked in asked {
                let found: Vec<&&Row> = hints
                    .iter()
                    .filter(|row| branch::under(&row.shown.row, asked))
                    .collect();
                if found.is_empty() {
                    let names = hints.iter().map(|row| row.shown.row.as_str());
                    return Err(error::choices(
                        format!("No hint named {asked} to take"),
                        asked,
                        names,
                    ));
                }
                chosen.extend(found.into_iter().map(|row| row.key.clone()));
            }
        }
    }
    if chosen.is_empty() {
        return Err(gone());
    }
    let picks: Vec<BranchPick> = stored
        .picks
        .iter()
        .filter(|pick| chosen.contains(&pick.key))
        .cloned()
        .collect();
    let mut next = match picks.is_empty() {
        true => into.working.clone(),
        false => against(&stored, &into.working, Some(picks.clone()))?.next,
    };
    for row in stored.rows.iter().filter(|row| chosen.contains(&row.key)) {
        if let Some(secret) = &row.secret
            && !put_secret(&mut next, &row.key, secret)
        {
            return Err(gone());
        }
    }
    let staged = branch::land(
        tx,
        who,
        &mut into,
        (&stored.landing.from, &stored.carried),
        next,
        &picks,
    )?;
    for row in &mut stored.rows {
        if chosen.contains(&row.key) {
            row.landed = Some(Landed::Staged);
        }
    }
    write(tx, id, &stored)?;
    Ok(Moved {
        from: stored.from.clone(),
        into: into.summary,
        staged,
        branch: None,
        conditional_save: Some(ConditionalSave {
            id: id.clone(),
            pull_request: found.number,
            rows: stored
                .rows
                .iter()
                .map(|row| row.shown.row.clone())
                .collect(),
            state: SaveState::Landed,
        }),
        checks: Vec::new(),
    })
}

/// Give the Service of `key`'s lineage in `intent` the pull request's sealed secret,
/// keeping the variable's identity. False when that Service or variable is gone.
fn put_secret(
    intent: &mut SavedEnvironmentIntent,
    key: &str,
    secret: &SavedVariableIntent,
) -> bool {
    let lineage = key.split_once(':').map_or(key, |(lineage, _)| lineage);
    let variable = intent
        .services
        .iter_mut()
        .find(|service| service.lineage_id == lineage)
        .and_then(|service| {
            service
                .variables
                .iter_mut()
                .find(|variable| variable.key == secret.key)
        });
    let Some(variable) = variable else {
        return false;
    };
    variable.value = secret.value.clone();
    variable
        .value_fingerprint
        .clone_from(&secret.value_fingerprint);
    true
}

/// The Environments the standing Conditional Saves of `event`'s pull request
/// involve: their PR Environments and Destinations.
pub(crate) fn involved(
    tx: &mut dyn Tx,
    who: &Actor,
    event: &PullRequest,
) -> Result<Vec<EnvironmentId>, RpcError> {
    let rows = tx.query(
        "SELECT pr_environment_id, environment_id FROM config_conditional_save \
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

/// Withdraw every standing Conditional Save of `event`'s pull request.
pub(crate) fn withdraw(tx: &mut dyn Tx, who: &Actor, event: &PullRequest) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_conditional_save \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3 AND state = 'standing'",
        &[
            who.organization.as_str().into(),
            event.repository_id.into(),
            event.number.into(),
        ],
    )?;
    Ok(())
}

/// The pull request closed: each of its Conditional Saves freezes with the merge
/// commit if it merged and still stands where it is still a Destination, and drops
/// otherwise. Frozen ones land at once where nothing deploys the target branch on
/// push, or where the head Cloud found the merge commit in
/// ([`PullRequest::merge_reached`]) deployed nothing; the next push that contains
/// the merge commit carries the rest. Runs before its PR Environments close.
pub(crate) fn settle(tx: &mut dyn Tx, who: &Actor, event: &PullRequest) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT id, pr_environment_id, environment_id, working_revision, target_branch \
         FROM config_conditional_save \
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
        let id = row.parse::<ConditionalSaveId>(0, "Conditional Save ID")?;
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
                "UPDATE config_conditional_save \
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
        // nothing here, so it saves now.
        if !wait_with(tx, &into, event, reached, &id)? {
            land(tx, who, &id, &mut destination)?;
        }
    }
    Ok(())
}

/// Attach frozen save `id` to the deploy waiting for CI at `head` in `into`.
fn wait_with(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    event: &PullRequest,
    head: &CommitSha,
    id: &ConditionalSaveId,
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
    let mut saves: Vec<ConditionalSaveId> = row.json(0, "waiting deploy")?;
    saves.push(id.clone());
    let [environment, repository, branch, head] = key;
    tx.execute(
        "UPDATE config_waiting_deploy SET saves = ?5 \
         WHERE environment_id = ?1 AND repository_id = ?2 AND branch = ?3 AND head = ?4",
        &[
            environment,
            repository,
            branch,
            head,
            serde_json::to_string(&saves).expect("JSON").as_str().into(),
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

/// The frozen saves a push of `branch` carries, by Destination: those whose merge
/// commit Cloud found in the new head, oldest first.
pub(crate) fn carried(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
    merged: &[CommitSha],
) -> Result<BTreeMap<EnvironmentId, Vec<ConditionalSaveId>>, RpcError> {
    let mut carried: BTreeMap<EnvironmentId, Vec<ConditionalSaveId>> = BTreeMap::new();
    if merged.is_empty() {
        return Ok(carried);
    }
    let rows = tx.query(
        "SELECT id, environment_id, merge_commit FROM config_conditional_save \
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
                .push(row.parse(0, "Conditional Save ID")?);
        }
    }
    Ok(carried)
}

pub(crate) fn pending(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> Result<PendingSaves, RpcError> {
    let rows = tx.query(
        "SELECT DISTINCT state, number, merge_commit FROM config_conditional_save \
         WHERE organization_id = ?1 AND repository_id = ?2 AND target_branch = ?3 \
         AND state IN ('standing', 'frozen') ORDER BY state, number, merge_commit",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            branch.as_str().into(),
        ],
    )?;
    let mut pending = PendingSaves::default();
    for row in rows {
        match row.text(0)? {
            "standing" => {
                let number = row.number(1, "Conditional Save")?;
                if !pending.standing.contains(&number) {
                    pending.standing.push(number);
                }
            }
            _ => {
                let commit = row.parse(2, "Conditional Save")?;
                if !pending.merged.contains(&commit) {
                    pending.merged.push(commit);
                }
            }
        }
    }
    Ok(pending)
}

/// Land frozen save `id` in `destination`, whose lock the caller holds.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &ConditionalSaveId,
    destination: &mut Environment,
) -> Result<(), RpcError> {
    let Some(found) = load(tx, who, id)? else {
        return Ok(());
    };
    if found.state != SaveState::Frozen || found.environment != destination.summary.id {
        return Ok(());
    }
    let mut stored = found.stored;
    let Some(latest) = review::latest_saved(tx, &destination.summary.id)? else {
        // Nothing saved there to land onto.
        return delete(tx, id);
    };
    let values_in = |into: &SavedEnvironmentIntent| -> Result<BTreeMap<String, Value>, RpcError> {
        Ok(against(&stored, into, None)?
            .rows
            .into_iter()
            .filter(|row| matches!(row.role, BranchRole::Move { .. }))
            .map(|row| (row.key.to_string(), row.into))
            .collect())
    };
    let in_saved = values_in(&latest.intent)?;
    let in_working = values_in(&destination.working)?;
    let reviewed: BTreeMap<&str, &Value> = stored
        .rows
        .iter()
        .map(|row| (row.key.as_str(), &row.into))
        .collect();
    let same = |left: Option<&Value>, right: Option<&Value>| {
        left.unwrap_or(&Value::Null) == right.unwrap_or(&Value::Null)
    };
    let unchanged = |key: &str| {
        let reviewed = reviewed.get(key).copied();
        same(in_saved.get(key), reviewed) || same(in_working.get(key), reviewed)
    };
    let lineage = |key: &str| {
        key.split_once(':')
            .map_or(key, |(lineage, _)| lineage)
            .to_owned()
    };
    let nodes = |picks: &[&BranchPick]| -> BTreeSet<String> {
        picks
            .iter()
            .filter(|pick| pick.key.ends_with(":node"))
            .map(|pick| lineage(&pick.key))
            .collect()
    };
    let all: Vec<&BranchPick> = stored.picks.iter().collect();
    let introduced = nodes(&all);

    // 1. Saved. A node the pull request introduced arrives with its variables, or not at all.
    let to_saved: Vec<&BranchPick> = all
        .iter()
        .copied()
        .filter(|pick| in_saved.contains_key(&pick.key) && unchanged(&pick.key))
        .collect();
    let arriving = nodes(&to_saved);
    let saved_picks: Vec<BranchPick> = to_saved
        .into_iter()
        .filter(|pick| {
            let lineage = lineage(&pick.key);
            !introduced.contains(&lineage) || arriving.contains(&lineage)
        })
        .cloned()
        .collect();
    let saved = match saved_picks.is_empty() {
        true => latest.intent.clone(),
        false => against(&stored, &latest.intent, Some(saved_picks.clone()))?.next,
    };

    // 2. Working: arriving nodes as Saved has them, so their ids match; then the rest,
    // where the Destination has no staged edit of its own, with the variables Saved
    // gained under Saved's ids.
    let mut into = destination.working.clone();
    into.services.extend(
        saved
            .services
            .iter()
            .filter(|node| arriving.contains(&node.lineage_id))
            .cloned(),
    );
    into.volumes.extend(
        saved
            .volumes
            .iter()
            .filter(|node| arriving.contains(&node.resource_lineage_id))
            .cloned(),
    );
    let working_picks: Vec<BranchPick> = all
        .iter()
        .filter(|pick| {
            !introduced.contains(&lineage(&pick.key))
                && in_working.contains_key(&pick.key)
                && same(in_saved.get(&pick.key), in_working.get(&pick.key))
        })
        .map(|pick| (*pick).clone())
        .collect();
    let next = match working_picks.is_empty() {
        true => into,
        false => {
            let next = against(&stored, &into, Some(working_picks.clone()))?.next;
            with_variable_ids_of(&saved, &into, next)
        }
    };

    // 3. Publish, stage, then what's left of the save.
    let (revision, _) = review::publish(tx, who, &destination.summary.id, saved, Some(&latest))?;
    if next != destination.working {
        let picks: Vec<BranchPick> = saved_picks.iter().chain(&working_picks).cloned().collect();
        branch::land(
            tx,
            who,
            destination,
            (&stored.landing.from, &stored.carried),
            next,
            &picks,
        )?;
    }
    let staged: BTreeSet<&str> = working_picks.iter().map(|pick| pick.key.as_str()).collect();
    // A secret the Destination holds too only ever lands as a hint.
    let left: Vec<Row> = stored
        .rows
        .iter()
        .filter(|row| {
            row.secret.is_some()
                || (in_saved.contains_key(&row.key) || in_working.contains_key(&row.key))
                    && !unchanged(&row.key)
        })
        .map(|row| Row {
            landed: Some(match staged.contains(row.key.as_str()) {
                true => Landed::Staged,
                false => Landed::Hint,
            }),
            ..row.clone()
        })
        .collect();
    tx.execute(
        "DELETE FROM config_conditional_save \
         WHERE environment_id = ?1 AND state = 'landed' AND id <> ?2",
        &[destination.summary.id.as_str().into(), id.as_str().into()],
    )?;
    if left.is_empty() {
        return delete(tx, id);
    }
    stored.rows = left;
    stored.landed = Some(revision);
    tx.execute(
        "UPDATE config_conditional_save SET state = 'landed', saved = ?2 WHERE id = ?1",
        &[id.as_str().into(), document(&stored).as_str().into()],
    )?;
    Ok(())
}

/// The pull requests' values landed saves left in `environment`, until its next
/// Saved revision.
pub(crate) fn hints(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<PullRequestHint>, RpcError> {
    let latest = review::latest_saved(tx, environment)?.map(|saved| saved.revision);
    let rows = tx.query(
        "SELECT id, number, saved FROM config_conditional_save \
         WHERE environment_id = ?1 AND state = 'landed' ORDER BY saved_at, id",
        &[environment.as_str().into()],
    )?;
    let mut hints = Vec::new();
    for row in rows {
        let stored = row.json::<Stored>(2, "Conditional Save")?;
        if stored.landed != latest {
            continue;
        }
        let number = row.number(1, "Conditional Save")?;
        for saved in stored.rows {
            hints.push(PullRequestHint {
                save: row.parse(0, "Conditional Save ID")?,
                pull_request: number,
                row: saved.shown.row,
                value: saved.shown.from,
                landed: saved.landed.unwrap_or(Landed::Hint),
            });
        }
    }
    Ok(hints)
}

/// PR Environment `pr`'s save for Destination `into`: its ID, whether it still
/// stands, and how many rows it holds.
pub(crate) fn standing_in(
    tx: &mut dyn Tx,
    pr: &Environment,
    into: &EnvironmentId,
    target: Option<&BranchName>,
) -> Result<Option<(ConditionalSaveId, bool, usize)>, RpcError> {
    let rows = tx.query(
        "SELECT id, working_revision, target_branch, saved FROM config_conditional_save \
         WHERE pr_environment_id = ?1 AND environment_id = ?2 AND state = 'standing'",
        &[pr.summary.id.as_str().into(), into.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let held = row.text(2)?;
    let stands = u64::try_from(row.int(1)?).ok() == Some(pr.summary.revision.0)
        && target.is_some_and(|target| held == target.as_str());
    Ok(Some((
        row.parse(0, "Conditional Save ID")?,
        stands,
        row.json::<Stored>(3, "Conditional Save")?.rows.len(),
    )))
}

/// Core's comparison of the saved side into `into`, with what `into` uses live.
fn against(
    stored: &Stored,
    into: &SavedEnvironmentIntent,
    picks: Option<Vec<BranchPick>>,
) -> Result<BranchChanges, RpcError> {
    let landing = &stored.landing;
    let moving = Moving {
        source: stored.from.id.clone(),
        branch: stored.from.id.clone(),
        nothing: String::new(),
        from: landing.from.clone(),
        base: landing.base.clone(),
        provided: branch::used_live(into).into_keys().collect(),
        hostnames: landing.hostnames.clone(),
        way: Way::Save {
            parent: landing.parent.clone(),
            from_kept: false,
        },
    };
    moving.compare(into, picks)
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

fn load(tx: &mut dyn Tx, who: &Actor, id: &ConditionalSaveId) -> Result<Option<Found>, RpcError> {
    let rows = tx.query(
        "SELECT environment_id, state, number, saved FROM config_conditional_save \
         WHERE id = ?1 AND organization_id = ?2",
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Found {
        environment: row.parse(0, "Environment ID")?,
        state: row.variant(1, "Conditional Save")?,
        number: row.number(2, "Conditional Save")?,
        stored: row.json(3, "Conditional Save")?,
    }))
}

fn write(tx: &mut dyn Tx, id: &ConditionalSaveId, stored: &Stored) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_conditional_save SET saved = ?2 WHERE id = ?1",
        &[id.as_str().into(), document(stored).as_str().into()],
    )?;
    Ok(())
}

fn delete(tx: &mut dyn Tx, id: &ConditionalSaveId) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_conditional_save WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    Ok(())
}

fn document(stored: &Stored) -> String {
    serde_json::to_string(stored).expect("a Conditional Save is JSON")
}
