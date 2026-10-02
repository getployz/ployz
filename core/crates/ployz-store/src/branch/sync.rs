//! Sync: one Environment's changes, chosen row by row, into another of the same
//! Project as the receiver's changes to deploy, now or at a pull request's merge.
//! It never deletes and never deploys; a secret the receiver lacks arrives with the
//! value the person syncing gives it, or without one.

use super::*;
use crate::SealingKey;
use crate::pull_request::{PullRequest, PullRequestRef};
use crate::variables::{VariableKey, validate_text};

/// Sync one Environment's changes into another of its Project: staged in the
/// receiver's Working State now, or a Conditional Sync that lands with the pull
/// request's merge. Nothing is deleted, published or deployed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SyncChanges {
    /// The Environment whose changes sync.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// Where they land, in the same Project; omitted, the sender's Parent now, or a
    /// PR Environment's only Destination at the merge.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// `now` stages the changes; `at_merge` makes them a Conditional Sync that goes
    /// live with the pull request's merge, replacing the one standing there.
    pub when: When,
    /// Close the Branch once its changes landed in its Parent: refused for a kept
    /// Branch, and for a Sync into anything but its Parent.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub close_after: bool,
    /// Refused with `conflict` unless the Sync view is still at this version.
    pub version: String,
    /// The rows to sync, by [`SyncRow::row`] or by name (`web.image`); omitted,
    /// every row ticked. One left out is offered again next time.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub picks: Option<Vec<String>>,
    /// Rows not to sync, as `picks` names them.
    #[serde(default)]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub skip: Vec<String>,
    /// A value for each picked secret the receiver lacks: sealed at once, never
    /// shown back. At the merge it is held until then.
    #[serde(default)]
    #[ts(as = "Option<BTreeMap<RowId, String>>", optional)]
    pub values: BTreeMap<RowId, String>,
}

/// Read what a Sync would stage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SyncQuery {
    /// As [`SyncChanges::from`].
    #[serde(default)]
    pub from: EnvironmentRef,
    /// As [`SyncChanges::into`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// As [`SyncChanges::when`]: which Sync to read, so no default picks it.
    pub when: When,
}

/// The rows a Sync would carry, and the version that guards them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SyncView {
    /// Where the changes come from.
    pub from: EnvironmentSummary,
    /// Where they land.
    pub into: EnvironmentSummary,
    /// The pull request whose merge they go live with; none when staged now.
    pub at_merge: Option<PullRequestNumber>,
    /// Pass to [`SyncChanges::version`] to sync exactly these rows.
    pub version: String,
    /// Each row a Sync can carry. Settings each Environment keeps as its own
    /// (sizing, domains, generated addresses, the Git branch, Volume data) never are.
    pub rows: Vec<SyncRow>,
    /// The rows it would carry but that either side marked Never sync.
    pub never_synced: Vec<NeverSyncedRow>,
}

/// A row a Sync would carry but for Never sync.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NeverSyncedRow {
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    /// Where it is marked Never sync: unmark it there to sync it.
    pub marked_in: Vec<EnvironmentName>,
}

/// One row a Sync can carry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SyncRow {
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    pub change: SyncChange,
    /// The value that syncs; secrets read `{"secret": true}`.
    pub from: Value,
    /// The receiver's value now.
    pub into: Value,
    /// Synced unless left out: every row but one the sender only inherited from a
    /// Parent it isn't syncing into.
    pub ticked: bool,
    /// The row of the new node it is in, which syncs with it.
    pub requires: Option<RowId>,
    /// A secret's row.
    pub secret: Option<SecretRow>,
}

/// What a row does in the receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SyncChange {
    /// It brings what the receiver lacks.
    New,
    /// It replaces the receiver's value.
    Changed,
    /// The receiver changed it too since the two last shared: syncing overwrites that.
    Conflict,
}

/// A secret a Sync carries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SecretRow {
    /// The receiver lacks it: give [`SyncChanges::values`] one, or it arrives
    /// without one and the receiver's Deploy refuses until it has one.
    pub needs_value: bool,
    /// A value is held for it to land with at the merge.
    pub held: bool,
}

