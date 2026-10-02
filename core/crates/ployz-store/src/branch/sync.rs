//! Sync: one Environment's changes, chosen change by change, into another of the
//! same Project as the receiver's changes to deploy. It never deletes, never deploys
//! and never carries a secret's value.

use super::*;
use crate::pull_request::{PullRequest, PullRequestRef};

/// Sync one Environment's changes into another of its Project, staging them in the
/// receiver's Working State: the sender's Working State, deployed or not. Nothing is
/// deleted, published or deployed.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SyncChanges {
    /// The Environment whose changes sync.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// Where they land, in the same Project; omitted, a PR Environment's only
    /// Destination (unless `when` is `now`), else the sender's Parent.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// The changes to sync, by [`SyncRow::key`]; omitted, every change ticked by
    /// default: all but what the sender only inherited from a Parent it isn't
    /// syncing into. One left out is offered again next time.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub picks: Option<Vec<String>>,
    /// Refuse with `conflict` unless the Sync view is still at this version.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
    /// Close the Branch once its changes landed in its Parent: refused for a kept
    /// Branch, and for a Sync into anything but its Parent.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub close_after: bool,
    /// `now` stages the changes; `at_merge` makes them a Conditional Sync that goes
    /// live with the pull request's merge; `withdraw` withdraws that Conditional
    /// Sync. Omitted: `at_merge` from a PR Environment into one of its Destinations,
    /// else `now`.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub when: Option<When>,
}

/// Read what a Sync would stage.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SyncQuery {
    /// As [`SyncChanges::from`].
    #[serde(default)]
    pub from: EnvironmentRef,
    /// As [`SyncChanges::into`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
}

/// The changes a Sync would stage, and the version that guards them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SyncView {
    /// Where the changes come from.
    pub from: EnvironmentSummary,
    /// Where they land.
    pub into: EnvironmentSummary,
    /// The pull request whose merge they go live with, as a Conditional Sync; none
    /// when they are staged now.
    pub at_merge: Option<PullRequestNumber>,
    /// Pass to [`SyncChanges::version`] to sync exactly these changes.
    pub version: String,
    /// Each change a Sync can carry. Settings each Environment keeps as its own
    /// (sizing, domains, generated addresses, the Git branch, Volume data) never are.
    pub rows: Vec<SyncRow>,
    /// The changes it would carry but that either side marked Never sync.
    pub never_synced: Vec<NeverSyncedRow>,
}

/// A change a Sync would carry but for Never sync.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NeverSyncedRow {
    /// As [`SyncRow::key`].
    pub key: String,
    /// As [`SyncRow::node`].
    pub node: NodeName,
    /// As [`SyncRow::label`].
    pub label: String,
    /// As [`SyncRow::path`].
    pub path: SettingPath,
    /// Where it is marked Never sync: unmark it there to sync it.
    pub marked_in: Vec<EnvironmentName>,
}

/// One change a Sync can carry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SyncRow {
    /// What [`SyncChanges::picks`] names it by; stable across renames.
    pub key: String,
    /// The Service or Volume it changes.
    pub node: NodeName,
    /// `NODE`, or `NODE.path` for one of its settings or variables: its name to show.
    pub label: String,
    /// The setting it changes as the receiver addresses it: what discard, Never
    /// sync and an edit of the receiver take.
    pub path: SettingPath,
    /// It is a whole Service or Volume, not one of its settings.
    pub whole: bool,
    /// The value that syncs; secrets read `{"secret": true}`.
    pub from: Value,
    /// The receiver's value now.
    pub into: Value,
    /// A Sync without picks carries it: every row, but for one the sender only
    /// inherited from a Parent it isn't syncing into.
    pub ticked: bool,
    /// The receiver changed it too since the two last shared: syncing it overwrites that.
    pub changed: bool,
    /// It brings a node, or a variable, the receiver lacks.
    pub new: bool,
    /// A secret the receiver lacks: it lands without a value, since a secret's value
    /// never syncs, and the receiver's Deploy refuses until it has one.
    pub secret: bool,
    /// A value is held for the secret to land with at the merge; never shown back.
    pub value_set: bool,
}

