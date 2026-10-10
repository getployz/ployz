//! Sync: one Environment's changes, chosen row by row, into another of the same
//! Project as the receiver's changes to deploy, now or at a pull request's merge.
//! It never deletes and never deploys; a secret the receiver lacks arrives with the
//! value the person syncing gives it, or without one.

use super::proposal::{self, Owners};
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
    /// Where they land, in the same Project; omitted, a PR Environment's only
    /// Destination at the merge, else the sender's Parent.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// When they land; omitted, at the merge from a PR Environment into one of its
    /// Destinations, else now.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub when: Option<When>,
    /// Refused with `conflict` unless the Sync view is still at this version.
    pub version: String,
    /// The rows to sync; omitted, every row ticked. One left out is offered again
    /// next time.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub picks: Option<Vec<RowRef>>,
    /// Rows left out of the picks, by RowId or by name in either side; a new node
    /// skipped takes its rows with it.
    #[serde(default)]
    #[ts(as = "Option<Vec<RowRef>>", optional)]
    pub skip: Vec<RowRef>,
    /// A value for each picked secret the receiver lacks: sealed at once, never
    /// shown back. At the merge it is held until then.
    #[serde(default)]
    #[ts(as = "Option<BTreeMap<RowRef, String>>", optional)]
    pub values: BTreeMap<RowRef, String>,
    /// The Sync's ID, so a retry is answered with what it did; omitted, a new one.
    /// Only a Sync now takes one.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub id: Option<SyncId>,
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
    /// As [`SyncChanges::when`]; `close_after` reads nothing different.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub when: Option<When>,
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
    /// `from` as it is included in `into`'s draft already; none when it isn't.
    pub proposal: Option<Included>,
}

/// A row a Sync would carry but for Never sync.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NeverSyncedRow {
    /// The row.
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    /// The marks keeping it from syncing: unmark them to sync it. A new node's row
    /// counts those on what it can't arrive without.
    pub marks: Vec<Mark>,
}

/// A row marked Never sync in an Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Mark {
    /// The Environment that marked it.
    pub environment: EnvironmentName,
    /// The marked row, as that Environment holds it.
    pub row: RowId,
}

/// One row a Sync can carry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SyncRow {
    /// The row.
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    /// What syncing it does there.
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
    /// The source whose included change the receiver holds at this row: it can't
    /// be synced until that is removed.
    pub held_by: Option<String>,
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

/// A secret a Sync carries without its value: give [`SyncChanges::values`] one, or
/// it arrives without one and the receiver's Deploy refuses until it has one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SecretRow {
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
    /// When they land.
    pub when: SyncedWhen,
}

/// When a Sync's changes land, as it decided.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncedWhen {
    /// Staged in the receiver now.
    Now {
        /// Nodes staged in `into`'s Working State.
        staged: Vec<NodeName>,
        /// The Branch is closing, as [`When::Now`] asked.
        closing: bool,
        /// The proposal that includes `from` in `into`'s draft.
        proposal: ProposalId,
    },
    /// Held for the pull request's merge.
    AtMerge {
        /// The Conditional Sync standing now.
        conditional_sync: crate::ConditionalSync,
    },
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
pub(crate) struct Target {
    from: EnvironmentId,
    into: EnvironmentId,
    lands: Lands,
}

/// When a Sync lands.
#[expect(clippy::large_enum_variant, reason = "one per command, never stored")]
enum Lands {
    Now { close_after: bool },
    AtMerge(PullRequest),
}

pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &SyncChanges,
) -> Result<Synced, RpcError> {
    let side = (&request.from, request.into.as_ref());
    let target = target(tx, who, side, request.when)?;
    scope::lock_project(tx, &target.from)?;
    let (from, mut into) = scope::load_pair(tx, who, (&target.from, &target.into), true)?;
    let close_after = match target.lands {
        Lands::AtMerge(pr) => {
            return crate::conditional_sync::sync(tx, who, sealing, request, (from, into, pr));
        }
        Lands::Now { close_after } => close_after,
    };
    let identity = proposal::identity(tx, &from)?;
    let mut found = proposal::find(tx, &into.summary.id, &identity)?;
    // A retry of the Sync that last included `from` is answered with what it did.
    if let Some(found) = &found
        && request.id.as_ref() == Some(&found.last_sync)
    {
        return proposal::receipt(tx, (from, into), found, close_after);
    }
    if let Some(id) = &request.id {
        proposal::refuse_reused_sync_id(tx, who, id)?;
    }
    if let Some(removal) = crate::teardown::removing(tx, &from.summary.id)? {
        return Err(crate::teardown::being_removed(&from, &removal));
    }
    if close_after {
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
    if let Some(found) = &mut found {
        proposal::rebind(tx, &into, found, &from)?;
    }
    let owners = proposal::owners(tx, &into.summary.id, found.as_ref().map(|found| &found.id))?;
    let sync = Move::include(tx, &from, &into, &owners)?;
    let checked = sync.check(tx, &into, Guard::Sync(&request.version))?;
    let sides = [&from.working, &into.working];
    let (picks, values) = picks(checked.rows(), &sides, request)?;
    proposal::refuse_held(checked.rows(), &sides, &picks, &owners)?;
    let values = sealed(sealing, checked.rows(), &sides, &picks, &values)?;
    let id = match &request.id {
        Some(id) => id.clone(),
        None => SyncId::parse(uuid::Uuid::new_v4().to_string())?,
    };
    let proposal = proposal::record(
        tx,
        who,
        &into.summary.id,
        &from,
        (found.as_ref(), &identity),
        &id,
    )?;
    let owner = Owner::Proposal {
        id: &proposal,
        sync: &id,
    };
    let before = proposal::credentials(tx, &into)?;
    let staged = checked.apply(tx, who, &mut into, &picks, &values, owner)?;
    proposal::keep_carried(tx, &into, &proposal, &before)?;
    if close_after {
        crate::pull_request::close(tx, who, &from.summary.id, &mut Default::default())?;
    }
    Ok(Synced {
        sync: id,
        from: from.summary,
        into: into.summary,
        when: SyncedWhen::Now {
            staged,
            closing: close_after,
            proposal,
        },
    })
}

pub(crate) fn sync_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &SyncQuery,
) -> Result<SyncView, RpcError> {
    let side = (&query.from, query.into.as_ref());
    let target = target(tx, who, side, query.when)?;
    let (from, into) = scope::load_pair(tx, who, (&target.from, &target.into), false)?;
    let pr = match target.lands {
        Lands::Now { .. } => None,
        Lands::AtMerge(pr) => Some(pr),
    };
    let held = match &pr {
        Some(pr) => crate::conditional_sync::held(tx, &into.summary.id, &pr.reference())?,
        None => BTreeMap::new(),
    };
    let (sync, found, owners) = match &pr {
        Some(_) => (Move::sync(tx, &from, &into)?, None, Owners::default()),
        None => proposal::planned(tx, &from, &into)?,
    };
    let plan = sync.plan(&into.working);
    let (from_names, into_names) = (from.names(), into.names());
    let mut rows = Vec::new();
    let mut never_synced = Vec::new();
    for row in plan.rows() {
        let at = named(&[&from.working, &into.working], &row.id)
            .ok_or_else(|| error::corrupt("Sync row"))?;
        match row.verdict {
            Verdict::Moves {
                conflict, ticked, ..
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
                    held: held.contains_key(&row.id),
                }),
                held_by: None,
                at,
            }),
            Verdict::Differs(Why::Included) => rows.push(SyncRow {
                change: match &row.into {
                    Cell::Absent => SyncChange::New,
                    into if *into != row.base => SyncChange::Conflict,
                    Cell::Value(_) | Cell::Secret { .. } | Cell::SecretWithoutValue => {
                        SyncChange::Changed
                    }
                },
                from: shown(&from.working, &from_names, &row.id, &row.from),
                into: shown(&into.working, &into_names, &row.id, &row.into),
                ticked: false,
                requires: row.requires.clone(),
                secret: (row.from.is_secret() || row.into.is_secret()).then(|| SecretRow {
                    held: held.contains_key(&row.id),
                }),
                held_by: owners.held.get(&row.id).cloned(),
                at,
            }),
            Verdict::Differs(Why::NeverSynced) => never_synced.push(NeverSyncedRow {
                marks: [
                    (&from, &sync.rules.from_marks),
                    (&into, &sync.rules.into_marks),
                ]
                .into_iter()
                .flat_map(|(side, marks)| {
                    marks_on(&row.id, marks).map(|row| Mark {
                        environment: side.summary.name.clone(),
                        row: row.clone(),
                    })
                })
                .collect(),
                at,
            }),
            Verdict::Differs(_) => {}
        }
    }
    let proposal = match found {
        Some(found) => proposal::included(tx, &into.summary.id)?
            .into_iter()
            .find(|included| included.proposal == found.id),
        None => None,
    };
    Ok(SyncView {
        proposal,
        version: version(&into, &plan),
        at_merge: pr.map(|pr| pr.number),
        from: from.summary,
        into: into.summary,
        rows,
        never_synced,
    })
}

