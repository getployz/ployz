//! Sync: one Environment's changes, chosen change by change, into another as the
//! receiver's changes to deploy. It never deletes, never deploys and never carries a
//! secret's value. For now the pair is a Branch and its Parent, the Branch sending.

use super::*;

/// Sync a Branch's changes into its Parent, staging them in the Parent's Working
/// State: the Branch's Working State, deployed or not. Nothing is deleted, published
/// or deployed.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SyncChanges {
    /// The Branch whose changes sync.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// Where they land; omitted, the Branch's Parent.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// The changes to sync, by [`SyncRow::key`]; omitted, every change ticked by
    /// default. One left out is offered again next time.
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
    /// A Sync without picks carries it.
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
}

pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &SyncChanges,
) -> Result<Synced, RpcError> {
    let mut sides = sides(
        tx,
        who,
        (&request.from, request.into.as_ref()),
        Direction::Save,
        true,
    )?;
    if let Some(removal) = crate::teardown::removing(tx, &sides.from.summary.id)? {
        return Err(crate::teardown::being_removed(&sides.from, &removal));
    }
    if request.close_after {
        closable(tx, &sides.from)?;
    }
    let moving = Moving::sync(tx, &sides.from, &sides.into)?;
    let changes = reviewed(&moving, &sides.into, request.version.as_deref())?;
    let current = version(&sides.into, &changes.review);
    let picks = picked(&moving, (&changes.rows, &current), request.picks.as_deref())?;
    let staged = moving.apply(tx, who, &mut sides.into, picks)?;
    if request.close_after {
        crate::pull_request::close(tx, who, &sides.from.summary.id, &mut Default::default())?;
    }
    Ok(Synced {
        from: sides.from.summary,
        into: sides.into.summary,
        staged,
        closing: request.close_after,
    })
}

pub(crate) fn sync_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &SyncQuery,
) -> Result<SyncView, RpcError> {
    let sides = sides(
        tx,
        who,
        (&query.from, query.into.as_ref()),
        Direction::Save,
        false,
    )?;
    let moving = Moving::sync(tx, &sides.from, &sides.into)?;
    let changes = moving.compare(&sides.into.working, None)?;
    let rows = changes
        .rows
        .iter()
        .filter(|row| matches!(row.role, BranchRole::Move { .. }))
        .map(|row| {
            let (label, from, into) = shown_row(&moving, &sides.from, &sides.into, row);
            let key = row.key.to_string();
            let (lineage, path) = split(&key);
            let node = node_of(&moving.from, lineage)
                .or_else(|| node_of(&sides.into.working, lineage))
                .ok_or_else(|| error::corrupt("Sync row"))?;
            Ok(SyncRow {
                new: path == "node" || (path.starts_with("variables.") && row.into.is_null()),
                secret: secret(row),
                changed: matches!(row.role, BranchRole::Move { conflict: true, .. }),
                ticked: ticked(row),
                key,
                node,
                label,
                from,
                into,
            })
        })
        .collect::<Result<_, RpcError>>()?;
    Ok(SyncView {
        version: version(&sides.into, &changes.review),
        from: sides.from.summary,
        into: sides.into.summary,
        rows,
    })
}

/// Whether a Sync without picks carries `row`: every change that moves.
pub(super) fn ticked(row: &BranchRow) -> bool {
    matches!(row.role, BranchRole::Move { .. })
}

/// Whether `row` moves a secret: core offers one only to a receiver that lacks it.
fn secret(row: &BranchRow) -> bool {
    matches!(&row.role, BranchRole::Move { choice: Some(choice), .. } if choice.secret)
}

/// Core's picks: the rows `asked` names by key, else every row ticked by default;
/// each variable lands with the sender's value, but a secret lands without one.
/// Nothing picked is refused with the `current` version to pick from.
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
            None => ticked(row),
        })
        .map(|(key, row)| {
            let choice = match &row.role {
                BranchRole::Move {
                    choice: Some(offered),
                    ..
                } => Some(if offered.secret {
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