/// What a Sync staged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Synced {
    /// Where the changes came from.
    pub from: EnvironmentSummary,
    /// Where they landed.
    pub into: EnvironmentSummary,
    /// Nodes staged in `into`'s Working State.
    pub staged: Vec<NodeName>,
    /// The Branch is closing, as [`SyncChanges::close_after`] asked: it leaves the
    /// Servers, then is deleted.
    pub closing: bool,
    /// The Conditional Sync standing now, for a Sync at merge; none for a Sync now
    /// and once withdrawn.
    pub conditional_sync: Option<crate::ConditionalSync>,
}

/// A Sync's two sides, and the pull request whose merge it waits for when it is a
/// Conditional Sync.
pub(crate) struct SyncTarget {
    pub(crate) from: Environment,
    pub(crate) into: Environment,
    pub(crate) merge: Option<PullRequest>,
}

pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &SyncChanges,
) -> Result<Synced, RpcError> {
    let target = target(
        tx,
        who,
        (&request.from, request.into.as_ref()),
        request.when,
        true,
    )?;
    if target.merge.is_some() {
        return crate::conditional_sync::sync(tx, who, request, target);
    }
    let SyncTarget { from, mut into, .. } = target;
    if let Some(removal) = crate::teardown::removing(tx, &from.summary.id)? {
        return Err(crate::teardown::being_removed(&from, &removal));
    }
    if request.close_after {
        if row(tx, &from.summary.id)?.is_some_and(|row| row.parent != into.summary.id) {
            return Err(error::invalid(
                format!(
                    "{} closes only once it syncs into its Parent: sync without close_after",
                    from.summary.name
                ),
                json!({}),
            ));
        }
        closable(tx, &from)?;
    }
    let sync = Comparison::sync(tx, &from, &into)?;
    let (changes, current) = reviewed(&sync, &into, request.version.as_deref())?;
    let picks = sync_picks(&sync, (&changes.rows, &current), request.picks.as_deref())?;
    let staged = sync.apply(tx, who, &mut into, picks)?;
    if request.close_after {
        crate::pull_request::close(tx, who, &from.summary.id, &mut Default::default())?;
    }
    Ok(Synced {
        from: from.summary,
        into: into.summary,
        staged,
        closing: request.close_after,
        conditional_sync: None,
    })
}

pub(crate) fn sync_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &SyncQuery,
) -> Result<SyncView, RpcError> {
    let SyncTarget { from, into, merge } =
        target(tx, who, (&query.from, query.into.as_ref()), None, false)?;
    let held = match &merge {
        Some(facts) => crate::conditional_sync::held_rows(tx, &into.summary.id, facts)?,
        None => Vec::new(),
    };
    let sync = Comparison::sync(tx, &from, &into)?;
    let changes = sync.compare(&into.working, None)?;
    let shown = |row: &BranchRow| -> Result<(String, NodeName, SettingPath), RpcError> {
        let key = row.key.to_string();
        let lineage = split_row_key(&key).0;
        let node = node_of(&sync.from, lineage)
            .or_else(|| node_of(&into.working, lineage))
            .ok_or_else(|| error::corrupt("Sync row"))?;
        Ok((key.clone(), node, path_of(&sync.from, &into.working, &key)?))
    };
    let mut rows = Vec::new();
    let mut never_synced = Vec::new();
    for row in &changes.rows {
        match row.role {
            BranchRole::Move { conflict } => {
                let (key, node, path) = shown(row)?;
                let (label, from_value, into_value) = shown_row(&sync, &from, &into, row);
                let field = split_row_key(&key).1;
                let secret = row.secret();
                rows.push(SyncRow {
                    new: field == "node" || (field.starts_with("variables.") && row.into.is_null()),
                    whole: field == "node",
                    value_set: secret && held.contains(&key),
                    secret,
                    changed: conflict,
                    ticked: sync.ticked(row),
                    key,
                    node,
                    label,
                    path,
                    from: from_value,
                    into: into_value,
                });
            }
            BranchRole::Differ {
                why: BranchReason::NeverSynced,
            } => {
                let (key, node, path) = shown(row)?;
                let marked_in = [&from, &into]
                    .into_iter()
                    .filter(|side| {
                        sync.never_synced.iter().any(|mark| {
                            mark.environment == side.summary.id && covers(&key, &mark.key)
                        })
                    })
                    .map(|side| side.summary.name.clone())
                    .collect();
                never_synced.push(NeverSyncedRow {
                    label: sync.name(&into.working, &key),
                    key,
                    node,
                    path,
                    marked_in,
                });
            }
            BranchRole::Differ { .. } => {}
        }
    }
    Ok(SyncView {
        version: version(&sync, &into, &changes),
        at_merge: merge.map(|facts| facts.number),
        from: from.summary,
        into: into.summary,
        rows,
        never_synced,
    })
}

