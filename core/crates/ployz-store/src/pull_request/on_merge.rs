//! A PR Environment's On merge list: what its pull request's merge lands in each
//! Destination. The list is derived each time, a Sync over the Destination's Saved
//! State whose removals move too and whose rows land even where the Destination
//! changed them. Exclude is a Never sync mark on the PR Environment; only an Edit, the
//! value a row lands with in place of the PR Environment's, is stored.

use std::collections::BTreeMap;

use ployz_core::config::{
    Arrives, At, Cell, Cells, Plan, RowId, SavedVariableValue, SealedCell, Verdict, Why, marks_on,
    put_back,
};
use serde_json::Value;

use super::*;
use crate::SealingKey;
use crate::automation;
use crate::branch::{
    Carried, Move, NamedRow, RowRef, config, end_included, json_of, named, resolve_one, row_id,
    share, shown,
};
use crate::storage;
use crate::variables::{self, VariableKey};

/// Read a PR Environment's On merge list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct OnMergeQuery {
    /// The PR Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// What a PR Environment's pull request lands when it merges.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OnMerge {
    /// The PR Environment.
    pub environment: EnvironmentSummary,
    /// Its pull request.
    pub pull_request: PullRequestRef,
    /// One list per Destination: each Environment that deploys the target branch.
    pub destinations: Vec<OnMergeInto>,
}

/// What the merge lands in one Destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OnMergeInto {
    /// The Destination.
    pub name: EnvironmentName,
    /// Each row the PR Environment changed, riding or not.
    pub rows: Vec<MergeRow>,
}

/// One row of the On merge list. Values read as the Sync view's do: text, `{"secret":
/// true}`, `{"secret": false}` for a secret without a value, or null where the row has
/// none.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MergeRow {
    /// The row.
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    /// The Destination's Saved value now.
    pub production: Value,
    /// What lands: the Edit, else the PR Environment's value. Null for a removal; a
    /// secret the Destination lacks lands Not set unless edited.
    pub lands: Value,
    /// The PR Environment's own value.
    pub preview: Value,
    /// Whether the merge lands it.
    pub ride: Ride,
    /// The Destination changed it too since the PR Environment started: the merge
    /// lands over that.
    pub conflict: bool,
    /// The row of the new node it is in, which rides with it.
    pub requires: Option<RowId>,
}

/// Whether a row lands at the merge.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Ride {
    /// It lands with the PR Environment's value.
    Rides,
    /// It lands with the value an Edit gave it.
    Edited,
    /// The PR Environment marks it Never sync: it stays here.
    Excluded,
    /// The Destination marks it Never sync: it keeps its own value.
    ProductionKeeps,
}

impl Ride {
    pub(crate) const fn rides(self) -> bool {
        matches!(self, Self::Rides | Self::Edited)
    }
}

/// Give a row of the On merge list the value it lands with in one Destination, in
/// place of the PR Environment's, or take that Edit back. Only a variable that rides
/// takes one; a secret's is sealed at once and never shown back.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetMergeValue {
    /// The PR Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Destination; omitted, its only one.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// The row, by RowId or by name in either side.
    pub row: RowRef,
    /// The value it lands with; omitted, the Edit is taken back.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub value: Option<String>,
}

/// What a merge lands in one Destination, planned.
pub(crate) struct Landing {
    /// The Destination.
    pub(crate) into: Environment,
    pub(crate) plan: Plan,
    pub(crate) rows: Vec<MergeRow>,
}

impl Landing {
    /// The rows that land.
    pub(crate) fn riding(&self) -> BTreeSet<RowId> {
        self.rows
            .iter()
            .filter(|row| row.ride.rides())
            .map(|row| row.at.row.clone())
            .collect()
    }
}

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &OnMergeQuery,
) -> Result<OnMerge, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    listed(tx, who, environment)
}

