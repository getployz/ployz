//! Sync: one Environment's changes, chosen change by change, into another of the
//! same Project as the receiver's changes to deploy. It never deletes and never
//! deploys.

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
    /// Pass to [`SyncChanges::version`] to sync exactly these changes.
    pub version: String,
    /// Each change a Sync can carry. Settings each Environment keeps as its own
    /// (sizing, domains, generated addresses, the Git branch, Volume data) never are.
    pub rows: Vec<SyncRow>,
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
}

pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &SyncChanges,
) -> Result<Synced, RpcError> {
    let (from, mut into) = pair(tx, who, (&request.from, request.into.as_ref()), true)?;
    if let Some(removal) = crate::teardown::removing(tx, &from.summary.id)? {
        return Err(crate::teardown::being_removed(&from, &removal));
    }
    if request.close_after {
        closable(tx, &from)?;
    }
    let moving = Moving::sync(tx, &from, &into)?;
    let changes = reviewed(&moving, &into, request.version.as_deref())?;
    let current = version(&into, &changes.review);
    let picks = picked(&moving, (&changes.rows, &current), request.picks.as_deref())?;
    let staged = moving.apply(tx, who, &mut into, picks)?;
    if request.close_after {
        crate::pull_request::close(tx, who, &from.summary.id, &mut Default::default())?;
    }
    Ok(Synced {
        from: from.summary,
        into: into.summary,
        staged,
        closing: request.close_after,
    })
}

pub(crate) fn sync_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &SyncQuery,
) -> Result<SyncView, RpcError> {
    let (from, into) = pair(tx, who, (&query.from, query.into.as_ref()), false)?;
    let moving = Moving::sync(tx, &from, &into)?;
    let changes = moving.compare(&into.working, None)?;
    let rows = changes
        .rows
        .iter()
        .filter(|row| matches!(row.role, BranchRole::Move { .. }))
        .map(|row| {
            let (label, shown_from, shown_into) = shown_row(&moving, &from, &into, row);
            let key = row.key.to_string();
            let (lineage, path) = split(&key);
            let node = node_of(&moving.from, lineage)
                .or_else(|| node_of(&into.working, lineage))
                .ok_or_else(|| error::corrupt("Sync row"))?;
            Ok(SyncRow {
                new: path == "node" || (path.starts_with("variables.") && row.into.is_null()),
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
    Ok(SyncView {
        version: version(&into, &changes.review),
        from: from.summary,
        into: into.summary,
        rows,
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

/// Core's picks: the rows `asked` names by key, else every row ticked by default;
/// each variable lands with the sender's value. Nothing picked is refused with the
/// `current` version to pick from.
fn picked(
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
            let variable = matches!(
                row.role,
                BranchRole::Move {
                    choice: Some(_),
                    ..
                }
            );
            BranchPick {
                key,
                choice: variable.then_some(BranchPickChoice::From),
            }
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
