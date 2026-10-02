//! Syncing a PR Environment's changes conditionally, and taking a Conditional
//! Sync's rows.

use super::*;

/// Sync a PR Environment's picked changes (row keys) for one Destination at the
/// merge, replacing its Conditional Sync there; `picks: []` withdraws it.
pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &SyncChanges,
) -> Result<Synced, RpcError> {
    if request.close_after {
        return Err(error::invalid(
            "A PR Environment closes with its pull request: sync it without close_after",
            json!({}),
        ));
    }
    let sides = sides(tx, who, &request.from, request.into.as_ref(), true)?;
    let conditional_sync = match request.picks.as_ref().is_some_and(Vec::is_empty) {
        true => {
            withdraw_from(tx, &sides)?;
            None
        }
        false => {
            let moving = ready(tx, &sides)?;
            let changes = branch::reviewed(&moving, &sides.into, request.version.as_deref())?;
            let current = branch::version(&sides.into, &changes.review);
            let picks =
                branch::sync_picks(&moving, (&changes.rows, &current), request.picks.as_deref())?;
            Some(stand(tx, who, &sides, moving, (&changes, picks))?)
        }
    };
    Ok(Synced {
        from: sides.pr.summary,
        into: sides.into.summary,
        staged: Vec::new(),
        closing: false,
        conditional_sync,
    })
}

/// Withdraw the PR Environment's standing Conditional Sync into the Destination.
fn withdraw_from(tx: &mut dyn Tx, sides: &Sides) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_conditional_sync \
         WHERE pr_environment_id = ?1 AND environment_id = ?2 AND state = 'standing'",
        &[
            sides.pr.summary.id.as_str().into(),
            sides.into.summary.id.as_str().into(),
        ],
    )?;
    Ok(())
}

/// The comparison a Conditional Sync records, refused while the pull request is
/// closed or its PR Environment is going.
fn ready(tx: &mut dyn Tx, sides: &Sides) -> Result<Moving, RpcError> {
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
    moving(tx, sides)
}

/// Record `picks` of `changes` as the PR Environment's standing Conditional Sync
/// into the Destination, replacing the one there.
fn stand(
    tx: &mut dyn Tx,
    who: &Actor,
    sides: &Sides,
    moving: Moving,
    (changes, picks): (&BranchChanges, Vec<String>),
) -> Result<ConditionalSync, RpcError> {
    // Core refuses picks it couldn't land, such as a new Service's variable without it.
    moving.compare(&sides.into.working, Some(picks.clone()))?;
    let rows: Vec<Row> = changes
        .rows
        .iter()
        .filter(|row| picks.contains(&row.key.to_string()))
        .filter_map(|row| {
            let BranchRole::Move { conflict } = row.role else {
                return None;
            };
            let (name, from, into) = branch::shown_row(&moving, &sides.pr, &sides.into, row);
            Some(Row {
                key: row.key.to_string(),
                into: row.into.clone(),
                shown: Shown {
                    row: name,
                    conflict,
                    from,
                    into,
                },
                landed: None,
            })
        })
        .collect();
    let names = rows.iter().map(|row| row.shown.row.clone()).collect();
    let stored = Stored {
        rows,
        picks: picks.into_iter().map(|key| Pick { key }).collect(),
        carried: Carried::of(tx, &sides.pr.summary.id, &moving.from)?,
        landing: Landing {
            from: moving.from,
            base: moving.base,
            hostnames: moving.hostnames,
        },
        from: sides.pr.summary.clone(),
        landed: None,
    };
    withdraw_from(tx, sides)?;
    let id = ConditionalSyncId::parse(uuid::Uuid::new_v4().to_string())?;
    tx.execute(
        "INSERT INTO config_conditional_sync (id, organization_id, environment_id, state, \
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
    Ok(ConditionalSync {
        id,
        pull_request: sides.facts.number,
        rows: names,
        state: ConditionalSyncState::Standing,
    })
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
    if found.state != ConditionalSyncState::Landed || stored.landed != latest {
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
    let picks: Vec<String> = stored
        .picks
        .iter()
        .filter(|pick| chosen.contains(&pick.key))
        .map(|pick| pick.key.clone())
        .collect();
    let next = match picks.is_empty() {
        true => into.working.clone(),
        false => against(&stored, &into.working, Some(picks.clone()))?.next,
    };
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
    Ok(Taken {
        from: stored.from.clone(),
        into: into.summary,
        staged,
        conditional_sync: Some(ConditionalSync {
            id: id.clone(),
            pull_request: found.number,
            rows: stored
                .rows
                .iter()
                .map(|row| row.shown.row.clone())
                .collect(),
            state: ConditionalSyncState::Landed,
        }),
    })
}
