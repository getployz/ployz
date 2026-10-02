//! Syncing a PR Environment's changes conditionally, withdrawing that, and taking
//! a landed Conditional Sync's hints.

use super::*;
use crate::branch::{Guard, Move, NamedRow, SyncChanges, Synced, SyncedWhen, Take, Taken};
use crate::id::SyncId;
use crate::{SealingKey, deployment, teardown};
use ployz_core::config::redact_environment_intent;

/// Sync a PR Environment's picked rows into one of its Destinations at the merge,
/// replacing its Conditional Sync there. The values given for its secrets are held
/// for the merge.
pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &SyncChanges,
    (from, into, pr): (Environment, Environment, PullRequest),
) -> Result<Synced, RpcError> {
    ready(tx, &from, &pr)?;
    let sync = Move::sync(tx, &from, &into)?;
    let checked = sync.check(tx, &into, Guard::Sync(&request.version))?;
    let sides = [&from.working, &into.working];
    let (picks, values) = branch::picks(checked.rows(), &sides, request)?;
    if picks.is_empty() {
        return Err(error::conflict(
            format!("Nothing to sync into {}", into.summary.name),
            json!({ "version": checked.version() }),
        ));
    }
    let values = branch::sealed(sealing, checked.rows(), &sides, &picks, &values)?;
    // Refuse what couldn't land, such as a new Service's variable without it.
    checked
        .plan()
        .apply(&picks, &values)
        .map_err(branch::config)?;
    let from_names = from.names();
    let mut kept = Vec::new();
    for row in checked.rows().iter().filter(|row| picks.contains(&row.id)) {
        kept.push(Pick {
            at: branch::named(&sides, &row.id).ok_or_else(|| error::corrupt("Sync row"))?,
            row: row.id.clone(),
            reviewed: row.into.clone(),
            from: branch::shown(&from.working, &from_names, &row.id, &row.from),
            staged: None,
        });
    }
    let rows = kept.iter().map(|pick| pick.at.clone()).collect();
    let stored = Stored {
        picks: kept,
        carried: Carried::of(tx, &from.summary.id, &sync.from)?,
        from: redact_environment_intent(sync.from),
        base: redact_environment_intent(sync.base),
        hostnames: sync.hostnames,
        environment: from.summary.clone(),
        landed: None,
    };
    withdraw_from(tx, &from.summary.id, &into.summary.id)?;
    let id = ConditionalSyncId::parse(uuid::Uuid::new_v4().to_string())?;
    tx.execute(
        "INSERT INTO config_conditional_sync (id, organization_id, environment_id, state, \
         pr_environment_id, repository_id, number, target_branch, working_revision, merge_commit, \
         saved_at, saved) VALUES (?1, ?2, ?3, 'standing', ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            into.summary.id.as_str().into(),
            from.summary.id.as_str().into(),
            pr.repository_id.into(),
            pr.number.into(),
            pr.target_branch.as_str().into(),
            scope::revision_param(from.summary.revision)?.into(),
            deployment::now().into(),
            document(&stored).as_str().into(),
        ],
    )?;
    for (row, cell) in &values {
        held::keep(
            tx,
            who,
            &into.summary.id,
            (pr.repository_id, pr.number),
            row,
            cell,
        )?;
    }
    Ok(Synced {
        sync: SyncId::parse(id.as_str())?,
        from: from.summary,
        into: into.summary,
        when: SyncedWhen::AtMerge {
            conditional_sync: ConditionalSync {
                id,
                pull_request: pr.number,
                rows,
                state: ConditionalSyncState::Standing,
            },
        },
    })
}

/// Withdraw the PR Environment's standing Conditional Sync into the Destination.
fn withdraw_from(
    tx: &mut dyn Tx,
    pr: &EnvironmentId,
    into: &EnvironmentId,
) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_conditional_sync \
         WHERE pr_environment_id = ?1 AND environment_id = ?2 AND state = 'standing'",
        &[pr.as_str().into(), into.as_str().into()],
    )?;
    Ok(())
}

