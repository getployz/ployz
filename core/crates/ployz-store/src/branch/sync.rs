//! Sync: one Environment's changes, chosen change by change, into another of the
//! same Project as the receiver's changes to deploy. It never deletes, never deploys
//! and never carries a secret's value.

use super::*;

/// Sync one Environment's changes into another of its Project, staging them in the
/// receiver's Working State: the sender's Working State, deployed or not. Nothing is
/// deleted, published or deployed.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SyncChanges {
    /// The Environment whose changes sync.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// Where they land, in the same Project; omitted, the sender's Parent.
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
    /// Close the Branch once its changes landed: refused for a kept Branch.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub close_after: bool,
    /// `now` stages the changes; `at_merge` makes them a Conditional Sync that goes
    /// live with the pull request's merge, and `picks: []` withdraws it. Omitted:
    /// `at_merge` from a PR Environment into one of its Destinations (`into`
    /// omitted, its only one), else `now`.
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
    /// As [`SyncChanges::into`]; from a PR Environment, omitted means its only
    /// Destination.
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
    /// `NODE`, or `NODE.path` for one of its settings or variables.
    pub label: String,
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

pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &SyncChanges,
) -> Result<Synced, RpcError> {
    let sides = (&request.from, request.into.as_ref());
    if crate::conditional_sync::at_merge(tx, who, sides, request.when)? {
        return crate::conditional_sync::sync(tx, who, request);
    }
    let (from, mut into) = pair(tx, who, sides, true)?;
    if let Some(removal) = crate::teardown::removing(tx, &from.summary.id)? {
        return Err(crate::teardown::being_removed(&from, &removal));
    }
    if request.close_after {
        closable(tx, &from)?;
    }
    let moving = Moving::sync(tx, &from, &into)?;
    let changes = reviewed(&moving, &into, request.version.as_deref())?;
    let current = version(&into, &changes.review);
    let picks = sync_picks(&moving, (&changes.rows, &current), request.picks.as_deref())?;
    let staged = moving.apply(tx, who, &mut into, picks)?;
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
    let sides = (&query.from, query.into.as_ref());
    if crate::conditional_sync::at_merge(tx, who, sides, None)? {
        return crate::conditional_sync::sync_view(tx, who, query);
    }
    let (from, into) = pair(tx, who, sides, false)?;
    let moving = Moving::sync(tx, &from, &into)?;
    sync_view_of(&moving, from, into, None)
}

/// What `moving` would stage from `from` into `into`, as the Sync view shows it;
/// `at_merge` once the pull request merges.
pub(crate) fn sync_view_of(
    moving: &Moving,
    from: Environment,
    into: Environment,
    at_merge: Option<PullRequestNumber>,
) -> Result<SyncView, RpcError> {
    let changes = moving.compare(&into.working, None)?;
    let rows = changes
        .rows
        .iter()
        .filter(|row| matches!(row.role, BranchRole::Move { .. }))
        .map(|row| {
            let (label, shown_from, shown_into) = shown_row(moving, &from, &into, row);
            let key = row.key.to_string();
            let (lineage, path) = split(&key);
            let node = node_of(&moving.from, lineage)
                .or_else(|| node_of(&into.working, lineage))
                .ok_or_else(|| error::corrupt("Sync row"))?;
            Ok(SyncRow {
                new: path == "node" || (path.starts_with("variables.") && row.into.is_null()),
                secret: secret(row) && !moving.carries_secrets(),
                changed: matches!(row.role, BranchRole::Move { conflict: true, .. }),
                ticked: moving.ticked(row),
                key,
                node,
                label,
                from: shown_from,
                into: shown_into,
            })
        })
        .collect::<Result<_, RpcError>>()?;
    let never_synced = changes
        .rows
        .iter()
        .filter(|row| {
            matches!(
                row.role,
                BranchRole::Differ {
                    why: BranchReason::NeverSynced
                }
            )
        })
        .map(|row| {
            let key = row.key.to_string();
            let (label, ..) = shown_row(moving, &from, &into, row);
            let node = node_of(&moving.from, split(&key).0)
                .or_else(|| node_of(&into.working, split(&key).0))
                .ok_or_else(|| error::corrupt("Sync row"))?;
            let marked_in = [&from, &into]
                .into_iter()
                .filter(|side| {
                    moving
                        .never_synced
                        .iter()
                        .any(|mark| mark.environment == side.summary.id && under(&key, &mark.key))
                })
                .map(|side| side.summary.name.clone())
                .collect();
            Ok(NeverSyncedRow {
                key,
                node,
                label,
                marked_in,
            })
        })
        .collect::<Result<_, RpcError>>()?;
    Ok(SyncView {
        version: version(&into, &changes.review),
        at_merge,
        from: from.summary,
        into: into.summary,
        rows,
        never_synced,
    })
}