/// Where a Sync from `from` goes, and when. It goes into `into`, else a PR
/// Environment's only Destination, else the sender's Parent; at the merge when that is
/// one of a PR Environment's Destinations and `when` doesn't say now, else now.
fn target(
    tx: &mut dyn Tx,
    who: &Actor,
    (from, into): (&EnvironmentRef, Option<&EnvironmentRef>),
    when: Option<When>,
) -> Result<Target, RpcError> {
    let summary = scope::environment(tx, who, from)?.summary;
    let next = json!({ "next": format!("ployz env sync --to ENV --project {} --env {}", summary.project, summary.name) });
    let merge = match when {
        Some(When::Now { .. }) => None,
        Some(When::AtMerge) | None => merge_of(tx, who, &summary)?,
    };
    let into = match (into, &merge) {
        (Some(into), _) => scope::environment(tx, who, into)?.summary,
        (None, Some((pr, destinations))) => destination(tx, pr, destinations)?,
        (None, None) => match row(tx, &summary.id)? {
            Some(row) => scope::load_by_id(tx, &row.parent)?.summary,
            None => {
                return Err(error::invalid(
                    format!("{} has no Parent: name where it syncs", summary.name),
                    next,
                ));
            }
        },
    };
    let destined = merge
        .as_ref()
        .is_some_and(|(_, destinations)| destinations.contains(&into.id));
    let lands = match (when, merge, destined) {
        (Some(When::AtMerge), None, _) => {
            return Err(error::invalid(
                format!(
                    "{} is not a PR Environment: its changes sync now",
                    summary.name
                ),
                json!({}),
            ));
        }
        (Some(When::AtMerge), Some((pr, destinations)), false) => {
            return Err(error::conflict(
                format!(
                    "That Environment doesn't deploy {}: sync into one that does",
                    pr.target_branch
                ),
                json!({ "valid_children": names(tx, &destinations)? }),
            ));
        }
        (Some(When::AtMerge) | None, Some((pr, _)), true) => Lands::AtMerge(pr),
        (Some(When::Now { close_after }), _, _) => Lands::Now { close_after },
        (None, _, _) => Lands::Now { close_after: false },
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
    Ok(Target {
        from: summary.id,
        into: into.id,
        lands,
    })
}

/// A PR Environment's pull request and the Environments that deploy its target
/// branch: its Destinations. None for any other Environment.
fn merge_of(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &EnvironmentSummary,
) -> Result<Option<(PullRequest, Vec<EnvironmentId>)>, RpcError> {
    let Some(PullRequestRef {
        repository_id,
        number,
    }) = crate::pull_request::of(tx, &from.id)?
    else {
        return Ok(None);
    };
    let pr = crate::pull_request::facts(tx, who, repository_id, number)?
        .ok_or_else(|| error::corrupt("pull request"))?;
    let project = scope::project_of(tx, &from.id)?.id;
    let destinations =
        crate::pull_request::destinations_of(tx, &project, repository_id, &pr.target_branch)?;
    Ok(Some((pr, destinations)))
}

/// The Destination of `pr` a Sync named nowhere goes into: its only one.
fn destination(
    tx: &mut dyn Tx,
    pr: &PullRequest,
    destinations: &[EnvironmentId],
) -> Result<EnvironmentSummary, RpcError> {
    match destinations {
        [one] => Ok(scope::load_by_id(tx, one)?.summary),
        [] => Err(error::conflict(
            format!(
                "Nothing deploys {} now: there is nowhere to sync into",
                pr.target_branch
            ),
            json!({}),
        )),
        _ => {
            let names = names(tx, destinations)?;
            Err(error::invalid(
                format!("Name the Destination: {}", names.join(", ")),
                json!({ "valid_children": names }),
            ))
        }
    }
}

fn names(tx: &mut dyn Tx, environments: &[EnvironmentId]) -> Result<Vec<String>, RpcError> {
    environments
        .iter()
        .map(|id| Ok(scope::load_by_id(tx, id)?.summary.name.to_string()))
        .collect()
}

/// The rows a Sync lands, and the values given for them: those `request` picks,
/// else every ticked one, less those it skips (each by RowId, or by name in either
/// side).
pub(crate) fn picks(
    rows: &[PlannedRow],
    sides: &[&SavedEnvironmentIntent; 2],
    request: &SyncChanges,
) -> Result<(BTreeSet<RowId>, BTreeMap<RowId, String>), RpcError> {
    let moves = rows.iter().filter(|row| {
        matches!(
            row.verdict,
            Verdict::Moves { .. } | Verdict::Differs(Why::Included)
        )
    });
    let named = named_in(sides, moves.map(|row| &row.id));
    let values = request
        .values
        .iter()
        .map(|(asked, value)| Ok((resolve_one(asked, &named)?, value.clone())))
        .collect::<Result<_, RpcError>>()?;
    let known: BTreeSet<RowId> = rows.iter().map(|row| row.id.clone()).collect();
    let skipped = chosen(Some(&request.skip), known.clone(), &named, "change")?;
    let picks = match request.picks.as_deref() {
        None => whole(
            rows,
            rows.iter()
                .filter(|row| matches!(row.verdict, Verdict::Moves { ticked: true, .. }))
                .map(|row| row.id.clone())
                .collect(),
        ),
        asked => chosen(asked, known, &named, "change")?,
    };
    // A new node skipped takes its rows; picked by hand, a row without its new node
    // is refused, not dropped.
    let landing = rows
        .iter()
        .filter(|row| picks.contains(&row.id) && !skipped.contains(&row.id))
        .filter(|row| {
            !row.requires
                .as_ref()
                .is_some_and(|node| skipped.contains(node))
        })
        .map(|row| row.id.clone())
        .collect();
    Ok((landing, values))
}

/// `values` sealed, each for a picked secret the receiver lacks.
pub(crate) fn sealed(
    sealing: &SealingKey,
    rows: &[PlannedRow],
    sides: &[&SavedEnvironmentIntent; 2],
    picks: &BTreeSet<RowId>,
    values: &BTreeMap<RowId, String>,
) -> Result<BTreeMap<RowId, SealedSecret>, RpcError> {
    values
        .iter()
        .map(|(row, value)| {
            let label =
                named(sides, row).map_or_else(|| row.to_string(), |named| named.to_string());
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
            if !(needs && picks.contains(row)) {
                return Err(error::invalid(
                    format!("{label}: only a picked secret the receiver lacks takes a value"),
                    json!({ "row": row }),
                ));
            }
            Ok((row.clone(), seal_secret(sealing, row, &label, value)?))
        })
        .collect()
}

/// `value`, sealed for secret `row` (labelled `label`), once it is one the row takes.
pub(crate) fn seal_secret(
    sealing: &SealingKey,
    row: &RowId,
    label: &str,
    value: &str,
) -> Result<SealedSecret, RpcError> {
    let At::Variable(key) = row.at() else {
        return Err(error::invalid(
            format!("{label} isn't a secret"),
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
    Ok(SealedSecret {
        fingerprint: sealing.fingerprint(value),
        value: sealing.seal(value),
    })
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
/// A Sync that included a proposal is undone by removing it, while it is that
/// proposal's only Sync.
pub(crate) fn undo(tx: &mut dyn Tx, who: &Actor, request: &UndoSync) -> Result<Undone, RpcError> {
    let id = &request.sync;
    if let Some(undone) = proposal::undo(tx, who, id)? {
        return Ok(undone);
    }
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