/// What a Sync staged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Synced {
    /// Pass to [`UndoSync::sync`] to undo it.
    pub sync: SyncId,
    /// Where the changes came from.
    pub from: EnvironmentSummary,
    /// Where they landed.
    pub into: EnvironmentSummary,
    /// Nodes staged in `into`'s Working State.
    pub staged: Vec<NodeName>,
    /// The Branch is closing, as [`SyncChanges::close_after`] asked.
    pub closing: bool,
    /// The Conditional Sync standing now, for a Sync at the merge.
    pub conditional_sync: Option<crate::ConditionalSync>,
}

/// Undo a Sync: what it staged goes back to what the receiver held, refused once
/// any of it deployed or changed since. A Conditional Sync still standing is
/// withdrawn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct UndoSync {
    /// As [`Synced::sync`].
    pub sync: SyncId,
}

/// A Sync undone.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Undone {
    /// The receiver now.
    pub into: EnvironmentSummary,
}

/// Where a Sync goes, and when it lands.
#[expect(clippy::large_enum_variant, reason = "one per command, never stored")]
pub(crate) enum Target {
    Now {
        from: Environment,
        into: Environment,
    },
    AtMerge {
        from: Environment,
        into: Environment,
        pr: PullRequest,
    },
}

pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &SyncChanges,
) -> Result<Synced, RpcError> {
    let side = (&request.from, request.into.as_ref());
    let (from, mut into) = match target(tx, who, side, request.when, true)? {
        Target::AtMerge { from, into, pr } => {
            return crate::conditional_sync::sync(tx, who, sealing, request, (from, into, pr));
        }
        Target::Now { from, into } => (from, into),
    };
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
    let sync = Move::sync(tx, &from, &into)?;
    let checked = sync.check(tx, &into, Guard::Sync(&request.version))?;
    let sides = [&from.working, &into.working];
    let picks = picks(checked.rows(), &sides, request)?;
    let values = sealed(sealing, checked.rows(), &sides, &picks, &request.values)?;
    let id = SyncId::parse(uuid::Uuid::new_v4().to_string())?;
    let staged = sync.apply(tx, who, &mut into, &checked, &picks, &values, Some(&id))?;
    if request.close_after {
        crate::pull_request::close(tx, who, &from.summary.id, &mut Default::default())?;
    }
    Ok(Synced {
        sync: id,
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
    let side = (&query.from, query.into.as_ref());
    let (from, into, pr) = match target(tx, who, side, query.when, false)? {
        Target::Now { from, into } => (from, into, None),
        Target::AtMerge { from, into, pr } => (from, into, Some(pr)),
    };
    let held = match &pr {
        Some(pr) => {
            crate::conditional_sync::held(tx, &into.summary.id, pr.repository_id, pr.number)?
        }
        None => BTreeMap::new(),
    };
    let sync = Move::sync(tx, &from, &into)?;
    let plan = sync.plan(&into.working);
    let (from_names, into_names) = (from.names(), into.names());
    let mut rows = Vec::new();
    let mut never_synced = Vec::new();
    for row in plan.rows() {
        let at = named(&[&from.working, &into.working], &row.id)
            .ok_or_else(|| error::corrupt("Sync row"))?;
        match row.verdict {
            Verdict::Moves {
                conflict,
                ticked,
                arrives,
            } => rows.push(SyncRow {
                change: match (conflict, &row.into) {
                    (true, _) => SyncChange::Conflict,
                    (false, Cell::Absent) => SyncChange::New,
                    (false, _) => SyncChange::Changed,
                },
                from: shown(&from.working, &from_names, &row.id, &row.from),
                into: shown(&into.working, &into_names, &row.id, &row.into),
                ticked,
                requires: row.requires.clone(),
                secret: (row.from.is_secret() || row.into.is_secret()).then(|| SecretRow {
                    needs_value: arrives == Arrives::NeedsValue,
                    held: held.contains_key(&row.id),
                }),
                at,
            }),
            Verdict::Differs(Why::NeverSynced) => never_synced.push(NeverSyncedRow {
                marked_in: [(&from, &sync.from_marks), (&into, &sync.into_marks)]
                    .into_iter()
                    .filter(|(_, marks)| marks.contains(&row.id))
                    .map(|(side, _)| side.summary.name.clone())
                    .collect(),
                at,
            }),
            Verdict::Differs(_) => {}
        }
    }
    Ok(SyncView {
        version: version(&into, &plan),
        at_merge: pr.map(|pr| pr.number),
        from: from.summary,
        into: into.summary,
        rows,
        never_synced,
    })
}

/// Where a Sync from `from` goes. Now: `into`, or omitted the sender's Parent. At
/// the merge: `from` must be a PR Environment and `into` one of its Destinations,
/// omitted its only one. `lock` takes the Project's lock, then both sides'.
pub(crate) fn target(
    tx: &mut dyn Tx,
    who: &Actor,
    (from, into): (&EnvironmentRef, Option<&EnvironmentRef>),
    when: When,
    lock: bool,
) -> Result<Target, RpcError> {
    let summary = scope::environment(tx, who, from)?.summary;
    let next = json!({ "next": format!("ployz env sync --to ENV --project {} --env {}", summary.project, summary.name) });
    let named = match into {
        Some(into) => Some(scope::environment(tx, who, into)?.summary),
        None => None,
    };
    let (into, pr) = match (when, named) {
        (When::AtMerge, named) => {
            let (into, pr) = destination(tx, who, &summary, named)?;
            (into, Some(pr))
        }
        (When::Now, Some(into)) => (into, None),
        (When::Now, None) => match row(tx, &summary.id)? {
            Some(row) => (scope::load_by_id(tx, &row.parent)?.summary, None),
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
    if lock {
        scope::lock_project(tx, &summary.id)?;
    }
    let (from, into) = scope::load_pair(tx, who, (&summary.id, &into.id), lock)?;
    Ok(match pr {
        Some(pr) => Target::AtMerge { from, into, pr },
        None => Target::Now { from, into },
    })
}

/// The Destination a PR Environment's Sync at the merge goes into: `named`, or its
/// only one; and its pull request.
fn destination(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &EnvironmentSummary,
    named: Option<EnvironmentSummary>,
) -> Result<(EnvironmentSummary, PullRequest), RpcError> {
    let Some(PullRequestRef {
        repository_id,
        number,
    }) = crate::pull_request::of(tx, &from.id)?
    else {
        return Err(error::invalid(
            format!(
                "{} is not a PR Environment: its changes sync now",
                from.name
            ),
            json!({}),
        ));
    };
    let pr = crate::pull_request::facts(tx, who, repository_id, number)?
        .ok_or_else(|| error::corrupt("pull request"))?;
    let project = scope::project_of(tx, &from.id)?.id;
    let destinations =
        crate::pull_request::destinations_of(tx, &project, repository_id, &pr.target_branch)?;
    let mut names = Vec::new();
    for id in &destinations {
        names.push(scope::load_by_id(tx, id)?.summary.name.to_string());
    }
    let into = match (named, destinations.as_slice()) {
        (Some(into), _) if destinations.contains(&into.id) => into,
        (Some(_), _) => {
            return Err(error::conflict(
                format!(
                    "That Environment doesn't deploy {}: sync into one that does",
                    pr.target_branch
                ),
                json!({ "valid_children": names }),
            ));
        }
        (None, [one]) => scope::load_by_id(tx, one)?.summary,
        (None, []) => {
            return Err(error::conflict(
                format!(
                    "Nothing deploys {} now: there is nowhere to sync into",
                    pr.target_branch
                ),
                json!({}),
            ));
        }
        (None, _) => {
            return Err(error::invalid(
                format!("Name the Destination: {}", names.join(", ")),
                json!({ "valid_children": names }),
            ));
        }
    };
    Ok((into, pr))
}

/// The rows `asked` names among `rows`: by row, or by name in either side.
fn resolve(
    rows: &[PlannedRow],
    sides: &[&SavedEnvironmentIntent; 2],
    asked: &str,
) -> Result<RowId, RpcError> {
    if let Ok(row) = asked.parse::<RowId>()
        && rows.iter().any(|planned| planned.id == row)
    {
        return Ok(row);
    }
    let labels: BTreeSet<(String, &RowId)> = rows
        .iter()
        .flat_map(|row| {
            sides
                .iter()
                .filter_map(|side| named(&[side], &row.id))
                .map(|named| (named.label(), &row.id))
        })
        .collect();
    let asked_as = SettingPath::parse(asked).map_or_else(|_| asked.to_owned(), |p| p.to_string());
    let found: BTreeSet<&RowId> = labels
        .iter()
        .filter(|(label, _)| *label == asked_as)
        .map(|(_, row)| *row)
        .collect();
    match found.len() {
        1 => Ok(found.into_iter().next().expect("one row").clone()),
        0 => Err(error::choices(
            format!("No change {asked} to sync"),
            asked,
            labels.iter().map(|(label, _)| label.as_str()),
        )),
        _ => {
            let rows: Vec<String> = found.iter().map(ToString::to_string).collect();
            Err(error::choices(
                format!("{asked} names more than one change: name its row"),
                asked,
                rows.iter().map(String::as_str),
            ))
        }
    }
}

/// The rows a Sync lands: those `request` picks, else every ticked one, less those
/// it skips.
pub(crate) fn picks(
    rows: &[PlannedRow],
    sides: &[&SavedEnvironmentIntent; 2],
    request: &SyncChanges,
) -> Result<BTreeSet<RowId>, RpcError> {
    let resolved = |asked: &[String]| -> Result<BTreeSet<RowId>, RpcError> {
        asked
            .iter()
            .map(|asked| resolve(rows, sides, asked))
            .collect()
    };
    let skip = resolved(&request.skip)?;
    Ok(match &request.picks {
        // Picked by hand, a row without its new node is refused, not dropped.
        Some(asked) => resolved(asked)?.difference(&skip).cloned().collect(),
        None => whole(
            rows,
            rows.iter()
                .filter(|row| matches!(row.verdict, Verdict::Moves { ticked: true, .. }))
                .map(|row| row.id.clone())
                .filter(|row| !skip.contains(row))
                .collect(),
        ),
    })
}

/// `values` sealed, each for a picked secret the receiver lacks.
pub(crate) fn sealed(
    sealing: &SealingKey,
    rows: &[PlannedRow],
    sides: &[&SavedEnvironmentIntent; 2],
    picks: &BTreeSet<RowId>,
    values: &BTreeMap<RowId, String>,
) -> Result<BTreeMap<RowId, Cell>, RpcError> {
    values
        .iter()
        .map(|(row, value)| {
            let label = named(sides, row).map_or_else(|| row.to_string(), |named| named.label());
            let needs = rows.iter().any(|planned| {
                planned.id == *row
                    && matches!(
                        planned.verdict,
                        Verdict::Moves {
                            arrives: Arrives::NeedsValue,
                            ..
                        }
                    )
            });
            let key = row.at();
            let key = key
                .strip_prefix("variables.")
                .filter(|_| needs && picks.contains(row));
            let Some(key) = key else {
                return Err(error::invalid(
                    format!("{label}: only a picked secret the receiver lacks takes a value"),
                    json!({ "row": row }),
                ));
            };
            if value.is_empty() {
                return Err(error::invalid(
                    format!("{label}: a secret needs a value"),
                    json!({ "row": row }),
                ));
            }
            validate_text(&VariableKey::parse(key)?, value)?;
            Ok((
                row.clone(),
                Cell::Secret {
                    fingerprint: sealing.fingerprint(value),
                    sealed: Some(sealing.seal(value)),
                },
            ))
        })
        .collect()
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

/// Undo Sync `request.sync` in its receiver, or withdraw the Conditional Sync it made.
pub(crate) fn undo(tx: &mut dyn Tx, who: &Actor, request: &UndoSync) -> Result<Undone, RpcError> {
    let id = &request.sync;
    let landed = tx.query(
        "SELECT environment_id FROM config_sync_arrival \
         WHERE sync_id = ?1 AND organization_id = ?2",
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    let missing = || error::not_found(format!("No Sync {id} to undo"), json!({}));
    let Some(receiver) = landed.first() else {
        let into = crate::conditional_sync::withdraw_one(tx, who, id)?.ok_or_else(missing)?;
        return Ok(Undone { into });
    };
    let receiver = receiver.parse::<EnvironmentId>(0, "Sync")?;
    scope::lock_project(tx, &receiver)?;
    let mut receiver = scope::lock_id(tx, who, &receiver)?;
    if !pair::undo(tx, &mut receiver, id)? {
        return Err(missing());
    }
    Ok(Undone {
        into: receiver.summary,
    })
}

/// Stage the hints `take` names, from a landed Conditional Sync or the Parent.
pub(crate) fn take(tx: &mut dyn Tx, who: &Actor, take: &Take) -> Result<Taken, RpcError> {
    match &take.from {
        HintSource::ConditionalSync(id) => crate::conditional_sync::take(tx, who, id, take),
        HintSource::Parent(parent) => follow::take(tx, who, parent, take),
    }
}