/// The On merge list of PR Environment `environment`.
fn listed(tx: &mut dyn Tx, who: &Actor, environment: Environment) -> Result<OnMerge, RpcError> {
    let (pull_request, landings) = landings(tx, who, &environment)?;
    Ok(OnMerge {
        destinations: landings
            .into_iter()
            .map(|landing| OnMergeInto {
                name: landing.into.summary.name,
                rows: landing.rows,
            })
            .collect(),
        pull_request,
        environment: environment.summary,
    })
}

/// What `from`'s pull request lands in each of its Destinations.
pub(crate) fn landings(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &Environment,
) -> Result<(PullRequestRef, Vec<Landing>), RpcError> {
    let Some(pull_request) = of(tx, &from.summary.id)? else {
        return Err(error::invalid(
            format!("{} isn't a PR Environment", from.summary.name),
            json!({}),
        ));
    };
    let target = facts(tx, who, pull_request.repository_id, pull_request.number)?
        .map(|facts| facts.target_branch);
    let mut landings = Vec::new();
    if let Some(target) = target {
        let project = scope::project_of(tx, &from.summary.id)?.id;
        for into in destinations_of(tx, &project, pull_request.repository_id, &target)? {
            let into = scope::load_by_id(tx, &into)?;
            landings.push(landing(tx, from, into)?);
        }
    }
    Ok((pull_request, landings))
}

fn landing(tx: &mut dyn Tx, from: &Environment, into: Environment) -> Result<Landing, RpcError> {
    let saved = review::latest_saved(tx, &into.summary.id)?
        .map_or_else(|| into.working.clone(), |saved| saved.intent);
    let way = Move::merge(tx, from, &into)?;
    let plan = way.plan(&saved);
    let stored = edits(tx, &from.summary.id, &into.summary.id)?;
    let (from_names, into_names) = (from.names(), into.names());
    let mut rows = Vec::new();
    for row in plan.rows() {
        let (ride, conflict, arrives) = match &row.verdict {
            Verdict::Moves {
                ticked: true,
                conflict,
                arrives,
            } => {
                let edited = stored.contains_key(&row.id);
                let ride = if edited { Ride::Edited } else { Ride::Rides };
                (ride, *conflict, *arrives)
            }
            Verdict::Differs(Why::NeverSynced) => {
                let excluded = marks_on(&row.id, &way.rules.from_marks).next().is_some();
                let ride = if excluded {
                    Ride::Excluded
                } else {
                    Ride::ProductionKeeps
                };
                (ride, false, Arrives::AsIs)
            }
            Verdict::Moves { ticked: false, .. } | Verdict::Differs(_) => continue,
        };
        let at = named(&[&from.working, &saved], &row.id)
            .ok_or_else(|| error::corrupt("On merge row"))?;
        let preview = shown(&from.working, &from_names, &row.id, &row.from);
        let lands = match (stored.get(&row.id), arrives) {
            (Some(edit), _) if ride == Ride::Edited => shown_edit(edit, &into_names),
            (_, Arrives::NeedsValue) => json!({ "secret": false }),
            _ => preview.clone(),
        };
        rows.push(MergeRow {
            production: shown(&saved, &into_names, &row.id, &row.into),
            lands,
            preview,
            ride,
            conflict,
            requires: row.requires.clone(),
            at,
        });
    }
    // A row of a new node that stays here stays with it.
    let staying: BTreeSet<RowId> = rows
        .iter()
        .filter(|row| !row.ride.rides())
        .map(|row| row.at.row.clone())
        .collect();
    for row in &mut rows {
        if row.ride.rides()
            && row
                .requires
                .as_ref()
                .is_some_and(|node| staying.contains(node))
        {
            row.ride = Ride::Excluded;
        }
    }
    Ok(Landing { into, plan, rows })
}