/// A Sync's sender and receiver, the receiver omitted its Parent: two Environments
/// of one Project. `lock` locks both, in ID order.
fn pair(
    tx: &mut dyn Tx,
    who: &Actor,
    (from, into): (&EnvironmentRef, Option<&EnvironmentRef>),
    lock: bool,
) -> Result<(Environment, Environment), RpcError> {
    let from = scope::environment(tx, who, from)?;
    let summary = &from.summary;
    let into = match into {
        Some(into) => scope::environment(tx, who, into)?.summary,
        None => {
            let Some(row) = row(tx, &summary.id)? else {
                return Err(error::invalid(
                    format!("{} has no Parent: name where it syncs", summary.name),
                    json!({ "next": format!("ployz env sync --to ENV --project {} --env {}", summary.project, summary.name) }),
                ));
            };
            scope::load_by_id(tx, &row.parent)?.summary
        }
    };
    if into.project != summary.project {
        return Err(error::invalid(
            format!(
                "Sync stays within a Project: {} is in {}, {} in {}",
                summary.name, summary.project, into.name, into.project
            ),
            json!({ "next": format!("ployz env sync --to ENV --project {} --env {}", summary.project, summary.name) }),
        ));
    }
    if into.id == summary.id {
        return Err(error::invalid(
            format!("{} can't sync into itself", summary.name),
            json!({ "next": format!("ployz env sync --to ENV --project {} --env {}", summary.project, summary.name) }),
        ));
    }
    scope::load_pair(tx, who, (&summary.id, &into.id), lock)
}

/// Whether `row` moves a secret: core offers one only to a receiver that lacks it.
fn secret(row: &BranchRow) -> bool {
    matches!(&row.role, BranchRole::Move { choice: Some(choice), .. } if choice.secret)
}

/// Core's picks: the rows `asked` names by key, else every row ticked by default;
/// each variable lands with the sender's value, but a Sync's secret lands without
/// one. Nothing picked is refused with the `current` version to pick from.
pub(crate) fn sync_picks(
    moving: &Moving,
    (rows, current): (&[BranchRow], &str),
    asked: Option<&[String]>,
) -> Result<Vec<BranchPick>, RpcError> {
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
    let picks: Vec<BranchPick> = offered
        .into_iter()
        .filter(|(key, row)| match asked {
            Some(asked) => asked.contains(key),
            None => moving.ticked(row),
        })
        .map(|(key, row)| {
            let choice = match &row.role {
                BranchRole::Move {
                    choice: Some(offered),
                    ..
                } => Some(if offered.secret && !moving.carries_secrets() {
                    BranchPickChoice::New { value: None }
                } else {
                    BranchPickChoice::From
                }),
                BranchRole::Move { choice: None, .. } | BranchRole::Differ { .. } => None,
            };
            BranchPick { key, choice }
        })
        .collect();
    if picks.is_empty() {
        return Err(error::conflict(
            moving.nothing.clone(),
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