/// Where a Sync from `from` goes: `into` when named; omitted, a PR Environment's
/// only Destination unless asked `now`, else `from`'s Parent. It is a Conditional
/// Sync, waiting for the merge, when `from` is a PR Environment, `into` one of its
/// Destinations, and `when` isn't `now`. `lock` locks both sides, in ID order.
pub(crate) fn target(
    tx: &mut dyn Tx,
    who: &Actor,
    (from, into): (&EnvironmentRef, Option<&EnvironmentRef>),
    when: Option<When>,
    lock: bool,
) -> Result<SyncTarget, RpcError> {
    let summary = scope::environment(tx, who, from)?.summary;
    let next = json!({ "next": format!("ployz env sync --to ENV --project {} --env {}", summary.project, summary.name) });
    let facts = match crate::pull_request::of(tx, &summary.id)? {
        Some(PullRequestRef {
            repository_id,
            number,
        }) => Some(
            crate::pull_request::facts(tx, who, repository_id, number)?
                .ok_or_else(|| error::corrupt("pull request"))?,
        ),
        None => None,
    };
    let destinations = match &facts {
        Some(facts) => {
            let project = scope::project_of(tx, &summary.id)?.id;
            crate::pull_request::destinations_of(
                tx,
                &project,
                facts.repository_id,
                &facts.target_branch,
            )?
        }
        None => Vec::new(),
    };
    let names = |tx: &mut dyn Tx| -> Result<Vec<String>, RpcError> {
        destinations
            .iter()
            .map(|id| Ok(scope::load_by_id(tx, id)?.summary.name.to_string()))
            .collect()
    };
    let into = match (into, &facts) {
        (Some(into), _) => scope::environment(tx, who, into)?.summary,
        (None, Some(facts)) if when != Some(When::Now) => match destinations.as_slice() {
            [one] => scope::load_by_id(tx, one)?.summary,
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
                let names = names(tx)?;
                return Err(error::invalid(
                    format!("Name the Destination: {}", names.join(", ")),
                    json!({ "valid_children": names }),
                ));
            }
        },
        (None, _) => match row(tx, &summary.id)? {
            Some(row) => scope::load_by_id(tx, &row.parent)?.summary,
            None => {
                return Err(error::invalid(
                    format!("{} has no Parent: name where it syncs", summary.name),
                    next,
                ));
            }
        },
    };
    if into.project != summary.project {
        return Err(error::invalid(
            format!(
                "Sync stays within a Project: {} is in {}, {} in {}",
                summary.name, summary.project, into.name, into.project
            ),
            next,
        ));
    }
    if into.id == summary.id {
        return Err(error::invalid(
            format!("{} can't sync into itself", summary.name),
            next,
        ));
    }
    let at_merge = when != Some(When::Now) && destinations.contains(&into.id);
    if !at_merge && matches!(when, Some(When::AtMerge | When::Withdraw)) {
        return Err(match &facts {
            None => error::invalid(
                format!(
                    "{} is not a PR Environment: its changes sync now",
                    summary.name
                ),
                json!({}),
            ),
            Some(facts) => error::conflict(
                format!(
                    "That Environment doesn't deploy {}: sync into one that does",
                    facts.target_branch
                ),
                json!({ "valid_children": names(tx)? }),
            ),
        });
    }
    let (from, into) = scope::load_pair(tx, who, (&summary.id, &into.id), lock)?;
    Ok(SyncTarget {
        from,
        into,
        merge: facts.filter(|_| at_merge),
    })
}