/// An Edit as reads show it.
fn shown_edit(edit: &SealedCell, names: &BTreeMap<String, String>) -> Value {
    match edit {
        SealedCell::Secret(_) => json!({ "secret": true }),
        SealedCell::Cell(Cell::Value(value)) => {
            let mut value = value.clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("fingerprint");
            }
            serde_json::from_value::<SavedVariableValue>(value)
                .map_or(Value::Null, |value| variables::shown_value(&value, names))
        }
        SealedCell::Cell(_) => Value::Null,
    }
}

/// The Edits stored for the merge of `from` into `into`.
fn edits(
    tx: &mut dyn Tx,
    from: &EnvironmentId,
    into: &EnvironmentId,
) -> Result<BTreeMap<RowId, SealedCell>, RpcError> {
    tx.query(
        "SELECT lineage, at, cell FROM config_merge_value \
         WHERE environment_id = ?1 AND destination_id = ?2",
        &[from.as_str().into(), into.as_str().into()],
    )?
    .iter()
    .map(|row| Ok((row_id(row.text(0)?, row.text(1)?)?, row.json(2, "Edit")?)))
    .collect()
}

pub(crate) fn set_merge_value(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &SetMergeValue,
) -> Result<OnMerge, RpcError> {
    let from = scope::lock(tx, who, &request.environment)?;
    let (_, landings) = landings(tx, who, &from)?;
    let landing = match &request.into {
        Some(into) => {
            let into = scope::environment(tx, who, into)?.summary;
            landings
                .into_iter()
                .find(|landing| landing.into.summary.id == into.id)
                .ok_or_else(|| {
                    error::invalid(
                        format!(
                            "{}'s merge doesn't land in {}",
                            from.summary.name, into.name
                        ),
                        json!({}),
                    )
                })?
        }
        None => {
            let mut landings = landings.into_iter();
            match (landings.next(), landings.next()) {
                (Some(landing), None) => landing,
                (None, _) => {
                    return Err(error::invalid(
                        format!("{}'s merge lands nowhere", from.summary.name),
                        json!({}),
                    ));
                }
                (Some(_), Some(_)) => {
                    return Err(error::invalid(
                        format!(
                            "{}'s merge lands in more than one Environment: name one",
                            from.summary.name
                        ),
                        json!({}),
                    ));
                }
            }
        }
    };
    let named: Vec<NamedRow> = landing.rows.iter().map(|row| row.at.clone()).collect();
    let row = resolve_one(&request.row, &named)?;
    let label = named
        .iter()
        .find(|named| named.row == row)
        .map_or_else(|| row.to_string(), ToString::to_string);
    if !landing.riding().contains(&row) {
        return Err(error::invalid(
            format!("{label} doesn't land in {}", landing.into.summary.name),
            json!({ "row": row }),
        ));
    }
    let At::Variable(key) = row.at() else {
        return Err(error::invalid(
            format!("{label}: only a variable takes an Edit"),
            json!({ "row": row }),
        ));
    };
    let at = row.at().to_string();
    let key_values = [
        from.summary.id.as_str().into(),
        landing.into.summary.id.as_str().into(),
        row.lineage().into(),
        at.as_str().into(),
    ];
    tx.execute(
        "DELETE FROM config_merge_value \
         WHERE environment_id = ?1 AND destination_id = ?2 AND lineage = ?3 AND at = ?4",
        &key_values,
    )?;
    if let Some(value) = &request.value {
        let planned = landing
            .plan
            .rows()
            .iter()
            .find(|planned| planned.id == row)
            .ok_or_else(|| error::corrupt("On merge row"))?;
        let cell = if planned.from.is_secret() || planned.into.is_secret() {
            SealedCell::Secret(crate::branch::seal_secret(sealing, &row, &label, value)?)
        } else {
            let (parts, fingerprint) =
                variables::text_parts(&VariableKey::parse(key)?, value, &landing.into.names())?;
            let mut value = json!(variables::stored(parts));
            if let Some(object) = value.as_object_mut() {
                object.insert("fingerprint".into(), json!(fingerprint));
            }
            SealedCell::Cell(Cell::Value(value))
        };
        let [environment, destination, lineage, at] = key_values;
        tx.execute(
            "INSERT INTO config_merge_value \
             (environment_id, destination_id, organization_id, lineage, at, cell) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[
                environment,
                destination,
                who.organization.as_str().into(),
                lineage,
                at,
                json_of(&cell).as_str().into(),
            ],
        )?;
    }
    listed(tx, who, from)
}

