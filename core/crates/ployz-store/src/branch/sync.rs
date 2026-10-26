//! Sync: one Environment's changes, chosen row by row, into another of the same
//! Project as the receiver's changes to deploy. It never deletes and never deploys; a secret the receiver lacks arrives with the
//! value the person syncing gives it, or without one.

use uuid::Uuid;

use super::proposal;
use super::*;
use crate::variables::{VariableKey, refuse_template, validate_text};
use crate::{SecretValue, Trusted, client_sealed, old_cli};

/// Sync one Environment's changes into another of its Project, staged in the
/// receiver's Working State. Nothing is deleted, published or deployed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SyncChanges {
    /// The Environment whose changes sync.
    #[serde(default)]
    pub from: EnvironmentRef,
    /// Where they land, in the same Project; omitted, the sender's Parent.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// Close the Branch once its changes landed in its Parent: refused for a kept
    /// Branch, and for a Sync into anything but its Parent.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub close_after: bool,
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
    /// A value for each picked secret the receiver lacks: plain text sealed at once,
    /// never shown back, or a value the browser sealed for the receiver.
    #[serde(default)]
    #[ts(as = "Option<BTreeMap<RowRef, SecretValue>>", optional)]
    pub values: BTreeMap<RowRef, SecretValue>,
    /// The Sync's ID, so a retry is answered with what it did; omitted, a new one.
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
}

/// The rows a Sync would carry, and the version that guards them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SyncView {
    pub from: EnvironmentSummary,
    pub into: EnvironmentSummary,
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
/// it arrives Not set and the receiver's deploys skip it until it has one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SecretRow {}

/// What a Sync staged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Synced {
    /// Pass to [`UndoSync::sync`] to undo it.
    pub sync: SyncId,
    pub from: EnvironmentSummary,
    pub into: EnvironmentSummary,
    /// Nodes staged in `into`'s Working State.
    pub staged: Vec<NodeName>,
    /// The Branch is closing, as [`SyncChanges::close_after`] asked.
    pub closing: bool,
    /// The proposal that includes `from` in `into`'s draft.
    pub proposal: ProposalId,
}

/// Undo a Sync: what it staged goes back to what the receiver held, refused once
/// any of it deployed or changed since.
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

/// Where a Sync goes.
pub(crate) struct Target {
    from: EnvironmentId,
    into: EnvironmentId,
}

pub(crate) fn sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &SyncChanges,
    trusted: &Trusted,
) -> Result<Synced, RpcError> {
    let side = (&request.from, request.into.as_ref());
    let target = target(tx, who, side)?;
    scope::lock_project(tx, &target.from)?;
    let (from, mut into) = scope::load_pair(tx, who, (&target.from, &target.into), true)?;
    let identity = proposal::identity(tx, &from, &into.summary.id)?;
    let mut found = proposal::find(tx, &into.summary.id, &identity)?;
    let close_after = request.close_after;
    // A retry of the Sync that last included `from` is answered with what it did.
    if let Some(found) = &found
        && request.id.as_ref() == Some(&found.last_sync)
    {
        return proposal::receipt(tx, (from, into), found, close_after);
    }
    if let Some(id) = &request.id {
        proposal::refuse_reused_sync_id(tx, who, id)?;
    }
    // A shut-down PR Environment (its removal applied, no mark) keeps every row: it syncs.
    crate::removal::marked(tx, &from)?;
    crate::removal::marked(tx, &into)?;
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
    let asks = review::move_asks(tx, &into, trusted)?;
    if let Some(found) = &mut found {
        proposal::rebind(tx, &into, found, &from)?;
    }
    let owners = proposal::owners(tx, &into.summary.id, found.as_ref().map(|found| &found.id))?;
    let sync = Move::include(tx, &from, &into, &owners)?;
    let checked = sync.check(tx, &into, Guard::Sync(&request.version))?;
    let sides = [&from.working, &into.working];
    let (picks, values) = picks(checked.rows(), &sides, request)?;
    proposal::refuse_held(checked.rows(), &sides, &picks, &owners)?;
    let values = sealed(
        checked.rows(),
        [&from, &into],
        old_cli::Guard::from(trusted),
        &picks,
        &values,
    )?;
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
    let staged = checked.apply(tx, who, &mut into, &picks, &values, (owner, Some(trusted)))?;
    proposal::keep_carried(tx, &into, &proposal, &before)?;
    if close_after {
        trusted.writes(&from.summary.name)?;
        let because = crate::removal::mark::Closed::Synced;
        crate::removal::close(tx, who, &from.summary.id, because, &mut Default::default())?;
    }
    let fingerprints = values
        .iter()
        .filter_map(|(row, cell)| match cell {
            SealedCell::Secret(sealed) => Some((row, &sealed.fingerprint)),
            SealedCell::Cell(_) => None,
        })
        .collect::<BTreeMap<_, _>>();
    let asked = json!({
        "version": request.version,
        "picks": request.picks,
        "skip": request.skip,
        "values": fingerprints,
        "close_after": close_after,
    });
    review::approve_move(
        tx,
        who,
        &into,
        asks,
        review::Moving::Sync(&from.summary, &asked),
    )?;
    Ok(Synced {
        sync: id,
        from: from.summary,
        into: into.summary,
        staged,
        closing: close_after,
        proposal,
    })
}