/// Core's picks: the rows `asked` names by key, else every row ticked by default.
/// Nothing picked is refused with the `current` version to pick from.
pub(crate) fn sync_picks(
    sync: &Comparison,
    (rows, current): (&[BranchRow], &str),
    asked: Option<&[String]>,
) -> Result<Vec<String>, RpcError> {
    let offered: Vec<(String, &BranchRow)> = rows
        .iter()
        .filter(|row| matches!(row.role, BranchRole::Move { .. }))
        .map(|row| (row.key.to_string(), row))
        .collect();
    if let Some(unknown) = asked
        .into_iter()
        .flatten()
        .find(|key| !offered.iter().any(|(offered, _)| offered == *key))
    {
        return Err(error::choices(
            format!("No change {unknown} to sync"),
            unknown,
            offered.iter().map(|(key, _)| key.as_str()),
        ));
    }
    let picks: Vec<String> = offered
        .into_iter()
        .filter(|(key, row)| match asked {
            Some(asked) => asked.contains(key),
            None => sync.ticked(row),
        })
        .map(|(key, _)| key)
        .collect();
    if picks.is_empty() {
        return Err(error::conflict(
            sync.nothing.clone(),
            json!({ "version": current }),
        ));
    }
    Ok(picks)
}

/// Refuse to close a kept Branch, or one the Store can't remove.
fn closable(tx: &mut dyn Tx, branch: &Environment) -> Result<(), RpcError> {
    let summary = &branch.summary;
    if branch_row(tx, branch)?.kept {
        return Err(error::invalid(
            format!("{} is kept: stop keeping it to close it", summary.name),
            json!({ "next": format!("ployz env keep --off --project {} --env {}", summary.project, summary.name) }),
        ));
    }
    crate::teardown::guard(tx, branch)
}

/// Stage the hints `take` names, from a landed Conditional Sync or the Parent.
pub(crate) fn take(tx: &mut dyn Tx, who: &Actor, take: &Take) -> Result<Taken, RpcError> {
    match &take.from {
        HintSource::ConditionalSync(id) => crate::conditional_sync::take(tx, who, id, take),
        HintSource::Parent(parent) => follow::take(tx, who, parent, take),
    }
}

/// Compare `sync` into `into`, with that comparison's version, refusing unless
/// `asked`, if any, is still it.
pub(crate) fn reviewed(
    sync: &Comparison,
    into: &Environment,
    asked: Option<&str>,
) -> Result<(BranchChanges, String), RpcError> {
    let changes = sync.compare(&into.working, None)?;
    let current = version(sync, into, &changes);
    if asked.is_some_and(|asked| asked != current) {
        return Err(error::conflict(
            "Changed since you reviewed: review the sync again",
            json!({ "version": current }),
        ));
    }
    Ok((changes, current))
}

/// A Sync's guard: the receiver's revision, core's review of the changes and the
/// rows a Sync without picks carries.
fn version(sync: &Comparison, into: &Environment, changes: &BranchChanges) -> String {
    let ticked: Vec<String> = changes
        .rows
        .iter()
        .filter(|row| sync.ticked(row))
        .map(|row| row.key.to_string())
        .collect();
    let reviewed = format!("{}\n{}", changes.review, ticked.join("\n"));
    format!(
        "{}:{}",
        into.summary.revision,
        crate::removal::short_digest(&reviewed)
    )
}

/// A row's name and its two values as reads show them.
pub(crate) fn shown_row(
    sync: &Comparison,
    from: &Environment,
    into: &Environment,
    row: &BranchRow,
) -> (String, Value, Value) {
    let key = row.key.to_string();
    let (lineage, path) = split_row_key(&key);
    let variable = |intent: &SavedEnvironmentIntent, names| {
        let key = path.strip_prefix("variables.")?;
        let service = intent.services.iter().find(|s| s.lineage_id == lineage)?;
        let found = service.variables.iter().find(|v| v.key == key)?;
        Some(crate::variables::shown(found, names))
    };
    let (from_names, into_names) = (from.names(), into.names());
    let from_value =
        variable(&sync.from, &from_names).unwrap_or_else(|| shown(path, row.from.clone()));
    let into_value =
        variable(&into.working, &into_names).unwrap_or_else(|| shown(path, row.into.clone()));
    (sync.name(&into.working, &key), from_value, into_value)
}