/// Whether `id` is the PR Environment of an open pull request whose merge lands
/// something.
pub(crate) fn awaits(tx: &mut dyn Tx, who: &Actor, id: &EnvironmentId) -> Result<bool, RpcError> {
    let Some(pull_request) = of(tx, id)? else {
        return Ok(false);
    };
    let open = facts(tx, who, pull_request.repository_id, pull_request.number)?
        .is_some_and(|facts| facts.open);
    if !open {
        return Ok(false);
    }
    let from = scope::load_by_id(tx, id)?;
    let (_, landings) = landings(tx, who, &from)?;
    Ok(landings.iter().any(|landing| !landing.riding().is_empty()))
}

/// A pull request's merge, as the Store first learned of it.
pub(crate) struct Merging<'a> {
    /// The merge commit.
    pub(crate) commit: &'a CommitSha,
    /// The branch it merged into.
    pub(crate) target: &'a BranchName,
    pub(crate) trusted: &'a Trusted,
}

/// Land what PR Environment `from`'s pull request lands in each Destination, one
/// Saved revision each, and deploy the Services it changed there. A Destination
/// that can't take it is skipped and keeps what it had; returns whether every one
/// took it. The caller holds the locks.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &EnvironmentId,
    merge: &Merging<'_>,
    automated: &mut Automated,
) -> Result<bool, RpcError> {
    let from = scope::load_by_id(tx, from)?;
    let (pull_request, landings) = landings(tx, who, &from)?;
    let mut all = true;
    for landing in landings {
        let into = landing.into.summary.clone();
        let landed = storage::attempt(tx, |tx| {
            land_one(tx, who, &from, landing, pull_request.number)
        });
        match landed {
            Ok(services) if services.is_empty() => {}
            Ok(services) => automation::deploy_landed(
                tx,
                who,
                &into.id,
                (pull_request.repository_id, merge.target, merge.commit),
                services,
                merge.trusted,
                automated,
            )?,
            Err(error) if automation::skippable(&error) => {
                all = false;
                automated.skipped.push(Skipped {
                    environment: into.id,
                    reason: format!(
                        "Didn't land #{} in {}: {}",
                        pull_request.number, into.name, error.message
                    ),
                });
            }
            Err(error) => return Err(error),
        }
    }
    Ok(all)
}