pub(crate) fn sync_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &SyncQuery,
) -> Result<SyncView, RpcError> {
    let side = (&query.from, query.into.as_ref());
    let target = target(tx, who, side)?;
    let (from, into) = scope::load_pair(tx, who, (&target.from, &target.into), false)?;
    let (sync, found, owners) = proposal::planned(tx, &from, &into)?;
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
                secret: (row.from.is_secret() || row.into.is_secret()).then_some(SecretRow {}),
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
                secret: (row.from.is_secret() || row.into.is_secret()).then_some(SecretRow {}),
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
            Verdict::Differs(_) | Verdict::Kept => {}
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
        from: from.summary,
        into: into.summary,
        rows,
        never_synced,
    })
}

/// Where a Sync from `from` goes: into `into`, else the sender's Parent.
fn target(
    tx: &mut dyn Tx,
    who: &Actor,
    (from, into): (&EnvironmentRef, Option<&EnvironmentRef>),
) -> Result<Target, RpcError> {
    let summary = scope::environment(tx, who, from)?.summary;
    let next = json!({ "next": format!("ployz env sync --to ENV --project {} --env {}", summary.project, summary.name) });
    let into = match into {
        Some(into) => scope::environment(tx, who, into)?.summary,
        None => match row(tx, &summary.id)? {
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
    Ok(Target {
        from: summary.id,
        into: into.id,
    })
}

/// The rows a Sync lands, and the values given for them: those `request` picks,
/// else every ticked one, less those it skips (each by RowId, or by name in either
/// side).
pub(crate) fn picks(
    rows: &[PlannedRow],
    sides: &[&SavedEnvironmentIntent; 2],
    request: &SyncChanges,
) -> Result<(BTreeSet<RowId>, BTreeMap<RowId, SecretValue>), RpcError> {
    let named = named_in(sides, moving(rows));
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

/// The rows of `rows` a Sync may land: those that move, and those another
/// proposal included.
fn moving(rows: &[PlannedRow]) -> impl Iterator<Item = &RowId> {
    rows.iter()
        .filter(|row| {
            matches!(
                row.verdict,
                Verdict::Moves { .. } | Verdict::Differs(Why::Included)
            )
        })
        .map(|row| &row.id)
}

/// Where each of `rows` lands in a Sync of `from` into `into` at `version`: the
/// receiver's own Service, by ID, and the variable, resolved as the Sync resolves
/// them. Returns the receiver with the targets.
pub(crate) fn sealing_targets(
    tx: &mut dyn Tx,
    who: &Actor,
    side: (&EnvironmentRef, Option<&EnvironmentRef>),
    version: &str,
    rows: &[RowRef],
) -> Result<
    (
        EnvironmentSummary,
        Vec<crate::sealing_targets::SyncSealingTarget>,
    ),
    RpcError,
> {
    let target = target(tx, who, side)?;
    let (from, into) = scope::load_pair(tx, who, (&target.from, &target.into), false)?;
    let (sync, _, _) = proposal::planned(tx, &from, &into)?;
    let checked = sync.check(tx, &into, Guard::Sync(version))?;
    let sides = [&from.working, &into.working];
    let named = named_in(&sides, moving(checked.rows()));
    let values = rows
        .iter()
        .map(|asked| {
            let row = resolve_one(asked, &named)?;
            let label = label(&sides, &row);
            let refuse = |message: &str| {
                error::invalid(format!("{label}: {message}"), json!({ "row": row }))
            };
            let At::Variable(key) = row.at().clone() else {
                return Err(refuse("it isn't a secret"));
            };
            let service = receiving(&into, &row).ok_or_else(|| {
                let EnvironmentSummary { name, project, .. } = &into.summary;
                let set = format!("ployz set {label} --secret --env {name} --project {project}");
                error::invalid(
                    format!("{label}: {NOT_THERE}"),
                    json!({
                        "row": row,
                        "next": format!("sync without --value first, then {set}"),
                    }),
                )
            })?;
            let service = Uuid::parse_str(&service.id)
                .map_err(|_| refuse(crate::sealing_targets::UNBOUND))?;
            Ok(crate::sealing_targets::SyncSealingTarget {
                asked: asked.clone(),
                row,
                service,
                key,
                label,
            })
        })
        .collect::<Result<_, RpcError>>()?;
    Ok((into.summary, values))
}

/// `row` as reads show it in either side, else its RowId.
fn label(sides: &[&SavedEnvironmentIntent; 2], row: &RowId) -> String {
    named(sides, row).map_or_else(|| row.to_string(), |named| named.to_string())
}

/// The receiver's own Service that secret `row` lands in: what a client-sealed value
/// binds to, so it must be there already.
fn receiving<'receiver>(
    receiver: &'receiver Environment,
    row: &RowId,
) -> Option<&'receiver SavedServiceIntent> {
    receiver
        .working
        .services
        .iter()
        .find(|service| service.lineage_id == row.lineage())
}

/// Why a client-sealed value for a row can't land: its Service isn't in the receiver.
const NOT_THERE: &str = "its Service isn't there yet, so nothing can be sealed for it";

/// `values` sealed, each for a picked secret the receiver `into` lacks. Plain text
/// is refused, through `guard` for the CLI.
pub(crate) fn sealed(
    rows: &[PlannedRow],
    [from, into]: [&Environment; 2],
    guard: old_cli::Guard,
    picks: &BTreeSet<RowId>,
    values: &BTreeMap<RowId, SecretValue>,
) -> Result<BTreeMap<RowId, SealedCell>, RpcError> {
    let sides = [&from.working, &into.working];
    values
        .iter()
        .map(|(row, value)| {
            let label = label(&sides, row);
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
            let sealed = seal_secret(row, &label, value, into, guard)?;
            Ok((row.clone(), SealedCell::Secret(sealed)))
        })
        .collect()
}

/// `value` for secret `row` (labelled `label`) of `receiver`, once it is one the row
/// takes. Plain text is refused, through `guard` for the CLI. A client-sealed value
/// binds to the receiver's own Service, so it needs that Service there already.
pub(crate) fn seal_secret(
    row: &RowId,
    label: &str,
    value: &SecretValue,
    receiver: &Environment,
    guard: old_cli::Guard,
) -> Result<SealedSecret, RpcError> {
    let At::Variable(key) = row.at() else {
        return Err(error::invalid(
            format!("{label} isn't a secret"),
            json!({ "row": row }),
        ));
    };
    let key = VariableKey::parse(key)?;
    let refused =
        |message: &str| error::invalid(format!("{label}: {message}"), json!({ "row": row }));
    match value {
        SecretValue::Text(_) => {
            Err(guard.refuse(key.as_str(), refused(client_sealed::PLAINTEXT_REFUSED)))
        }
        SecretValue::ClientSealed(value) => {
            let service = receiving(receiver, row).ok_or_else(|| refused(NOT_THERE))?;
            let receiver = client_sealed::Receiver {
                environment: &receiver.summary.id,
                service,
            };
            client_sealed::accept(value.clone(), &key, &receiver, refused)
        }
    }
}

/// Refuse plain text `value` for secret `key` at `row` (labelled `label`), as a
/// Sync refuses it before sealing. A CLI that seals the text itself runs this first.
pub(crate) fn admit_secret_text(
    row: &RowId,
    label: &str,
    key: &VariableKey,
    value: &str,
) -> Result<(), RpcError> {
    if value.is_empty() {
        return Err(error::invalid(
            format!("{label}: a secret needs a value"),
            json!({ "row": row }),
        ));
    }
    validate_text(key, value)?;
    refuse_template(key, value)
}

/// Refuse to close a kept Branch, or one the Store can't remove.
fn closable(tx: &mut dyn Tx, branch: &Environment) -> Result<(), RpcError> {
    let summary = &branch.summary;
    if branch_row(tx, branch)?.kept {
        return Err(error::invalid(
            format!("{} is kept: stop keeping it to close it", summary.name),
            json!({ "next": crate::removal::next("keep_branch", json!({ "environment": { "project": summary.project, "environment": summary.name }, "kept": false })) }),
        ));
    }
    crate::removal::guard(tx, branch)
}

/// Undo Sync `request.sync` in its receiver. A Sync that included a proposal is
/// undone by removing it, while it is that proposal's only Sync.
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
        return Err(missing());
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

/// Stage the hints `take` names, from the Parent.
pub(crate) fn take(
    tx: &mut dyn Tx,
    who: &Actor,
    take: &Take,
    trusted: &Trusted,
) -> Result<Taken, RpcError> {
    let HintSource::Parent(parent) = &take.from;
    follow::take(tx, who, parent, take, trusted)
}
