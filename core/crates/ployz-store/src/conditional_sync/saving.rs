//! Syncing a PR Environment's changes conditionally, and taking a Conditional
//! Sync's rows.

use super::*;
use ployz_core::config::BranchPickChoice;

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

/// Save a PR Environment's picked changes for one Destination, replacing its
/// Conditional Sync there; `picks: []` withdraws it.
pub(crate) fn save(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &crate::SealingKey,
    request: &Save,
) -> Result<Moved, RpcError> {
    let sides = sides(tx, who, &request.from, request.into.as_ref(), true)?;
    if request.picks.as_ref().is_some_and(Vec::is_empty) {
        withdraw_from(tx, &sides)?;
        return Ok(Moved {
            branch: Some(branch::view(tx, &sides.pr)?),
            from: sides.pr.summary,
            into: sides.into.summary,
            staged: Vec::new(),
            conditional_sync: None,
        });
    }
    let moving = ready(tx, &sides)?;
    let changes = branch::reviewed(&moving, &sides.into, request.version.as_deref())?;
    let picks = branch::picks(
        &moving,
        &sides.into,
        sealing,
        &changes.rows,
        request.picks.as_deref(),
    )?;
    let conditional_sync = stand(tx, who, &sides, moving, (&changes, picks))?;
    Ok(Moved {
        branch: Some(branch::view(tx, &sides.pr)?),
        from: sides.pr.summary,
        into: sides.into.summary,
        staged: Vec::new(),
        conditional_sync: Some(conditional_sync),
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
    (changes, picks): (&BranchChanges, Vec<BranchPick>),
) -> Result<ConditionalSync, RpcError> {
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
                // Kept sealed in case the Destination gains the key before the merge:
                // core never moves a secret over one it holds, so it lands as a hint.
                secret: picked_secret(&moving, &picks, &row.key.to_string()),
            })
        })
        .collect();
    rows.extend(secret_hints(&moving, &sides.into));
    let names = rows.iter().map(|row| row.shown.row.clone()).collect();
    let parent = moving.parent().cloned();
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

/// The pull request's secret that row `key` moves, if it moves one.
fn sealed(moving: &Moving, key: &str) -> Option<SavedVariableIntent> {
    let (lineage, path) = key.split_once(':')?;
    let name = path.strip_prefix("variables.")?;
    moving
        .from
        .services
        .iter()
        .find(|service| service.lineage_id == lineage)?
        .variables
        .iter()
        .find(|variable| {
            variable.key == name && matches!(variable.value, SavedVariableValue::Secret { .. })
        })
        .cloned()
}

/// The sealed secret row `key` lands as its pick has it: the pull request's, or the
/// new value picked for it; none when it is left out or takes the Parent's.
fn picked_secret(moving: &Moving, picks: &[BranchPick], key: &str) -> Option<SavedVariableIntent> {
    let mut secret = sealed(moving, key)?;
    match &picks.iter().find(|pick| pick.key == key)?.choice {
        None | Some(BranchPickChoice::From) => {}
        Some(BranchPickChoice::New { value: Some(value) }) => {
            secret.value = value.value.clone();
            secret
                .value_fingerprint
                .clone_from(&value.value_fingerprint);
        }
        Some(
            BranchPickChoice::New { value: None }
            | BranchPickChoice::Parent
            | BranchPickChoice::LeaveOut,
        ) => return None,
    }
    Some(secret)
}

/// The secrets the PR Environment changed that the Destination holds too: core
/// moves none of them, so each is kept, sealed, to land as a hint.
pub(super) fn secret_hints(moving: &Moving, into: &Environment) -> Vec<Row> {
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

/// Stage the picked hints (omitted: every one) of a landed Conditional Sync in its
/// Destination: the pull request's value replaces the Destination's own edit.
pub(crate) fn take(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &ConditionalSyncId,
    take: &Take,
) -> Result<Moved, RpcError> {
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
    // A secret stages from its sealed value alone: core moves none over the Destination's.
    let sealed: BTreeSet<&str> = stored
        .rows
        .iter()
        .filter(|row| row.secret.is_some())
        .map(|row| row.key.as_str())
        .collect();
    let picks: Vec<BranchPick> = stored
        .picks
        .iter()
        .filter(|pick| chosen.contains(&pick.key) && !sealed.contains(pick.key.as_str()))
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

/// Give the Service of `key`'s lineage in `intent` the pull request's sealed secret,
/// keeping the variable's identity. False when that Service or variable is gone.
pub(super) fn put_secret(
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