/// Land `landing` in its Destination: Saved State takes every riding row, and
/// Working State each one it still holds as Saved did, so an edit staged there
/// stays and wins at the next Save. Returns the IDs of the Services it changed.
fn land_one(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &Environment,
    landing: Landing,
    number: PullRequestNumber,
) -> Result<Vec<String>, RpcError> {
    let picks = landing.riding();
    let Landing {
        into: mut destination,
        plan,
        ..
    } = landing;
    if picks.is_empty() {
        return Ok(Vec::new());
    }
    let into = destination.summary.id.clone();
    let values = edits(tx, &from.summary.id, &into)?;
    let applied = plan.apply(&picks, &values).map_err(config)?;
    let latest = review::latest_saved(tx, &into)?;
    let old = latest
        .as_ref()
        .map_or_else(|| destination.working.clone(), |saved| saved.intent.clone());
    let (saved, _) = scope::checked(
        tx,
        &destination.summary,
        &old,
        Some(&from.working),
        &applied.next,
    )?;
    review::publish(
        tx,
        who,
        &into,
        saved.clone(),
        latest.as_ref(),
        Some(&format!("Merged #{number}")),
    )?;

    // Working: the nodes arriving as Saved has them, so their IDs match; then each
    // row Working State holds as Saved did, with the variables Saved gained under
    // Saved's IDs.
    let arriving: BTreeSet<&str> = picks
        .iter()
        .filter(|pick| *pick.at() == At::Node)
        .map(RowId::lineage)
        .collect();
    let mut working = destination.working.clone();
    working.services.extend(
        saved
            .services
            .iter()
            .filter(|node| arriving.contains(node.lineage_id.as_str()))
            .cloned(),
    );
    working.volumes.extend(
        saved
            .volumes
            .iter()
            .filter(|node| arriving.contains(node.resource_lineage_id.as_str()))
            .cloned(),
    );
    working.configs.extend(
        saved
            .configs
            .iter()
            .filter(|node| arriving.contains(node.resource_lineage_id.as_str()))
            .cloned(),
    );
    let way = Move::merge(tx, from, &destination)?;
    let old_cells = Cells::of(&old, &way.hostnames.into);
    let mirror = way.plan(&working);
    let staged: BTreeSet<RowId> = mirror
        .rows()
        .iter()
        .filter(|row| {
            picks.contains(&row.id)
                && !arriving.contains(row.id.lineage())
                && matches!(row.verdict, Verdict::Moves { .. })
                && row.into == *old_cells.at(&row.id)
        })
        .map(|row| row.id.clone())
        .collect();
    if !staged.is_empty() {
        let next = mirror.apply(&staged, &values).map_err(config)?.next;
        working = with_variable_ids_of(&saved, &working, next);
    }
    let consumed = end_included(tx, &into, Some(&from.summary.id))?;
    if working != destination.working {
        let carried = Carried::of(tx, &from.summary.id, &from.working)?;
        crate::branch::land(
            tx,
            who,
            &mut destination,
            (&from.working, &carried),
            working,
            &staged,
        )?;
    } else if consumed {
        scope::persist_working(tx, &mut destination)?;
    }

    // The pair shares what landed: an Edit's value, not the PR Environment's.
    let mut base = applied.base;
    for landed in &applied.landed {
        if values.contains_key(&landed.row) {
            base = put_back(&base, &landed.row, &landed.value).map_err(config)?;
        }
    }
    share(tx, (&into, &from.summary.id), &base)?;
    tx.execute(
        "DELETE FROM config_merge_value WHERE environment_id = ?1 AND destination_id = ?2",
        &[from.summary.id.as_str().into(), into.as_str().into()],
    )?;
    let lineages: BTreeSet<&str> = picks.iter().map(RowId::lineage).collect();
    Ok(saved
        .services
        .iter()
        .filter(|service| lineages.contains(service.lineage_id.as_str()))
        .map(|service| service.id.clone())
        .collect())
}

/// `next` with each variable `before` lacked under the ID `saved` gave it (by
/// Service lineage and key), so Saved and Working State agree.
fn with_variable_ids_of(
    saved: &SavedEnvironmentIntent,
    before: &SavedEnvironmentIntent,
    mut next: SavedEnvironmentIntent,
) -> SavedEnvironmentIntent {
    for node in &mut next.services {
        let had: BTreeSet<&str> = before
            .services
            .iter()
            .find(|own| own.id == node.id)
            .map(|own| own.variables.iter().map(|v| v.id.as_str()).collect())
            .unwrap_or_default();
        let Some(in_saved) = saved
            .services
            .iter()
            .find(|own| own.lineage_id == node.lineage_id)
        else {
            continue;
        };
        for variable in &mut node.variables {
            if had.contains(variable.id.as_str()) {
                continue;
            }
            if let Some(own) = in_saved.variables.iter().find(|v| v.key == variable.key) {
                variable.id.clone_from(&own.id);
            }
        }
    }
    next
}

/// Forget the Edits of PR Environment `environment`: its pull request retargeted.
pub(crate) fn forget_edits(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_merge_value WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    Ok(())
}
