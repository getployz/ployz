//! Syncing a PR Environment's changes conditionally, and taking a Conditional
//! Sync's rows.

use super::*;

/// Sync a PR Environment's picked changes (row keys) for one Destination at the
/// merge, replacing its Conditional Sync there, or withdraw that one.
pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &SyncChanges,
    target: SyncTarget,
) -> Result<Synced, RpcError> {
    if request.close_after {
        return Err(error::invalid(
            "A PR Environment closes with its pull request: sync it without close_after",
            json!({}),
        ));
    }
    let conditional_sync = match request.when {
        Some(When::Withdraw) => {
            withdraw_from(tx, &target)?;
            None
        }
        _ => {
            ready(tx, &target)?;
            let sync = Comparison::sync(tx, &target.from, &target.into)?;
            let (changes, current) =
                branch::reviewed(&sync, &target.into, request.version.as_deref())?;
            let picks =
                branch::sync_picks(&sync, (&changes.rows, &current), request.picks.as_deref())?;
            Some(stand(tx, who, &target, sync, (&changes, picks))?)
        }
    };
    Ok(Synced {
        from: target.from.summary,
        into: target.into.summary,
        staged: Vec::new(),
        closing: false,
        conditional_sync,
    })
}

/// The pull request a Conditional Sync target waits for.
fn merge(target: &SyncTarget) -> Result<&PullRequest, RpcError> {
    target
        .merge
        .as_ref()
        .ok_or_else(|| error::internal("A Conditional Sync without its pull request"))
}

/// Withdraw the PR Environment's standing Conditional Sync into the Destination.
fn withdraw_from(tx: &mut dyn Tx, target: &SyncTarget) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_conditional_sync \
         WHERE pr_environment_id = ?1 AND environment_id = ?2 AND state = 'standing'",
        &[
            target.from.summary.id.as_str().into(),
            target.into.summary.id.as_str().into(),
        ],
    )?;
    Ok(())
}

/// Refuse a Conditional Sync while the pull request is closed or its PR
/// Environment is going.
fn ready(tx: &mut dyn Tx, target: &SyncTarget) -> Result<(), RpcError> {
    let facts = merge(target)?;
    if !facts.open {
        return Err(error::conflict(
            format!("PR #{} is closed", facts.number),
            json!({}),
        ));
    }
    let pr = &target.from;
    if pull_request::closing(tx, &pr.summary.id)? {
        return Err(error::conflict(
            format!("{} is closing", pr.summary.name),
            json!({}),
        ));
    }
    if let Some(removal) = teardown::removing(tx, &pr.summary.id)? {
        return Err(teardown::being_removed(pr, &removal));
    }
    Ok(())
}

/// `intent` without its secrets' values: they never land, so the stored document
/// keeps only their fingerprints, which comparisons read.
fn sealed_out(mut intent: SavedEnvironmentIntent) -> SavedEnvironmentIntent {
    for variable in intent
        .services
        .iter_mut()
        .flat_map(|service| &mut service.variables)
    {
        if let SavedVariableValue::Secret { encrypted_value } = &mut variable.value {
            *encrypted_value = None;
        }
    }
    intent
}

/// Record `picks` of `changes` as the PR Environment's standing Conditional Sync
/// into the Destination, replacing the one there.
fn stand(
    tx: &mut dyn Tx,
    who: &Actor,
    target: &SyncTarget,
    sync: Comparison,
    (changes, picks): (&BranchChanges, Vec<String>),
) -> Result<ConditionalSync, RpcError> {
    let facts = merge(target)?;
    // Core refuses picks it couldn't land, such as a new Service's variable without it.
    sync.compare(&target.into.working, Some(picks.clone()))?;
    let rows: Vec<Row> = changes
        .rows
        .iter()
        .filter(|row| picks.contains(&row.key.to_string()))
        .filter(|row| matches!(row.role, BranchRole::Move { .. }))
        .map(|row| {
            let (name, from, _) = branch::shown_row(&sync, &target.from, &target.into, row);
            Row {
                key: row.key.to_string(),
                into: row.into.clone(),
                shown: Shown { row: name, from },
                landed: None,
            }
        })
        .collect();
    let names = rows.iter().map(|row| row.shown.row.clone()).collect();
    let stored = Stored {
        rows,
        picks: picks.into_iter().map(|key| Pick { key }).collect(),
        carried: Carried::of(tx, &target.from.summary.id, &sync.from)?,
        landing: Landing {
            from: sealed_out(sync.from),
            base: sealed_out(sync.base),
            hostnames: sync.hostnames,
        },
        from: target.from.summary.clone(),
        landed: None,
    };
    withdraw_from(tx, target)?;
    let id = ConditionalSyncId::parse(uuid::Uuid::new_v4().to_string())?;
    tx.execute(
        "INSERT INTO config_conditional_sync (id, organization_id, environment_id, state, \
         pr_environment_id, repository_id, number, target_branch, working_revision, merge_commit, \
         saved_at, saved) VALUES (?1, ?2, ?3, 'standing', ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            target.into.summary.id.as_str().into(),
            target.from.summary.id.as_str().into(),
            facts.repository_id.into(),
            facts.number.into(),
            facts.target_branch.as_str().into(),
            scope::revision_param(target.from.summary.revision)?.into(),
            deployment::now().into(),
            document(&stored).as_str().into(),
        ],
    )?;
    Ok(ConditionalSync {
        id,
        pull_request: facts.number,
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
    let mut hints = Vec::new();
    for row in stored
        .rows
        .iter()
        .filter(|row| row.landed == Some(Landed::Hint))
    {
        let path = branch::path_of(&stored.landing.from, &into.working, &row.key)?;
        hints.push((path.to_string(), row.key.clone()));
    }
    let mut chosen = BTreeSet::new();
    match &take.rows {
        None => chosen.extend(hints.iter().map(|(_, key)| key.clone())),
        Some(asked) => {
            for asked in asked {
                let found: Vec<&String> = hints
                    .iter()
                    .filter(|(path, _)| covers(path, asked))
                    .map(|(_, key)| key)
                    .collect();
                if found.is_empty() {
                    return Err(error::choices(
                        format!("No hint {asked} to take"),
                        asked,
                        hints.iter().map(|(path, _)| path.as_str()),
                    ));
                }
                chosen.extend(found.into_iter().cloned());
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