/// Withdraw standing Conditional Sync `sync`, as Undo names it: its Destination, or
/// none when nothing stands by that ID.
pub(crate) fn withdraw_one(
    tx: &mut dyn Tx,
    who: &Actor,
    sync: &SyncId,
) -> Result<Option<EnvironmentSummary>, RpcError> {
    let id = ConditionalSyncId::parse(sync.as_str())?;
    let rows = tx.query(
        "SELECT environment_id FROM config_conditional_sync \
         WHERE id = ?1 AND organization_id = ?2 AND state = 'standing'",
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let into = scope::lock_id(tx, who, &row.parse::<EnvironmentId>(0, "Environment ID")?)?;
    delete(tx, &id)?;
    Ok(Some(into.summary))
}

/// Refuse a Conditional Sync while the pull request is closed or its PR
/// Environment is going.
fn ready(tx: &mut dyn Tx, from: &Environment, pr: &PullRequest) -> Result<(), RpcError> {
    if !pr.open {
        return Err(error::conflict(
            format!("PR #{} is closed", pr.number),
            json!({}),
        ));
    }
    if pull_request::closing(tx, &from.summary.id)? {
        return Err(error::conflict(
            format!("{} is closing", from.summary.name),
            json!({}),
        ));
    }
    if let Some(removal) = teardown::removing(tx, &from.summary.id)? {
        return Err(teardown::being_removed(from, &removal));
    }
    Ok(())
}

/// Stage the picked hints (omitted: every one) of a landed Conditional Sync in its
/// Destination: the pull request's value replaces the Destination's own edit.
pub(crate) fn take(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &ConditionalSyncId,
    take: &Take,
) -> Result<Taken, RpcError> {
    let missing = || error::not_found(format!("No Conditional Sync {id}"), json!({}));
    let found = load(tx, who, id)?.ok_or_else(missing)?;
    let mut into = scope::lock_id(tx, who, &found.environment)?;
    if let Some(at) = &take.into
        && scope::environment(tx, who, at)?.summary.id != into.summary.id
    {
        return Err(error::invalid(
            format!("Conditional Sync {id} landed in {}", into.summary.name),
            json!({}),
        ));
    }
    // Read again under the Destination's lock.
    let found = load(tx, who, id)?.ok_or_else(missing)?;
    review::check(&review::review(tx, &into)?, Some(&take.version))?;
    let latest = review::latest_saved(tx, &into.summary.id)?.map(|saved| saved.revision);
    let gone = || {
        error::conflict(
            "That value isn't there to use any more: read the diff again",
            json!({}),
        )
    };
    let mut stored = found.stored;
    if found.state != ConditionalSyncState::Landed || stored.landed != latest {
        return Err(gone());
    }
    let hints: Vec<NamedRow> = stored
        .picks
        .iter()
        .filter(|pick| pick.landed(&into.working, &stored.hostnames.into) == Landed::Hint)
        .map(|pick| pick.at.clone())
        .collect();
    let rows = hints.iter().map(|hint| hint.row.clone()).collect();
    let chosen = branch::chosen(take.rows.as_deref(), rows, &hints)?;
    let marks = marked(tx, &into.summary.id)?;
    let admitted = admit(&stored, &into.working, &marks, &BTreeMap::new(), |row| {
        chosen.contains(&row.id)
    })?;
    if admitted.picks.is_empty() {
        return Err(gone());
    }
    let cells = staged_cells(&admitted.applied);
    let staged = branch::land(
        tx,
        who,
        &mut into,
        (&stored.from, &stored.carried),
        admitted.applied.next,
        &admitted.picks,
    )?;
    for pick in &mut stored.picks {
        if let Some(cell) = cells.get(&pick.row) {
            pick.staged = Some(cell.clone());
        }
    }
    write(tx, id, &stored)?;
    Ok(Taken {
        from: stored.environment.clone(),
        into: into.summary,
        staged,
        conditional_sync: Some(ConditionalSync {
            id: id.clone(),
            pull_request: found.number,
            rows: stored.picks.iter().map(|pick| pick.at.clone()).collect(),
            state: ConditionalSyncState::Landed,
        }),
    })
}
